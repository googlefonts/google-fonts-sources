//! CLI args

use std::path::PathBuf;

#[derive(Clone, Debug, Default, clap::Parser)]
#[command(version, about)]
pub struct Args {
    /// Path to local checkout of google/fonts repository
    #[arg(short, long)]
    pub repo_path: Option<PathBuf>,
    #[arg(short, long)]
    /// Path to a directory where we should checkout fonts; will reuse existing checkouts
    pub fonts_dir: Option<PathBuf>,
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
