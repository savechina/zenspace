-- Plan-DAG workflow persistence (001 A.2/A.6 convergence, T376).
-- One row per plan.execute invocation; one checkpoint row per DAG task.
-- Resume: a `running` plan's task checkpoints are replayed (ok tasks skip
-- re-run, idempotency by (plan_id, task_id)); pending/failed tasks re-run.
CREATE TABLE IF NOT EXISTS workflow_plans (
    plan_id        TEXT PRIMARY KEY,
    name           TEXT,
    status         TEXT NOT NULL DEFAULT 'running', -- running|completed|failed
    spec_json      TEXT NOT NULL,
    summary        TEXT,
    plan_approved  INTEGER,
    delivery_ready INTEGER,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS workflow_tasks (
    plan_id    TEXT NOT NULL,
    task_id    TEXT NOT NULL,
    agent      TEXT NOT NULL,
    status     TEXT NOT NULL, -- pending|ok|failed|skipped
    response   TEXT,
    error      TEXT,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (plan_id, task_id)
);
