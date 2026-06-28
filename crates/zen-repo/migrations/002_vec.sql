-- Vector embeddings for notes (requires sqlite-vec extension)
CREATE VIRTUAL TABLE IF NOT EXISTS note_embeddings USING vec0(
    note_id TEXT PRIMARY KEY,
    embedding FLOAT[384]
);

-- Vector embeddings for entities (requires sqlite-vec extension)
CREATE VIRTUAL TABLE IF NOT EXISTS entity_embeddings USING vec0(
    entity_id TEXT PRIMARY KEY,
    embedding FLOAT[384]
);
