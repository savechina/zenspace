-- Review fence (2026-09-09, /review issue #6): exactly one resume may run
-- a `running` plan. `owner` is an opaque claim token set by the conditional
-- UPDATE in WorkflowRepo::claim_plan (test-and-set against the single
-- writer); complete_plan clears it at close-out. Nullable, no default —
-- migration 006 rows stay valid. Forward-only additive (Principle XIII).
ALTER TABLE workflow_plans ADD COLUMN owner TEXT;
