//! font repository information

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

use crate::{error::LoadRepoError, Config, Metadata};

/// Information about a font source in a git repository
#[derive(
    Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[non_exhaustive]
pub struct FontSource {
    /// The repository's url
    pub repo_url: String,
    /// The commit, as stored in the metadata file.
    rev: String,
    /// The path to the config file for this font, relative to the repo root.
    #[serde(alias = "config_files")]
    pub config: PathBuf,
    /// If `true`, this is a private googlefonts repo.
    ///
    /// We don't discover these repos, but they can be specified in json and
    /// we will load them. In this case, a valid oauth token must be specified
    /// via the `GITHUB_TOKEN` environment variable.
    #[serde(default, skip_serializing_if = "is_false")]
    auth: bool,
    /// if `true`, there are multiple sources in this repo with different git revs.
    ///
    /// In this case we will check this source out into its own directory, with
    /// the sha appended (like 'repo_$SHA') to disambiguate.
    ///
    /// This field is set in `crate::discover_sources`, and only considers sources
    /// in that list.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) has_rev_conflict: bool,
}

// a little helper used above
fn is_false(b: &bool) -> bool {
    !*b
}

impl FontSource {
    /// Create a `FontSource` after some validation.
    ///
    /// Returns `None` if the url has some unexpected format, or if there are
    /// no config files
    pub(crate) fn new(repo_url: String, rev: String, config: PathBuf) -> Result<Self, String> {
        if repo_name_and_org_from_url(&repo_url).is_none() {
            log::warn!("unexpected repo url '{repo_url}'");
            return Err(repo_url);
        }
        Ok(Self {
            repo_url,
            rev,
            config,
            auth: false,
            has_rev_conflict: false,
        })
    }

    /// just for testing: doesn't care if a URL is well formed/exists etc
    #[cfg(test)]
    pub(crate) fn for_test(url: &str, rev: &str, config: &str) -> Self {
        Self {
            repo_url: url.into(),
            rev: rev.into(),
            config: config.into(),
            auth: false,
            has_rev_conflict: false,
        }
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
        self.repo_path_for_url(cache_dir).unwrap()
    }

    fn repo_path_for_url(&self, cache_dir: &Path) -> Option<PathBuf> {
        let (org, name) = repo_name_and_org_from_url(&self.repo_url)?;
        let mut path = cache_dir.join(org);
        if self.has_rev_conflict {
            path.push(format!(
                "{name}_{}",
                self.rev.get(..10).unwrap_or(self.rev.as_str())
            ));
        } else {
            path.push(name);
        }
        Some(path)
    }

    /// Return the URL we'll use to fetch the repo, handling authentication.
    fn repo_url_with_auth_token_if_needed(&self) -> Result<Cow<str>, LoadRepoError> {
        if self.auth {
            let auth_token =
                std::env::var("GITHUB_TOKEN").map_err(|_| LoadRepoError::MissingAuth)?;
            let url_body = self
                .repo_url
                .trim_start_matches("https://")
                .trim_start_matches("www.");
            let add_dot_git = if self.repo_url.ends_with(".git") {
                ""
            } else {
                ".git"
            };

            let auth_url = format!("https://{auth_token}:x-oauth-basic@{url_body}{add_dot_git}");
            Ok(auth_url.into())
        } else {
            Ok(self.repo_url.as_str().into())
        }
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

        if font_dir.exists() && !font_dir.join(".git").exists() {
            log::debug!("{} exists but is not a repo, removing", font_dir.display());
            if let Err(e) = std::fs::remove_dir(&font_dir) {
                // we don't want to remove a non-empty directory, just in case
                log::warn!("could not remove {}: '{e}'", font_dir.display());
            }
        }

        if !font_dir.exists() {
            std::fs::create_dir_all(&font_dir)?;
            let repo_url = self.repo_url_with_auth_token_if_needed()?;
            log::info!("cloning {repo_url}");
            super::clone_repo(&repo_url, &font_dir)?;
        }

        if !super::checkout_rev(&font_dir, &self.rev)? {
            return Err(LoadRepoError::NoCommit {
                sha: self.rev.clone(),
            });
        }
        Ok(font_dir)
    }

    /// Return path to the config file for this repo, if it exists.
    ///
    /// Returns an error if the repo cannot be cloned, or if no config files
    /// are found.
    pub fn config_path(&self, cache_dir: &Path) -> Result<PathBuf, LoadRepoError> {
        let font_dir = self.instantiate(cache_dir)?;
        let config_path = font_dir.join(&self.config);
        if !config_path.exists() {
            Err(LoadRepoError::NoConfig)
        } else {
            Ok(config_path)
        }
    }

    /// Return a `Vec` of source files in this respository.
    ///
    /// If necessary, this will create a new checkout of this repo at
    /// '{git_cache_dir}/{repo_org}/{repo_name}'.
    pub fn get_sources(&self, git_cache_dir: &Path) -> Result<Vec<PathBuf>, LoadRepoError> {
        let font_dir = self.instantiate(git_cache_dir)?;
        let config_path = font_dir.join(&self.config);
        let config = Config::load(&config_path)?;
        let mut sources = config
            .sources
            .iter()
            .filter_map(|source| {
                let source = config_path.parent().unwrap_or(&font_dir).join(source);
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

#[derive(Clone, Debug, thiserror::Error)]
pub enum TryFromMetadataError {
    #[error("missing field '{0}'")]
    MissingField(&'static str),
    #[error("unfamiliar URL '{0}'")]
    UnfamiliarUrl(String),
}

impl TryFrom<Metadata> for FontSource {
    type Error = TryFromMetadataError;

    fn try_from(meta: Metadata) -> Result<Self, Self::Error> {
        if let Some(badurl) = meta.unknown_repo_url() {
            return Err(TryFromMetadataError::UnfamiliarUrl(badurl.to_owned()));
        }
        FontSource::new(
            meta.repo_url
                .ok_or(TryFromMetadataError::MissingField("repo_url"))?,
            meta.commit
                .ok_or(TryFromMetadataError::MissingField("commit"))?,
            meta.config_yaml
                .ok_or(TryFromMetadataError::MissingField("config_yaml"))?
                .into(),
        )
        .map_err(TryFromMetadataError::UnfamiliarUrl)
    }
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

    #[test]
    fn test_non_sources_config() {
        let source = FontSource::for_test(
            "https://github.com/danhhong/Nokora",
            "9c5f991b700b9be3519315a854a7b986e6877ace",
            "Source/builder.yaml",
        );
        let temp_dir = tempfile::tempdir().unwrap();
        let sources = source
            .get_sources(temp_dir.path())
            .expect("should be able to get sources");
        assert_eq!(sources.len(), 1);
        assert_eq!(
            sources[0],
            temp_dir.path().join("danhhong/Nokora/Source/Nokora.glyphs")
        );
    }
}
