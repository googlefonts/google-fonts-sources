//! Finding sources for Google Fonts fonts

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::channel,
        Arc,
    },
    time::Duration,
};

use kdam::{tqdm, BarExt};

mod args;
mod error;
mod metadata;

pub use args::Args;
use error::{MetadataError, UnwrapOrDie};
use metadata::Metadata;

static GF_REPO_URL: &str = "https://github.com/google/fonts";
static METADATA_FILE: &str = "METADATA.pb";

/// Information about a font repository
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct RepoInfo {
    /// The name of the repository.
    ///
    /// This is everything after the trailing '/' in e.g. "https://github.com/PaoloBiagini/Joan"
    pub repo_name: String,
    /// The repository's url
    pub repo_url: String,
    /// The names of config files that exist in this repository's source directory
    pub config_files: Vec<PathBuf>,
}

/// entry point for the cli tool
pub fn run(args: &Args) {
    let repos = discover_sources(
        args.repo_path.as_deref(),
        args.fonts_dir.as_deref(),
        args.verbose,
    );
    let output = if args.list {
        let urls = repos.into_iter().map(|r| r.repo_url).collect::<Vec<_>>();
        urls.join("\n")
    } else {
        serde_json::to_string_pretty(&repos)
            .unwrap_or_die(|e| eprintln!("failed to serialize repo info: '{e}'"))
    };

    if let Some(out) = args.out.as_ref() {
        std::fs::write(out, output).unwrap_or_die(|e| eprintln!("failed to write output: '{e}'"));
    } else {
        println!("{output}")
    }
}

/// Discover repositories containing font source files.
///
/// This looks at every font in the google/fonts github repo, looks to see if
/// we have a known upstream repository for that font, and then looks to see if
/// that repo contains a config.yaml file.
pub fn discover_sources(
    fonts_repo_path: Option<&Path>,
    sources_dir: Option<&Path>,
    verbose: bool,
) -> Vec<RepoInfo> {
    let candidates = match fonts_repo_path {
        Some(path) => get_candidates_from_local_checkout(path, verbose),
        None => get_candidates_from_remote(verbose),
    };

    let have_repo = pruned_candidates(&candidates);

    eprintln!(
        "checking {} repositories for config.yaml files",
        have_repo.len()
    );
    let has_config_files = if let Some(font_path) = sources_dir {
        find_config_files(&have_repo, &font_path)
    } else {
        let tempdir = tempfile::tempdir().unwrap();
        find_config_files(&have_repo, tempdir.path())
    };

    if verbose {
        eprintln!(
            "{} of {} candidates have known repo url",
            have_repo.len(),
            candidates.len()
        );

        eprintln!(
            "{} of {} have sources/config.yaml",
            has_config_files.len(),
            have_repo.len()
        );
    }

    let mut repos: Vec<_> = have_repo
        .iter()
        .filter_map(|meta| {
            has_config_files.get(&meta.name).map(|configs| RepoInfo {
                repo_name: meta
                    .repo_url
                    .as_deref()
                    .and_then(repo_name_from_url)
                    .expect("already checked")
                    .to_owned(),
                repo_url: meta.repo_url.clone().unwrap(),
                config_files: configs.clone(),
            })
        })
        .collect();

    repos.sort();
    repos
}

/// Returns the set of candidates that have a unique repository URL
fn pruned_candidates(candidates: &BTreeSet<Metadata>) -> BTreeSet<Metadata> {
    let mut seen_repos = HashSet::new();
    let mut result = BTreeSet::new();
    for metadata in candidates {
        let Some(url) = metadata.repo_url.as_ref() else {
            continue;
        };

        if seen_repos.insert(url) {
            result.insert(metadata.clone());
        }
    }
    result
}

