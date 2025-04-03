//! Finding sources for Google Fonts fonts
//!
//! # basic usage:
//!
//! ```no_run
//! # use std::path::Path;
//! use google_fonts_sources as gfsources;
//! // get a list of repositories:
//!
//! let repo_cache = Path::new("~/where_i_want_to_checkout_fonts");
//! let font_repos = gfsources::discover_sources(repo_cache).unwrap();
//!
//! // for each repo we find, do something with each source:
//!
//! for repo in &font_repos {
//!     let sources = match repo.get_sources(repo_cache) {
//!         Ok(sources) => sources,
//!         Err(e) => {
//!             eprintln!("skipping repo '{}': '{e}'", repo.repo_name());
//!             continue;
//!         }
//!     };
//!
//!     println!("repo '{}' contains sources {sources:?}", repo.repo_name());
//! }
//! ```

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

mod args;
mod config;
mod error;
mod font_source;
mod metadata;

pub use args::Args;
pub use config::Config;
pub use error::{BadConfig, Error, GitFail, LoadRepoError};
use error::{MetadataError, UnwrapOrDie};
pub use font_source::FontSource;
use metadata::Metadata;

static GF_REPO_URL: &str = "https://github.com/google/fonts";
static METADATA_FILE: &str = "METADATA.pb";

/// entry point for the cli tool
#[doc(hidden)] // only intended to be used from our binary
pub fn run(args: &Args) {
    let repos = discover_sources(&args.fonts_dir).unwrap_or_die(|e| eprintln!("{e}"));
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
/// Returns a vec of `FontSource` structs describing repositories containing
/// known font sources.
///
/// This looks at every font in the [google/fonts] github repo, looks to see if
/// we have a known upstream repository for that font, and then looks to see if
/// that repo contains a config.yaml file.
///
/// The 'git_cache_dir' is the path to a directory where repositories will be
/// checked out, if necessary. Because we check out lots of repos (and it is
/// likely that the caller will want to check these out again later) it makes
/// sense to cache these in most cases.
///
/// [google/fonts]: https://github.com/google/fonts
pub fn discover_sources(git_cache_dir: &Path) -> Result<Vec<FontSource>, Error> {
    let google_slash_fonts = git_cache_dir.join("google/fonts");
    update_google_fonts_checkout(&google_slash_fonts)?;
    let candidates = find_ofl_metadata_files(&google_slash_fonts);
    log::info!("found {} metadata files", candidates.len());
    let sources: BTreeSet<_> = candidates
        .into_iter()
        .filter_map(|meta| meta.try_into().ok())
        .collect();

    log::info!(
        "found {} fonts with repo/commit/config fields",
        sources.len()
    );
    Ok(sources.into_iter().collect())
}

fn update_google_fonts_checkout(path: &Path) -> Result<(), Error> {
    if !path.exists() {
        log::info!("cloning {GF_REPO_URL} to {}", path.display());
        std::fs::create_dir_all(path)?;
        clone_repo(GF_REPO_URL, path)?;
    } else {
        log::info!("fetching latest from {GF_REPO_URL}");
        fetch_latest(path)?;
    }
    Ok(())
}

fn find_ofl_metadata_files(path: &Path) -> BTreeSet<Metadata> {
    let ofl_dir = path.join("ofl");
    log::debug!("searching for candidates in {}", ofl_dir.display());
    let mut result = BTreeSet::new();
    for font_dir in iter_ofl_subdirectories(&ofl_dir) {
        let metadata = match load_metadata(&font_dir) {
            Ok(metadata) => metadata,
            Err(e) => {
                log::debug!("no metadata for font {}: '{}'", font_dir.display(), e);
                continue;
            }
        };
        result.insert(metadata);
    }
    result
}

/// Get the short sha of the current commit in the provided repository.
///
/// If no repo provided, run in current directory
///
/// returns `None` if the `git` command fails (for instance if the path is not
/// a git repository)
fn get_git_rev(repo_path: &Path) -> Result<String, GitFail> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["rev-parse", "HEAD"]).current_dir(repo_path);
    let output = cmd.output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitFail::GitError {
            path: repo_path.to_owned(),
            stderr: stderr.into_owned(),
        });
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

    // but if they're _not_ shallow, we need normal fetch :/
    let _ = std::process::Command::new("git")
        .current_dir(repo_dir)
        .args(["fetch"])
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
        return Err(GitFail::GitError {
            path: to_dir.to_owned(),
            stderr: stderr.into_owned(),
        });
    }
    Ok(())
}

/// On success returns whether there were any changes
fn fetch_latest(path: &Path) -> Result<(), GitFail> {
    let mut output = std::process::Command::new("git")
        // if a repo requires credentials fail instead of waiting
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("fetch")
        .current_dir(path)
        .output()?;
    if output.status.success() {
        output = std::process::Command::new("git")
            .arg("checkout")
            .arg("origin/HEAD")
            .current_dir(path)
            .output()?;
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitFail::GitError {
            path: path.to_owned(),
            stderr: stderr.into_owned(),
        });
    }
    Ok(())
}
