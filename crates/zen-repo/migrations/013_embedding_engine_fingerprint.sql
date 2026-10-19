-- Migration 013: embedding engine fingerprint (D.1).
--
-- Records the (backend, model, dim) that produced the stored vec0 vectors so
-- the reindex layer can refuse to mix vectors from different engines: vec0 KNN
-- is L2 distance, so mixing silently corrupts results. Single-row enforced by
-- `CHECK (id = 1)`. Forward-only additive (Principle XIII): new table only —
-- a plain SQLite table, no vec0 involvement.
CREATE TABLE IF NOT EXISTS embedding_engine (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    backend TEXT NOT NULL,
    model TEXT NOT NULL,
    dim INTEGER NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
