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
//! for repo in &font_repos.sources {
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
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
};

use serde::{de, Deserialize, Serialize};

mod args;
mod config;
mod error;
mod font_source;
mod metadata;

pub use args::Args;
pub use config::Config;
use error::UnwrapOrDie;
pub use error::{BadConfig, Error, GitFail, LoadRepoError};
pub use font_source::FontSource;
use metadata::Metadata;

static GF_REPO_URL: &str = "https://github.com/google/fonts";
static METADATA_FILE: &str = "METADATA.pb";
static VIRTUAL_CONFIG_FILE: &str = "config.yaml";

const CURRENT_VERSION: Version = Version { major: 1, minor: 0 };

/// A (major, minor) version number.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
}

/// A versioned file format representing a set of font sources
#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SourceSet {
    /// The (major, minor) vesion. Serializes as a string.
    version: Version,
    /// The list of discovered sources.
    pub sources: Vec<FontSource>,
}

/// entry point for the cli tool
#[doc(hidden)] // only intended to be used from our binary
pub fn run(args: &Args) {
    let repos = discover_sources(&args.fonts_dir).unwrap_or_die(|e| eprintln!("{e}"));
    let output = if args.list {
        let urls = repos
            .sources
            .iter()
            .map(|r| r.repo_url.as_str())
            .collect::<Vec<_>>();
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
pub fn discover_sources(git_cache_dir: &Path) -> Result<SourceSet, Error> {
    let google_slash_fonts = git_cache_dir.join("google/fonts");
    update_google_fonts_checkout(&google_slash_fonts)?;
    let candidates = find_ofl_metadata_files(&google_slash_fonts);
    log::info!("found {} metadata files", candidates.len());
    let sources: BTreeSet<_> = candidates
        .into_iter()
        .filter_map(|(meta, path)| {
            let virtual_config_path = path.with_file_name(VIRTUAL_CONFIG_FILE);
            let virtual_config = virtual_config_path
                .exists()
                .then(|| virtual_config_path.strip_prefix(git_cache_dir).unwrap());

            let src = match virtual_config {
                Some(config) => FontSource::with_virtual_config(meta.clone(), config),
                None => FontSource::try_from(meta.clone()),
            };
            match src {
                Ok(item) => Some(item),
                Err(e) => {
                    log::warn!("bad metadata for '{}': {e}", meta.name);
                    None
                }
            }
        })
        .collect();

    log::info!(
        "found {} fonts with repo/commit/config fields",
        sources.len()
    );
    let sources = sources.into_iter().collect();
    let sources = mark_rev_conflicts(sources);
    Ok(SourceSet {
        version: CURRENT_VERSION,
        sources,
    })
}

fn mark_rev_conflicts(mut sources: Vec<FontSource>) -> Vec<FontSource> {
    let mut revs = HashMap::new();

    for source in &sources {
        *revs
            .entry(source.repo_url.clone())
            .or_insert(BTreeMap::new())
            .entry(source.git_rev().to_owned())
            .or_insert(0u32) += 1;
    }

    revs.retain(|_k, v| v.len() > 1);
    // in some cases several sources will share the same rev, while another
    // source has a specific rev; so we want the most common rev to be the 'default'.
    // In the case of ties, we choose the (lexicographic) `max` rev. (This is
    // arbitrary, but deterministic.)
    let has_conflict = revs
        .iter()
        .flat_map(|(repo, v)| {
            let most_common = v.iter().max_by_key(|(rev, v)| (**v, *rev)).unwrap().0;
            v.keys()
                // only mark repos that don't use the most common rev
                .filter_map(move |rev| {
                    (rev != most_common).then_some((repo.as_str(), rev.as_str()))
                })
        })
        .collect::<HashSet<_>>();

    // finally mark the repos we consider a conflict
    for source in &mut sources {
        if has_conflict.contains(&(source.repo_url.as_str(), source.git_rev())) {
            source.has_rev_conflict = true;
        }
    }
    sources
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

fn find_ofl_metadata_files(path: &Path) -> BTreeSet<(Metadata, PathBuf)> {
    let ofl_dir = path.join("ofl");
    log::debug!("searching for candidates in {}", ofl_dir.display());
    let mut result = BTreeSet::new();
    for font_dir in iter_ofl_subdirectories(&ofl_dir) {
        let metadata_path = font_dir.join(METADATA_FILE);
        let metadata = match Metadata::load(&metadata_path) {
            Ok(metadata) => (metadata, metadata_path),
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

impl Serialize for Version {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        format!("{}.{}", self.major, self.minor).serialize(serializer)
    }
}

// we currently only have one version, so let's keep this simple, we'll need
// to figure out a better approach if we add more stuff in the future.
impl<'de> Deserialize<'de> for Version {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw: &str = Deserialize::deserialize(deserializer)?;
        let (major, minor) = raw
            .split_once('.')
            .ok_or(de::Error::custom("invalid version"))?;
        let major = major.parse();
        let minor = minor.parse();
        match (major, minor) {
            (Ok(major), Ok(minor)) if major != 1 => Err(de::Error::custom(format!(
                "unsupported version {major}.{minor}"
            ))),
            (Ok(major), Ok(minor)) => Ok(Version { major, minor }),
            _ => Err(de::Error::custom("invalid version")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_conflicts() {
        let items_and_expected_conflict = vec![
            (FontSource::for_test("hello", "abc", "config.yaml"), false),
            (FontSource::for_test("hi", "abc", "config_one.yaml"), false),
            (FontSource::for_test("hi", "def", "config_two.yaml"), true),
            (
                FontSource::for_test("hi", "abc", "config_three.yaml"),
                false,
            ),
            (FontSource::for_test("oopsy", "123", "config.yaml"), true),
            (
                FontSource::for_test("oopsy", "456", "config_hi.yaml"),
                false,
            ),
        ];

        let (items, expected): (Vec<_>, Vec<_>) =
            items_and_expected_conflict.iter().cloned().unzip();

        let items = mark_rev_conflicts(items);
        assert_eq!(
            items
                .iter()
                .map(|item| item.has_rev_conflict)
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn roundtrip() {
        let thingie = SourceSet {
            version: Version { major: 1, minor: 0 },
            sources: vec![FontSource::for_test("hi", "abc", "config.yaml")],
        };

        let serd = serde_json::to_string(&thingie).unwrap();
        let de: SourceSet = serde_json::from_str(&serd).unwrap();

        assert_eq!(thingie, de);
    }

    #[test]
    #[should_panic(expected = "unsupported version")]
    fn deny_unknown_version() {
        let bad_thingie = SourceSet {
            version: Version { major: 2, minor: 0 },
            sources: Vec::new(),
        };

        let serd = serde_json::to_string(&bad_thingie).unwrap();
        let _de: SourceSet = serde_json::from_str(&serd).unwrap();
    }
}
