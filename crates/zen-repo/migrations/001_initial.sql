-- Core schema for zen-repo
-- Sessions
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    file_path TEXT NOT NULL,
    agent_name TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    workspace TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at DESC);

-- Notes metadata
CREATE TABLE IF NOT EXISTS notes_meta (
    id TEXT PRIMARY KEY,
    file_path TEXT NOT NULL,
    source TEXT NOT NULL,
    domain TEXT NOT NULL DEFAULT '',
    project TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    content_hash TEXT NOT NULL DEFAULT ''
);

-- Full-text search for notes
CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
    id, title, content, tags,
    content='',
    tokenize='porter'
);

-- Trigger to clean up FTS when notes are deleted
CREATE TRIGGER IF NOT EXISTS notes_fts_delete
    AFTER DELETE ON notes_meta
BEGIN
    DELETE FROM notes_fts WHERE id = old.id;
END;

-- Entities
CREATE TABLE IF NOT EXISTS notions (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    aliases TEXT,
    created_at TEXT NOT NULL,
    last_updated TEXT NOT NULL,
    domain TEXT,
    UNIQUE(name, kind)
);
CREATE INDEX IF NOT EXISTS idx_notions_name ON notions(name);
CREATE INDEX IF NOT EXISTS idx_notions_type ON notions(kind);

-- Relationships
CREATE TABLE IF NOT EXISTS relationships (
    id TEXT PRIMARY KEY,
    source_notion_id TEXT NOT NULL,
    target_notion_id TEXT NOT NULL,
    relation_type TEXT NOT NULL,
    confidence REAL NOT NULL DEFAULT 1.0,
    source_note_ids TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY (source_notion_id) REFERENCES notions(id),
    FOREIGN KEY (target_notion_id) REFERENCES notions(id),
    CHECK(source_notion_id != target_notion_id)
);
CREATE INDEX IF NOT EXISTS idx_rel_source ON relationships(source_notion_id);
CREATE INDEX IF NOT EXISTS idx_rel_target ON relationships(target_notion_id);
CREATE INDEX IF NOT EXISTS idx_rel_type ON relationships(relation_type);

-- Notion aliases
CREATE TABLE IF NOT EXISTS notion_aliases (
    alias TEXT NOT NULL,
    canonical_notion_id TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (alias, canonical_notion_id),
    FOREIGN KEY (canonical_notion_id) REFERENCES notions(id)
);
CREATE INDEX IF NOT EXISTS idx_aliases_lookup ON notion_aliases(alias);

-- Dispatch tasks
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
CREATE INDEX IF NOT EXISTS idx_dispatch_status ON dispatch_tasks(status);

-- Self nodes (idnotion graph)
CREATE TABLE IF NOT EXISTS self_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    layer TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    domain TEXT NOT NULL DEFAULT 'uncategorized',
    is_explicit INTEGER,
    sufficient_for TEXT,
    necessary_for TEXT,
    controllability REAL,
    humility_score REAL,
    optionality_count INTEGER,
    core_pursuit TEXT,
    source TEXT NOT NULL DEFAULT 'manual',
    confidence REAL NOT NULL DEFAULT 0.5,
    evidence_refs TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(name, layer)
);
CREATE INDEX IF NOT EXISTS idx_self_nodes_layer ON self_nodes(layer);

-- Goal nodes
CREATE TABLE IF NOT EXISTS goal_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    controllability REAL NOT NULL,
    core_pursuit TEXT NOT NULL,
    deadline TEXT,
    created_at TEXT NOT NULL,
    last_updated TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_goal_nodes_name ON goal_nodes(name);

-- Path nodes
CREATE TABLE IF NOT EXISTS path_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    serves_goal_id TEXT,
    is_default INTEGER NOT NULL DEFAULT 0,
    crowdedness REAL NOT NULL DEFAULT 0.5,
    alternatives TEXT,
    created_at TEXT NOT NULL,
    last_updated TEXT NOT NULL,
    FOREIGN KEY (serves_goal_id) REFERENCES goal_nodes(id)
);
CREATE INDEX IF NOT EXISTS idx_path_nodes_name ON path_nodes(name);

-- Belief nodes
CREATE TABLE IF NOT EXISTS belief_nodes (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    proposition TEXT NOT NULL,
    prior REAL NOT NULL DEFAULT 0.5,
    posterior REAL NOT NULL DEFAULT 0.5,
    evidence_count INTEGER NOT NULL DEFAULT 0,
    last_updated TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_belief_nodes_name ON belief_nodes(name);
