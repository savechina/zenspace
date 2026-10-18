//! Embedding refresher — 30-minute incremental vec0 freshness worker (option C).
//!
//! Scope logic (Constitution XV):
//! - **Functionality**: every 30 minutes, re-embeds ONLY the stale vectors in
//!   the global `state.db` by calling the SHARED [`Reindexer`] with embeddings
//!   ENABLED (i.e. NOT [`Reindexer::without_embeddings`]). The freshness gate
//!   (`notes_meta.embedded_hash != content_hash`, migration 012) makes the run
//!   incremental: unchanged pages cost nothing, dirty pages are re-embedded,
//!   and `content_hash`/`notes_fts` are NOT rewritten for pages whose content
//!   did not change (the vector-only refresh path).
//! - **User impact**: semantic (tier-3) search stops silently degrading. The
//!   daemon's 5-minute `zen-loop` deliberately runs
//!   `Reindexer::without_embeddings()` — it only MARKS vectors dirty — so
//!   without this worker a daemon-rewritten page's vector stays stale forever
//!   unless someone runs `zen wiki reindex` by hand. This is the recorded
//!   "option C" automatic heal.
//! - **Default**: cron `0 */30 * * * *`; registered in BOTH scheduler profiles
//!   (`Full` + `InApp`) — the cross-process scheduler lease guarantees exactly
//!   one host refreshes.
//! - **Interaction**: fail-fast guard. The run's FIRST embedding result is a
//!   fixed provider probe; if it is degraded (hash fallback ⇒ the real provider
//!   is unavailable/hanging) the run ABORTS before embedding any page (each
//!   page can cost up to a 240s provider timeout, so N pages is unbounded). The
//!   abort warning is gated once per process. The worker never deletes, resets,
//!   DROPs or rebuilds vec0, and never marks a degraded vector fresh (it stops
//!   at the first one).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use anyhow::Result;
use tracing::{info, warn};

use zen_core::jsonl::append_jsonl_line;
use zen_core::paths::ZenPaths;
use zen_vault::{ReindexReport, Reindexer, SqliteClient};

use super::super::{WorkerContext, WorkerReport, ZenWorker};
use super::zen_loop::log_once;

/// Cron: every 30 minutes (6-field, seconds first) — same field style as the
/// neighbouring `wiki-compiler` worker (`0 */30 * * * *`).
pub const EMBEDDING_REFRESH_SCHEDULE: &str = "0 */30 * * * *";

/// The run's first embedding is a fixed, content-free probe: only whether the
/// provider returned a real vector (vs the byte-hash fallback) matters, so the
/// probe text is deliberately tiny and stable.
const PROVIDER_PROBE_TEXT: &str = "zen embedding refresher provider probe";

/// Once-per-process gate for the degraded-provider abort warning (mirrors the
/// sibling workers' `log_once` discipline — a hanging provider must not flood).
static PROVIDER_DEGRADED_LOGGED: AtomicBool = AtomicBool::new(false);

/// Once-per-process gate for repeated `state.db` open failures.
static DB_OPEN_FAILED_LOGGED: AtomicBool = AtomicBool::new(false);

/// Provider health probe: `true` when embedding `text` came back degraded (the
/// hash fallback was used ⇒ the real provider is unavailable/hanging).
type ProbeFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// The production probe: the shared tagged embedding entry point, degraded on
/// any error (an erroring provider is "unavailable" just as much as a
/// hash-fallback one).
fn default_provider_degraded(text: &str) -> bool {
    match zen_vault::tindy::embeddings::compute_embeddings_for_text_tagged(text) {
        Ok((_embedding, degraded)) => degraded,
        Err(_) => true,
    }
}

/// One 30-minute incremental embedding-refresh run.
pub struct EmbeddingRefresherWorker {
    scheduled: Option<&'static str>,
    probe: ProbeFn,
}

impl Default for EmbeddingRefresherWorker {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddingRefresherWorker {
    pub fn new() -> Self {
        Self {
            scheduled: None,
            probe: Arc::new(default_provider_degraded),
        }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }

    /// Test seam: inject a deterministic provider probe so the abort path can
    /// be exercised without touching a real (possibly absent/hanging) provider.
    #[cfg(test)]
    fn with_probe(mut self, probe: ProbeFn) -> Self {
        self.probe = probe;
        self
    }

