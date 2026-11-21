-- 007-agentic-admission Phase 2 (T002, research D6/D8, FR-011/FR-013): three
-- additive trust-ledger tables behind the host-enforced admission gate.
--
-- PURPOSE: every inbound channel trigger produces a durable decision record
-- (would_accept|rejected|admitted|unresolved|duplicate); an accepted decision
-- grants an immutable mandate; an admitted mandate opens a ticket that carries
-- the reply address and the hosted-turn binding for the authorized work.
-- USAGE: applied automatically by sqlx::migrate!() on every SqliteClient::open().
-- EXPECTED: three empty tables + dedup/filter indexes; existing tables untouched.
-- ERRORS: none — additive CREATE TABLE/INDEX only (Principle XIII #2/#3:
--   forward-only, no ALTER of existing tables, no backfill).
CREATE TABLE IF NOT EXISTS admission_decisions (
    id             TEXT PRIMARY KEY,
    source         TEXT NOT NULL,
    event_id       TEXT,
    content_hash   TEXT NOT NULL,
    sender         TEXT NOT NULL,
    thread_key     TEXT NOT NULL,
    content        TEXT NOT NULL,
    -- disposition vocabulary (record of record, research D6):
    -- would_accept|rejected|admitted|unresolved|duplicate. A 'duplicate' row
    -- is written for a replayed trigger and never re-admitted (FR-011, D8).
    disposition    TEXT NOT NULL,
    matched_rule   TEXT,
    scope          TEXT,
    accept_mode    TEXT NOT NULL,
    strength       TEXT,
    policy_version TEXT NOT NULL,
    reason         TEXT,
    created_at     INTEGER NOT NULL
);

-- Stable-id dedup (FR-011): a replayed channel event collides here. SQLite
-- treats NULLs as distinct, so sources without event ids fall through to the
-- content-hash index below.
CREATE UNIQUE INDEX IF NOT EXISTS idx_admission_decisions_source_event
    ON admission_decisions(source, event_id);

-- Content-hash fallback (research D8): only rows whose event_id is NULL
-- participate, so a stable-id source never loses an otherwise-distinct
-- message that happens to share a body with an earlier event.
CREATE UNIQUE INDEX IF NOT EXISTS idx_admission_decisions_source_content
    ON admission_decisions(source, content_hash) WHERE event_id IS NULL;

-- Ledger read path: `zen admission list` filters disposition + since.
CREATE INDEX IF NOT EXISTS idx_admission_decisions_disposition_created
    ON admission_decisions(disposition, created_at DESC);

-- Continuation lookup: a live ticket on the trigger's thread (FR-003).
CREATE INDEX IF NOT EXISTS idx_admission_decisions_thread
    ON admission_decisions(thread_key, created_at DESC);

CREATE TABLE IF NOT EXISTS admission_mandates (
    mandate_id      TEXT PRIMARY KEY,
    decision_id     TEXT NOT NULL REFERENCES admission_decisions(id),
    matched_rule    TEXT,
    scope           TEXT NOT NULL,
    strength        TEXT NOT NULL,
    accept_mode     TEXT NOT NULL,
    policy_version  TEXT NOT NULL,
    granted_at      INTEGER NOT NULL,
    withdrawn_at    INTEGER,
    withdrawn_reason TEXT
);

-- FR-013/FR-014 (research D6): a mandate is immutable once granted. The only
-- write path beside the initial INSERT is withdraw_mandate, which UPDATEs
-- EXCLUSIVELY withdrawn_at/withdrawn_reason (WHERE withdrawn_at IS NULL);
-- scope is never widened in place — widening mints a new mandate row.
-- The repo exposes no other UPDATE/DELETE path over this table.

CREATE TABLE IF NOT EXISTS admission_tickets (
    ticket_id     TEXT PRIMARY KEY,
    mandate_id    TEXT NOT NULL REFERENCES admission_mandates(mandate_id),
    -- status vocabulary: open|ended — the ticket lifecycle is independent of
    -- the executor's turn state (data-model Ticket).
    status        TEXT NOT NULL,
    -- report_state vocabulary (set on close): ready|failed|dropped|stopped.
    report_state  TEXT,
    reply_address TEXT NOT NULL,
    turn_ref      TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_admission_tickets_mandate
    ON admission_tickets(mandate_id, status);
