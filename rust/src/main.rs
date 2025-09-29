use std::error::Error;

#[macro_use]
extern crate getset;
extern crate fs_extra;

mod cli;
mod cmd;
mod config;
mod errors;
mod infra;
mod model;
mod service;
mod util;

fn main() -> Result<(), Box<dyn Error>> {
    // Load environment variables from .env file.
    // Fails if .env file not found, not readable or invalid.
    if let Err(err) = dotenvy::dotenv() {
        eprintln!("Warn: Failed to load .env file: {}", err);
        // std::process::exit(1);
    }

    config::load().expect("failed to load config");

    cli::shell();

    Ok(())
}
