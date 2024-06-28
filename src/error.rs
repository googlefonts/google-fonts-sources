use std::fmt::Display;

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
