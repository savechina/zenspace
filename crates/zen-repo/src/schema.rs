use sqlx::sqlite::SqlitePool;

/// All table creation SQL for the zen-repo Phase 2 repository layer.
/// Run on first connection via `migrate(pool)`.
pub static NOTES_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS notes (
    id          TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL,
    content     TEXT NOT NULL,
    sensitivity TEXT NOT NULL DEFAULT 'Private',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_notes_session ON notes(session_id);
CREATE INDEX IF NOT EXISTS idx_notes_sensitivity ON notes(sensitivity);
"#;

pub static AGENT_PROFILES_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS agent_profiles (
    name        TEXT PRIMARY KEY,
    role        TEXT,
    config_json TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
"#;

pub static AUDIT_LOGS_TABLE_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS audit_logs (
    id          TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL,
    prompt_hash TEXT NOT NULL,
    timestamp   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_session ON audit_logs(session_id);
CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_logs(timestamp);
"#;

/// Run all embedded schema migrations against the pool.
/// Safe to call multiple times — all statements use IF NOT EXISTS.
pub async fn migrate(pool: &SqlitePool) -> sqlx::Result<()> {
    sqlx::query(NOTES_TABLE_SQL).execute(pool).await?;
    sqlx::query(AGENT_PROFILES_TABLE_SQL).execute(pool).await?;
    sqlx::query(AUDIT_LOGS_TABLE_SQL).execute(pool).await?;
    Ok(())
}
