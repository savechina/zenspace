use rusqlite::Connection;
use std::ffi::{c_char, c_int};
use std::path::Path;
use thiserror::Error;

// ---------------------------------------------------------------------------
// T027: SqliteRepo — minimal wrapper around rusqlite for schema management
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum SqliteError {
    #[error("sqlite error: {0}")]
    Rusqlite(#[from] rusqlite::Error),

    #[error("sqlite-vec extension not loaded: {0}")]
    VecExtensionMissing(String),
}

pub type Result<T> = std::result::Result<T, SqliteError>;

pub struct SqliteRepo {
    conn: Connection,
}

impl SqliteRepo {
    /// Open (or create) a SQLite database file.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        Ok(Self { conn })
    }

    /// Execute SQL with no result rows.
    pub fn execute(&self, sql: &str, params: &[&dyn rusqlite::types::ToSql]) -> Result<usize> {
        Ok(self.conn.execute(sql, params)?)
    }

    /// Execute a batch of SQL statements (DDL).
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        Ok(self.conn.execute_batch(sql)?)
    }

    /// Query a single row into a type via rusqlite's FromRow.
    pub fn query_row<T, F>(
        &self,
        sql: &str,
        params: &[&dyn rusqlite::types::ToSql],
        f: F,
    ) -> Result<T>
    where
        F: FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    {
        Ok(self.conn.query_row(sql, params, f)?)
    }

    /// Query multiple rows via rusqlite's FromRow.
    pub fn query_map<T, F>(
        &self,
        sql: &str,
        params: &[&dyn rusqlite::types::ToSql],
        f: F,
    ) -> Result<Vec<T>>
    where
        F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, f)?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Prepare a statement for repeated execution.
    pub fn prepare(&self, sql: &str) -> Result<rusqlite::Statement<'_>> {
        Ok(self.conn.prepare(sql)?)
    }

    /// Begin a transaction.
    pub fn transaction(&mut self) -> Result<rusqlite::Transaction<'_>> {
        Ok(self.conn.transaction()?)
    }

    /// Access the inner connection for advanced usage.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

// ---------------------------------------------------------------------------
// T028: kb.db schema — FTS5 full-text search index
// Reference: data-model.md §8.1
// ---------------------------------------------------------------------------

pub fn init_kb_schema(repo: &SqliteRepo) -> Result<()> {
    repo.execute_batch(
        r#"
        -- FTS5 virtual table for full-text search over notes and wiki pages
        CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
            id UNINDEXED,
            title,
            content,
            tags,
            tokenize='porter'
        );

        -- Metadata shadow table for ranking and filtering
        CREATE TABLE IF NOT EXISTS notes_meta (
            id TEXT PRIMARY KEY,
            file_path TEXT NOT NULL,
            source TEXT NOT NULL,
            domain TEXT,
            project TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            content_hash TEXT
        );

        -- Trigger: sync FTS index on note metadata insert
        CREATE TRIGGER IF NOT EXISTS notes_fts_after_insert AFTER INSERT ON notes_meta BEGIN
            INSERT INTO notes_fts(rowid, id, title, content, tags)
            VALUES (new.rowid, new.id, '', '', '');
        END;
        "#,
    )
}

// ---------------------------------------------------------------------------
// T029: vec.db schema — vector embeddings (sqlite-vec required)
// Reference: data-model.md §8.2
// ---------------------------------------------------------------------------

pub fn init_vec_schema(repo: &SqliteRepo) -> Result<()> {
    repo.execute_batch(
        r#"
        -- Note and wiki page embeddings
        CREATE VIRTUAL TABLE IF NOT EXISTS note_embeddings USING vec0(
            note_id TEXT PRIMARY KEY,
            embedding FLOAT[384]
        );

        -- Entity embeddings for entity similarity search
        CREATE VIRTUAL TABLE IF NOT EXISTS entity_embeddings USING vec0(
            entity_id TEXT PRIMARY KEY,
            embedding FLOAT[384]
        );

        -- Metadata: tracks which version/model produced embeddings
        CREATE TABLE IF NOT EXISTS embedding_meta (
            entity_id TEXT PRIMARY KEY,
            model_name TEXT DEFAULT 'all-MiniLM-L6-v2',
            model_version TEXT DEFAULT '1.0',
            embedded_at TEXT NOT NULL
        );
        "#,
    )
    .map_err(|e| {
        let msg = e.to_string();
        if msg.contains("vec0") || msg.contains("no such virtual table module") {
            SqliteError::VecExtensionMissing(
                "sqlite-vec extension not loaded. \
                 Call load_vec_extension() before init_vec_schema(), \
                 or see crates/zen-data/src/sqlite_repo.rs for setup instructions."
                    .into(),
            )
        } else {
            e
        }
    })
}

