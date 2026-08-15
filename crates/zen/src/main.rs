use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    zen_core::process_hardening::init();

    if let Err(err) = dotenvy::dotenv() {
        eprintln!("Warn: Failed to load .env file: {}", err);
    }

    zen_cli::shell().await?;

    Ok(())
}
