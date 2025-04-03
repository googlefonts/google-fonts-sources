//! reading the MANIFEST.pb files that describe a font
//!
//! this format is defined at
//! <https://github.com/googlefonts/gftools/blob/main/Lib/gftools/fonts_public.proto>

use std::{fmt::Display, path::Path, str::FromStr};

use crate::error::MetadataError;

// in the future we would like to generate a type for this from the protobuf definition
// but there's no official rust protobuf impl, and no informal impl correctly
// handles the protobuf text format
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Metadata {
    pub(crate) name: String,
    pub(crate) repo_url: Option<String>,
    pub(crate) commit: Option<String>,
    pub(crate) config_yaml: Option<String>,
}

/// Ways parsing metadata can fail
pub(crate) enum BadMetadata {
    /// The required 'name' field was missing
    NoName,
}

impl Metadata {
    fn new(
        name: String,
        repo_url: Option<&str>,
        commit: Option<&str>,
        config_yaml: Option<&str>,
    ) -> Self {
        let repo_url = repo_url.and_then(post_process_repo_url);
        let config_yaml = config_yaml
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let commit = commit
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        Self {
            name,
            repo_url,
            commit,
            config_yaml,
        }
    }

    pub fn load(path: &Path) -> Result<Self, MetadataError> {
        let string = std::fs::read_to_string(path).map_err(MetadataError::Read)?;
        string.parse().map_err(MetadataError::Parse)
    }

    /// If we have a repo url but we don't recognize it, (or it is invalid) return it
    ///
    /// (used for logging)
    pub fn unknown_repo_url(&self) -> Option<&str> {
        self.repo_url.as_deref().filter(|s| {
            !s.starts_with("https://github.com") || ureq::http::Uri::from_str(s).is_err()
        })
    }

    /// If we have a repo_url and it is a host we know (github) use it
    #[cfg(test)]
    fn known_repo_url(&self) -> Option<&str> {
        self.repo_url
            .as_deref()
            .filter(|s| s.starts_with("https://github.com") && ureq::http::Uri::from_str(s).is_ok())
    }
}

impl FromStr for Metadata {
    type Err = BadMetadata;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        static NAME_KEY: &str = "name: ";
        static REPO_KEY: &str = "repository_url: ";
        static CONFIG_YAML_KEY: &str = "config_yaml: ";
        static COMMIT_KEY: &str = "commit: ";

        let Some(pos) = s.find(NAME_KEY) else {
            return Err(BadMetadata::NoName);
        };
        let pos = pos + NAME_KEY.len();
        let name = extract_litstr(&s[pos..])
            .ok_or(BadMetadata::NoName)?
            .to_owned();

        let repo_url = s
            .find(REPO_KEY)
            .and_then(|pos| extract_litstr(&s[pos + REPO_KEY.len()..]));

        let config_yaml = s
            .find(CONFIG_YAML_KEY)
            .and_then(|pos| extract_litstr(&s[pos + CONFIG_YAML_KEY.len()..]));

        let commit = s
            .find(COMMIT_KEY)
            .and_then(|pos| extract_litstr(&s[pos + COMMIT_KEY.len()..]));

        Ok(Metadata::new(name, repo_url, commit, config_yaml))
    }
}

// normalize the url, filtering out various things that aren't valid
fn post_process_repo_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/'); // trailing slash is not meaningful
    if url.is_empty() {
        return None;
    }
    let url = if let Some(suffix) = url.strip_prefix("https://www.github") {
        format!("https://github{suffix}") // remove www if present
    } else if url.starts_with("github") {
        format!("https://{url}") // we've seen this once at least..
    } else {
        url.to_owned()
    };
    Some(url)
}

/// extract the contents of a string literal, e.g. the stuff between the quotation marks
///
/// This expects the next non-whitespace char in `s` to be `"`.
///
/// This is expected to be temporary (until the official protobufs crate is done? and isn't
/// fully spec compliant, e.g. doesn't handle escape sequences)
#[allow(clippy::skip_while_next)] // we use skip_while so we can track if last byte was `\`
fn extract_litstr(s: &str) -> Option<&str> {
    let s = s.trim();
    if s.bytes().next() != Some(b'"') {
        return None;
    }
    let s = &s[1..];

    let mut is_escaped = false;
    let end = s
        .bytes()
        .enumerate()
        // just find the position of the closing quote
        .skip_while(|(_, b)| match *b {
            b'\\' if !is_escaped => {
                is_escaped = true;
                true
            }
            b'"' if !is_escaped => false,
            _ => {
                is_escaped = false;
                true
            }
        })
        .next()
        .map(|(i, _)| i)?;
    Some(&s[..end])
}

impl Display for BadMetadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BadMetadata::NoName => f.write_str("missing required field 'name'"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_str() {
        assert_eq!(extract_litstr(r#" "foo" "#), Some("foo"));
        assert_eq!(extract_litstr(r#" "Lâm" "#), Some("Lâm"));
        // no opening quote
        assert_eq!(extract_litstr(r#" foo" "#), None);
        // no closing quote
        assert_eq!(extract_litstr(r#" "foo "#), None);
        // ignore escaped " (but we don't actually handle the escaping)
        assert_eq!(extract_litstr(r#" "foo\"bar" "#), Some("foo\\\"bar"));
    }

    #[test]
    fn metadata_urls() {
        // add https
        assert_eq!(
            Metadata::new("hi".into(), Some("github.com/hi/mom"), None, None).known_repo_url(),
            Some("https://github.com/hi/mom")
        );
        // remove www
        assert_eq!(
            Metadata::new(
                "hi".into(),
                Some("https://www.github.com/hi/mom"),
                None,
                None
            )
            .known_repo_url(),
            Some("https://github.com/hi/mom")
        );
        // ignore gitlab
        assert_eq!(
            Metadata::new(
                "hi".into(),
                Some("https://www.gitlab.com/hi/mom"),
                None,
                None
            )
            .known_repo_url(),
            None
        );
        // ignore invalid urls
        assert_eq!(
            Metadata::new(
                "hi".into(),
                Some("https://www.github.com/hi/mom but with spaces! that's bad"),
                None,
                None
            )
            .known_repo_url(),
            None
        );
    }
}