// ---------------------------------------------------------------------------
// T030: graph.db schema — entity graph and dispatch tasks
// Reference: data-model.md §8.3
// ---------------------------------------------------------------------------

pub fn init_graph_schema(repo: &SqliteRepo) -> Result<()> {
    repo.execute_batch(
        r#"
        -- Entity nodes from knowledge consolidation
        CREATE TABLE IF NOT EXISTS entities (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            entity_type TEXT NOT NULL,
            aliases TEXT,
            first_seen TEXT NOT NULL,
            last_updated TEXT NOT NULL,
            domain TEXT,
            UNIQUE(name, entity_type)
        );

        -- Indexes for entity lookups
        CREATE INDEX IF NOT EXISTS idx_entities_name ON entities(name);
        CREATE INDEX IF NOT EXISTS idx_entities_type ON entities(entity_type);

        -- Relationship edges between entities
        CREATE TABLE IF NOT EXISTS relationships (
            id TEXT PRIMARY KEY,
            source_entity_id TEXT NOT NULL,
            target_entity_id TEXT NOT NULL,
            relation_type TEXT NOT NULL,
            confidence REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
            source_note_ids TEXT,
            created_at TEXT NOT NULL,
            FOREIGN KEY (source_entity_id) REFERENCES entities(id),
            FOREIGN KEY (target_entity_id) REFERENCES entities(id),
            CHECK(source_entity_id != target_entity_id)
        );

        -- Indexes for graph traversal
        CREATE INDEX IF NOT EXISTS idx_rel_source ON relationships(source_entity_id);
        CREATE INDEX IF NOT EXISTS idx_rel_target ON relationships(target_entity_id);
        CREATE INDEX IF NOT EXISTS idx_rel_type ON relationships(relation_type);

        -- Dispatch task tracking for sub-agent spawns
        CREATE TABLE IF NOT EXISTS dispatch_tasks (
            id TEXT PRIMARY KEY,
            target TEXT NOT NULL,
            task_description TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'queued',
            context_files TEXT,
            result_summary TEXT,
            created_at TEXT NOT NULL,
            completed_at TEXT
        );

        -- Indexes for dispatch task queries
        CREATE INDEX IF NOT EXISTS idx_dispatch_status ON dispatch_tasks(status);
        CREATE INDEX IF NOT EXISTS idx_dispatch_created ON dispatch_tasks(created_at);
        "#,
    )
}

// ---------------------------------------------------------------------------
// T031: habit/finance schema
// Reference: data-model.md §8.4
// ---------------------------------------------------------------------------

