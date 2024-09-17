use clap::Parser;

use google_fonts_sources::Args;

fn main() {
    env_logger::init();
    let args = Args::parse();
    google_fonts_sources::run(&args);
}
