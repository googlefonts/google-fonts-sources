//! CLI args

use std::path::PathBuf;

#[derive(Clone, Debug, Default, clap::Parser)]
#[command(version, about)]
#[doc(hidden)] // only intended to be used from our binary
pub struct Args {
    /// Path to a directory where we will store font sources.
    ///
    /// This should be a directory dedicated to this task; the tool will
    /// assume that anything in it can be modified or deleted as needed.
    pub fonts_dir: PathBuf,
    /// Path to write output. If omitted, output is printed to stdout
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Just print a list of repository URLs
    #[arg(short, long)]
    pub list: bool,
    /// Print more info to stderr
    #[arg(short, long)]
    pub verbose: bool,
}
