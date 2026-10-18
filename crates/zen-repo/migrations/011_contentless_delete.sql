-- Migration 011: enable DELETE on the contentless notes_fts index (2026-10-17
-- FTS5 rowid-churn data-bloat fix).
--
-- WHY contentless_delete=1 IS REQUIRED
-- `index_note` now derives ONE deterministic rowid from `req.id` and writes it
-- to BOTH `notes_meta` and `notes_fts`, so a re-index collides on the FTS rowid
-- and replaces instead of appending. On a BARE contentless table (`content=''`)
-- that collision is a trap: `INSERT OR REPLACE` leaves every previous version's
-- terms in the index, and because search JOINs
-- `notes_fts.rowid = notes_meta.rowid` there are no orphan rows left to filter
-- them out — the stale terms would be returned as hits for the CURRENT note,
-- i.e. user-visible search FALSE POSITIVES. Empirically verified with the
-- bundled SQLite:
--   bare contentless + stable rowid + INSERT OR REPLACE -> MATCH old term = 1
--   content='' , contentless_delete=1                   -> MATCH old term = 0
--   plain fts5 (control)                                -> MATCH old term = 0
-- So the stable rowid fix and this option ship together or not at all.
--
-- WHY DROP+CREATE IS THE SANCTIONED PROTOCOL
-- `notes_fts` is a purely DERIVED index: markdown files in the vault are the
-- canonical source of truth (AGENTS.md), and the FTS rows are a rebuildable
-- ranking structure. Dropping and recreating it loses no user data. This is the
-- backup-recreate-reimport shape Constitution Principle XIII prescribes for
-- destructive index changes: the rebuilt table is empty and is repopulated by
-- the existing reindex path (`zen wiki reindex`, and the zen-loop cycle every
-- ~5 minutes), which reads the canonical files and re-inserts them.
--
-- WHY THE PREVIOUS TRIGGER NEVER WORKED
-- Migration 001's `notes_fts_delete` trigger issued
-- `DELETE FROM notes_fts WHERE id = old.id`. Plain DELETE is UNSUPPORTED on a
-- bare contentless FTS5 table (`Error: cannot DELETE from contentless fts5
-- table`), and the trigger only fires on `notes_meta` DELETE — which
-- `INSERT OR REPLACE` never performs against the FTS table anyway. It was
-- inert. The index therefore only ever grew: production reached ~554k FTS rows
-- against ~2.5k live `notes_meta` rows (~554,739 orphans, 99.6%).
--
-- WHY THE NEW TRIGGER DELETES BY rowid
-- A contentless table stores no column values, so `id` is not readable and
-- cannot be matched; `rowid` is the ONLY key a contentless FTS5 table can
-- delete by. The recreated trigger uses `old.rowid`, which now coincides with
-- the deterministic rowid in both tables.
--
-- WHY THE UPDATE IS LOAD-BEARING
-- The recreated `notes_fts` is EMPTY while `notes_meta` keeps every
-- `content_hash`. The reindex change-gate compares each file's on-disk
-- checksum against the stored hash and skips matches, so without clearing the
-- hashes every file would read as "unchanged" and the index would never be
-- repopulated. Clearing the hashes forces one full reindex on the next run —
-- a self-healing migration whose cost is a single re-scan.
--
-- Forward-only additive (Principle XIII): no user data is altered; only the
-- derived index is rebuilt. The legacy trigger in 001 is left in place
-- (migrations are immutable); this migration replaces it by name.

DROP TRIGGER IF EXISTS notes_fts_delete;
DROP TABLE IF EXISTS notes_fts;
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
    id, title, content, tags,
    content='',
    contentless_delete=1,
    tokenize='porter'
);
CREATE TRIGGER IF NOT EXISTS notes_fts_delete
    AFTER DELETE ON notes_meta
BEGIN
    DELETE FROM notes_fts WHERE rowid = old.rowid;
END;
UPDATE notes_meta SET content_hash = '';
