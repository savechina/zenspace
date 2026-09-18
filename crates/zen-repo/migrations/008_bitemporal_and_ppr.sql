-- Bi-temporal relationship validity (Graphiti pattern) + Personalized PageRank
-- (HippoRAG pattern, pure computation over existing tables — no schema change).
--
-- `relationships` gains a system-side validity window [t_valid, t_invalid):
--   t_valid   TEXT NOT NULL DEFAULT ''  -- RFC3339 UTC; '' = valid since epoch
--   t_invalid TEXT                      -- NULL = open-ended (still valid)
--
-- Contradictions never DELETE: soft invalidation stamps t_invalid, history
-- stays queryable point-in-time (NotionsRepo::relationships_as_of).
--
-- Legacy rows are backfilled to t_valid = created_at (best-known validity
-- start). Constant default + backfill instead of DEFAULT (datetime('now')):
-- a non-constant ADD COLUMN default is re-evaluated on reads of pre-existing
-- rows, and datetime('now')'s 'YYYY-MM-DD HH:MM:SS' format does not sort
-- lexicographically against the RFC3339 timestamps used everywhere else.
--
-- Forward-only additive (Principle XIII): ADD COLUMN + backfill UPDATE only.
ALTER TABLE relationships ADD COLUMN t_valid TEXT NOT NULL DEFAULT '';
ALTER TABLE relationships ADD COLUMN t_invalid TEXT;

UPDATE relationships SET t_valid = created_at WHERE t_valid = '';
