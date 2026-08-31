use std::path::{Path, PathBuf};
use std::sync::Once;

use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use thiserror::Error;
use tokio_rusqlite::Connection;

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
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
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
                Err(SqliteError::Sqlx(sqlx::Error::Io(std::io::Error::other(
                    msg,
                ))))
            }
        }
    }
}

/// Connect a connection pool to the given SQLite file.
///
/// # Errors
/// Returns [`SqliteError::Sqlx`] if the pool cannot be connected.
async fn connect_pool(db_path: &Path, lazy: bool) -> Result<SqlitePool> {
    let url = format!("sqlite://{}", db_path.display());
    let opts = SqlitePoolOptions::new().max_connections(4);
    let pool = if lazy {
        opts.connect_lazy(&url)?
    } else {
        opts.connect(&url).await?
    };
    Ok(pool)
}

pub struct SqliteClient {
    writer: Connection,
    pool: SqlitePool,
}

impl SqliteClient {
    pub async fn open(db_path: &Path) -> Result<Self> {
        register_sqlite_vec();
        let writer = setup_writer(db_path).await?;
        let pool = connect_pool(db_path, false).await?;
        run_migrations(&pool).await?;
        Ok(Self { writer, pool })
    }

    pub async fn open_lazy(db_path: &Path) -> Result<Self> {
        register_sqlite_vec();
        let writer = setup_writer(db_path).await?;
        let pool = connect_pool(db_path, true).await?;
        run_migrations(&pool).await?;
        Ok(Self { writer, pool })
    }

    /// Open a versioned SQLite database with automatic schema migration.
    ///
    /// Performs file-level version detection for `{domain}_{version}.sqlite`
    /// files in `logs_dir`. The resolver selects the latest versioned file
    /// by numeric version number, migrates it forward if its schema is
    /// behind, and returns a connected client.
    ///
    /// **Version semantics**: higher N = newer. The resolver picks the file
    /// with the highest N; if none exist, creates `{domain}_1.sqlite` fresh
    /// and runs all migrations.
    ///
    /// **Legacy fallback**: if no `{domain}_{N}.sqlite` files exist but a
    /// legacy unversioned `{domain}.db` file does exist, it is treated as
    /// version 1 and used as-is. This ensures backward compatibility with
    /// callers that previously opened `state.db`.
    ///
    /// **Rollback retention**: migration happens in-place on the selected
    /// file. Other versioned files on disk are never deleted or overwritten,
    /// so a previous binary version can fall back to its own file.
    ///
    /// **`version.json` exclusion**: `version.json` is CLI-release-only
    /// metadata and MUST NOT participate in DB file selection. This function
    /// only inspects `{domain}_{N}.sqlite` files and the legacy unversioned
    /// `{domain}.db`.
    ///
    /// # Parameters
    /// - `logs_dir`: Directory to scan for versioned DB files (typically the
    ///   data or logs directory).
    /// - `domain`: File stem prefix (e.g. `"state"`). The resolver looks for
    ///   `state_1.sqlite`, `state_2.sqlite`, etc.
    ///
    /// # Returns
    /// A [`SqliteClient`] connected to the selected (or newly created) file,
    /// with all pending migrations applied.
    ///
    /// # Errors
    /// Returns [`SqliteError`] if directory scanning, file creation, pool
    /// connection, or migration fails.
    ///
    /// # Example
    /// ```no_run
    /// # async fn example() -> std::result::Result<(), zen_repo::SqliteError> {
    /// use std::path::Path;
    /// let logs = Path::new("/data");
    /// let client = zen_repo::SqliteClient::open_versioned(logs, "state").await?;
    /// // client is connected to the latest state_N.sqlite (or state_1.sqlite)
    /// # Ok(())
    /// # }
    /// ```
    pub async fn open_versioned(logs_dir: &Path, domain: &str) -> Result<Self> {
        let resolved = resolve_versioned_path(logs_dir, domain);
        Self::open(&resolved).await
    }

    pub fn writer(&self) -> &Connection {
        &self.writer
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// Scan `logs_dir` for `{domain}_{N}.sqlite` files and return the highest
/// version number found. Returns `None` if no versioned files exist.
///
/// This helper only considers files matching the exact naming convention
/// `{domain}_{N}.sqlite` where N is a decimal integer. The legacy unversioned
/// `{domain}.db` file is NOT scanned by this helper — callers that need the
/// legacy fallback handle it separately.
pub fn probe_latest_version(logs_dir: &Path, domain: &str) -> Option<u32> {
    let prefix = format!("{domain}_");
    let suffix = ".sqlite";

    let mut latest: Option<u32> = None;

    let entries = std::fs::read_dir(logs_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();

        let Some(stripped) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some(stripped) = stripped.strip_suffix(suffix) else {
            continue;
        };
        if let Ok(version) = stripped.parse::<u32>() {
            latest = Some(latest.map_or(version, |v| v.max(version)));
        }
    }

    latest
}

/// Resolve the path for a versioned database file in `logs_dir`.
///
/// Selection order:
/// 1. If `{domain}_{N}.sqlite` files exist, returns the path for the latest N.
/// 2. If no versioned files exist but `{domain}.db` exists (legacy), returns
///    `{domain}.db` — treated as version 1 for backward compatibility.
/// 3. Otherwise returns `{domain}_1.sqlite` — caller will create it fresh.
fn resolve_versioned_path(logs_dir: &Path, domain: &str) -> PathBuf {
    if let Some(latest) = probe_latest_version(logs_dir, domain) {
        return logs_dir.join(format!("{domain}_{latest}.sqlite"));
    }

    let legacy = logs_dir.join(format!("{domain}.db"));
    if legacy.exists() {
        return legacy;
    }

    logs_dir.join(format!("{domain}_1.sqlite"))
}