    /// Testable core: refresh stale vectors under an explicit [`ZenPaths`].
    ///
    /// The provider is probed FIRST (the run's first embedding result). A
    /// degraded probe aborts before the [`Reindexer`] is ever constructed, so a
    /// dead provider costs one bounded attempt instead of one per dirty page.
    pub async fn execute_with_paths(
        &self,
        paths: &ZenPaths,
        ctx: &WorkerContext,
    ) -> Result<WorkerReport> {
        let start = Instant::now();

        let state_db = paths.data().join("state.db");
        let client = match SqliteClient::open(&state_db).await {
            Ok(c) => c,
            Err(e) => {
                if log_once(&DB_OPEN_FAILED_LOGGED) {
                    warn!(
                        error = %e,
                        "embedding-refresher: failed to open state.db — skipping refresh"
                    );
                }
                return Ok(self.finish(paths, ctx, 0, 0, start, false));
            }
        };

        // Fail-fast: probe the provider once. `spawn_blocking` mirrors the
        // embedding providers' blocking/nested-runtime discipline; a join
        // failure is treated as degraded (fail-safe).
        let probe = Arc::clone(&self.probe);
        let probe_degraded = tokio::task::spawn_blocking(move || probe(PROVIDER_PROBE_TEXT))
            .await
            .unwrap_or(true);
        if probe_degraded {
            if log_once(&PROVIDER_DEGRADED_LOGGED) {
                warn!(
                    "embedding-refresher: embedding provider degraded (hash fallback) — aborting \
                     before embedding any page; run `zen wiki reindex` once the local provider is \
                     reachable"
                );
            }
            return Ok(self.finish(paths, ctx, 0, 0, start, true));
        }

        // Shared incremental machinery: embeddings ENABLED (no
        // `without_embeddings`), so `embedded_hash != content_hash` pages are
        // re-embedded while unchanged pages are skipped and `content_hash` /
        // `notes_fts` are left untouched.
        let report = Reindexer::with_client(client)
            .reindex(&paths.vault())
            .await?;

        let (refreshed, degraded) = refreshed_and_degraded(&report);
        Ok(self.finish(paths, ctx, refreshed, degraded, start, false))
    }

    /// Emit the `loop.embedding.refresh` audit line (when the run was not a
    /// clean no-op) and build the [`WorkerReport`]. Pure side-effecting helper,
    /// sync by design so tests can drive it with a synthetic `ReindexReport`.
    fn finish(
        &self,
        paths: &ZenPaths,
        ctx: &WorkerContext,
        refreshed: usize,
        degraded: usize,
        start: Instant,
        aborted: bool,
    ) -> WorkerReport {
        let elapsed_ms = start.elapsed().as_millis() as u64;
        if should_emit_audit(refreshed, degraded, aborted) {
            let audit = build_audit(ctx, refreshed, degraded, elapsed_ms, aborted);
            let audit_path = paths.logs().join("audit.jsonl");
            if let Err(e) = append_jsonl_line(&audit_path, &audit) {
                warn!(
                    error = %e, path = %audit_path.display(),
                    "embedding-refresher: audit append failed (result kept in worker report)"
                );
            }
        }
        info!(
            refreshed,
            degraded, elapsed_ms, aborted, "embedding-refresher: run complete"
        );
        WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: refreshed,
            duration_ms: elapsed_ms,
            llm_cost_usd: 0.0,
        }
    }
}

/// Flood discipline: a clean no-op (nothing refreshed, nothing degraded, no
/// abort) writes NOTHING. Any refreshed vector, any observed degraded vector,
/// or an abort is an operational event an operator must be able to see.
fn should_emit_audit(refreshed: usize, degraded: usize, aborted: bool) -> bool {
    refreshed > 0 || degraded > 0 || aborted
}

/// Split a [`ReindexReport`] into `(refreshed, degraded)`.
///
/// `refreshed` counts rows whose freshness marker was actually advanced:
/// vectors stored minus the degraded (hash-fallback) ones. `embeddings_degraded`
/// is documented as a subset of `files_embedded`, and a degraded vector is
/// stored but never marked fresh — so this subtraction is exactly the
/// "every row marked fresh was NOT produced from a degraded embedding"
/// invariant, in count form.
fn refreshed_and_degraded(report: &ReindexReport) -> (usize, usize) {
    let degraded = report.embeddings_degraded;
    (report.files_embedded.saturating_sub(degraded), degraded)
}

