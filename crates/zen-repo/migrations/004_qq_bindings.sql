-- QQBot IM carrier chat bindings (004-agentic-gateway Phase 13, T061).
-- Maps a QQ chat identity (group_openid or user_openid) to the hosted
-- gateway sessionId serving it, so conversations survive daemon restarts
-- (contracts/05-carrier-qqbot.md binding-table activation step 3).
CREATE TABLE IF NOT EXISTS qq_bindings (
    chat_id    TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
