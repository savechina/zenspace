use crate::client::{Result, SqliteClient, SqliteError};

/// One task checkpoint upsert — the T376 persistence unit.
#[derive(Debug, Clone)]
pub struct TaskCheckpoint<'a> {
    pub plan_id: &'a str,
    pub task_id: &'a str,
    pub agent: &'a str,
    pub status: &'a str,
    pub response: Option<&'a str>,
    pub error: Option<&'a str>,
    pub now: i64,
    /// W9.1 dispatch-history fields (migration 014). All optional: existing
    /// callers omit them and rows keep NULL history.
    pub model: Option<&'a str>,
    pub duration_ms: Option<u64>,
    pub cost_usd: Option<f64>,
}

/// A plan claim older than this (no checkpoint progress) may be stolen by
/// a fresh resume — a crashed run must not wedge the plan forever.
///
/// W6.2b (D24 §3): this is the DOCUMENTED FALLBACK value of the unified
/// staleness definition — 4× the default turn watchdog (900s ⇒ 3600s),
/// aligned with the watchdog-renewal ceiling (`PLAN_WATCHDOG_CEILING` =
/// 4× watchdog, zen-gateway guards.rs; W12.2/eng-review N1 — a smaller
/// window stale-sweeps live plans mid-task and invites a double-spend).
/// Callers resolve the live value (4× `[gateway].turn_watchdog_secs`) and
/// pass it explicitly; the sweep and the resume fence share one definition.
pub const DEFAULT_STALE_CLAIM_SECS: i64 = 3600;

/// W11.13/T14 (F8, D22 code-constant policy): the forecast reads at most
/// this many recent completed-task duration rows. A code constant, not a
/// config key — a machine needing a different window edits the constant.
pub const FORECAST_HISTORY_WINDOW: i64 = 500;

/// W14.4b (D22 in-code policy): the induction corpus reads at most this
/// many newest plan rows. A code constant, not a config key — truncation
/// is deterministic newest-first so a large ledger cannot unboundedly
/// inflate the corpus join.
pub const INDUCTION_CORPUS_LIMIT: i64 = 1_000;

/// One draft task row for [`WorkflowRepo::mint_plan_with_tasks`]. The item
/// *title* never lands here — it lives in the plan row's `spec_json`
/// (D21 §2 write-path rule (a)); the row carries only the id and the
/// initial status (`pending|progressing|completed|failed|skipped`).
#[derive(Debug, Clone)]
pub struct TaskDraft {
    pub id: String,
    pub status: String,
}

/// Plan-DAG persistence (T376): `workflow_plans` + `workflow_tasks`
/// checkpoints in state.db. Resume is idempotent on (plan_id, task_id);
/// the `owner` claim token fences concurrent resumes (/review #6).
pub struct WorkflowRepo<'a> {
    client: &'a SqliteClient,
}

impl<'a> WorkflowRepo<'a> {
    pub fn new(client: &'a SqliteClient) -> Self {
        Self { client }
    }