/// The `loop.embedding.refresh` audit payload (append-only JSONL). Keys let an
/// operator distinguish "refreshed 30 vectors" (`aborted:false, refreshed:30`)
/// from "aborted because no provider" (`aborted:true, refreshed:0`).
fn build_audit(
    ctx: &WorkerContext,
    refreshed: usize,
    degraded: usize,
    elapsed_ms: u64,
    aborted: bool,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "loop.embedding.refresh",
        "ts": ctx.now.to_rfc3339(),
        "refreshed": refreshed,
        "degraded": degraded,
        "elapsed_ms": elapsed_ms,
        "aborted": aborted,
    })
}

#[async_trait::async_trait]
impl ZenWorker for EmbeddingRefresherWorker {
    fn id(&self) -> &'static str {
        "embedding-refresher"
    }

    fn description(&self) -> &'static str {
        "Refresh stale embedding vectors (vec0) incrementally every 30 minutes — the embedding-enabled companion to the zen-loop reindex"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or(EMBEDDING_REFRESH_SCHEDULE)
    }

    async fn execute(&self, ctx: &WorkerContext) -> Result<WorkerReport> {
        let paths = ZenPaths::detect()?;
        self.execute_with_paths(&paths, ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::path::Path;
    use zen_repo::{IndexNoteRequest, NotesRepo};
    use zen_vault::compute_file_checksum;

    fn test_paths() -> (tempfile::TempDir, ZenPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ZenPaths::for_testing(tmp.path().to_path_buf());
        paths.ensure_runtime_dirs().unwrap();
        (tmp, paths)
    }

    /// Write a note into the vault and index it. `index_note` leaves
    /// `embedded_hash = ''`, so the row starts DIRTY (content unchanged).
    async fn seed_dirty_note(
        paths: &ZenPaths,
        client: &SqliteClient,
        id: &str,
    ) -> (String, String) {
        let dir = paths.vault().join("notes");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{id}.md"));
        let body =
            format!("---\nid: {id}\ntitle: {id}\n---\n\n# {id}\n\nsome substantive body text\n");
        std::fs::write(&file, &body).unwrap();
        let file_path = file.display().to_string();
        NotesRepo::new(client)
            .index_note(IndexNoteRequest {
                id,
                title: id,
                content: &body,
                tags: "",
                file_path: &file_path,
                source: "vault",
            })
            .await
            .unwrap();
        (id.to_string(), file_path)
    }

    fn read_audit_lines(paths: &ZenPaths) -> Vec<serde_json::Value> {
        let path = paths.logs().join("audit.jsonl");
        if !path.exists() {
            return Vec::new();
        }
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn healthy_probe() -> ProbeFn {
        Arc::new(|_text: &str| false)
    }

    fn degraded_probe() -> ProbeFn {
        Arc::new(|_text: &str| true)
    }

    #[test]
    fn worker_identity_and_schedule() {
        let worker = EmbeddingRefresherWorker::new();
        assert_eq!(worker.id(), "embedding-refresher");
        assert_eq!(worker.schedule(), "0 */30 * * * *");
        assert!(worker.description().contains("stale embedding"));
        assert_eq!(
            EmbeddingRefresherWorker::new()
                .with_schedule("0 15 * * * *")
                .schedule(),
            "0 15 * * * *"
        );
    }

    #[test]
    fn audit_flood_discipline() {
        assert!(
            !should_emit_audit(0, 0, false),
            "clean no-op must stay silent"
        );
        assert!(should_emit_audit(1, 0, false));
        assert!(should_emit_audit(0, 1, false));
        assert!(should_emit_audit(0, 0, true));
    }

    /// The core safety invariant in count form: a degraded (hash-fallback)
    /// vector is never counted as refreshed.
    #[test]
    fn degraded_vectors_are_never_counted_as_refreshed() {
        let all_degraded = ReindexReport {
            files_embedded: 5,
            embeddings_degraded: 5,
            ..ReindexReport::default()
        };
        assert_eq!(refreshed_and_degraded(&all_degraded), (0, 5));

        let mixed = ReindexReport {
            files_embedded: 5,
            embeddings_degraded: 2,
            ..ReindexReport::default()
        };
        assert_eq!(refreshed_and_degraded(&mixed), (3, 2));

        let clean = ReindexReport {
            files_embedded: 5,
            embeddings_degraded: 0,
            ..ReindexReport::default()
        };
        assert_eq!(refreshed_and_degraded(&clean), (5, 0));
    }

    /// The audit contract: an operator can read "refreshed 30 vectors" apart
    /// from "aborted because no provider".
    #[tokio::test]
    async fn refreshed_run_emits_audit_with_counts() {
        let (_tmp, paths) = test_paths();
        let worker = EmbeddingRefresherWorker::new();
        let ctx = WorkerContext::new(Utc::now());
        let report = worker.finish(&paths, &ctx, 30, 0, Instant::now(), false);

        assert_eq!(report.fact_count, 30);
        let lines = read_audit_lines(&paths);
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert_eq!(line["kind"], "loop.embedding.refresh");
        assert_eq!(line["refreshed"], 30);
        assert_eq!(line["degraded"], 0);
        assert_eq!(line["aborted"], false);
        assert!(line["ts"].is_string());
        assert!(line["elapsed_ms"].is_u64());
    }

    #[tokio::test]
    async fn aborted_run_emits_audit_with_aborted_flag() {
        let (_tmp, paths) = test_paths();
        let worker = EmbeddingRefresherWorker::new();
        let ctx = WorkerContext::new(Utc::now());
        worker.finish(&paths, &ctx, 0, 0, Instant::now(), true);

        let lines = read_audit_lines(&paths);
        assert_eq!(lines.len(), 1, "an abort is not a silent no-op");
        assert_eq!(lines[0]["aborted"], true);
        assert_eq!(lines[0]["refreshed"], 0);
    }

    /// Test 1 — clean/no-op run: with nothing dirty the worker writes nothing
    /// and emits no audit line. Host-independent: a fresh row is never
    /// embedded, so no provider is contacted.
    #[tokio::test]
    async fn clean_run_performs_no_writes_and_no_audit() {
        let (_tmp, paths) = test_paths();
        let state_db = paths.data().join("state.db");
        let client = SqliteClient::open(&state_db).await.unwrap();

        let (id, file_path) = seed_dirty_note(&paths, &client, "clean-note").await;
        // Mark it fresh: `embedded_hash == current file checksum`.
        let checksum = compute_file_checksum(Path::new(&file_path)).unwrap();
        NotesRepo::new(&client)
            .mark_embedded(&id, &checksum)
            .await
            .unwrap();
        drop(client);

        let worker = EmbeddingRefresherWorker::new().with_probe(healthy_probe());
        let ctx = WorkerContext::new(Utc::now());
        let report = worker.execute_with_paths(&paths, &ctx).await.unwrap();

        assert_eq!(report.fact_count, 0, "a fresh vault has nothing to refresh");
        assert!(
            read_audit_lines(&paths).is_empty(),
            "a clean run must not emit an audit line"
        );
    }

    /// Test 2 — invariant on the abort path: a degraded first result means
    /// every dirty row stays dirty (nothing newly marked fresh). Host-independent:
    /// the injected probe aborts before the Reindexer is constructed.
    #[tokio::test]
    async fn degraded_probe_aborts_and_leaves_dirty_rows_dirty() {
        let (_tmp, paths) = test_paths();
        let state_db = paths.data().join("state.db");
        let client = SqliteClient::open(&state_db).await.unwrap();

        let (id, _file_path) = seed_dirty_note(&paths, &client, "dirty-note").await;
        let before = NotesRepo::new(&client).all_embedded_hashes().await.unwrap();
        assert_eq!(
            before.get(&id).map(String::as_str),
            Some(""),
            "a freshly indexed row starts dirty"
        );
        drop(client);

        let worker = EmbeddingRefresherWorker::new().with_probe(degraded_probe());
        let ctx = WorkerContext::new(Utc::now());
        let report = worker.execute_with_paths(&paths, &ctx).await.unwrap();
        assert_eq!(report.fact_count, 0, "a degraded probe refreshes nothing");

        let lines = read_audit_lines(&paths);
        assert_eq!(lines.len(), 1, "the abort must be audited");
        assert_eq!(lines[0]["kind"], "loop.embedding.refresh");
        assert_eq!(lines[0]["aborted"], true);
        assert_eq!(lines[0]["refreshed"], 0);

        // Invariant: nothing was newly marked fresh — the row is still dirty.
        let client = SqliteClient::open(&state_db).await.unwrap();
        let after = NotesRepo::new(&client).all_embedded_hashes().await.unwrap();
        assert_eq!(
            after.get(&id).map(String::as_str),
            Some(""),
            "an aborted run must leave the dirty row dirty"
        );
    }
}
