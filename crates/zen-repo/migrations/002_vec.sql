-- Vector embeddings for notes (requires sqlite-vec extension)
CREATE VIRTUAL TABLE IF NOT EXISTS note_embeddings USING vec0(
    note_id TEXT PRIMARY KEY,
    embedding FLOAT[384]
);

-- Vector embeddings for notions (requires sqlite-vec extension)
CREATE VIRTUAL TABLE IF NOT EXISTS notion_embeddings USING vec0(
    notion_id TEXT PRIMARY KEY,
    embedding FLOAT[384]
);
