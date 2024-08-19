//! font repository information

use std::path::{Path, PathBuf};

use crate::{error::LoadRepoError, Config};

/// Information about a git repository containing font sources
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct RepoInfo {
    /// The name of the repository.
    ///
    /// This is everything after the trailing '/' in e.g. `https://github.com/PaoloBiagini/Joan`
    pub repo_name: String,
    /// The repository's url
    pub repo_url: String,
    /// The commit rev of the repository's main branch, at discovery time.
    pub rev: String,
    /// The names of config files that exist in this repository's source directory
    pub config_files: Vec<PathBuf>,
}

impl RepoInfo {
    /// Return the a `Vec` of source files in this respository.
    ///
    /// If necessary, this will create a new checkout of this repo at
    /// '{font_dir}/{repo_name}'.
    pub fn get_sources(&self, font_repos_dir: &Path) -> Result<Vec<PathBuf>, LoadRepoError> {
        let font_dir = font_repos_dir.join(&self.repo_name);

        if !font_dir.exists() {
            std::fs::create_dir_all(&font_dir)?;
            super::clone_repo(&self.repo_url, &font_dir)?;
        }

        if !super::checkout_rev(&font_dir, &self.rev)? {
            return Err(LoadRepoError::NoCommit {
                sha: self.rev.clone(),
            });
        }

        let source_dir = font_dir.join("sources");
        let configs = self
            .config_files
            .iter()
            .map(|filename| {
                let config_path = source_dir.join(filename);
                Config::load(&config_path)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if configs.is_empty() {
            return Err(LoadRepoError::NoConfig);
        }

        let mut sources = configs
            .iter()
            .flat_map(|c| c.sources.iter())
            .map(|source| source_dir.join(source))
            .collect::<Vec<_>>();
        sources.sort_unstable();
        sources.dedup();

        Ok(sources)
    }
}
