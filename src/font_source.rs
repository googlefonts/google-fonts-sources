//! font repository information

use std::path::{Path, PathBuf};

use ureq::typestate::WithoutBody;

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
    ///
    /// (if it is an external config, it is relative to the git cache root.)
    pub config: PathBuf,
    /// If `true`, this config does not exist in the repo
    ///
    /// In this case the config actually lives in the google/fonts repository,
    /// alongside the metadata file.
    ///
    /// External configs are treated as if they live at `$REPO/source/config.yaml`.
    #[serde(default, skip_serializing_if = "is_false")]
    config_is_external: bool,
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
            config_is_external: false,
        })
    }

    pub(crate) fn with_external_config(
        metadata: Metadata,
        external_config_path: &Path,
    ) -> Result<Self, TryFromMetadataError> {
        let repo_url = metadata
            .repo_url
            .ok_or(TryFromMetadataError::MissingField("repo_url"))?;
        let commit = metadata
            .commit
            .ok_or(TryFromMetadataError::MissingField("commit"))?;

        let mut result = Self::new(repo_url, commit, external_config_path.to_path_buf())
            .map_err(TryFromMetadataError::UnfamiliarUrl)?;
        result.config_is_external = true;
        Ok(result)
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
            config_is_external: false,
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
        path.push(format!(
            "{name}_{}",
            self.rev.get(..10).unwrap_or(self.rev.as_str())
        ));
        Some(path)
    }

    /// Build the HTTP request for downloading this repo's tarball at its pinned commit.
    ///
    /// For private repos this uses the GitHub REST API endpoint (which requires
    /// Bearer auth) and sets the appropriate headers. For public repos it uses
    /// the direct archive URL.
    fn tarball_request(&self) -> Result<ureq::RequestBuilder<WithoutBody>, LoadRepoError> {
        if self.auth {
            let token = std::env::var("GITHUB_TOKEN").map_err(|_| LoadRepoError::MissingAuth)?;
            let url = format!(
                "https://api.github.com/repos/{}/{}/tarball/{}",
                self.repo_org(),
                self.repo_name(),
                self.rev
            );
            Ok(ureq::get(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28"))
        } else {
            let url = format!(
                "{}/archive/{}.tar.gz",
                self.repo_url.trim_end_matches('/'),
                self.rev
            );
            Ok(ureq::get(&url))
        }
    }

    /// Attempt to fetch this repo's sources into the provided `cache_dir`.
    ///
    /// Downloads the tarball for the pinned commit and extracts it to
    /// `'{cache_dir}/{repo_org}/{repo_name}_{sha}'`. Returns that path on
    /// success. If the directory already exists it is returned immediately
    /// without re-fetching.
    pub fn instantiate(&self, cache_dir: &Path) -> Result<PathBuf, LoadRepoError> {
        let font_dir = self.repo_path(cache_dir);

        if font_dir.exists() {
            return Ok(font_dir);
        }

        let request = self.tarball_request()?;
        log::info!(
            "fetching tarball for {}/{}",
            self.repo_org(),
            self.repo_name()
        );

        // Extract into a sibling temp dir then atomically rename to avoid
        // leaving a partial directory on failure.
        let parent = font_dir.parent().unwrap();
        std::fs::create_dir_all(parent)?;
        let tmp = tempfile::TempDir::new_in(parent)?;
        fetch_tarball(request, tmp.path())?;
        let tmp_path = tmp.keep();
        std::fs::rename(&tmp_path, &font_dir)?;

        Ok(font_dir)
    }

    /// An 'external' config is one that does not exist in the source repository.
    ///
    /// Instead it lives in the google/fonts repository, alongside the metadata
    /// file for this family.
    ///
    /// The caller must figure out how to handle this. The actual config path can
    /// be retrieved using the [`config_path`][Self::config_path] method here.
    pub fn config_is_external(&self) -> bool {
        self.config_is_external
    }

    /// Return path to the config file for this repo, if it exists.
    ///
    /// Returns an error if the repo cannot be cloned, or if no config files
    /// are found.
    pub fn config_path(&self, cache_dir: &Path) -> Result<PathBuf, LoadRepoError> {
        let base_dir = if self.config_is_external() {
            cache_dir.to_owned()
        } else {
            self.instantiate(cache_dir)?
        };
        let config_path = base_dir.join(&self.config);
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

/// Download a tarball from `url` and extract its contents into `target_dir`.
///
/// We previously used git for all of our interactions with repos, but shallow
/// git checkouts are annoying to manage and don't work when we want to grab
/// commits that are not on the main branch; likewise non-shallow clones can be
/// enormous and take a lot of time.
///
/// This uses a (github-specific?) url scheme that lets us grab a tarball
/// snapshot of the repository at a specific commit.
fn fetch_tarball(
    request: ureq::RequestBuilder<WithoutBody>,
    target_dir: &Path,
) -> Result<(), LoadRepoError> {
    let response = request.call()?;
    let mut body = response.into_body();
    let gz = flate2::read::GzDecoder::new(body.as_reader());
    let mut archive = tar::Archive::new(gz);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        // Skip the top-level directory entry itself; strip it from all paths.
        let stripped: PathBuf = path.components().skip(1).collect();
        if stripped.as_os_str().is_empty() {
            continue;
        }
        entry.unpack(target_dir.join(stripped))?;
    }

    Ok(())
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

/// The impl does not account for possible external config files.
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
            temp_dir
                .path()
                .join("danhhong/Nokora_9c5f991b70/Source/Nokora.glyphs")
        );
    }
}
