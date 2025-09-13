use std::error::Error;

mod cli;
mod cmd;
mod config;
mod service;
mod util;

fn main() -> Result<(), Box<dyn Error>> {
    // Load environment variables from .env file.
    // Fails if .env file not found, not readable or invalid.
    if let Err(err) = dotenvy::dotenv() {
        eprintln!("Warn: Failed to load .env file: {}", err);
        // std::process::exit(1);
    }

    cli::shell();

    Ok(())
}
