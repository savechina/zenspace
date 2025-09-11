mod cli;
mod cmd;
mod config;
mod service;

fn main() {
    // Load environment variables from .env file.
    // Fails if .env file not found, not readable or invalid.
    if let Err(err) = dotenvy::dotenv() {
        eprintln!("Failed to load .env file: {}", err);
        std::process::exit(1);
    }

    cli::shell();
}
