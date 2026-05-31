use std::error::Error;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    if let Err(err) = dotenvy::dotenv() {
        eprintln!("Warn: Failed to load .env file: {}", err);
    }

    zen_core::config::load_config()?;
    zen_cli::shell().await?;

    Ok(())
}
