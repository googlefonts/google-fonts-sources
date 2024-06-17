//! CLI args

use std::path::PathBuf;

#[derive(Clone, Debug, clap::Parser)]
#[command(version, about)]
pub struct Args {
    /// Path to local checkout of google/fonts repository
    #[arg(short, long)]
    pub repo_path: Option<PathBuf>,
}
