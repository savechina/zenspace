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
}

/// A plan claim older than this (no checkpoint progress) may be stolen by
/// a fresh resume — a crashed run must not wedge the plan forever.
const STALE_CLAIM_SECS: i64 = 3600;

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

    pub async fn checkpoint_task(&self, cp: TaskCheckpoint<'_>) -> Result<()> {
        let TaskCheckpoint {
            plan_id,
            task_id,
            agent,
            status,
            response,
            error,
            now,
        } = cp;
        let plan_id = plan_id.to_string();
        let task_id = task_id.to_string();
        let agent = agent.to_string();
        let status = status.to_string();
        let response = response.map(|s| s.to_string());
        let error = error.map(|s| s.to_string());
        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO workflow_tasks \
                     (plan_id, task_id, agent, status, response, error, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![plan_id, task_id, agent, status, response, error, now],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    /// Atomic resume fence (/review #6): test-and-set an owner token on a
    /// `running` plan. The conditional UPDATE serializes through the single
    /// writer, so two concurrent resumes can never both observe
    /// `owner IS NULL`. Returns `Ok(true)` when this caller holds the
    /// claim, `Ok(false)` when another run holds it or the plan went
    /// terminal. A stale claim (no progress for `STALE_CLAIM_SECS`) may be
    /// stolen so a crashed resume does not wedge the plan forever.
    pub async fn claim_plan(&self, plan_id: &str, owner: &str, now: i64) -> Result<bool> {
        let plan_id = plan_id.to_string();
        let owner = owner.to_string();
        self.client
            .writer()
            .call(move |conn| {
                let changed = conn.execute(
                    "UPDATE workflow_plans SET owner = ?1, updated_at = ?2 \
                     WHERE plan_id = ?3 AND status = 'running' \
                       AND (owner IS NULL OR owner = ?1 OR updated_at < ?4)",
                    rusqlite::params![owner, now, plan_id, now - STALE_CLAIM_SECS],
                )?;
                Ok(changed == 1)
            })
            .await
            .map_err(SqliteError::TokioRusqlite)
    }

    pub async fn complete_plan(
        &self,
        plan_id: &str,
        status: &str,
        summary: Option<&str>,
        plan_approved: Option<bool>,
        delivery_ready: Option<bool>,
        now: i64,
    ) -> Result<()> {
        let plan_id = plan_id.to_string();
        let status = status.to_string();
        let summary = summary.map(|s| s.to_string());
        let plan_approved = plan_approved.map(|b| b as i64);
        let delivery_ready = delivery_ready.map(|b| b as i64);
        self.client
            .writer()
            .call(move |conn| {
                conn.execute(
                    "UPDATE workflow_plans SET status = ?1, summary = ?2, plan_approved = ?3, \
                     delivery_ready = ?4, updated_at = ?5, owner = NULL WHERE plan_id = ?6",
                    rusqlite::params![status, summary, plan_approved, delivery_ready, now, plan_id],
                )?;
                Ok(())
            })
            .await
            .map_err(SqliteError::TokioRusqlite)?;
        Ok(())
    }

    pub async fn load_plan(&self, plan_id: &str) -> Result<Option<WorkflowPlanRow>> {
        Ok(sqlx::query_as::<_, WorkflowPlanRow>(
            "SELECT plan_id, name, status, spec_json, summary, plan_approved, delivery_ready, created_at, updated_at \
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
            status: "ok",
            response: Some("first"),
            error: None,
            now: 1001,
        })
        .await
        .unwrap();
        repo.checkpoint_task(TaskCheckpoint {
            plan_id: "p1",
            task_id: "a",
            agent: "Explore",
            status: "ok",
            response: Some("second"),
            error: None,
            now: 1002,
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

    #[tokio::test]
    async fn claim_plan_fences_concurrent_resumes() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_claim.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        repo.create_plan("p2", None, "{}", 1000).await.unwrap();

        // First claim succeeds
        assert!(repo.claim_plan("p2", "run-a", 1000).await.unwrap());
        // Different owner blocked
        assert!(!repo.claim_plan("p2", "run-b", 1000).await.unwrap());
        // Same owner re-claims (claim is idempotent for the owner)
        assert!(repo.claim_plan("p2", "run-a", 1000).await.unwrap());

        // Complete the plan (clears owner)
        repo.complete_plan("p2", "completed", None, None, None, 2000)
            .await
            .unwrap();
        // Terminal plans refuse claims
        assert!(!repo.claim_plan("p2", "run-c", 2000).await.unwrap());
    }

    #[tokio::test]
    async fn stale_claim_may_be_stolen() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("wf_stale.db");
        let client = crate::SqliteClient::open(&db).await.unwrap();
        let repo = WorkflowRepo::new(&client);

        repo.create_plan("p3", None, "{}", 1000).await.unwrap();

        // Dead runner claims at t=1000
        assert!(repo.claim_plan("p3", "dead-run", 1000).await.unwrap());

        // Thief tries at t=1000+3599 — still fresh (1s < STALE_CLAIM_SECS=3600)
        assert!(
            !repo.claim_plan("p3", "thief", 1000 + 3599).await.unwrap(),
            "claim should still be fresh at now-1s"
        );

        // Fresh runner steals at t=1000+3601 — stale (1s > STALE_CLAIM_SECS=3600)
        assert!(
            repo.claim_plan("p3", "fresh-run", 1000 + 3601)
                .await
                .unwrap(),
            "stale claim should be stealable"
        );
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
                    status: "ok",
                    response: Some("done"),
                    error: None,
                    now: 1001,
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
                    status: "ok",
                    response: Some(resp),
                    error: None,
                    now: 1002,
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
}
