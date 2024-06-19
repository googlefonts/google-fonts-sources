//! Finding sources for Google Fonts fonts

use kdam::BarExt;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
};

mod args;
mod error;
mod metadata;

pub use args::Args;
use error::{MetadataError, UnwrapOrDie};
use metadata::Metadata;

static GF_REPO_URL: &str = "https://github.com/google/fonts";
static METADATA_FILE: &str = "METADATA.pb";
static TOKEN_VAR: &str = "GH_TOKEN";

//TODO: figure out what this returns
pub fn generate_sources_list(args: &Args) -> () {
    // before starting work make sure we have a github token:
    let token = get_gh_token_or_die(args.gh_token_path.as_deref());
    let candidates = match args.repo_path.as_deref() {
        Some(path) => get_candidates_from_local_checkout(path),
        None => get_candidates_from_remote(),
    };

    let have_repo = pruned_candidates(&candidates);

    let has_config_files = if let Some(font_path) = args.fonts_dir.as_ref() {
        find_config_files(&candidates, &token, &font_path)
    } else {
        let tempdir = tempfile::tempdir().unwrap();
        find_config_files(&candidates, &token, tempdir.path())
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

    report_config_stats(&has_config_files)
}

fn pruned_candidates(candidates: &BTreeMap<String, Metadata>) -> BTreeMap<String, Metadata> {
    let mut seen_repos = HashSet::new();
    let mut result = BTreeMap::new();
    for metadata in candidates.values() {
        let Some(url) = metadata.repo_url.as_ref() else {
            continue;
        };

        if seen_repos.insert(url) {
            result.insert(metadata.name.clone(), metadata.clone());
        } else {
            eprintln!("duplicate repo '{url}' for font {}", metadata.name);
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
            .unwrap_or_die(|e| eprintln!("could not read file at {}: '{e}'", path.display())),
        None => std::env::var(TOKEN_VAR).unwrap_or_die(|_| {
            eprintln!("please provide a github auth token via --gh_token arg or GH_TOKEN env var")
        }),
    }
}

fn find_config_files(
    fonts: &BTreeMap<String, Metadata>,
    token: &str,
    checkout_font_dir: &Path,
) -> BTreeMap<String, Vec<PathBuf>> {
    let n_has_repo = fonts.values().filter(|md| md.repo_url.is_some()).count();
    let mut result = BTreeMap::new();
    let mut progressbar = kdam::tqdm!(total = n_has_repo);
    for (name, repo) in fonts
        .iter()
        .filter_map(|(name, meta)| meta.repo_url.as_ref().map(|repo| (name, repo)))
    {
        if let Some(config) = config_file_name(&repo, token, checkout_font_dir) {
            result.insert(name.clone(), config);
        }
        progressbar.update(1).unwrap();
    }
    result
}

fn config_file_name(repo_url: &str, token: &str, checkout_font_dir: &Path) -> Option<Vec<PathBuf>> {
    has_config_file_naive(repo_url, None)
        .map(|p| vec![p])
        .or_else(|| has_config_file_checkout(repo_url, token, checkout_font_dir))
}

// just check for the presence of the most common file names
fn has_config_file_naive(repo_url: &str, token: Option<&str>) -> Option<PathBuf> {
    for filename in ["config.yaml", "config.yml"] {
        let config_url = format!("{repo_url}/tree/HEAD/sources/{filename}");
        let mut req = ureq::head(&config_url);
        if let Some(token) = token {
            req = req.set("Authorization: Bearer", token);
        }

        match req.call() {
            Ok(resp) if resp.status() == 200 => return Some(filename.into()),
            Ok(resp) => {
                eprintln!("{repo_url}: {}", resp.status());
            }
            Err(ureq::Error::Status(404, _)) => (),
            Err(e) => {
                eprintln!("{repo_url}: err '{e}'");
                return None;
            }
        }
    }
    None
}

fn has_config_file_checkout(
    repo_url: &str,
    token: &str,
    checkout_font_dir: &Path,
) -> Option<Vec<PathBuf>> {
    let Some((_, repo_name)) = repo_url.rsplit_once('/') else {
        eprintln!("bad repo name: '{repo_url}'");
        return None;
    };

    let out_path = checkout_font_dir.join(repo_name);
    if out_path.exists() {
        // should we always fetch? idk
    } else {
        std::fs::create_dir_all(&out_path).unwrap();
        if let Err(e) = clone_repo(repo_url, &out_path) {
            eprintln!("checkout '{repo_url}' failed: '{e}'");
        }
    }
    get_config_paths(&out_path)
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

fn get_candidates_from_remote() -> BTreeMap<String, Metadata> {
    let tempdir = tempfile::tempdir().unwrap();
    clone_repo(GF_REPO_URL, tempdir.path())
        .unwrap_or_die(|e| eprintln!("failed to checkout {GF_REPO_URL}: '{e}'"));
    get_candidates_from_local_checkout(tempdir.path())
}

fn get_candidates_from_local_checkout(path: &Path) -> BTreeMap<String, Metadata> {
    let ofl_dir = path.join("ofl");
    let mut result = BTreeMap::new();
    for font_dir in iter_ofl_subdirectories(&ofl_dir) {
        let metadata = match load_metadata(&font_dir) {
            Ok(metadata) => metadata,
            Err(e) => {
                eprintln!("no metadata for font {}: '{}'", font_dir.display(), e);
                continue;
            }
        };
        result.insert(metadata.name.clone(), metadata);
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
        assert!(has_config_file_naive("https://github.com/PaoloBiagini/Joan", None).is_some());
        assert!(has_config_file_naive("https://github.com/googlefonts/bangers", None).is_none());
    }
}
