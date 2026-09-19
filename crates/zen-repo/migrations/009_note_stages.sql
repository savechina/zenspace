-- Migration 009: durable per-note processing projection (FR-011).
--
-- FR-011 requires duplicate processing to be prevented via a durable per-note
-- identity: `notes_meta.content_hash` plus the last-completed stage, persisted
-- at every state transition, with job state re-derived from this projection
-- after a crash.
--
-- Why a separate table rather than columns on `notes_meta`:
-- `NotesRepo::index_note` writes `notes_meta` with INSERT OR REPLACE and an
-- explicit column list, so columns it does not name are reset to their
-- defaults. Re-indexing a note would therefore erase its recorded stage and
-- silently re-open a note that had already reached a terminal stage. Keeping
-- the projection separate decouples it from the search index's write path.
--
-- The key is (file_path, content_hash): a note whose content changed produces a
-- new hash, has no recorded stage, and is processed again — which is what
-- "same note, new content" should do. Additive and forward-only (Principle
-- XIII): no existing table or column is altered or dropped.

CREATE TABLE IF NOT EXISTS note_stages (
    file_path TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    last_completed_stage TEXT NOT NULL,
    stage_updated_at TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (file_path, content_hash)
);

CREATE INDEX IF NOT EXISTS idx_note_stages_stage ON note_stages(last_completed_stage);
