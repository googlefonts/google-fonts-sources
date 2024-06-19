//! CLI args

use std::path::PathBuf;

#[derive(Clone, Debug, clap::Parser)]
#[command(version, about)]
pub struct Args {
    /// Path to local checkout of google/fonts repository
    #[arg(short, long)]
    pub repo_path: Option<PathBuf>,
    #[arg(short, long)]
    /// Path to a directory where we should checkout fonts; will reuse existing checkouts
    pub fonts_dir: Option<PathBuf>,
    /// Path to file containing github auth token
    #[arg(long = "gh_token")]
    pub gh_token_path: Option<PathBuf>,
}