/// for each font for which we have metadata, check remote repository for a config file.
///
/// By convention repositories containing sources we use should have a config file
/// in the sources/ directory.
///
/// This file is often called 'config.yaml', but it may be another name starting with
/// 'config' (because multiple such files can exist) and it may also use 'yml'
/// as an extension.
///
/// We naively look for the most common file names using a simple http request,
/// and if we don't find anything then we clone the repo locally and inspect
/// its contents.
fn find_config_files(
    fonts: &BTreeSet<Metadata>,
    checkout_font_dir: &Path,
) -> BTreeMap<String, Vec<PathBuf>> {
    let n_has_repo = fonts.iter().filter(|md| md.repo_url.is_some()).count();

    // messages sent from a worker thread
    enum Message {
        Finished { font: String, configs: Vec<PathBuf> },
        ErrorMsg(String),
        RateLimit(usize),
    }

    rayon::scope(|s| {
        let mut result = BTreeMap::new();
        let mut seen = 0;
        let mut sent = 0;
        let mut progressbar = kdam::tqdm!(total = n_has_repo);
        let rate_limited = Arc::new(AtomicBool::new(false));

        let (tx, rx) = channel();
        for (name, repo) in fonts
            .iter()
            .filter_map(|meta| meta.repo_url.as_ref().map(|repo| (&meta.name, repo)))
        {
            let repo = repo.clone();
            let name = name.clone();
            let tx = tx.clone();
            let rate_limited = rate_limited.clone();
            s.spawn(move |_| {
                loop {
                    // first, if we're currently rate-limited we spin:
                    while rate_limited.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    // then try to get configs (which may trigger rate limiting)
                    match config_files_for_repo(&repo, checkout_font_dir) {
                        Ok(configs) => {
                            tx.send(Message::Finished {
                                font: name,
                                configs,
                            })
                            .unwrap();
                            break;
                        }
                        Err(ConfigFetchIssue::NoConfigFound) => {
                            tx.send(Message::Finished {
                                font: name,
                                configs: Default::default(),
                            })
                            .unwrap();
                            break;
                        }
                        // if we're rate limited, set the flag telling other threads
                        // to spin, sleep, and then unset the flag
                        Err(ConfigFetchIssue::RateLimit(backoff)) => {
                            if !rate_limited.swap(true, Ordering::Acquire) {
                                tx.send(Message::RateLimit(backoff)).unwrap();
                                std::thread::sleep(Duration::from_secs(backoff as _));
                                rate_limited.store(false, Ordering::Release);
                            }
                        }
                        Err(e) => {
                            let msg = match e {
                                ConfigFetchIssue::BadRepoUrl(s) | ConfigFetchIssue::GitFail(s) => s,
                                ConfigFetchIssue::Http(e) => e.to_string(),
                                _ => unreachable!(), // handled above
                            };
                            tx.send(Message::ErrorMsg(msg)).unwrap();
                            break;
                        }
                    }
                }
            });
            sent += 1;
        }

        while seen < sent {
            match rx.recv() {
                Ok(Message::Finished { font, configs }) => {
                    if !configs.is_empty() {
                        result.insert(font, configs);
                    }
                    seen += 1;
                }
                Ok(Message::RateLimit(seconds)) => {
                    progressbar
                        .write(format!(
                            "rate limit hit, cooling down for {seconds} seconds"
                        ))
                        .unwrap();
                    let mut limit_progress = tqdm!(
                        total = seconds,
                        desc = "cooldown",
                        position = 1,
                        leave = false,
                        bar_format = "{desc}|{animation}| {count}/{total}"
                    );
                    for _ in 0..seconds {
                        std::thread::sleep(Duration::from_secs(1));
                        limit_progress.update(1).unwrap();
                    }
                }
                Ok(Message::ErrorMsg(msg)) => {
                    progressbar.write(msg).unwrap();
                    seen += 1;
                }
                Err(e) => {
                    eprintln!("channel error: '{e}'");
                    break;
                }
            }
            progressbar.update(1).unwrap();
        }
        result
    })
}

/// Conditions under which we fail to find a config.
///
/// different conditions are handled differently; NoConfigFound is fine,
/// RateLimit means we need to wait and retry, other things are errors we report
#[derive(Debug)]
enum ConfigFetchIssue {
    NoConfigFound,
    RateLimit(usize),
    BadRepoUrl(String),
    // contains stderr
    GitFail(String),
    Http(ureq::Error),
}

/// Checks for a config file in a given repo.
fn config_files_for_repo(
    repo_url: &str,
    checkout_font_dir: &Path,
) -> Result<Vec<PathBuf>, ConfigFetchIssue> {
    let repo_name = repo_name_from_url(repo_url)
        .ok_or_else(|| ConfigFetchIssue::BadRepoUrl(repo_url.into()))?;

    let local_repo_dir = checkout_font_dir.join(repo_name);
    // - if local repo already exists, then look there
    // - otherwise try naive http requests first,
    // - and then finally clone the repo and look
    let local_git_dir = local_repo_dir.join(".git");
    if local_git_dir.exists() {}

    let naive = config_file_from_remote_naive(repo_url).map(|p| vec![p]);
    // if not found, try checking out and looking; otherwise return the result
    if !matches!(naive, Err(ConfigFetchIssue::NoConfigFound)) {
        naive
    } else {
        config_files_from_local_checkout(repo_url, &local_repo_dir)
    }
}

// just check for the presence of the most common file names
fn config_file_from_remote_naive(repo_url: &str) -> Result<PathBuf, ConfigFetchIssue> {
    for filename in ["config.yaml", "config.yml"] {
        let config_url = format!("{repo_url}/tree/HEAD/sources/{filename}");
        let req = ureq::head(&config_url);

        match req.call() {
            Ok(resp) if resp.status() == 200 => return Ok(filename.into()),
            Ok(resp) => {
                // seems very unlikely but it feels bad to just skip this branch?
                eprintln!("unexpected response code for {repo_url}: {}", resp.status());
            }
            Err(ureq::Error::Status(404, _)) => (),
            Err(ureq::Error::Status(429, resp)) => {
                let backoff = resp
                    .header("Retry-After")
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(60);
                return Err(ConfigFetchIssue::RateLimit(backoff));
            }
            Err(e) => {
                return Err(ConfigFetchIssue::Http(e));
            }
        }
    }
    Err(ConfigFetchIssue::NoConfigFound)
}

