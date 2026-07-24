-- Vector embeddings for notes (requires sqlite-vec extension)
-- Uses FLOAT[4096] as maximum dimension for maximum compatibility:
--   qwen3-embedding (4096-dim): native
--   nomic-embed-text (384-dim): zero-padded at application layer
--   fastembed        (384-dim): zero-padded at application layer
--   hash fallback    (4096-dim): native
-- See pad_to_dim() in zen-vault/src/tindy/embeddings.rs
CREATE VIRTUAL TABLE IF NOT EXISTS note_embeddings USING vec0(
    note_id TEXT PRIMARY KEY,
    embedding FLOAT[4096]
);

-- Vector embeddings for notions (requires sqlite-vec extension)
CREATE VIRTUAL TABLE IF NOT EXISTS notion_embeddings USING vec0(
    notion_id TEXT PRIMARY KEY,
    embedding FLOAT[4096]
);
