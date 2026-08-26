-- Memvid replay checkpoint offsets (004-agentic-gateway Phase 11, T049).
-- Stores the applied jsonl byte offset per session file so the
-- SessionReplayer can resume incremental jsonl-to-mv2 replay after
-- restarts instead of full-replaying every session (Phase 11 locked
-- design D1=A: incremental replay + state.db checkpoint; baseline
-- invariant: mv2 never sole-copy of user data).
CREATE TABLE IF NOT EXISTS memvid_replay_offsets (
    session_path   TEXT PRIMARY KEY,
    applied_offset INTEGER NOT NULL
);