fn config_files_from_local_checkout(
    repo_url: &str,
    local_repo_dir: &Path,
) -> Result<Vec<PathBuf>, ConfigFetchIssue> {
    if local_repo_dir.exists() {
        // should we always fetch? idk
    } else {
        std::fs::create_dir_all(local_repo_dir).unwrap();
        clone_repo(repo_url, local_repo_dir).map_err(ConfigFetchIssue::GitFail)?;
    }
    get_config_paths(local_repo_dir).ok_or(ConfigFetchIssue::NoConfigFound)
}

/// Look for a file like 'config.yaml' in a google fonts font checkout.
///
/// This will look for all files that begin with 'config' and have either the
/// 'yaml' or 'yml' extension; if multiple files match this pattern it will
/// return the one with the shortest name.
fn get_config_paths(font_dir: &Path) -> Option<Vec<PathBuf>> {
    #[allow(clippy::ptr_arg)] // we don't use &Path so we can pass this to a closure below
    fn looks_like_config_file(path: &PathBuf) -> bool {
        let (Some(stem), Some(extension)) =
            (path.file_stem().and_then(|s| s.to_str()), path.extension())
        else {
            return false;
        };
        stem.starts_with("config") && (extension == "yaml" || extension == "yml")
    }

    let sources_dir = font_dir.join("sources");
    let contents = std::fs::read_dir(sources_dir).ok()?;
    let mut config_files = contents
        .filter_map(|entry| {
            entry
                .ok()
                .and_then(|e| e.path().file_name().map(PathBuf::from))
        })
        .filter(looks_like_config_file)
        .collect::<Vec<_>>();

    config_files.sort_by_key(|p| p.to_str().map(|s| s.len()).unwrap_or(usize::MAX));
    Some(config_files)
}

fn get_candidates_from_remote(verbose: bool) -> BTreeSet<Metadata> {
    let tempdir = tempfile::tempdir().unwrap();
    if verbose {
        eprintln!("cloning {GF_REPO_URL} to {}", tempdir.path().display());
    }
    clone_repo(GF_REPO_URL, tempdir.path())
        .unwrap_or_die(|e| eprintln!("failed to checkout {GF_REPO_URL}: '{e}'"));
    get_candidates_from_local_checkout(tempdir.path(), verbose)
}

fn get_candidates_from_local_checkout(path: &Path, verbose: bool) -> BTreeSet<Metadata> {
    let ofl_dir = path.join("ofl");
    let mut result = BTreeSet::new();
    for font_dir in iter_ofl_subdirectories(&ofl_dir) {
        let metadata = match load_metadata(&font_dir) {
            Ok(metadata) => metadata,
            Err(e) => {
                if verbose {
                    eprintln!("no metadata for font {}: '{}'", font_dir.display(), e);
                }
                continue;
            }
        };
        result.insert(metadata);
    }
    result
}

fn load_metadata(path: &Path) -> Result<Metadata, MetadataError> {
    let meta_path = path.join(METADATA_FILE);
    Metadata::load(&meta_path)
}

fn iter_ofl_subdirectories(path: &Path) -> impl Iterator<Item = PathBuf> {
    let contents =
        std::fs::read_dir(path).unwrap_or_die(|e| eprintln!("failed to read ofl directory: '{e}'"));
    contents.filter_map(|entry| entry.ok().map(|d| d.path()).filter(|p| p.is_dir()))
}

// on fail returns contents of stderr
fn clone_repo(url: &str, to_dir: &Path) -> Result<(), String> {
    assert!(to_dir.exists());
    let output = std::process::Command::new("git")
        // if a repo requires credentials fail instead of waiting
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("clone")
        .args(["--depth", "1"])
        .arg(url)
        .arg(to_dir)
        .output()
        .expect("failed to execute git command");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.into_owned());
    }
    Ok(())
}

fn repo_name_from_url(url: &str) -> Option<&str> {
    let url = url.trim_end_matches('/');
    url.rsplit_once('/').map(|(_, tail)| tail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naive_config() {
        assert!(config_file_from_remote_naive("https://github.com/PaoloBiagini/Joan").is_ok());
        assert!(matches!(
            config_file_from_remote_naive("https://github.com/googlefonts/bangers"),
            Err(ConfigFetchIssue::NoConfigFound)
        ));
    }

    #[test]
    fn name_from_url() {
        assert_eq!(
            repo_name_from_url("https://github.com/hyper-type/hahmlet/"),
            Some("hahmlet"),
        );
        assert_eq!(
            repo_name_from_url("https://github.com/hyper-type/Advent"),
            Some("Advent"),
        );
    }
}
