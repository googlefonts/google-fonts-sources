use std::{fmt::Display, path::PathBuf};

use crate::metadata::BadMetadata;

//use protobuf::text_format::ParseError;

/// A little helper trait for reporting results we can't recover from
pub(crate) trait UnwrapOrDie<T, E> {
    // print_msg should be a closure that eprints a message before termination
    fn unwrap_or_die(self, print_msg: impl FnOnce(E)) -> T;
}

impl<T, E: Display> UnwrapOrDie<T, E> for Result<T, E> {
    fn unwrap_or_die(self, print_msg: impl FnOnce(E)) -> T {
        match self {
            Ok(val) => val,
            Err(e) => {
                print_msg(e);
                std::process::exit(1)
            }
        }
    }
}

/// Errors that occur while trying to find sources
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An io error occurred
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// an error with reading the google/fonts repo
    #[error(transparent)]
    Git(#[from] GitFail),
}

/// Errors that occur while trying to load a config file
#[derive(Debug, thiserror::Error)]
pub enum BadConfig {
    /// The file could not be read
    #[error(transparent)]
    Read(#[from] std::io::Error),
    /// The yaml could not be parsed
    #[error(transparent)]
    Yaml(serde_yaml::Error),
}

/// Things that go wrong when trying to clone and read a font repo
#[derive(Debug, thiserror::Error)]
pub enum LoadRepoError {
    #[error("could not create local directory: '{0}'")]
    Io(
        #[from]
        #[source]
        std::io::Error,
    ),
    #[error(transparent)]
    GitFail(#[from] GitFail),
    /// The expected commit could not be found
    #[error("could not find commit '{sha}'")]
    NoCommit { sha: String },

    /// No config file was found
    #[error("no config file was found")]
    NoConfig,
    #[error("couldn't load config file: '{0}'")]
    BadConfig(
        #[source]
        #[from]
        BadConfig,
    ),
    #[error("reposity requires an auth token but GITHUB_TOKEN not set")]
    MissingAuth,
}

/// Things that go wrong when trying to run a git command
#[derive(Debug, thiserror::Error)]
pub enum GitFail {
    /// The git command itself does not execute
    #[error("git process failed: '{0}'")]
    ProcessFailed(
        #[from]
        #[source]
        std::io::Error,
    ),
    /// The git command returns a non-zero status
    #[error("git failed: '{stderr}'")]
    GitError { path: PathBuf, stderr: String },
}

pub(crate) enum MetadataError {
    Read(std::io::Error),
    Parse(BadMetadata),
}

impl Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MetadataError::Read(e) => e.fmt(f),
            MetadataError::Parse(e) => e.fmt(f),
        }
    }
}
