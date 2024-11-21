//! font repository information

use std::path::{Path, PathBuf};

use crate::{error::LoadRepoError, Config};

/// Information about a git repository containing font sources
#[derive(
    Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[non_exhaustive]
pub struct RepoInfo {
    /// The repository's url
    pub repo_url: String,
    /// The commit rev of the repository's main branch, at discovery time.
    //NOTE: this is private because we want to force the use of `new` for
    //construction, so we can ensure urls are well formed
    rev: String,
    /// Paths to config files in this repo, relative to the repo root.
    pub config_files: Vec<PathBuf>,
}

impl RepoInfo {
    /// Create a `RepoInfo` after some validation.
    ///
    /// Returns `None` if the url has some unexpected format, or if there are
    /// no config files
    pub(crate) fn new(repo_url: String, rev: String, config_files: Vec<PathBuf>) -> Option<Self> {
        if repo_name_and_org_from_url(&repo_url).is_none() {
            log::warn!("unexpected repo url '{repo_url}'");
            return None;
        }
        Some(Self {
            repo_url,
            rev,
            config_files,
        })
    }

    /// The name of the user or org that the repository lives under.
    ///
    /// This is 'googlefonts' for the repo `https://github.com/googlefonts/google-fonts-sources`
    pub fn repo_org(&self) -> &str {
        // unwrap is safe because we validate at construction time
        repo_name_and_org_from_url(&self.repo_url).unwrap().0
    }

    /// The name of the repository.
    ///
    /// This is everything after the trailing '/' in e.g. `https://github.com/PaoloBiagini/Joan`
    pub fn repo_name(&self) -> &str {
        repo_name_and_org_from_url(&self.repo_url).unwrap().1
    }

    /// The commit rev of the repository's main branch, at discovery time.
    pub fn git_rev(&self) -> &str {
        &self.rev
    }

    /// Given a root cache directory, return the local path this repo.
    ///
    /// This is in the format, `{cache_dir}/{repo_org}/{repo_name}`
    pub fn repo_path(&self, cache_dir: &Path) -> PathBuf {
        // unwrap is okay because we already know the url is well formed
        repo_path_for_url(&self.repo_url, cache_dir).unwrap()
    }

    /// Attempt to checkout/update this repo to the provided `cache_dir`.
    ///
    /// The repo will be checked out to '{cache_dir}/{repo_org}/{repo_name}',
    /// and HEAD will be set to the `self.git_rev()`.
    ///
    /// Returns the path to the checkout on success.
    ///
    /// Returns an error if the repo cannot be cloned, the git rev cannot be
    /// found, or if there is an io error.
    pub fn instantiate(&self, cache_dir: &Path) -> Result<PathBuf, LoadRepoError> {
        let font_dir = self.repo_path(cache_dir);
        if !font_dir.exists() {
            std::fs::create_dir_all(&font_dir)?;
            super::clone_repo(&self.repo_url, &font_dir)?;
        }

        if !super::checkout_rev(&font_dir, &self.rev)? {
            return Err(LoadRepoError::NoCommit {
                sha: self.rev.clone(),
            });
        }
        Ok(font_dir)
    }

    /// Iterate paths to config files in this repo, checking it out if necessary
    ///
    /// This returns paths to the actual location on disk of the files.
    pub fn iter_configs(
        &self,
        cache_dir: &Path,
    ) -> Result<impl Iterator<Item = PathBuf> + '_, LoadRepoError> {
        let font_dir = self.instantiate(cache_dir)?;
        Ok(self.config_files.iter().map(move |config| {
            // old style we only stored the config file name
            if config.parent() == Some(Path::new("")) {
                if let Some(source_dir) = super::find_sources_dir(&font_dir) {
                    return source_dir.join(config);
                }
            }
            // now we include the full path relative to the font_dir
            font_dir.join(config)
        }))
    }

    /// Return a `Vec` of source files in this respository.
    ///
    /// If necessary, this will create a new checkout of this repo at
    /// '{git_cache_dir}/{repo_org}/{repo_name}'.
    pub fn get_sources(&self, git_cache_dir: &Path) -> Result<Vec<PathBuf>, LoadRepoError> {
        let font_dir = self.instantiate(git_cache_dir)?;
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
            .filter_map(|source| {
                let source = source_dir.join(source);
                source.exists().then_some(source)
            })
            .collect::<Vec<_>>();
        sources.sort_unstable();
        sources.dedup();

        Ok(sources)
    }
}

fn repo_name_and_org_from_url(url: &str) -> Option<(&str, &str)> {
    let url = url.trim_end_matches('/');
    let (rest, name) = url.rsplit_once('/')?;
    let (_, org) = rest.rsplit_once('/')?;
    Some((org, name))
}

pub(super) fn repo_path_for_url(url: &str, base_cache_dir: &Path) -> Option<PathBuf> {
    let (org, name) = repo_name_and_org_from_url(url)?;
    let mut path = base_cache_dir.join(org);
    path.push(name);
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn org_and_name_from_url() {
        assert_eq!(
            repo_name_and_org_from_url("https://github.com/hyper-type/hahmlet/"),
            Some(("hyper-type", "hahmlet")),
        );
        assert_eq!(
            repo_name_and_org_from_url("https://github.com/hyper-type/Advent"),
            Some(("hyper-type", "Advent")),
        );
    }
}