    pub async fn create_plan(
        &self,
        plan_id: &str,
        name: Option<&str>,
        spec_json: &str,
        now: i64,
    ) -> Result<()> {
        let plan_id = plan_id.to_string();
        let name = name.map(|s| s.to_string());
        let spec_json = spec_json.to_string();
        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO workflow_plans (plan_id, name, status, spec_json, created_at, updated_at) \
                     VALUES (?1, ?2, 'running', ?3, ?4, ?4)",
                    rusqlite::params![plan_id, name, spec_json, now],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    /// W14.1 (eng-review T5): merge a dispatched batch's DAG into the plan
    /// row's spec_json — the corpus-observability seam. The rewrite is
    /// additive-by-key: callers preserve `items`/`session_id`/`sensitivity`
    /// (the mint-once LIKE guard and the W12.15 resume policy read them);
    /// only the `tasks` array is replaced (the row observes the
    /// last-dispatched shape). Heartbeat semantics: bumps `updated_at` in
    /// the same statement (W12.2 discipline). Guarded `status = 'running'`
    /// so a terminal row is never mutated.
    pub async fn attach_batch_spec(&self, plan_id: &str, spec_json: &str, now: i64) -> Result<()> {
        let plan_id = plan_id.to_string();
        let spec_json = spec_json.to_string();
        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "UPDATE workflow_plans SET spec_json = ?1, updated_at = ?2 \
                     WHERE plan_id = ?3 AND status = 'running'",
                    rusqlite::params![spec_json, now, plan_id],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn checkpoint_task(&self, cp: TaskCheckpoint<'_>) -> Result<()> {
        let TaskCheckpoint {
            plan_id,
            task_id,
            agent,
            status,
            response,
            error,
            now,
            model,
            duration_ms,
            cost_usd,
        } = cp;
        let plan_id = plan_id.to_string();
        let task_id = task_id.to_string();
        let agent = agent.to_string();
        let status = status.to_string();
        let response = response.map(|s| s.to_string());
        let error = error.map(|s| s.to_string());
        let model = model.map(|s| s.to_string());
        let duration_ms = duration_ms.map(|v| v as i64);
        self.client
            .writer()
            .call(move |conn| {
                let tx = conn.transaction()?;
                tx.execute(
                    "INSERT OR REPLACE INTO workflow_tasks \
                     (plan_id, task_id, agent, status, response, error, updated_at, \
                      model, duration_ms, cost_usd) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    rusqlite::params![
                        plan_id,
                        task_id,
                        agent,
                        status,
                        response,
                        error,
                        now,
                        model,
                        duration_ms,
                        cost_usd
                    ],
                )?;
                // W12.2 (eng-review N1): progress heartbeat — bump the plan
                // row's `updated_at` in the SAME transaction as the task
                // write. A live plan checkpoints per task, so a fresh
                // `updated_at` proves liveness to the stale-claim sweep and
                // the resume fence. Without this bump the plan row froze at
                // mint time and a sanctioned long task (the watchdog-renewal
                // ceiling, `PLAN_WATCHDOG_CEILING` = 4× watchdog in
                // zen-gateway guards.rs) got stale-swept (owner NULLed) and
                // its claim stolen by a second resume — a true double-spend.
                // F8 (W15): guarded `status = 'running'` like
                // `attach_batch_spec` — a late checkpoint must never move a
                // terminal (cancelled/completed/failed) row's heartbeat.
                tx.execute(
                    "UPDATE workflow_plans SET updated_at = ?1 \
                     WHERE plan_id = ?2 AND status = 'running'",
                    rusqlite::params![now, plan_id],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    /// Mint one plan row WITH its draft task rows in a single writer
    /// transaction (W6.2 / D21 §2, F4): the plan row is created with
    /// `owner` already SET (claim-at-create, D24 §3 — the stale sweep must
    /// be able to decide live-vs-ownerless from the first moment) and the
    /// draft rows carry the `''` agent sentinel (agent is written at
    /// dispatch time only — migration 006 `agent TEXT NOT NULL`).
    ///
    /// # Errors
    /// Duplicate `plan_id` (PK) or any SQLite failure surfaces as
    /// [`SqliteError`].
    pub async fn mint_plan_with_tasks(
        &self,
        plan_id: &str,
        name: Option<&str>,
        spec_json: &str,
        owner: &str,
        items: &[TaskDraft],
        now: i64,
    ) -> Result<()> {
        let plan_id = plan_id.to_string();
        let name = name.map(|s| s.to_string());
        let spec_json = spec_json.to_string();
        let owner = owner.to_string();
        let drafts: Vec<(String, String)> = items
            .iter()
            .map(|d| (d.id.clone(), d.status.clone()))
            .collect();
        self.client
            .writer()
            .call(move |conn| {
                let tx = conn.transaction()?;
                tx.execute(
                    "INSERT INTO workflow_plans (plan_id, name, status, spec_json, owner, created_at, updated_at) \
                     VALUES (?1, ?2, 'running', ?3, ?4, ?5, ?5)",
                    rusqlite::params![plan_id, name, spec_json, owner, now],
                )?;
                for (task_id, status) in &drafts {
                    tx.execute(
                        "INSERT INTO workflow_tasks (plan_id, task_id, agent, status, updated_at) \
                         VALUES (?1, ?2, '', ?3, ?4)",
                        rusqlite::params![plan_id, task_id, status, now],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    /// Mark-only stale sweep (W6.2b / D24 §3): every `running` plan whose
    /// `updated_at` is older than `now - stale_secs` flips to terminal
    /// `cancelled` with `summary = 'stale'` (the chosen reason record —
    /// one field, documented). Returns the cancelled plan ids for audit.
    /// A live plan (fresh `updated_at`, claimed owner) is never touched.
    pub async fn mark_stale_plans(&self, now: i64, stale_secs: i64) -> Result<Vec<String>> {
        let cutoff = now - stale_secs;
        self.client
            .writer()
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "UPDATE workflow_plans SET status = 'cancelled', summary = 'stale', \
                      owner = NULL, updated_at = ?2 \
                      WHERE status = 'running' AND updated_at < ?1 RETURNING plan_id",
                )?;
                let rows =
                    stmt.query_map(rusqlite::params![cutoff, now], |r| r.get::<_, String>(0))?;
                Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
            })
            .await
            .map_err(SqliteError::TokioRusqlite)
    }

    /// Cancel-path finalizer, plan row (W6.2b / D24 §2): flip a still-
    /// `running` plan to terminal `cancelled` (reason `cancelled` in the
    /// summary, owner released). Returns `false` when the plan was already
    /// terminal (idempotent — the engine outcome wins over the finalizer).
    pub async fn cancel_running_plan(&self, plan_id: &str, now: i64) -> Result<bool> {
        let plan_id = plan_id.to_string();
        let changed = self
            .client
            .writer()
            .call(move |conn| {
                Ok(conn.execute(
                    "UPDATE workflow_plans SET status = 'cancelled', summary = 'cancelled', \
                     owner = NULL, updated_at = ?2 WHERE plan_id = ?1 AND status = 'running'",
                    rusqlite::params![plan_id, now],
                )?)
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(changed == 1)
    }

    /// Cancel-path finalizer, in-flight rows (D24 §2): the ids that were
    /// dispatched in the live batch become `failed` with `error = reason`
    /// — only non-terminal rows (`pending`/`progressing`) are touched, so
    /// an id that settled between abort and finalize keeps its outcome.
    /// Returns rows affected.
    pub async fn fail_tasks_with_reason(
        &self,
        plan_id: &str,
        task_ids: &[String],
        reason: &str,
        now: i64,
    ) -> Result<u64> {
        let plan_id = plan_id.to_string();
        let ids = task_ids.to_vec();
        let reason = reason.to_string();
        let changed = self
            .client
            .writer()
            .call(move |conn| {
                let mut total = 0usize;
                for id in &ids {
                    total += conn.execute(
                        "UPDATE workflow_tasks SET status = 'failed', error = ?1, updated_at = ?2 \
                         WHERE plan_id = ?3 AND task_id = ?4 AND status IN ('pending', 'progressing')",
                        rusqlite::params![reason, now, plan_id, id],
                    )?;
                }
                Ok(total)
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(changed as u64)
    }

    /// Cancel-path finalizer, never-dispatched rows (D24 §2): every
    /// remaining non-terminal (`pending`/`progressing`) row of the plan
    /// becomes `skipped`. Rows just failed by
    /// [`WorkflowRepo::fail_tasks_with_reason`] are terminal and excluded
    /// by the status filter. Returns rows affected.
    pub async fn skip_remaining_tasks(&self, plan_id: &str, now: i64) -> Result<u64> {
        let plan_id = plan_id.to_string();
        let changed = self
            .client
            .writer()
            .call(move |conn| {
                Ok(conn.execute(
                    "UPDATE workflow_tasks SET status = 'skipped', updated_at = ?2 \
                     WHERE plan_id = ?1 AND status IN ('pending', 'progressing')",
                    rusqlite::params![plan_id, now],
                )?)
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(changed as u64)
    }

    /// W9.3 append: INSERT-only draft rows for `plan.control`'s append op —
    /// the durable ledger record of tasks added to a LIVE plan between
    /// batches. Rows mirror [`WorkflowRepo::mint_plan_with_tasks`]'s draft
    /// arm: `''` agent sentinel (agent is written at dispatch time) and the
    /// caller-supplied status (`pending` from the control tool). The task
    /// specs themselves (agent/prompt/deps) are merged into the plan's
    /// `spec_json` by the caller via [`WorkflowRepo::attach_batch_spec`]
    /// (F3); the row is the persistence receipt. A duplicate
    /// (plan_id, task_id) violates the PK and surfaces as [`SqliteError`].
    /// Returns rows inserted.
    pub async fn append_tasks(&self, plan_id: &str, tasks: &[TaskDraft], now: i64) -> Result<u64> {
        let plan_id = plan_id.to_string();
        let drafts: Vec<(String, String)> = tasks
            .iter()
            .map(|d| (d.id.clone(), d.status.clone()))
            .collect();
        self.client
            .writer()
            .call(move |conn| {
                let tx = conn.transaction()?;
                let mut inserted = 0usize;
                for (task_id, status) in &drafts {
                    tx.execute(
                        "INSERT INTO workflow_tasks (plan_id, task_id, agent, status, updated_at) \
                         VALUES (?1, ?2, '', ?3, ?4)",
                        rusqlite::params![plan_id, task_id, status, now],
                    )?;
                    inserted += 1;
                }
                tx.commit()?;
                Ok(inserted as u64)
            })
            .await
            .map_err(SqliteError::TokioRusqlite)
    }

    /// 90-day retention, task side (W6.2b / D24 §4): delete rows older
    /// than `cutoff` by `updated_at`. Invoked by the GC tick BEFORE
    /// [`WorkflowRepo::delete_expired_plans`] (F5 delete ordering — no FK
    /// between the tables, tasks-then-plans avoids orphaning rows of a
    /// just-deleted plan). Returns rows deleted.
    pub async fn delete_expired_tasks(&self, cutoff: i64, _now: i64) -> Result<u64> {
        let changed = self
            .client
            .writer()
            .call(move |conn| {
                Ok(conn.execute(
                    "DELETE FROM workflow_tasks WHERE updated_at < ?1",
                    rusqlite::params![cutoff],
                )?)
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(changed as u64)
    }

    /// 90-day retention, plan side (W6.2b / D24 §4): delete rows older
    /// than `cutoff` by `updated_at`; `running` rows are exempt — a live
    /// plan is never deleted. Call after
    /// [`WorkflowRepo::delete_expired_tasks`] (F5). Returns rows deleted.
    pub async fn delete_expired_plans(&self, cutoff: i64, _now: i64) -> Result<u64> {
        let changed = self
            .client
            .writer()
            .call(move |conn| {
                Ok(conn.execute(
                    "DELETE FROM workflow_plans WHERE updated_at < ?1 AND status != 'running'",
                    rusqlite::params![cutoff],
                )?)
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(changed as u64)
    }

    /// Resolve "the current plan" for a session (D21 §2 / D24 §1/§5): the
    /// most recent plan row whose `spec_json` carries
    /// `"session_id":"<id>"`. Matched via SQL `LIKE` on the marker with
    /// `LIKE`-wildcard escaping (D31/F2: a `%`/`_` in a legacy id must not
    /// match another session's plan). Tiebreak is `rowid DESC` — insertion
    /// order, deterministic for same-second mints (T6/F6: `created_at` has
    /// second granularity and plan_id is a random UUID).
    pub async fn latest_plan_for_session(
        &self,
        session_id: &str,
    ) -> Result<Option<WorkflowPlanRow>> {
        let escaped = session_id
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%\"session_id\":\"{}\"%", escaped);
        Ok(sqlx::query_as::<_, WorkflowPlanRow>(
            "SELECT plan_id, name, status, spec_json, summary, plan_approved, delivery_ready, owner, created_at, updated_at \
             FROM workflow_plans WHERE spec_json LIKE ?1 ESCAPE '\\' ORDER BY created_at DESC, rowid DESC LIMIT 1",
        )
        .bind(pattern)
        .fetch_optional(self.client.pool())
        .await?)
    }

    /// Atomic resume fence (/review #6): test-and-set an owner token on a
    /// `running` plan. The conditional UPDATE serializes through the single
    /// writer, so two concurrent resumes can never both observe
    /// `owner IS NULL`. Returns `Ok(true)` when this caller holds the
    /// claim, `Ok(false)` when another run holds it or the plan went
    /// terminal. A claim with no progress for `stale_secs` may be stolen
    /// so a crashed resume does not wedge the plan forever (W6.2b: the
    /// caller passes the unified staleness value — 4× the turn watchdog,
    /// the `PLAN_WATCHDOG_CEILING` renewal ceiling (W12.2/N1);
    /// [`DEFAULT_STALE_CLAIM_SECS`] is the documented fallback).
    pub async fn claim_plan(
        &self,
        plan_id: &str,
        owner: &str,
        now: i64,
        stale_secs: i64,
    ) -> Result<bool> {
        let plan_id = plan_id.to_string();
        let owner = owner.to_string();
        self.client
            .writer()
            .call(move |conn| {
                let changed = conn.execute(
                    "UPDATE workflow_plans SET owner = ?1, updated_at = ?2 \
                     WHERE plan_id = ?3 AND status = 'running' \
                       AND (owner IS NULL OR owner = ?1 OR updated_at < ?4)",
                    rusqlite::params![owner, now, plan_id, now - stale_secs],
                )?;
                Ok(changed == 1)
            })
            .await
            .map_err(SqliteError::TokioRusqlite)
    }

    /// Engine-outcome terminal write (T376): flip the plan row to
    /// `completed`/`failed` with the gate verdict and merged summary.
    ///
    /// W12.16 (review Q1): the write is GUARDED to flip only still-
    /// `running` rows — a `cancelled` row written by `plan.control`, the
    /// drain finalizer, or the mark-only stale sweep during the
    /// quality-gate tail window is never silently reverted to a terminal
    /// engine outcome (cancel irreversibility, W9.3 semantics).
    ///
    /// Returns `Ok(true)` when THIS call flipped the row, `Ok(false)` when
    /// the row was already terminal or does not exist — the caller treats a
    /// non-flip as having lost the terminal race (engine outcome yields to
    /// the recorded cancel). Pre-existing callers (`.unwrap()` sites in the
    /// morning-brief worker and the migration 006 pin) compile unchanged.
    pub async fn complete_plan(
        &self,
        plan_id: &str,
        status: &str,
        summary: Option<&str>,
        plan_approved: Option<bool>,
        delivery_ready: Option<bool>,
        now: i64,
    ) -> Result<bool> {
        let plan_id = plan_id.to_string();
        let status = status.to_string();
        let summary = summary.map(|s| s.to_string());
        let plan_approved = plan_approved.map(|b| b as i64);
        let delivery_ready = delivery_ready.map(|b| b as i64);
        let changed = self
            .client
            .writer()
            .call(move |conn| {
                Ok(conn.execute(
                    "UPDATE workflow_plans SET status = ?1, summary = ?2, plan_approved = ?3, \
                     delivery_ready = ?4, updated_at = ?5, owner = NULL \
                     WHERE plan_id = ?6 AND status = 'running'",
                    rusqlite::params![status, summary, plan_approved, delivery_ready, now, plan_id],
                )?)
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(changed == 1)
    }

    pub async fn load_plan(&self, plan_id: &str) -> Result<Option<WorkflowPlanRow>> {
        Ok(sqlx::query_as::<_, WorkflowPlanRow>(
            "SELECT plan_id, name, status, spec_json, summary, plan_approved, delivery_ready, owner, created_at, updated_at \
             FROM workflow_plans WHERE plan_id = ?1",
        )
        .bind(plan_id)
        .fetch_optional(self.client.pool())
        .await?)
    }

    pub async fn load_task_rows(&self, plan_id: &str) -> Result<Vec<WorkflowTaskRow>> {
        Ok(sqlx::query_as::<_, WorkflowTaskRow>(
            "SELECT plan_id, task_id, agent, status, response, error, updated_at \
             FROM workflow_tasks WHERE plan_id = ?1 ORDER BY task_id",
        )
        .bind(plan_id)
        .fetch_all(self.client.pool())
        .await?)
    }

    /// W9.1 forecast history: completed tasks with a recorded sub-turn
    /// duration (migration 014). Pairs without history never appear here,
    /// which is what makes the forecast omit rather than guess.
    ///
    /// W11.13/T14 (F8): bounded to the [`FORECAST_HISTORY_WINDOW`] most
    /// recently updated rows — recency improves the estimate and the query
    /// no longer grows unboundedly with ledger age (dispatch latency).
    pub async fn duration_history(&self) -> Result<Vec<DurationHistoryRow>> {
        Ok(sqlx::query_as::<_, DurationHistoryRow>(
            "SELECT agent, model, duration_ms, cost_usd \
             FROM workflow_tasks WHERE status = 'completed' AND duration_ms IS NOT NULL \
             ORDER BY updated_at DESC LIMIT ?1",
        )
        .bind(FORECAST_HISTORY_WINDOW)
        .fetch_all(self.client.pool())
        .await?)
    }

    /// W9.2 morning-brief window: plan rows touched at or after `cutoff`
    /// (epoch seconds), newest first.
    pub async fn recent_plans(&self, cutoff: i64) -> Result<Vec<WorkflowPlanRow>> {
        Ok(sqlx::query_as::<_, WorkflowPlanRow>(
            "SELECT plan_id, name, status, spec_json, summary, plan_approved, delivery_ready, owner, created_at, updated_at \
             FROM workflow_plans WHERE updated_at >= ?1 ORDER BY updated_at DESC",
        )
        .bind(cutoff)
        .fetch_all(self.client.pool())
        .await?)
    }

    /// W13.2 workflow-induction corpus: every plan row CREATED at or after
    /// `cutoff` (epoch seconds) with its task outcome rows, newest first.
    ///
    /// One LEFT JOIN fetched in a single query and grouped in Rust: the
    /// induction reader needs per-task status + `duration_ms`/`cost_usd`
    /// (migration 014), which [`WorkflowTaskRow`] does not carry, and an
    /// N+1 per-plan fetch would walk the ledger once per row. A plan with
    /// no task rows yields an empty `tasks` vector — callers treat that as
    /// not fully completed. Read-only; no side effects.
    ///
    /// `created_at` (not `updated_at`) anchors the window: induction
    /// observes plan SHAPES as minted, and a resumed plan's `updated_at`
    /// drifts forward with every checkpoint.
    pub async fn induction_corpus(&self, cutoff: i64) -> Result<Vec<InductionPlanRow>> {
        let flat = sqlx::query_as::<_, InductionCorpusFlatRow>(
            "SELECT p.plan_id, p.name, p.status, p.spec_json, p.created_at, \
                    t.task_id, t.agent, t.status AS task_status, t.duration_ms, t.cost_usd \
             FROM workflow_plans p \
             LEFT JOIN workflow_tasks t ON t.plan_id = p.plan_id \
             WHERE p.plan_id IN ( \
                 SELECT plan_id FROM workflow_plans WHERE created_at >= ?1 \
                 ORDER BY created_at DESC, plan_id DESC LIMIT ?2 \
             ) \
             ORDER BY p.created_at DESC, p.plan_id DESC, t.task_id ASC",
        )
        .bind(cutoff)
        .bind(INDUCTION_CORPUS_LIMIT)
        .fetch_all(self.client.pool())
        .await?;

        let mut plans: Vec<InductionPlanRow> = Vec::new();
        for row in flat {
            let continue_plan = plans.last().is_some_and(|p| p.plan_id == row.plan_id);
            if !continue_plan {
                plans.push(InductionPlanRow {
                    plan_id: row.plan_id.clone(),
                    name: row.name.clone(),
                    status: row.status.clone(),
                    spec_json: row.spec_json.clone(),
                    created_at: row.created_at,
                    tasks: Vec::new(),
                });
            }
            if let (Some(task_id), Some(task_status), Some(agent)) =
                (row.task_id, row.task_status, row.agent)
                && let Some(plan) = plans.last_mut()
            {
                plan.tasks.push(InductionTaskRow {
                    task_id,
                    agent,
                    status: task_status,
                    duration_ms: row.duration_ms,
                    cost_usd: row.cost_usd,
                });
            }
        }
        Ok(plans)
    }
}

/// One plan in the workflow-induction corpus, with its task outcome rows.
#[derive(Debug, Clone)]
pub struct InductionPlanRow {
    pub plan_id: String,
    pub name: Option<String>,
    pub status: String,
    pub spec_json: String,
    pub created_at: i64,
    pub tasks: Vec<InductionTaskRow>,
}

/// One task outcome row of an [`InductionPlanRow`].
#[derive(Debug, Clone)]
pub struct InductionTaskRow {
    pub task_id: String,
    pub agent: String,
    pub status: String,
    pub duration_ms: Option<i64>,
    pub cost_usd: Option<f64>,
}

/// Flat JOIN row backing [`WorkflowRepo::induction_corpus`]; the `Option`
/// task columns are NULL for a plan that has no task rows yet.
#[derive(sqlx::FromRow)]
struct InductionCorpusFlatRow {
    plan_id: String,
    name: Option<String>,
    status: String,
    spec_json: String,
    created_at: i64,
    task_id: Option<String>,
    agent: Option<String>,
    task_status: Option<String>,
    duration_ms: Option<i64>,
    cost_usd: Option<f64>,
}

#[derive(sqlx::FromRow)]
pub struct WorkflowPlanRow {
    pub plan_id: String,
    pub name: Option<String>,
    pub status: String,
    pub spec_json: String,
    pub summary: Option<String>,
    pub plan_approved: Option<i64>,
    pub delivery_ready: Option<i64>,
    /// Claim token (migration 007): `Some` while a run holds the plan.
    pub owner: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(sqlx::FromRow)]
pub struct WorkflowTaskRow {
    pub plan_id: String,
    pub task_id: String,
    pub agent: String,
    pub status: String,
    pub response: Option<String>,
    pub error: Option<String>,
    pub updated_at: i64,
}

/// One completed task with recorded dispatch history (migration 014).
#[derive(sqlx::FromRow, Debug, Clone, PartialEq)]
pub struct DurationHistoryRow {
    pub agent: String,
    pub model: Option<String>,
    pub duration_ms: i64,
    pub cost_usd: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn create_plan_rejects_duplicate_plan_id() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_dup.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        repo.create_plan("p1", Some("test"), "{}", 1000)
            .await
            .unwrap();
        let dup = repo.create_plan("p1", Some("dup"), "{}", 1001).await;
        assert!(
            dup.is_err(),
            "duplicate plan_id must be rejected (PK violation)"
        );
    }

    #[tokio::test]
    async fn load_plan_unknown_returns_none() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_ghost.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        let result = repo.load_plan("ghost").await.unwrap();
        assert!(result.is_none(), "unknown plan_id should return None");
    }

    #[tokio::test]
    async fn complete_plan_on_unknown_plan_is_a_noop() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_noop.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        repo.complete_plan("ghost", "completed", None, None, None, 2000)
            .await
            .unwrap();
        assert!(
            repo.load_plan("ghost").await.unwrap().is_none(),
            "completing a nonexistent plan should not create a row"
        );
    }

    // W12.16a: `complete_plan` is guarded `AND status = 'running'` — a
    // late engine completion must not resurrect a plan the cancel path
    // already finalized (engine outcome yields to recorded cancel, W9.3).
    #[tokio::test]
    async fn complete_plan_after_cancel_keeps_cancelled_row() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_cancel_guard.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        repo.create_plan("p1", None, "{}", 1000).await.unwrap();
        assert!(
            repo.cancel_running_plan("p1", 1001).await.unwrap(),
            "running plan should cancel"
        );
        let flipped = repo
            .complete_plan("p1", "completed", Some("late engine"), None, None, 1002)
            .await
            .unwrap();
        assert!(
            !flipped,
            "complete_plan must not flip an already-cancelled row"
        );
        let row = repo.load_plan("p1").await.unwrap().expect("row exists");
        assert_eq!(row.status, "cancelled");
        assert_eq!(row.summary.as_deref(), Some("cancelled"));
    }

    #[tokio::test]
    async fn checkpoint_task_is_idempotent_per_plan_task() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_idem.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        repo.create_plan("p1", None, "{}", 1000).await.unwrap();
        repo.checkpoint_task(TaskCheckpoint {
            plan_id: "p1",
            task_id: "a",
            agent: "Explore",
            status: "completed",
            response: Some("first"),
            error: None,
            now: 1001,

            model: None,
            duration_ms: None,
            cost_usd: None,
        })
        .await
        .unwrap();
        repo.checkpoint_task(TaskCheckpoint {
            plan_id: "p1",
            task_id: "a",
            agent: "Explore",
            status: "completed",
            response: Some("second"),
            error: None,
            now: 1002,

            model: None,
            duration_ms: None,
            cost_usd: None,
        })
        .await
        .unwrap();

        let tasks = repo.load_task_rows("p1").await.unwrap();
        assert_eq!(
            tasks.len(),
            1,
            "same (plan, task) replaces, never duplicates"
        );
        assert_eq!(tasks[0].response.as_deref(), Some("second"));
    }

    /// F8 (W15): the checkpoint heartbeat is guarded `status = 'running'` —
    /// a checkpoint arriving after a cancel/complete must still write the
    /// task row but leave the terminal plan row's `updated_at` untouched.
    #[tokio::test]
    async fn checkpoint_does_not_bump_a_cancelled_plan_row() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_cp_guard.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        repo.create_plan("p1", None, "{}", 1000).await.unwrap();
        assert!(repo.cancel_running_plan("p1", 1500).await.unwrap());
        repo.checkpoint_task(TaskCheckpoint {
            plan_id: "p1",
            task_id: "late",
            agent: "Explore",
            status: "completed",
            response: Some("late answer"),
            error: None,
            now: 2000,
            model: None,
            duration_ms: None,
            cost_usd: None,
        })
        .await
        .unwrap();

        let row = repo.load_plan("p1").await.unwrap().expect("row");
        assert_eq!(row.status, "cancelled");
        assert_eq!(
            row.updated_at, 1500,
            "a terminal plan row must not be heartbeated by a late checkpoint"
        );
        let tasks = repo.load_task_rows("p1").await.unwrap();
        assert_eq!(tasks.len(), 1, "the checkpoint task row still lands");
        assert_eq!(tasks[0].response.as_deref(), Some("late answer"));
    }

    #[tokio::test]
    async fn claim_plan_fences_concurrent_resumes() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_claim.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        repo.create_plan("p2", None, "{}", 1000).await.unwrap();

        // First claim succeeds
        assert!(
            repo.claim_plan("p2", "run-a", 1000, DEFAULT_STALE_CLAIM_SECS)
                .await
                .unwrap()
        );
        // Different owner blocked
        assert!(
            !repo
                .claim_plan("p2", "run-b", 1000, DEFAULT_STALE_CLAIM_SECS)
                .await
                .unwrap()
        );
        // Same owner re-claims (claim is idempotent for the owner)
        assert!(
            repo.claim_plan("p2", "run-a", 1000, DEFAULT_STALE_CLAIM_SECS)
                .await
                .unwrap()
        );

        // Complete the plan (clears owner)
        repo.complete_plan("p2", "completed", None, None, None, 2000)
            .await
            .unwrap();
        // Terminal plans refuse claims
        assert!(
            !repo
                .claim_plan("p2", "run-c", 2000, DEFAULT_STALE_CLAIM_SECS)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn stale_claim_may_be_stolen() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_stale.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        repo.create_plan("p3", None, "{}", 1000).await.unwrap();

        // Dead runner claims at t=1000 with a 3600s staleness window.
        let stale_secs = 3600i64;
        assert!(
            repo.claim_plan("p3", "dead-run", 1000, stale_secs)
                .await
                .unwrap()
        );

        // Thief tries at t=1000+3599 — still fresh (1s inside the window)
        assert!(
            !repo
                .claim_plan("p3", "thief", 1000 + 3599, stale_secs)
                .await
                .unwrap(),
            "claim should still be fresh at now-1s"
        );

        // Fresh runner steals at t=1000+3601 — stale (1s beyond the window)
        assert!(
            repo.claim_plan("p3", "fresh-run", 1000 + 3601, stale_secs)
                .await
                .unwrap(),
            "stale claim should be stealable"
        );
    }

    /// W6.2b pin + W12.2 (N1): the documented fallback staleness value is
    /// 4× the default turn watchdog (900s ⇒ 3600s) — the unified definition
    /// the sweep and the resume fence share, aligned with the
    /// `PLAN_WATCHDOG_CEILING` renewal ceiling (4× watchdog).
    #[test]
    fn default_stale_claim_is_four_times_the_default_watchdog() {
        assert_eq!(
            DEFAULT_STALE_CLAIM_SECS,
            4 * 900,
            "unified staleness = 4× turn watchdog default (≥ renewal ceiling)"
        );
    }

    /// W6.2 pin: `mint_plan_with_tasks` creates the plan row with the
    /// owner CLAIMED at create time (claim-at-create, D24 §3) and the
    /// draft task rows carry the `''` agent sentinel (agent is known at
    /// dispatch time only, D21 §2 rule (a)).
    #[tokio::test]
    async fn mint_plan_with_tasks_claims_owner_and_seeds_draft_rows() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_mint.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        repo.mint_plan_with_tasks(
            "minted",
            None,
            r#"{"items":[],"sensitivity":"Private","session_id":"sess-m"}"#,
            "sess-m",
            &[
                TaskDraft {
                    id: "a".into(),
                    status: "pending".into(),
                },
                TaskDraft {
                    id: "b".into(),
                    status: "progressing".into(),
                },
            ],
            1000,
        )
        .await
        .unwrap();

        let plan = repo.load_plan("minted").await.unwrap().unwrap();
        assert_eq!(plan.status, "running", "minted plan starts running");
        assert_eq!(plan.owner.as_deref(), Some("sess-m"), "claim-at-create");

        // Another owner's claim is refused while the fresh claim holds.
        assert!(
            !repo
                .claim_plan("minted", "intruder", 1001, DEFAULT_STALE_CLAIM_SECS)
                .await
                .unwrap(),
            "fresh claim must not be stealable"
        );

        let rows = repo.load_task_rows("minted").await.unwrap();
        assert_eq!(rows.len(), 2, "one draft row per item");
        assert!(
            rows.iter().all(|r| r.agent.is_empty()),
            "draft rows carry the '' agent sentinel"
        );
        assert_eq!(rows[0].status, "pending");
        assert_eq!(rows[1].status, "progressing");
    }

    /// W6.2b pin: the mark-only sweep cancels ONLY `running` plans older
    /// than the staleness bound and records the reason as `summary='stale'`
    /// (the documented reason record); a fresh live plan and terminal plans
    /// are untouched, and the cancelled ids are returned for audit.
    #[tokio::test]
    async fn mark_stale_plans_touches_only_old_running_rows() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_stale_sweep.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        let now = 10_000i64;
        let stale_secs = 1800i64;
        repo.create_plan("old-running", None, "{}", now - 3600)
            .await
            .unwrap();
        repo.create_plan("live-plan", None, "{}", now - 10)
            .await
            .unwrap();
        repo.create_plan("done", None, "{}", now - 7200)
            .await
            .unwrap();
        repo.complete_plan("done", "completed", None, None, None, now - 3600)
            .await
            .unwrap();

        let cancelled = repo.mark_stale_plans(now, stale_secs).await.unwrap();
        assert_eq!(cancelled, vec!["old-running".to_string()], "audit ids");

        let swept = repo.load_plan("old-running").await.unwrap().unwrap();
        assert_eq!(swept.status, "cancelled");
        assert_eq!(swept.summary.as_deref(), Some("stale"), "reason record");
        assert!(swept.owner.is_none(), "sweep releases the claim");

        let live = repo.load_plan("live-plan").await.unwrap().unwrap();
        assert_eq!(live.status, "running", "live plan never touched");
        let done = repo.load_plan("done").await.unwrap().unwrap();
        assert_eq!(done.status, "completed", "terminal plans untouched");

        // Idempotent: a second sweep finds nothing.
        assert!(
            repo.mark_stale_plans(now, stale_secs)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// W6.2b pin: the retention deletes respect the 90-day bound with
    /// running rows exempt on the plan table, and the F5 ordering keeps
    /// tasks deletable even while their plan row survives.
    #[tokio::test]
    async fn retention_keeps_90day_deletes_91day_and_exempts_running() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_retention.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        let now = 100_000i64;
        let day: i64 = 24 * 3600;
        let cutoff = now - 90 * day;

        // Tasks: 89-day kept, 91-day deleted (tasks have no running state).
        repo.create_plan("t-plan", None, "{}", now - 91 * day)
            .await
            .unwrap();
        repo.checkpoint_task(TaskCheckpoint {
            plan_id: "t-plan",
            task_id: "old",
            agent: "A",
            status: "completed",
            response: None,
            error: None,
            now: now - 91 * day,

            model: None,
            duration_ms: None,
            cost_usd: None,
        })
        .await
        .unwrap();
        repo.checkpoint_task(TaskCheckpoint {
            plan_id: "t-plan",
            task_id: "fresh",
            agent: "A",
            status: "completed",
            response: None,
            error: None,
            now: now - 89 * day,

            model: None,
            duration_ms: None,
            cost_usd: None,
        })
        .await
        .unwrap();

        let deleted_tasks = repo.delete_expired_tasks(cutoff, now).await.unwrap();
        assert_eq!(deleted_tasks, 1, "91-day task deleted, 89-day kept");
        let remaining: Vec<String> = repo
            .load_task_rows("t-plan")
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.task_id)
            .collect();
        assert_eq!(remaining, vec!["fresh".to_string()]);

        // Plans: old terminal deleted, old running exempt, fresh kept.
        repo.create_plan("old-done", None, "{}", now - 91 * day)
            .await
            .unwrap();
        repo.complete_plan("old-done", "failed", None, None, None, now - 91 * day)
            .await
            .unwrap();
        repo.create_plan("old-running", None, "{}", now - 91 * day)
            .await
            .unwrap();
        repo.create_plan("fresh-plan", None, "{}", now - day)
            .await
            .unwrap();

        let deleted_plans = repo.delete_expired_plans(cutoff, now).await.unwrap();
        assert_eq!(deleted_plans, 1, "91-day terminal plan deleted");
        assert!(repo.load_plan("old-done").await.unwrap().is_none());
        assert!(
            repo.load_plan("old-running").await.unwrap().is_some(),
            "running plan exempt from retention"
        );
        assert!(repo.load_plan("fresh-plan").await.unwrap().is_some());

        // F5 ordering: the expired task rows of a still-running plan were
        // already removed while the plan row survives.
        assert!(repo.load_task_rows("t-plan").await.unwrap().len() == 1);
    }

    /// W6.2b pin: the cancel finalizer primitives — in-flight (still
    /// non-terminal) rows become `failed` with the reason in `error`,
    /// never-dispatched rows become `skipped`, and the plan flips to
    /// terminal `cancelled` exactly once (idempotent on the second call).
    #[tokio::test]
    async fn cancel_finalizer_writes_terminal_states_idempotently() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_cancel.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        let now = 5000i64;
        repo.mint_plan_with_tasks(
            "cxl",
            None,
            "{}",
            "sess-c",
            &[
                TaskDraft {
                    id: "inflight".into(),
                    status: "progressing".into(),
                },
                TaskDraft {
                    id: "draft".into(),
                    status: "pending".into(),
                },
            ],
            now,
        )
        .await
        .unwrap();
        // One id already settled before the finalizer ran (race window).
        repo.checkpoint_task(TaskCheckpoint {
            plan_id: "cxl",
            task_id: "settled",
            agent: "A",
            status: "completed",
            response: Some("kept"),
            error: None,
            now,

            model: None,
            duration_ms: None,
            cost_usd: None,
        })
        .await
        .unwrap();

        assert!(repo.cancel_running_plan("cxl", now + 1).await.unwrap());
        assert_eq!(
            repo.fail_tasks_with_reason("cxl", &["inflight".to_string()], "cancelled", now + 1)
                .await
                .unwrap(),
            1
        );
        assert_eq!(repo.skip_remaining_tasks("cxl", now + 1).await.unwrap(), 1);

        let rows = repo.load_task_rows("cxl").await.unwrap();
        let by_id = |id: &str| rows.iter().find(|r| r.task_id == id).unwrap();
        assert_eq!(by_id("inflight").status, "failed");
        assert_eq!(by_id("inflight").error.as_deref(), Some("cancelled"));
        assert_eq!(by_id("draft").status, "skipped");
        assert_eq!(by_id("settled").status, "completed", "settled outcome kept");
        assert_eq!(by_id("settled").response.as_deref(), Some("kept"));

        let plan = repo.load_plan("cxl").await.unwrap().unwrap();
        assert_eq!(plan.status, "cancelled");
        assert!(
            !repo.cancel_running_plan("cxl", now + 2).await.unwrap(),
            "idempotent"
        );
    }

    /// W6.2 pin: the session marker in `spec_json` resolves the current
    /// plan — latest by created_at, other sessions' plans invisible.
    #[tokio::test]
    async fn latest_plan_for_session_resolves_the_marker() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_marker.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        repo.mint_plan_with_tasks("first", None, r#"{"session_id":"s1"}"#, "s1", &[], 1000)
            .await
            .unwrap();
        repo.mint_plan_with_tasks("second", None, r#"{"session_id":"s1"}"#, "s1", &[], 2000)
            .await
            .unwrap();
        repo.mint_plan_with_tasks("other", None, r#"{"session_id":"s2"}"#, "s2", &[], 3000)
            .await
            .unwrap();

        let found = repo.latest_plan_for_session("s1").await.unwrap().unwrap();
        assert_eq!(found.plan_id, "second", "most recent wins");
        assert!(
            repo.latest_plan_for_session("ghost")
                .await
                .unwrap()
                .is_none(),
            "unknown session → None"
        );
    }

    /// W9.3 pin: `append_tasks` is INSERT-only — appended tasks land as
    /// `pending` draft rows with the `''` agent sentinel, settled rows are
    /// untouched, and a duplicate task id violates the PK (structured
    /// error upstream, never a silent overwrite).
    #[tokio::test]
    async fn append_tasks_inserts_pending_draft_rows_and_rejects_duplicates() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_append.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        repo.mint_plan_with_tasks(
            "live-plan",
            None,
            "{}",
            "sess-a",
            &[TaskDraft {
                id: "settled".into(),
                status: "completed".into(),
            }],
            1000,
        )
        .await
        .unwrap();
        repo.checkpoint_task(TaskCheckpoint {
            plan_id: "live-plan",
            task_id: "settled",
            agent: "Explore",
            status: "completed",
            response: Some("kept"),
            error: None,
            now: 1001,

            model: None,
            duration_ms: None,
            cost_usd: None,
        })
        .await
        .unwrap();

        let inserted = repo
            .append_tasks(
                "live-plan",
                &[
                    TaskDraft {
                        id: "extra-a".into(),
                        status: "pending".into(),
                    },
                    TaskDraft {
                        id: "extra-b".into(),
                        status: "pending".into(),
                    },
                ],
                1002,
            )
            .await
            .unwrap();
        assert_eq!(inserted, 2, "one row per appended draft");

        let rows = repo.load_task_rows("live-plan").await.unwrap();
        assert_eq!(rows.len(), 3);
        let settled = rows.iter().find(|r| r.task_id == "settled").unwrap();
        assert_eq!(settled.status, "completed", "settled row untouched");
        assert_eq!(settled.response.as_deref(), Some("kept"));
        for id in ["extra-a", "extra-b"] {
            let row = rows.iter().find(|r| r.task_id == id).unwrap();
            assert_eq!(row.status, "pending", "appended rows start pending");
            assert_eq!(row.agent, "", "'' agent sentinel until dispatch");
        }

        let dup = repo
            .append_tasks(
                "live-plan",
                &[TaskDraft {
                    id: "extra-a".into(),
                    status: "pending".into(),
                }],
                1003,
            )
            .await;
        assert!(dup.is_err(), "duplicate (plan, task) id violates the PK");
    }

    #[tokio::test]
    async fn concurrent_checkpoints_serialize_through_single_writer() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_conc.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        repo.create_plan("p4", None, "{}", 1000).await.unwrap();

        let futs: Vec<_> = (0..16u32)
            .map(|i| {
                let tid = Box::leak(format!("t{i}").into_boxed_str());
                repo.checkpoint_task(TaskCheckpoint {
                    plan_id: "p4",
                    task_id: tid,
                    agent: "Explore",
                    status: "completed",
                    response: Some("done"),
                    error: None,
                    now: 1001,

                    model: None,
                    duration_ms: None,
                    cost_usd: None,
                })
            })
            .collect();
        let results = futures::future::join_all(futs).await;
        for r in results {
            r.unwrap();
        }

        let rows = repo.load_task_rows("p4").await.unwrap();
        assert_eq!(rows.len(), 16, "16 distinct task_ids → 16 rows");

        // 8 concurrent checkpoints on the SAME (p4, "hot") task_id.
        // INSERT OR REPLACE under serialization means exactly 1 row survives.
        let hot_futs: Vec<_> = (0..8u32)
            .map(|i| {
                let resp = Box::leak(format!("hot-{i}").into_boxed_str());
                repo.checkpoint_task(TaskCheckpoint {
                    plan_id: "p4",
                    task_id: "hot",
                    agent: "Explore",
                    status: "completed",
                    response: Some(resp),
                    error: None,
                    now: 1002,

                    model: None,
                    duration_ms: None,
                    cost_usd: None,
                })
            })
            .collect();
        let hot_results = futures::future::join_all(hot_futs).await;
        for r in hot_results {
            r.unwrap();
        }

        let hot_rows: Vec<_> = repo
            .load_task_rows("p4")
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.task_id == "hot")
            .collect();
        assert_eq!(hot_rows.len(), 1, "same (plan, task) → exactly 1 row");
        assert!(
            hot_rows[0].response.as_deref().unwrap().starts_with("hot-"),
            "response should be one of the 8 written values, not corruption"
        );
    }

    /// W11.2/T2 (D31/F2): a `%`/`_`-bearing legacy session id must NOT match
    /// another session's plan — the LIKE marker is escaped, so the wildcard
    /// is a literal. Pre-fix, `%"session_id":"s%1"%` matched every s*1 plan.
    #[tokio::test]
    async fn latest_plan_for_session_wildcard_id_is_isolated() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_wild.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        // A legacy row whose session id carries a LIKE wildcard.
        repo.mint_plan_with_tasks("wild", None, r#"{"session_id":"s%1"}"#, "s%1", &[], 1000)
            .await
            .unwrap();
        repo.mint_plan_with_tasks("plain", None, r#"{"session_id":"sX1"}"#, "sX1", &[], 2000)
            .await
            .unwrap();

        // The wildcard id resolves ONLY its own row (exact literal match).
        let wild = repo.latest_plan_for_session("s%1").await.unwrap().unwrap();
        assert_eq!(wild.plan_id, "wild");
        // Neither a concrete nor a wildcard query may cross-match.
        assert!(
            repo.latest_plan_for_session("sX1")
                .await
                .unwrap()
                .unwrap()
                .plan_id
                == "plain"
        );
        assert!(
            repo.latest_plan_for_session("s1").await.unwrap().is_none(),
            "a `%` id must not match a concrete session"
        );
        assert!(
            repo.latest_plan_for_session("%").await.unwrap().is_none(),
            "a bare wildcard must not match every plan"
        );
    }

    /// W11.6/T6 (F6): same-second mints tiebreak deterministically by
    /// insertion order (rowid DESC), not the random plan_id UUID.
    #[tokio::test]
    async fn latest_plan_for_session_same_second_tiebreak_is_insertion_order() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_tie.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        // Identical created_at (second granularity) — the tiebreak decides.
        for id in ["aaa-first", "zzz-second"] {
            repo.mint_plan_with_tasks(id, None, r#"{"session_id":"s1"}"#, "s1", &[], 5000)
                .await
                .unwrap();
        }
        let found = repo.latest_plan_for_session("s1").await.unwrap().unwrap();
        assert_eq!(
            found.plan_id, "zzz-second",
            "the later-inserted row wins regardless of UUID ordering"
        );
    }

    /// W11.13/T14 (F8): duration_history is bounded to the most recent
    /// FORECAST_HISTORY_WINDOW rows — old rows are excluded.
    #[tokio::test]
    async fn duration_history_excludes_rows_beyond_the_recent_window() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_window.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);
        repo.create_plan("pw", None, "{}", 0).await.unwrap();

        let total = FORECAST_HISTORY_WINDOW + 5;
        for i in 0..total {
            repo.checkpoint_task(TaskCheckpoint {
                plan_id: "pw",
                task_id: &format!("t{i}"),
                agent: "Explore",
                status: "completed",
                response: Some("ok"),
                error: None,
                now: i,
                model: Some("m"),
                duration_ms: Some((1000 + i) as u64),
                cost_usd: None,
            })
            .await
            .unwrap();
        }
        let rows = repo.duration_history().await.unwrap();
        assert_eq!(
            rows.len() as i64,
            FORECAST_HISTORY_WINDOW,
            "the window caps the history"
        );
        // The oldest rows (updated_at 0..5) fall outside the recent window.
        assert!(
            rows.iter().all(|r| r.duration_ms >= 1005),
            "old rows must be excluded: {:?}",
            rows.iter().map(|r| r.duration_ms).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn induction_corpus_caps_plan_count_at_limit() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_corpus_limit.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        let total = INDUCTION_CORPUS_LIMIT + 50;
        for i in 0..total {
            let pid = format!("p{i:05}");
            repo.create_plan(&pid, None, "{}", 10_000 + i)
                .await
                .unwrap();
        }

        let rows = repo.induction_corpus(0).await.unwrap();
        assert_eq!(
            rows.len() as i64,
            INDUCTION_CORPUS_LIMIT,
            "the corpus caps the plan count"
        );
        let oldest_included = rows.iter().map(|r| r.created_at).min().unwrap();
        assert_eq!(
            oldest_included, 10_050,
            "the oldest 50 plans are truncated; the 1000th newest is the floor"
        );
    }

    #[tokio::test]
    async fn attach_batch_spec_merges_while_running_and_guards_terminal() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_attach_spec.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        let original = r#"{"items":[{"id":"a","title":"first","status":"pending"}],"session_id":"s1","sensitivity":"private"}"#;
        repo.create_plan("p1", None, original, 1_000).await.unwrap();

        let merged = r#"{"items":[{"id":"a","title":"first","status":"pending"}],"tasks":[{"id":"a","agent":"Explore","prompt":"do a","depends_on":[]}],"session_id":"s1","sensitivity":"private"}"#;
        repo.attach_batch_spec("p1", merged, 1_005).await.unwrap();
        let row = repo.load_plan("p1").await.unwrap().expect("row");
        assert_eq!(row.spec_json, merged, "spec_json replaced by the batch DAG");
        assert_eq!(
            row.updated_at, 1_005,
            "heartbeat bump in the same statement"
        );

        assert!(
            repo.complete_plan("p1", "completed", None, None, None, 1_010)
                .await
                .unwrap()
        );
        let unchanged = r#"{"tasks":[{"id":"z","agent":"Ghost","prompt":"x","depends_on":[]}]}"#;
        repo.attach_batch_spec("p1", unchanged, 2_000)
            .await
            .unwrap();
        let row = repo.load_plan("p1").await.unwrap().expect("row");
        assert_eq!(row.spec_json, merged, "a terminal row must not be mutated");
        assert_eq!(
            row.updated_at, 1_010,
            "terminal guard also skips the heartbeat"
        );
    }
}
