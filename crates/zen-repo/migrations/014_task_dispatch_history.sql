-- W9.1 dispatch history (006-agentic-orchestration): per-task duration/model/cost
-- history for the plan.execute dispatch forecast.
--
-- PURPOSE: give the W9.1 forecast line a durable per-task history source so it can
-- emit `plan N tasks · ~a-b min · ~$lo-$hi (estimate)` only for agent/model pairs
-- that actually have recorded durations.
-- USAGE: applied automatically by sqlx::migrate!() on every SqliteClient::open().
-- EXPECTED: three new nullable columns on workflow_tasks; existing readers are
--   unaffected (they select named columns; NULLs are simply absent history).
-- ERRORS: none — additive ALTER TABLE ADD COLUMN is forward-only safe
--   (Principle XIII #5); no backfill, no rewrite.
--
-- `duration_ms` — wall time of the sub-turn, read from the delegate outcome
--   value (`duration_ms`, delegate_task.rs) at checkpoint time.
-- `model` — resolved model label for the (agent, model) pair key; NULL until a
--   per-task model producer exists (plan task specs carry no model hint today).
-- `cost_usd` — per-task cost; NULL until the delegate outcome carries cost
--   (reader supports the `$lo-$hi` segment; producer deferred — never a
--   fabricated number).
ALTER TABLE workflow_tasks ADD COLUMN model TEXT;
ALTER TABLE workflow_tasks ADD COLUMN duration_ms INTEGER;
ALTER TABLE workflow_tasks ADD COLUMN cost_usd REAL;
