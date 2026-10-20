use std::path::{Path, PathBuf};
use std::sync::{Once, OnceLock};

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

    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),

    #[error("migration lock error: {0}")]
    MigrationLock(String),
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

/// Acquire the migration lock without blocking the async runtime: the wait
/// loop is a blocking sleep, so it runs on the blocking pool. The returned
/// guard must be held across `run_migrations`.
async fn migration_guard(db_path: &Path) -> Result<std::fs::File> {
    let owned = db_path.to_path_buf();
    tokio::task::spawn_blocking(move || acquire_migration_lock(&owned))
        .await
        .map_err(|e| SqliteError::MigrationLock(format!("migration lock task: {e}")))?
}

/// Rebuild the embedded migration set with `ignore_missing: true`.
///
/// Spec D16 (Runtime Compatibility Guard — Design, owner decision 2026-10-14:
/// B+). An old binary opening a database already migrated by a newer build
/// must keep working: zen's migrations are strictly additive (Principle XIII),
/// so new-SQL-only objects are invisible to the old code's reads/writes and the
/// old binary is safe. `Migrator` is not `Clone` and `sqlx::migrate!()` yields a
/// const the macro cannot mark `ignore_missing`, so the set is rebuilt
/// field-by-field after the codex-rs `state/src/migrations.rs`
/// `runtime_migrator` pattern.
///
/// The six `sqlx::migrate::Migrator` fields (verified against sqlx-core
/// 0.9.0 `src/migrate/migrator.rs`: `migrations`, `ignore_missing`,
/// `locking`, `no_tx`, `table_name`, `create_schemas`) are copied verbatim
/// from the embedded const; only `ignore_missing` is overridden.
///
/// Semantics preserved: `ignore_missing` short-circuits ONLY
/// `validate_applied_migrations` (applied ⊋ embedded). Checksum validation for
/// versions present in both sets (`VersionMismatch`) and the `Dirty` guard
/// still fire in sqlx's apply loop — tolerance does NOT weaken them.
fn runtime_migrator() -> sqlx::migrate::Migrator {
    let embedded = sqlx::migrate!("./migrations");
    sqlx::migrate::Migrator {
        migrations: embedded.migrations,
        ignore_missing: true,
        locking: embedded.locking,
        no_tx: embedded.no_tx,
        table_name: embedded.table_name,
        create_schemas: embedded.create_schemas,
    }
}

/// Highest version recorded in the embedded migration set.
///
/// Spec D16 skew observability: paired with [`detect_schema_skew`] so a caller
/// can compare the on-disk schema against the binary's embedded set. Built from
/// the same `sqlx::migrate!()` const used by [`runtime_migrator`] and cached
/// once per process (the set is static). Returns `0` for an empty set.
pub fn embedded_migration_max() -> i64 {
    static MAX: OnceLock<i64> = OnceLock::new();
    *MAX.get_or_init(|| {
        sqlx::migrate!("./migrations")
            .iter()
            .map(|m| m.version)
            .max()
            .unwrap_or(0)
    })
}

/// Read-only applied-ahead skew probe.
///
/// Returns `Some((db_max, embedded_max))` ONLY when the database's highest
/// applied migration exceeds the binary's embedded set — the
/// old-binary-vs-newer-DB direction that [`runtime_migrator`] tolerates. A
/// missing `_sqlx_migrations` table (fresh DB) or an empty table yields
/// `None`, never an error: the query is wrapped to `None` so callers can run
/// it on every startup without guarding.
///
/// **Callers own logging** — zen-repo has no `tracing` dependency and must stay
/// that way, so this returns data only. The doctor probe and the gateway daemon
/// startup line are the intended consumers (spec D16/D17), each emitting at most
/// one informational line; under tolerance the state is benign, never a refusal.
pub async fn detect_schema_skew(pool: &SqlitePool) -> Option<(i64, i64)> {
    let db_max: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(pool)
        .await
        .ok()?;
    let embedded_max = embedded_migration_max();
    if db_max > embedded_max {
        Some((db_max, embedded_max))
    } else {
        None
    }
}

async fn run_migrations(pool: &SqlitePool) -> Result<()> {
    let migrations = runtime_migrator();

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
    /// Per-database [`GraphCore`](crate::notions_repo::GraphCoreCache)
    /// snapshot cache — a field (not a static) so concurrent clients on
    /// DIFFERENT databases (tests) can never serve each other's graph.
    pub(crate) graph_core_cache: crate::notions_repo::GraphCoreCache,
}

/// How long a process waits for another process to finish migrating before
/// giving up (250 ms x 120 = ~30 s).
const MIGRATION_LOCK_ATTEMPTS: u32 = 120;
const MIGRATION_LOCK_RETRY: std::time::Duration = std::time::Duration::from_millis(250);

/// Lock file serializing migrations for one database.
fn migration_lock_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.file_name().unwrap_or_default().to_os_string();
    name.push(".migrate.lock");
    db_path.with_file_name(name)
}

/// Take the cross-process migration lock for `db_path`.
///
/// sqlx-sqlite's own `Migrator::lock` is a no-op, so two processes opening a
/// fresh database at the same time both read the same pending migration set
/// and both try to apply it — the loser's `open` fails. Holding this flock
/// makes the second process wait, then find the migration already recorded.
/// The lock is advisory and released by the kernel if the holder dies.
fn acquire_migration_lock(db_path: &Path) -> Result<std::fs::File> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let path = migration_lock_path(db_path);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    for attempt in 0..MIGRATION_LOCK_ATTEMPTS {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                if attempt + 1 == MIGRATION_LOCK_ATTEMPTS {
                    return Err(SqliteError::MigrationLock(format!(
                        "timed out waiting for {}",
                        path.display()
                    )));
                }
                std::thread::sleep(MIGRATION_LOCK_RETRY);
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(SqliteError::MigrationLock(format!(
                    "lock {}: {e}",
                    path.display()
                )));
            }
        }
    }
    unreachable!("loop returns on success, timeout and error")
}

impl SqliteClient {
    pub async fn open(db_path: &Path) -> Result<Self> {
        register_sqlite_vec();
        let writer = setup_writer(db_path).await?;
        let pool = connect_pool(db_path, false).await?;
        let _migration_lock = migration_guard(db_path).await?;
        run_migrations(&pool).await?;
        Ok(Self {
            writer,
            pool,
            graph_core_cache: crate::notions_repo::GraphCoreCache::new(),
        })
    }

    pub async fn open_lazy(db_path: &Path) -> Result<Self> {
        register_sqlite_vec();
        let writer = setup_writer(db_path).await?;
        let pool = connect_pool(db_path, true).await?;
        let _migration_lock = migration_guard(db_path).await?;
        run_migrations(&pool).await?;
        Ok(Self {
            writer,
            pool,
            graph_core_cache: crate::notions_repo::GraphCoreCache::new(),
        })
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
