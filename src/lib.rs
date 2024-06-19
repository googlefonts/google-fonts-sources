//! Finding sources for Google Fonts fonts

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
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
static TOKEN_VAR: &str = "GH_TOKEN";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RepoInfo {
    font_name: String,
    repo_url: String,
    config_files: Vec<PathBuf>,
}

pub fn generate_sources_list(args: &Args) -> Vec<RepoInfo> {
    // before starting work make sure we have a github token:
    //let token = get_gh_token_or_die(args.gh_token_path.as_deref());
    let candidates = match args.repo_path.as_deref() {
        Some(path) => get_candidates_from_local_checkout(path),
        None => get_candidates_from_remote(),
    };

    let have_repo = pruned_candidates(&candidates);

    let has_config_files = if let Some(font_path) = args.fonts_dir.as_ref() {
        find_config_files(&have_repo, &font_path)
    } else {
        let tempdir = tempfile::tempdir().unwrap();
        find_config_files(&have_repo, tempdir.path())
    };

    println!(
        "{} of {} candidates have known repo url",
        have_repo.len(),
        candidates.len()
    );

    println!(
        "{} of {} have sources/config.yaml",
        has_config_files.len(),
        have_repo.len()
    );

    let mut repos: Vec<_> = have_repo
        .iter()
        .filter_map(|meta| {
            has_config_files.get(&meta.name).map(|configs| RepoInfo {
                font_name: meta.name.clone(),
                repo_url: meta.repo_url.clone().unwrap(),
                config_files: configs.clone(),
            })
        })
        .collect();

    repos.sort();
    repos

    // now what do we actually want to generate?
}

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

fn report_config_stats(configs: &BTreeMap<String, Vec<PathBuf>>) {
    let mut counts = HashMap::new();
    for (_, filenames) in configs {
        for filename in filenames {
            *counts.entry(filename).or_insert(0_usize) += 1;
        }
    }
    let mut tovec = counts.into_iter().collect::<Vec<_>>();
    tovec.sort_by_key(|(_, n)| *n);
    tovec.reverse();
    for (filename, count) in tovec {
        eprintln!("{count:<3} {}", filename.display())
    }
}

fn get_gh_token_or_die(token_path: Option<&Path>) -> String {
    match token_path {
        Some(path) => std::fs::read_to_string(path)
            .map(|s| s.trim().to_owned())
            .unwrap_or_die(|e| eprintln!("could not read file at {}: '{e}'", path.display())),
        None => std::env::var(TOKEN_VAR).unwrap_or_die(|_| {
            eprintln!("please provide a github auth token via --gh_token arg or GH_TOKEN env var")
        }),
    }
}

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
                    match config_file_name(&repo, checkout_font_dir) {
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

#[derive(Debug)]
enum ConfigFetchIssue {
    NoConfigFound,
    RateLimit(usize),
    BadRepoUrl(String),
    // contains stderr
    GitFail(String),
    Http(ureq::Error),
}

// error is number of seconds to wait if we're rate-limited
fn config_file_name(
    repo_url: &str,
    checkout_font_dir: &Path,
) -> Result<Vec<PathBuf>, ConfigFetchIssue> {
    let naive = has_config_file_naive(repo_url).map(|p| vec![p]);
    // if not found, try checking out and looking; otherwise return the result
    if !matches!(naive, Err(ConfigFetchIssue::NoConfigFound)) {
        naive
    } else {
        has_config_file_checkout(repo_url, checkout_font_dir)
    }
}

// just check for the presence of the most common file names
fn has_config_file_naive(repo_url: &str) -> Result<PathBuf, ConfigFetchIssue> {
    for filename in ["config.yaml", "config.yml"] {
        let config_url = format!("{repo_url}/tree/HEAD/sources/{filename}");
        let req = ureq::head(&config_url);

        match req.call() {
            Ok(resp) if resp.status() == 200 => return Ok(filename.into()),
            Ok(resp) => {
                eprintln!("{repo_url}: {}", resp.status());
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

fn has_config_file_checkout(
    repo_url: &str,
    checkout_font_dir: &Path,
) -> Result<Vec<PathBuf>, ConfigFetchIssue> {
    let Some((_, repo_name)) = repo_url.rsplit_once('/') else {
        return Err(ConfigFetchIssue::BadRepoUrl(repo_url.into()));
    };

    let out_path = checkout_font_dir.join(repo_name);
    if out_path.exists() {
        // should we always fetch? idk
    } else {
        std::fs::create_dir_all(&out_path).unwrap();
        clone_repo(repo_url, &out_path).map_err(ConfigFetchIssue::GitFail)?;
    }
    get_config_paths(&out_path).ok_or(ConfigFetchIssue::NoConfigFound)
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

    // if multiple exist just... take the shortest one?
    config_files.sort_by_key(|p| p.to_str().map(|s| s.len()).unwrap_or(usize::MAX));
    Some(config_files)
}

fn get_candidates_from_remote() -> BTreeSet<Metadata> {
    let tempdir = tempfile::tempdir().unwrap();
    clone_repo(GF_REPO_URL, tempdir.path())
        .unwrap_or_die(|e| eprintln!("failed to checkout {GF_REPO_URL}: '{e}'"));
    get_candidates_from_local_checkout(tempdir.path())
}

fn get_candidates_from_local_checkout(path: &Path) -> BTreeSet<Metadata> {
    let ofl_dir = path.join("ofl");
    let mut result = BTreeSet::new();
    for font_dir in iter_ofl_subdirectories(&ofl_dir) {
        let metadata = match load_metadata(&font_dir) {
            Ok(metadata) => metadata,
            Err(e) => {
                eprintln!("no metadata for font {}: '{}'", font_dir.display(), e);
                continue;
            }
        };
        result.insert(metadata);
    }
    result
}

fn load_metadata(path: &Path) -> Result<Metadata, MetadataError> {
    let meta_path = path.join(METADATA_FILE);
    let string = std::fs::read_to_string(meta_path).map_err(MetadataError::Read)?;
    string.parse().map_err(MetadataError::Parse)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naive_config() {
        assert!(has_config_file_naive("https://github.com/PaoloBiagini/Joan").is_ok());
        assert!(matches!(
            has_config_file_naive("https://github.com/googlefonts/bangers"),
            Err(ConfigFetchIssue::NoConfigFound)
        ));
    }
}
