-- Migration 010: community detection persistence (T141).
--
-- T141's partitioner (deterministic Louvain over the undirected weighted
-- projection of open relationship edges) persists its output here so the
-- summarization surface (part B: wiki pages for communities above a minimum
-- size) has a durable read path.
--
-- Recomputation replaces, never grows: `replace_communities` deletes the
-- previous rows for the same (algorithm, resolution) before inserting, so the
-- tables cannot grow monotonically with each cycle. Different algorithm or
-- resolution runs coexist — resolution is data, not a constant.
--
-- The members table carries no foreign key (consistent with the rest of the
-- schema): `replace_communities` deletes members before their communities
-- within one transaction, so referential integrity is maintained by the
-- writer, not the schema.
--
-- Additive and forward-only (Principle XIII): no existing table or column is
-- altered or dropped.

CREATE TABLE IF NOT EXISTS notion_communities (
    id TEXT NOT NULL,
    algorithm TEXT NOT NULL,
    resolution REAL NOT NULL,
    computed_at TEXT NOT NULL,
    label TEXT NOT NULL,
    size INTEGER NOT NULL,
    PRIMARY KEY (id, algorithm, resolution)
);

CREATE TABLE IF NOT EXISTS notion_community_members (
    community_id TEXT NOT NULL,
    entity_name TEXT NOT NULL,
    weight REAL NOT NULL,
    PRIMARY KEY (community_id, entity_name)
);

-- Replace deletes by (algorithm, resolution); the PK leads with id, so this
-- index serves the delete's WHERE clause.
CREATE INDEX IF NOT EXISTS idx_notion_communities_algorithm_resolution
    ON notion_communities(algorithm, resolution);

-- Reverse lookup: which communities contain a given entity (cross-linking in
-- the summarization surface).
CREATE INDEX IF NOT EXISTS idx_notion_community_members_entity
    ON notion_community_members(entity_name);