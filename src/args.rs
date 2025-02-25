//! CLI args

use std::path::PathBuf;

/// Finding source repositories for Google Fonts fonts
///
/// This tool crawls fonts listed at <https://github.com/google/fonts> and looks
/// for those that have repositories listed in their metadata files. It then
/// looks for a config.yaml file in a `sources` directory for those repositories.
///
/// Finally it generates JSON output that list all of the repositories found
/// that contain a config file we understand.
#[derive(Clone, Debug, Default, clap::Parser)]
#[command(version)]
#[doc(hidden)] // only intended to be used from our binary
pub struct Args {
    /// Path to a directory where we will store font sources.
    ///
    /// This should be a directory dedicated to this task; the tool will
    /// assume that anything in it can be modified or deleted as needed.
    pub fonts_dir: PathBuf,
    /// File path to write output JSON. If omitted, output is printed to stdout
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Just print a list of repository URLs
    #[arg(short, long)]
    pub list: bool,
    /// Don't fetch/update repositories that already exist (but find new ones)
    ///
    /// Default is `false`, meaning we look for updates for all existing repos.
    #[arg(short, long, default_value_t = false)]
    pub no_fetch: bool,
    /// Print more info to stderr
    #[arg(short, long)]
    pub verbose: bool,
}