pub fn init_transactions_schema(repo: &SqliteRepo) -> Result<()> {
    repo.execute_batch(
        r#"
        -- Habit definitions (synced from habits.toml)
        CREATE TABLE IF NOT EXISTS habits (
            name TEXT PRIMARY KEY,
            frequency TEXT NOT NULL,
            target TEXT NOT NULL,
            reminders_enabled INTEGER DEFAULT 0,
            streak_count INTEGER DEFAULT 0,
            created_at TEXT NOT NULL
        );

        -- Habit check-in events (authoritative for streak calculation)
        CREATE TABLE IF NOT EXISTS habit_checkins (
            id TEXT PRIMARY KEY,
            habit_name TEXT NOT NULL REFERENCES habits(name),
            checked_at TEXT NOT NULL,
            note TEXT,
            UNIQUE(habit_name, checked_at)
        );

        CREATE INDEX IF NOT EXISTS idx_checkins_habit_date
            ON habit_checkins(habit_name, checked_at);

        -- Finance transactions (structured storage)
        CREATE TABLE IF NOT EXISTS finance_transactions (
            id TEXT PRIMARY KEY,
            entry_type TEXT NOT NULL,
            description TEXT NOT NULL,
            amount REAL NOT NULL,
            currency TEXT DEFAULT 'CNY',
            asset TEXT,
            date TEXT NOT NULL,
            source TEXT NOT NULL,
            category TEXT,
            converted_amount REAL,
            base_currency TEXT DEFAULT 'CNY',
            created_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_txn_date ON finance_transactions(date);
        CREATE INDEX IF NOT EXISTS idx_txn_category ON finance_transactions(category);
        CREATE INDEX IF NOT EXISTS idx_txn_asset ON finance_transactions(asset);

        -- Finance positions (aggregated from transactions)
        CREATE TABLE IF NOT EXISTS finance_positions (
            asset TEXT PRIMARY KEY,
            total_shares REAL NOT NULL,
            avg_cost REAL NOT NULL,
            current_value REAL,
            last_updated TEXT NOT NULL
        );
        "#,
    )
}

// ---------------------------------------------------------------------------
// T032: sqlite-vec extension loading
// Reference: research.md R7, R8
//
// sqlite-vec is a loadable SQLite extension. rusqlite's bundled build
// does NOT include it. Two loading approaches:
//
// **Approach A — Manual loading at runtime (recommended)**
//
// 1. Download or build sqlite-vec shared library:
//    ```bash
//    # Pre-built binary
//    curl -L -o ~/.zen/lib/sqlite_vec.dylib \
//      https://github.com/asg017/sqlite-vec/releases/latest/download/sqlite-vec-x86_64.dylib
//
//    # Or build from source
//    git clone https://github.com/asg017/sqlite-vec && cd sqlite-vec
//    cmake -S . -B .cargo-out -DBUILD_SHARED_LIBS=ON
//    cmake --build .cargo-out --target sqlite_vec0
//    cp .cargo-out/libsqlite_vec0.dylib ~/.zen/lib/
//    ```
//
// 2. Load via SQLite's load_extension:
//    ```rust
//    sqlz::sqlite_repo::load_vec_extension("/path/to/libsqlite_vec0.dylib")?;
//    // Now vec0 virtual tables work in subsequent open() calls
//    ```
//
// **Approach B — sqlite3_auto_extension**
//
// This is NOT directly available through rusqlite's safe API.
// Using it would require unsafe FFI via `conn.handle()` to get the raw
// `sqlite3*` pointer, then call `sqlite3_auto_extension()` with the
// `sqlite3_vec_init` function pointer. See the sqlite-vec documentation
// for Rust integration examples.
// ---------------------------------------------------------------------------

/// Load the sqlite-vec extension for the current process.
///
/// After calling this, any subsequent `Connection::open()` can use `vec0`
/// virtual tables. Must be called BEFORE opening vec.db.
///
/// Path should point to the sqlite-vec shared library:
/// - macOS: `libsqlite_vec0.dylib`
/// - Linux: `libsqlite_vec0.so`
/// - Windows: `sqlite_vec0.dll`
///
/// On success, subsequent calls are no-ops (sqlite3_auto_extension is idempotent).
#[cfg(target_os = "macos")]
pub fn load_vec_extension(lib_path: impl AsRef<Path>) -> Result<()> {
    // Safety: sqlite3_auto_extension registers a function pointer that
    // will be called at sqlite3_open time. The function is from a
    // well-known, stable extension (sqlite-vec).
    use std::ffi::{CString, c_void};

    let path_c =
        CString::new(lib_path.as_ref().to_str().ok_or_else(|| {
            SqliteError::VecExtensionMissing("path contains invalid UTF-8".into())
        })?)
        .map_err(|e| SqliteError::VecExtensionMissing(format!("CString error: {e}")))?;

    let handle = unsafe { libc::dlopen(path_c.as_ptr(), libc::RTLD_NOW) };
    if handle.is_null() {
        let err = unsafe { libc::dlerror() };
        let msg = if err.is_null() {
            "dlopen failed".into()
        } else {
            unsafe { std::ffi::CStr::from_ptr(err).to_string_lossy().into_owned() }
        };
        return Err(SqliteError::VecExtensionMissing(format!("dlopen: {msg}")));
    }

    let symbol = CString::new("sqlite3_vec_init").unwrap();
    let init_fn: unsafe extern "C" fn(
        *mut c_void,
        *mut *mut c_char,
        *const rusqlite::ffi::sqlite3_api_routines,
    ) -> c_int = unsafe {
        let sym = libc::dlsym(handle, symbol.as_ptr());
        if sym.is_null() {
            return Err(SqliteError::VecExtensionMissing(
                "sqlite3_vec_init symbol not found in library".into(),
            ));
        }
        std::mem::transmute(sym)
    };

    let raw_ptr: *mut c_void = init_fn as *mut c_void;
    unsafe {
        #[allow(clippy::missing_transmute_annotations)]
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(raw_ptr)));
    }

    Ok(())
}

