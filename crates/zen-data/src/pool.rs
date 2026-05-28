use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use std::path::Path;

use crate::schema;

pub async fn create_pool(db_path: &Path) -> Result<SqlitePool, sqlx::Error> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            sqlx::Error::Io(std::io::Error::other(format!(
                "failed to create db directory: {e}"
            )))
        })?;
    }

    let url = format!("sqlite://{}", db_path.display());
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await?;

    schema::migrate(&pool).await?;

    Ok(pool)
}
