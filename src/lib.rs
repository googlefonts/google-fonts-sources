//! Finding sources for Google Fonts fonts

use std::{
    collections::BTreeMap,
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

//TODO: figure out what this returns
pub fn generate_sources_list(repo_path: Option<&Path>) -> () {
    let candidates = match repo_path {
        Some(path) => get_candidates_from_local_checkout(path),
        None => get_candidates_from_remote(),
    };

    let n_has_repo = candidates
        .values()
        .filter(|md| md.repo_url.is_some())
        .count();

    println!(
        "{n_has_repo} of {} candidates have known repo url",
        candidates.len()
    )
}

fn get_candidates_from_remote() -> BTreeMap<String, Metadata> {
    let tempdir = tempfile::tempdir().unwrap();
    clone_main_repo(tempdir.path())
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
fn clone_main_repo(to_dir: &Path) -> Result<(), String> {
    assert!(to_dir.exists());
    //let url = format!("https://github.com/{repo}",);
    eprintln!("cloning '{GF_REPO_URL}' to {}", to_dir.display());
    let output = std::process::Command::new("git")
        // if a repo requires credentials fail instead of waiting
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("clone")
        .args(["--depth", "1"])
        .arg(GF_REPO_URL)
        .arg(to_dir)
        .output()
        .expect("failed to execute git command");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.into_owned());
    }
    Ok(())
}
