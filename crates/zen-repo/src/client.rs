use std::path::Path;
use std::sync::Once;

use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use tokio_rusqlite::Connection;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SqliteError {
    #[error("sqlite error: {0}")]
    Rusqlite(#[from] rusqlite::Error),

    #[error("sqlite error: {0}")]
    TokioRusqlite(#[from] tokio_rusqlite::Error),

    #[error("sqlite error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("sqlite-vec extension not loaded: {0}")]
    VecExtensionMissing(String),
}

pub type Result<T> = std::result::Result<T, SqliteError>;

static REGISTER_VEC_EXTENSION: Once = Once::new();

fn register_sqlite_vec() {
    REGISTER_VEC_EXTENSION.call_once(|| {
        #[allow(clippy::missing_transmute_annotations)]
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(
                std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ()),
            ));
        }
    });
}

async fn setup_writer(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            SqliteError::Sqlx(sqlx::Error::Io(std::io::Error::other(format!(
                "failed to create db directory: {e}"
            ))))
        })?;
    }

    register_sqlite_vec();

    let writer = Connection::open(db_path).await?;
    writer
        .call(|conn| {
            conn.execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA foreign_keys=ON;
                 PRAGMA synchronous=NORMAL;
                 PRAGMA busy_timeout=5000;",
            )?;
            Ok(())
        })
        .await?;

    Ok(writer)
}

async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    let migrations = sqlx::migrate!("./migrations");

    match migrations.run(pool).await {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("vec0") || msg.contains("no such virtual table module") {
                eprintln!(
                    "[zen-repo] WARNING: sqlite-vec extension not loaded. \
                     Vector search disabled: {msg}"
                );
                Ok(())
            } else {
                Err(SqliteError::Sqlx(sqlx::Error::Io(
                    std::io::Error::other(msg),
                )))
            }
        }
    }
}

pub struct SqliteClient {
    writer: Connection,
    pool: SqlitePool,
}

impl SqliteClient {
    pub async fn open(db_path: &Path) -> Result<Self> {
        register_sqlite_vec();
        let writer = setup_writer(db_path).await?;

        let url = format!("sqlite://{}", db_path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await?;

        run_migrations(&pool).await?;

        Ok(Self { writer, pool })
    }

    pub async fn open_lazy(db_path: &Path) -> Result<Self> {
        register_sqlite_vec();
        let writer = setup_writer(db_path).await?;

        let url = format!("sqlite://{}", db_path.display());
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_lazy(&url)?;

        run_migrations(&pool).await?;

        Ok(Self { writer, pool })
    }

    pub fn writer(&self) -> &Connection {
        &self.writer
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}