/// Fallback for non-macOS platforms.
///
/// On non-macOS, use per-connection `load_extension` instead:
/// ```ignore
/// let repo = SqliteRepo::open(&path)?;
/// repo.conn().load_extension("/path/to/libsqlite_vec0.so", None)?;
/// init_vec_schema(&repo)?;
/// ```
#[cfg(not(target_os = "macos"))]
pub fn load_vec_extension(_lib_path: impl AsRef<Path>) -> Result<()> {
    Err(SqliteError::VecExtensionMissing(
        "sqlite-vec auto-extension loading is only supported on macOS with dlopen. \
         On other platforms, use repo.conn().load_extension(path, None)? before init_vec_schema()."
            .into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        (dir, path)
    }

    #[test]
    fn test_open_creates_db_file() {
        let (_dir, mut path) = setup();
        path.push("test.db");
        let result = SqliteRepo::open(&path);
        assert!(result.is_ok());
        assert!(path.exists());
    }

    #[test]
    fn test_kb_schema_creates_tables_and_trigger() {
        let (_dir, mut path) = setup();
        path.push("kb.db");
        let repo = SqliteRepo::open(&path).unwrap();
        init_kb_schema(&repo).unwrap();

        // Verify tables exist via sqlite_master
        let count: i32 = repo
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('notes_meta')",
                &[],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // FTS5 virtual table
        let vcount: i32 = repo
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='notes_fts'",
                &[],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(vcount, 1);

        // Trigger exists
        let tcount: i32 = repo
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='trigger' AND name='notes_fts_after_insert'",
                &[],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tcount, 1);
    }

    #[test]
    fn test_graph_schema_creates_tables_and_indexes() {
        let (_dir, mut path) = setup();
        path.push("graph.db");
        let repo = SqliteRepo::open(&path).unwrap();
        init_graph_schema(&repo).unwrap();

        // Verify entities table
        let count: i32 = repo
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='entities'",
                &[],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Verify relationships table
        let rcount: i32 = repo
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='relationships'",
                &[],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rcount, 1);

        // Verify dispatch_tasks table
        let dcount: i32 = repo
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='dispatch_tasks'",
                &[],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dcount, 1);

        // Verify indexes
        let idx_count: i32 = repo
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name LIKE 'idx_%'",
                &[],
                |r| r.get(0),
            )
            .unwrap();
        assert!(idx_count >= 6); // 2 for entities + 3 for relationships + 2 for dispatch (7 total, minus possible AUTOINDEX)
    }

    #[test]
    fn test_transactions_schema_creates_all_tables() {
        let (_dir, mut path) = setup();
        path.push("transactions.db");
        let repo = SqliteRepo::open(&path).unwrap();
        init_transactions_schema(&repo).unwrap();

        let tables = [
            "habits",
            "habit_checkins",
            "finance_transactions",
            "finance_positions",
        ];
        for table in tables {
            let count: i32 = repo
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{table}'"
                    ),
                    &[],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table {table} not found");
        }

        // Verify unique constraint on habit_name + checked_at
        let idx_count: i32 = repo
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_checkins_habit_date'",
                &[],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx_count, 1);
    }

    #[test]
    fn test_kb_schema_is_idempotent() {
        let (_dir, mut path) = setup();
        path.push("kb.db");
        let repo = SqliteRepo::open(&path).unwrap();
        init_kb_schema(&repo).unwrap();
        // Should not error on second call
        init_kb_schema(&repo).unwrap();
    }

    #[test]
    fn test_graph_schema_is_idempotent() {
        let (_dir, mut path) = setup();
        path.push("graph.db");
        let repo = SqliteRepo::open(&path).unwrap();
        init_graph_schema(&repo).unwrap();
        init_graph_schema(&repo).unwrap();
    }

    #[test]
    fn test_transactions_schema_is_idempotent() {
        let (_dir, mut path) = setup();
        path.push("transactions.db");
        let repo = SqliteRepo::open(&path).unwrap();
        init_transactions_schema(&repo).unwrap();
        init_transactions_schema(&repo).unwrap();
    }
}
