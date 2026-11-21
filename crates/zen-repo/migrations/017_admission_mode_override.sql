-- Migration 017: admission accept-mode runtime override (007 deviation closeout).
--
-- PURPOSE: `zen admission mode <value>` writes this single row; the qqbot
-- admission gate resolves per message: override row > config
-- [channels.qqbot] accept_mode > default Observe. The CLI contract
-- (docs/specs/007-agentic-admission/contracts/cli-admission.md) always
-- specified an owner-only trust flip — this table is the runtime surface
-- that makes it real while config stays the 5-layer read-only surface
-- (D22: one config key, no env key).
-- USAGE: applied automatically by sqlx::migrate!() on every
-- SqliteClient::open(). Single-row enforced by CHECK (id = 1), mirroring
-- migration 013's embedding_engine. Hot effect: the daemon re-resolves per
-- inbound message, so a CLI flip lands without `zen serve restart`.
-- EXPECTED: one empty table; existing tables untouched.
-- ERRORS: none — additive CREATE TABLE only (Principle XIII #2/#3:
--   forward-only, no ALTER of existing tables, no backfill).

CREATE TABLE IF NOT EXISTS admission_mode_override (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    -- off|observe|auto — validated by the CLI write path; an invalid stored
    -- value (hand-edited db) degrades loudly at read: warn + Observe (E11).
    accept_mode TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
