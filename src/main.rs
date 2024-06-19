use clap::Parser;

use google_fonts_sources::Args;

fn main() {
    let args = Args::parse();
    let _repos = google_fonts_sources::generate_sources_list(&args);
}
