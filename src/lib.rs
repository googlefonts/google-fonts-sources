//! Finding sources for Google Fonts fonts
//!
//! # basic usage:
//!
//! ```
//! // get a list of repositories:
//!
//! let font_repo_cache = Path::new("~/where_i_want_to_checkout_fonts");
//! let font_repos = google_fonts_sources::discover_sources(None, Some(font_repo_cache), false)
//!
//! // for each repo we find, do something with each source:
//!
//! for repo in &font_repos {
//!     let sources = match repo.get_sources(font_repo_cache) {
//!         Ok(sources) => sources,
//!         Err(e) => {
//!             eprintln!("skipping repo '{}': '{e}'", repo.repo_name);
//!             continue;
//!         }
//!     };
//!
//!     println!("repo '{}' contains sources {sources:?}", repo.repo_name);
//! }
//! ```

use std::{
    collections::{BTreeSet, HashSet},
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
mod config;
mod error;
mod metadata;
mod repo_info;

pub use args::Args;
pub use config::Config;
pub use error::{BadConfig, LoadRepoError};
use error::{GitFail, MetadataError, UnwrapOrDie};
use metadata::Metadata;
pub use repo_info::RepoInfo;

static GF_REPO_URL: &str = "https://github.com/google/fonts";
static METADATA_FILE: &str = "METADATA.pb";

type GitRev = String;

/// entry point for the cli tool
#[doc(hidden)] // only intended to be used from our binary
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
/// Returns a vec of `RepoInfo` structs describing repositories containing
/// known font sources.
///
/// This looks at every font in the google/fonts github repo, looks to see if
/// we have a known upstream repository for that font, and then looks to see if
/// that repo contains a config.yaml file.
///
/// The 'fonts_repo_path' is the path to a local checkout of the [google/fonts]
/// repository. If this is `None`, we will clone that repository to a tempdir.
///
/// The 'git_cache_dir' is the path to a directory where repositories will be
/// checked out, if necessary. Because we check out lots of repos (and it is
/// likely that the caller will want to check these out again later) it makes
/// sense to cache these in most cases.
///
/// [google/fonts]: https://github.com/google/fonts
pub fn discover_sources(
    fonts_repo_path: Option<&Path>,
    git_cache_dir: Option<&Path>,
    verbose: bool,
) -> Vec<RepoInfo> {
    let candidates = match fonts_repo_path {
        Some(path) => get_candidates_from_local_checkout(path, verbose),
        None => get_candidates_from_remote(verbose),
    };

    let have_repo = candidates_with_known_repo(&candidates);

    log::info!(
        "checking {} repositories for config.yaml files",
        have_repo.len()
    );
    let repos_with_config_files = if let Some(git_cache) = git_cache_dir {
        find_config_files(&have_repo, git_cache)
    } else {
        let tempdir = tempfile::tempdir().unwrap();
        find_config_files(&have_repo, tempdir.path())
    };

    if verbose {
        log::debug!(
            "{} of {} candidates have known repo url",
            have_repo.len(),
            candidates.len()
        );

        log::debug!(
            "{} of {} have sources/config.yaml",
            repos_with_config_files.len(),
            have_repo.len()
        );
    }

    repos_with_config_files
}

/// Returns the set of candidates that have a unique repository URL
fn candidates_with_known_repo(candidates: &BTreeSet<Metadata>) -> BTreeSet<Metadata> {
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
fn find_config_files(fonts: &BTreeSet<Metadata>, git_cache_dir: &Path) -> Vec<RepoInfo> {
    let n_has_repo = fonts.iter().filter(|md| md.repo_url.is_some()).count();

    // messages sent from a worker thread
    enum Message {
        Finished(Option<RepoInfo>),
        ErrorMsg(String),
        RateLimit(usize),
    }

    rayon::scope(|s| {
        let mut result = Vec::new();
        let mut seen = 0;
        let mut sent = 0;
        let mut progressbar = kdam::tqdm!(total = n_has_repo);
        let rate_limited = Arc::new(AtomicBool::new(false));

        let (tx, rx) = channel();
        for repo_url in fonts.iter().filter_map(|meta| meta.repo_url.clone()) {
            let tx = tx.clone();
            let rate_limited = rate_limited.clone();
            s.spawn(move |_| {
                loop {
                    // first, if we're currently rate-limited we spin:
                    while rate_limited.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_secs(1));
                    }
                    // then try to get configs (which may trigger rate limiting)
                    match config_files_and_rev_for_repo(&repo_url, git_cache_dir) {
                        Ok((config_files, rev)) if !config_files.is_empty() => {
                            let info = RepoInfo::new(repo_url, rev, config_files);
                            tx.send(Message::Finished(info)).unwrap();
                            break;
                        }
                        // no configs found or looking for configs failed:
                        Err(ConfigFetchIssue::NoConfigFound) | Ok(_) => {
                            tx.send(Message::Finished(None)).unwrap();
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
                                ConfigFetchIssue::BadRepoUrl(s) => s,
                                ConfigFetchIssue::GitFail(e) => e.to_string(),
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
                Ok(Message::Finished(info)) => {
                    if let Some(info) = info {
                        result.push(info);
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
                    log::error!("channel error: '{e}'");
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
    GitFail(GitFail),
    Http(Box<ureq::Error>),
}

/// Checks for a config file in a given repo; also returns git rev
fn config_files_and_rev_for_repo(
    repo_url: &str,
    checkout_font_dir: &Path,
) -> Result<(Vec<PathBuf>, GitRev), ConfigFetchIssue> {
    let local_repo_dir = repo_info::repo_path_for_url(repo_url, checkout_font_dir)
        .ok_or_else(|| ConfigFetchIssue::BadRepoUrl(repo_url.to_owned()))?;
    // - if local repo already exists, then look there
    // - otherwise try naive http requests first,
    // - and then finally clone the repo and look
    let local_git_dir = local_repo_dir.join(".git");
    if local_git_dir.exists() {
        let rev = get_git_rev(&local_repo_dir).map_err(ConfigFetchIssue::GitFail)?;
        let configs = get_config_paths(&local_repo_dir).ok_or(ConfigFetchIssue::NoConfigFound)?;
        return Ok((configs, rev));
    }

    let naive = config_file_and_rev_from_remote_naive(repo_url).map(|(p, rev)| (vec![p], rev));
    // if not found, try checking out and looking; otherwise return the result
    if !matches!(naive, Err(ConfigFetchIssue::NoConfigFound)) {
        naive
    } else {
        let configs = config_files_from_local_checkout(repo_url, &local_repo_dir)?;
        let rev = get_git_rev(&local_repo_dir).map_err(ConfigFetchIssue::GitFail)?;
        Ok((configs, rev))
    }
}

fn config_file_and_rev_from_remote_naive(
    repo_url: &str,
) -> Result<(PathBuf, GitRev), ConfigFetchIssue> {
    config_file_from_remote_naive(repo_url)
        .and_then(|config| get_git_rev_remote(repo_url).map(|rev| (config, rev)))
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
                log::warn!("unexpected response code for {repo_url}: {}", resp.status());
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
                return Err(ConfigFetchIssue::Http(Box::new(e)));
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
        log::info!("cloning {GF_REPO_URL} to {}", tempdir.path().display());
    }
    clone_repo(GF_REPO_URL, tempdir.path())
        .unwrap_or_die(|e| eprintln!("failed to checkout {GF_REPO_URL}: '{e}'"));
    get_candidates_from_local_checkout(tempdir.path(), verbose)
}

fn get_candidates_from_local_checkout(path: &Path, verbose: bool) -> BTreeSet<Metadata> {
    let ofl_dir = path.join("ofl");
    if verbose {
        log::debug!("searching for candidates in {}", ofl_dir.display());
    }
    let mut result = BTreeSet::new();
    for font_dir in iter_ofl_subdirectories(&ofl_dir) {
        let metadata = match load_metadata(&font_dir) {
            Ok(metadata) => metadata,
            Err(e) => {
                if verbose {
                    log::warn!("no metadata for font {}: '{}'", font_dir.display(), e);
                }
                continue;
            }
        };
        result.insert(metadata);
    }
    result
}

fn get_git_rev_remote(repo_url: &str) -> Result<GitRev, ConfigFetchIssue> {
    let output = std::process::Command::new("git")
        .arg("ls-remote")
        .arg(repo_url)
        .arg("HEAD")
        .output()
        .expect("should not fail if we found configs at this path");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let sha = stdout
        .split_whitespace()
        .next()
        .map(String::from)
        .unwrap_or_else(|| stdout.into_owned());
    Ok(sha)
}

/// Get the short sha of the current commit in the provided repository.
///
/// If no repo provided, run in current directory
///
/// returns `None` if the `git` command fails (for instance if the path is not
/// a git repository)
fn get_git_rev(repo_path: &Path) -> Result<String, GitFail> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["rev-parse", "--short", "HEAD"])
        .current_dir(repo_path);
    let output = cmd.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitFail::GitError(stderr.into_owned()));
    }

    Ok(std::str::from_utf8(&output.stdout)
        .expect("rev is always ascii/hex string")
        .trim()
        .to_owned())
}

// try to checkout this rev.
//
// returns `true` if successful, `false` otherwise (indicating a git error)
fn checkout_rev(repo_dir: &Path, rev: &str) -> Result<bool, GitFail> {
    let sha = get_git_rev(repo_dir)?;
    // the longer str is on the left, so we check if shorter str is a prefix
    let (left, right) = if sha.len() > rev.len() {
        (sha.as_str(), rev)
    } else {
        (rev, sha.as_str())
    };
    if left.starts_with(right) {
        return Ok(true);
    }
    log::info!(
        "repo {} needs fetch for {rev} (at {sha})",
        repo_dir.display()
    );
    // checkouts might be shallow, so unshallow before looking for a rev:
    let _ = std::process::Command::new("git")
        .current_dir(repo_dir)
        .args(["fetch", "--unshallow"])
        .output();

    let result = std::process::Command::new("git")
        .current_dir(repo_dir)
        .arg("checkout")
        .arg(rev)
        .output()?;

    if result.status.success() {
        Ok(true)
    } else {
        log::warn!("failed to find rev {rev} for {}", repo_dir.display());
        Ok(false)
    }
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

fn clone_repo(url: &str, to_dir: &Path) -> Result<(), GitFail> {
    assert!(to_dir.exists());
    let output = std::process::Command::new("git")
        // if a repo requires credentials fail instead of waiting
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("clone")
        .args(["--depth", "1"])
        .arg(url)
        .arg(to_dir)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitFail::GitError(stderr.into_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naive_config() {
        assert!(
            config_file_and_rev_from_remote_naive("https://github.com/PaoloBiagini/Joan").is_ok()
        );
        assert!(matches!(
            config_file_and_rev_from_remote_naive("https://github.com/googlefonts/bangers"),
            Err(ConfigFetchIssue::NoConfigFound)
        ));
    }

    #[test]
    fn remote_sha() {
        let rev = get_git_rev_remote("https://github.com/googlefonts/fontations").unwrap();
        // this will change over time so we're just sanity checking
        assert!(rev.len() > 16);
        assert!(rev.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
