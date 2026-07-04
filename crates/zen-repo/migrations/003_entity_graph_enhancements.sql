-- Entity Graph Enhancements
-- Inspired by sqlite-graph (rohansx, MIT) research: bitemporal edges, FTS5 sync,
-- learning attributes, bidirectional traversal support.
--
-- All ALTER TABLE ADD COLUMN are safe (SQLite >= 3.35). The dead `aliases` TEXT
-- column on entities is left in place (backward compat with existing DBs) but
-- no longer selected or populated — canonical aliases live in entity_aliases.

-- ═══════════════════════════════════════════════════════════════════════════
-- ENTITIES: learning + description + properties
-- ═══════════════════════════════════════════════════════════════════════════
ALTER TABLE entities ADD COLUMN description TEXT NOT NULL DEFAULT '';
ALTER TABLE entities ADD COLUMN properties TEXT NOT NULL DEFAULT '{}';
ALTER TABLE entities ADD COLUMN access_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE entities ADD COLUMN last_accessed_at TEXT;
ALTER TABLE entities ADD COLUMN confidence REAL NOT NULL DEFAULT 0.5;
ALTER TABLE entities ADD COLUMN source TEXT NOT NULL DEFAULT 'manual';
ALTER TABLE entities ADD COLUMN promoted_at TEXT;

-- ═══════════════════════════════════════════════════════════════════════════
-- RELATIONSHIPS: description + bitemporal + weight
-- ═══════════════════════════════════════════════════════════════════════════
ALTER TABLE relationships ADD COLUMN description TEXT NOT NULL DEFAULT '';
ALTER TABLE relationships ADD COLUMN valid_from TEXT;
ALTER TABLE relationships ADD COLUMN valid_until TEXT;
ALTER TABLE relationships ADD COLUMN recorded_at TEXT NOT NULL DEFAULT (datetime('now'));
ALTER TABLE relationships ADD COLUMN weight REAL NOT NULL DEFAULT 1.0;

-- Composite indexes for bidirectional traversal + relation filtering
CREATE INDEX IF NOT EXISTS idx_rel_source_type ON relationships(source_entity_id, relation_type);
CREATE INDEX IF NOT EXISTS idx_rel_target_type ON relationships(target_entity_id, relation_type);

-- ═══════════════════════════════════════════════════════════════════════════
-- ENTITY FTS5: full-text search over entity name + description
-- ═══════════════════════════════════════════════════════════════════════════
CREATE VIRTUAL TABLE IF NOT EXISTS entities_fts USING fts5(
    entity_id UNINDEXED,
    name,
    description,
    entity_type,
    tokenize='porter'
);

-- Sync triggers: keep entities_fts in lockstep with entities
CREATE TRIGGER IF NOT EXISTS entities_fts_ai AFTER INSERT ON entities BEGIN
    INSERT INTO entities_fts (entity_id, name, description, entity_type)
    VALUES (new.id, new.name, new.description, new.entity_type);
END;

CREATE TRIGGER IF NOT EXISTS entities_fts_ad AFTER DELETE ON entities BEGIN
    DELETE FROM entities_fts WHERE entity_id = old.id;
END;

CREATE TRIGGER IF NOT EXISTS entities_fts_au AFTER UPDATE ON entities BEGIN
    DELETE FROM entities_fts WHERE entity_id = old.id;
    INSERT INTO entities_fts (entity_id, name, description, entity_type)
    VALUES (new.id, new.name, new.description, new.entity_type);
END;
