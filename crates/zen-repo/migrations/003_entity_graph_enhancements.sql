-- Notion Graph Enhancements
-- Inspired by sqlite-graph (rohansx, MIT) research: bitemporal edges, FTS5 sync,
-- learning attributes, bidirectional traversal support.
--
-- All ALTER TABLE ADD COLUMN are safe (SQLite >= 3.35). The dead `aliases` TEXT
-- column on notions is left in place (backward compat with existing DBs) but
-- no longer selected or populated — canonical aliases live in notion_aliases.

-- ═══════════════════════════════════════════════════════════════════════════
-- ENTITIES: learning + description + properties
-- ═══════════════════════════════════════════════════════════════════════════
ALTER TABLE notions ADD COLUMN description TEXT NOT NULL DEFAULT '';
ALTER TABLE notions ADD COLUMN properties TEXT NOT NULL DEFAULT '{}';
ALTER TABLE notions ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE notions ADD COLUMN last_accessed_at TEXT;
ALTER TABLE notions ADD COLUMN confidence REAL NOT NULL DEFAULT 0.5;
ALTER TABLE notions ADD COLUMN source TEXT NOT NULL DEFAULT 'manual';
ALTER TABLE notions ADD COLUMN promoted_at TEXT;

-- ═══════════════════════════════════════════════════════════════════════════
-- RELATIONSHIPS: description + bitemporal + weight
-- ═══════════════════════════════════════════════════════════════════════════
ALTER TABLE relationships ADD COLUMN description TEXT NOT NULL DEFAULT '';
ALTER TABLE relationships ADD COLUMN valid_from TEXT;
ALTER TABLE relationships ADD COLUMN valid_until TEXT;
ALTER TABLE relationships ADD COLUMN recorded_at TEXT NOT NULL DEFAULT (datetime('now'));
ALTER TABLE relationships ADD COLUMN weight REAL NOT NULL DEFAULT 1.0;

-- Composite indexes for bidirectional traversal + relation filtering
CREATE INDEX IF NOT EXISTS idx_rel_source_type ON relationships(source_notion_id, relation_type);
CREATE INDEX IF NOT EXISTS idx_rel_target_type ON relationships(target_notion_id, relation_type);

-- ═══════════════════════════════════════════════════════════════════════════
-- ENTITY FTS5: full-text search over notion name + description
-- ═══════════════════════════════════════════════════════════════════════════
CREATE VIRTUAL TABLE IF NOT EXISTS notions_fts USING fts5(
    notion_id UNINDEXED,
    name,
    description,
    kind,
    tokenize='porter'
);

-- Sync triggers: keep notions_fts in lockstep with notions
CREATE TRIGGER IF NOT EXISTS notions_fts_ai AFTER INSERT ON notions BEGIN
    INSERT INTO notions_fts (notion_id, name, description, kind)
    VALUES (new.id, new.name, new.description, new.kind);
END;

CREATE TRIGGER IF NOT EXISTS notions_fts_ad AFTER DELETE ON notions BEGIN
    DELETE FROM notions_fts WHERE notion_id = old.id;
END;

CREATE TRIGGER IF NOT EXISTS notions_fts_au AFTER UPDATE ON notions BEGIN
    DELETE FROM notions_fts WHERE notion_id = old.id;
    INSERT INTO notions_fts (notion_id, name, description, kind)
    VALUES (new.id, new.name, new.description, new.kind);
END;
