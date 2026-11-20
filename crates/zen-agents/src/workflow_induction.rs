//! Workflow induction (W13.1-W13.4) — reusable DAG templates induced from
//! repeated manual `plan.execute` shapes.
//!
//! Design of record: `docs/designs/workflow-induction-2026-11-18.md`
//! (sections 2-5a + 10). Advisory-only by construction: nothing here reads a
//! template to alter plan execution; an induced draft waits for the human
//! gate (`zen discover confirm-workflow`, Phase 2).
//!
//! Pipeline:
//! 1. Shape (W13.1) — parse `workflow_plans.spec_json` into a canonical
//!    `(agent, in_degree, out_degree)` multiset signature. The `plan.todos`
//!    mint shape (`{items:[...]}`) has no edges and is classified
//!    [`SpecShape::TitlesOnly`], never corrupt.
//! 2. Corpus (W13.2) — fully-completed plans (every task `completed` or
//!    `skipped`, zero `failed`) created within [`INDUCTION_WINDOW_DAYS`].
//! 3. Cluster + draft (W13.3) — greedy seed-around-first-plan over
//!    [`trigram_jaccard`] at [`OBSERVATION_SIMILARITY`]; a cluster of
//!    [`MIN_OBSERVATIONS`] emits one draft into
//!    `logs/workflow-induction-drafts.json` (overwrite, atomic).
//! 4. Flock (W13.4) — [`acquire_induction_lock`] serializes the whole
//!    read-merge-write cycle against confirm/reject (Phase 2).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use zen_repo::{InductionTaskRow, SqliteClient, WorkflowRepo};
use zen_vault::distill::trigram_jaccard;

/// Cluster similarity for shape signatures — the skill-precipitation value
/// (`skill_precipitation.rs`), in-code per D22 (no config key).
pub const OBSERVATION_SIMILARITY: f64 = 0.72;

/// Minimum plans in one shape cluster before a draft is staged.
pub const MIN_OBSERVATIONS: usize = 2;

/// Corpus window in days — mirrors the daemon's workflow-row GC window
/// (`daemon.rs` 90d) and the `workflow_plans` retention bound.
pub const INDUCTION_WINDOW_DAYS: i64 = 90;

/// Draft queue file under the logs dir (overwritten per induction run).
pub const DRAFTS_FILE: &str = "workflow-induction-drafts.json";

/// Rejected-signature ledger under the logs dir (append-on-reject, read on
/// every run; signatures are quarantined permanently).
pub const REJECTED_FILE: &str = "workflow-induction-rejected.json";

/// Cross-process lock file under the logs dir.
pub const LOCK_FILE: &str = "workflow-induction.lock";

/// Last-run sidecar under the logs dir (W13.8). Records the per-run counters
/// the drafts queue cannot reconstruct (see [`InductionRunSummary`]).
pub const LAST_RUN_FILE: &str = "workflow-induction-last-run.json";

/// Promoted-template directory under the vault root (design §6).
const WORKFLOW_TEMPLATE_SUBDIR: &str = "wiki/wisdom/workflow-templates";

const PROMPT_SKELETON_MAX_CHARS: usize = 200;
const SECS_PER_DAY: i64 = 24 * 3600;

/// A malformed `spec_json` — the row is skipped and counted
/// `skipped_corrupt`, never fatal (AC#8).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShapeError {
    #[error("spec_json is not valid JSON: {0}")]
    InvalidJson(String),
    #[error("spec_json is not a JSON object")]
    NotAnObject,
    #[error("spec_json has neither a `tasks` nor an `items` array")]
    MissingTasks,
    #[error("spec_json `tasks` is not an array")]
    MalformedTasks,
    #[error("spec_json `tasks` is empty")]
    EmptyTasks,
    #[error("spec_json task #{index} is not an object with id/agent/depends_on")]
    MalformedTask { index: usize },
    #[error("duplicate task id `{0}`")]
    DuplicateTaskId(String),
    #[error("task `{task}` depends on unknown task `{dep}`")]
    UnknownDependency { task: String, dep: String },
    #[error("plan contains a dependency cycle")]
    Cycle,
}

/// One task of a parsed DAG shape, in declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapeTask {
    pub id: String,
    pub agent: String,
    pub in_degree: usize,
    pub out_degree: usize,
    pub prompt: String,
    pub depends_on: Vec<String>,
}

/// Result of parsing a `spec_json`: a canonical DAG signature or the
/// `plan.todos` titles-only shape (a valid classification, not an error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecShape {
    Dag {
        signature: String,
        tasks: Vec<ShapeTask>,
    },
    TitlesOnly,
}

/// A staged workflow template awaiting the human confirm gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowDraft {
    pub name: String,
    pub shape_signature: String,
    pub source_plan_ids: Vec<String>,
    pub source_plan_dates: Vec<String>,
    pub tasks: Vec<DraftTaskTemplate>,
    pub success_stats: DraftSuccessStats,
    pub induced_at: String,
}

/// One induced task template (literal median prompt, no placeholder
/// extraction — design §5 deviation 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftTaskTemplate {
    pub id: String,
    pub agent: String,
    pub role: String,
    pub typical_prompt_skeleton: String,
    pub depends_on: Vec<String>,
}

/// Cluster aggregate for a draft (design §5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftSuccessStats {
    pub cluster_size: usize,
    pub avg_duration_ms: u64,
    pub avg_cost_usd: f64,
    pub success_rate: f64,
}

/// One permanently-quarantined shape (design §5 rejected ledger).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RejectedSignature {
    pub shape_signature: String,
    pub rejected_at: String,
    pub source_plan_ids: Vec<String>,
}

/// Why an induction run produced no drafts (design §7 honest states).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsufficientReason {
    NoPlansInWindow,
    InsufficientCompletedPlans,
    NoFullyCompletedPlans,
    NoSimilarCluster,
    /// W14.5 (eng-review #11): every parsed row was a titles-only
    /// `plan.todos` mint — the corpus cannot fill until DAG-shaped
    /// (attached or fresh-minted) plans exist.
    TitlesOnlyCorpus,
}

impl InsufficientReason {
    /// The design §7 `missing` sentence for the report surface (W13.8).
    pub fn missing(self) -> &'static str {
        match self {
            Self::NoPlansInWindow => "no workflow_plans in 90d window",
            Self::InsufficientCompletedPlans => "insufficient completed plans (< 2)",
            Self::NoFullyCompletedPlans => "no fully-completed plans",
            Self::NoSimilarCluster => "no similar shape cluster (threshold 0.72)",
            Self::TitlesOnlyCorpus => {
                "no DAG-shaped plans in the window (titles-only mints excluded)"
            }
        }
    }
}

/// Outcome of one induction run — consumed by the worker (W13.7) and the
/// `zen discover report` section (W13.8).
#[derive(Debug, Clone, PartialEq)]
pub struct InductionRun {
    pub drafts_written: usize,
    pub skipped_corrupt: usize,
    pub skipped_titles_only: usize,
    pub insufficient: Option<InsufficientReason>,
}

/// Persisted outcome of the last induction run (W13.8 sidecar). The drafts
/// queue holds only current drafts, so the per-run counters and insufficiency
/// reason are recorded here for the report — written under the drafts flock,
/// atomically. Missing or corrupt ⇒ the report renders `{"recorded": false}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InductionRunSummary {
    pub drafts_written: usize,
    pub skipped_corrupt: usize,
    pub skipped_titles_only: usize,
    pub insufficient: bool,
    #[serde(default)]
    pub missing: Option<String>,
    pub induced_at: String,
}

/// Induction failure modes (I/O, JSON, DB). Corrupt CORPUS rows are not
/// errors — they are counted in [`InductionRun::skipped_corrupt`].
#[derive(Debug, thiserror::Error)]
pub enum InductionError {
    #[error("workflow induction io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("workflow induction json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("workflow induction repo error: {0}")]
    Repo(#[from] zen_repo::SqliteError),
    #[error("no staged workflow draft named `{0}` (see `zen discover report`)")]
    UnknownDraft(String),
}

/// Result of [`confirm_workflow`]: a fresh promotion, or the idempotent
/// no-op when the template is already on disk (design §6; AC#3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmOutcome {
    Promoted {
        template_path: PathBuf,
        /// True when `<name>.md` was taken by a different shape and the
        /// deterministic `<name>-<sha8>.md` variant was written instead.
        suffixed: bool,
    },
    AlreadyPromoted {
        template_path: PathBuf,
    },
}

/// Result of [`reject_workflow`]: a fresh quarantine, or the idempotent
/// no-op when the signature is already in the rejected ledger (AC#4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectOutcome {
    Rejected { shape_signature: String },
    AlreadyRejected { shape_signature: String },
}

fn io_err(path: &Path, source: std::io::Error) -> InductionError {
    InductionError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Exclusive-flock guard: unlocks on drop, so the critical section is
/// exactly the read-merge-write scope (a panic or process death also
/// releases — the kernel releases the advisory lock).
pub struct InductionLock {
    file: std::fs::File,
}

impl Drop for InductionLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Block until the exclusive `<logs_dir>/workflow-induction.lock` flock is
/// held. Shared by induction runs (Phase 1) and confirm/reject mutations
/// (Phase 2) so neither loses an update (AC#9).
pub fn acquire_induction_lock(logs_dir: &Path) -> Result<InductionLock, InductionError> {
    std::fs::create_dir_all(logs_dir).map_err(|e| io_err(logs_dir, e))?;
    let path = logs_dir.join(LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| io_err(&path, e))?;
    file.lock().map_err(|e| io_err(&path, e))?;
    Ok(InductionLock { file })
}

/// Parse a plan's `spec_json` into its canonical shape signature.
///
/// Discriminator (design §2): only a `"tasks"` array whose entries carry
/// `agent` + `depends_on` is a DAG; the `plan.todos` mint shape
/// (`{"items":[...]}`) is [`SpecShape::TitlesOnly`]. Anything else that
/// cannot yield degrees is a [`ShapeError`] the caller counts as corrupt.
pub fn shape_signature_from_spec_json(spec_json: &str) -> Result<SpecShape, ShapeError> {
    let value: serde_json::Value =
        serde_json::from_str(spec_json).map_err(|e| ShapeError::InvalidJson(e.to_string()))?;
    let Some(object) = value.as_object() else {
        return Err(ShapeError::NotAnObject);
    };
    let Some(tasks_value) = object.get("tasks") else {
        if object.get("items").is_some_and(serde_json::Value::is_array) {
            return Ok(SpecShape::TitlesOnly);
        }
        return Err(ShapeError::MissingTasks);
    };
    let Some(entries) = tasks_value.as_array() else {
        return Err(ShapeError::MalformedTasks);
    };
    if entries.is_empty() {
        return Err(ShapeError::EmptyTasks);
    }

    let mut tasks: Vec<ShapeTask> = Vec::with_capacity(entries.len());
    let mut index: HashMap<String, usize> = HashMap::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        let Some(task) = entry.as_object() else {
            return Err(ShapeError::MalformedTask { index: i });
        };
        let Some(id) = task.get("id").and_then(serde_json::Value::as_str) else {
            return Err(ShapeError::MalformedTask { index: i });
        };
        let Some(agent) = task.get("agent").and_then(serde_json::Value::as_str) else {
            return Err(ShapeError::MalformedTask { index: i });
        };
        // W14.2 (eng-review #2): the tool contract makes `depends_on`
        // optional (`plan_task.rs:457` `depends_on?`) — a MISSING key is
        // the flat/parallel DAG shape, not corruption; only a
        // present-but-non-array key is malformed.
        let depends_on: Vec<String> = match task.get("depends_on") {
            None => Vec::new(),
            Some(serde_json::Value::Array(deps)) => {
                let mut parsed = Vec::with_capacity(deps.len());
                for dep in deps {
                    let Some(dep) = dep.as_str() else {
                        return Err(ShapeError::MalformedTask { index: i });
                    };
                    parsed.push(dep.to_string());
                }
                parsed
            }
            Some(_) => return Err(ShapeError::MalformedTask { index: i }),
        };
        let prompt = task
            .get("prompt")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        if index.insert(id.to_string(), i).is_some() {
            return Err(ShapeError::DuplicateTaskId(id.to_string()));
        }
        tasks.push(ShapeTask {
            id: id.to_string(),
            agent: agent.to_string(),
            in_degree: 0,
            out_degree: 0,
            prompt,
            depends_on,
        });
    }

    // Degrees mirror `plan_task::topological_layers`: in-degree counts each
    // DISTINCT dependency, out-degree counts each task that depends on this
    // one (duplicate `depends_on` entries are one edge).
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); tasks.len()];
    for (i, task) in tasks.iter_mut().enumerate() {
        let mut seen: HashSet<String> = HashSet::new();
        let task_id = task.id.clone();
        let depends_on = task.depends_on.clone();
        for dep in &depends_on {
            if !seen.insert(dep.clone()) {
                continue;
            }
            let Some(&j) = index.get(dep) else {
                return Err(ShapeError::UnknownDependency {
                    task: task_id.clone(),
                    dep: dep.clone(),
                });
            };
            task.in_degree += 1;
            dependents[j].push(i);
        }
    }
    for (j, dependents_of_j) in dependents.iter().enumerate() {
        tasks[j].out_degree = dependents_of_j.len();
    }

    if placed_count(&tasks, &dependents) < tasks.len() {
        return Err(ShapeError::Cycle);
    }

    let mut tuples: Vec<(&str, usize, usize)> = tasks
        .iter()
        .map(|t| (t.agent.as_str(), t.in_degree, t.out_degree))
        .collect();
    tuples.sort_unstable();
    let signature = tuples
        .iter()
        .map(|(agent, in_degree, out_degree)| format!("{agent}:{in_degree},{out_degree}"))
        .collect::<Vec<_>>()
        .join(";");

    Ok(SpecShape::Dag { signature, tasks })
}

/// Kahn reachability count over the deduplicated edges — equals the task
/// count for a DAG.
fn placed_count(tasks: &[ShapeTask], dependents: &[Vec<usize>]) -> usize {
    let mut remaining: Vec<usize> = tasks.iter().map(|t| t.in_degree).collect();
    let mut ready: Vec<usize> = (0..tasks.len()).filter(|&i| remaining[i] == 0).collect();
    let mut placed = 0usize;
    while let Some(i) = ready.pop() {
        placed += 1;
        for &dependent in &dependents[i] {
            remaining[dependent] -= 1;
            if remaining[dependent] == 0 {
                ready.push(dependent);
            }
        }
    }
    placed
}

/// Read the staged drafts. Fail-open by contract (AC#7): missing,
/// unreadable or corrupt file ⇒ empty queue (never an error).
pub fn read_drafts(logs_dir: &Path) -> Vec<WorkflowDraft> {
    let path = logs_dir.join(DRAFTS_FILE);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str(&raw) {
        Ok(drafts) => drafts,
        Err(e) => {
            warn!(error = %e, path = %path.display(), "corrupt workflow induction drafts; treating as empty");
            Vec::new()
        }
    }
}

/// Read the last-run sidecar. Fail-open (AC#7): a missing or corrupt file
/// yields `None`, which the report renders as `{"recorded": false}`.
pub fn read_last_run(logs_dir: &Path) -> Option<InductionRunSummary> {
    let path = logs_dir.join(LAST_RUN_FILE);
    let raw = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str(&raw) {
        Ok(summary) => Some(summary),
        Err(e) => {
            warn!(error = %e, path = %path.display(), "corrupt workflow induction last-run sidecar; ignoring");
            None
        }
    }
}

/// Read the permanently-rejected signatures. Fail-open: a missing or
/// corrupt ledger excludes nothing rather than failing the run.
pub fn read_rejected(logs_dir: &Path) -> Vec<RejectedSignature> {
    let path = logs_dir.join(REJECTED_FILE);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str(&raw) {
        Ok(rejected) => rejected,
        Err(e) => {
            warn!(error = %e, path = %path.display(), "corrupt workflow induction rejected ledger; ignoring");
            Vec::new()
        }
    }
}

/// Promote one staged draft into `<vault_root>/wiki/wisdom/workflow-templates/`.
///
/// Holds the [`acquire_induction_lock`] flock across the whole queue/template
/// read-modify-write (AC#9). A same-name template with different content gets
/// the deterministic `<name>-<sha8>.md` collision suffix; an identical target
/// is a no-op with the queue entry reconciled (AC#3). Unknown names are an
/// [`InductionError::UnknownDraft`] unless a template for them already exists,
/// which is the already-promoted state.
pub async fn confirm_workflow(
    logs_dir: &Path,
    vault_root: &Path,
    draft_name: &str,
    now: i64,
) -> Result<ConfirmOutcome, InductionError> {
    let _lock = acquire_induction_lock(logs_dir)?;
    let mut drafts = read_drafts(logs_dir);
    let Some(pos) = drafts.iter().position(|draft| draft.name == draft_name) else {
        let template_dir = template_dir(vault_root);
        return match promoted_template_path(&template_dir, draft_name) {
            Some(template_path) => {
                warn!(
                    draft = draft_name,
                    path = %template_path.display(),
                    "workflow draft is already promoted; not rewriting"
                );
                Ok(ConfirmOutcome::AlreadyPromoted { template_path })
            }
            None => Err(InductionError::UnknownDraft(draft_name.to_string())),
        };
    };
    let draft = drafts[pos].clone();
    let rendered = render_workflow_template(&draft);

    let template_dir = template_dir(vault_root);
    std::fs::create_dir_all(&template_dir).map_err(|e| io_err(&template_dir, e))?;
    let base = template_dir.join(format!("{}.md", draft.name));
    let (target, suffixed) = if base.exists() {
        let existing = std::fs::read_to_string(&base).unwrap_or_default();
        if existing == rendered {
            drafts.remove(pos);
            write_drafts(logs_dir, &drafts)?;
            warn!(
                draft = draft_name,
                path = %base.display(),
                "workflow template already promoted with identical content; not rewriting"
            );
            return Ok(ConfirmOutcome::AlreadyPromoted {
                template_path: base,
            });
        }
        (
            template_dir.join(format!(
                "{}-{}.md",
                draft.name,
                shape_sha8(&draft.shape_signature)
            )),
            true,
        )
    } else {
        (base, false)
    };

    zen_core::atomic_file::write_atomic(&target, rendered.as_bytes())
        .map_err(|e| io_err(&target, e))?;
    drafts.remove(pos);
    write_drafts(logs_dir, &drafts)?;
    info!(
        draft = %draft.name,
        path = %target.display(),
        suffixed,
        confirmed_at = %utc_timestamp(now),
        "workflow template promoted"
    );
    Ok(ConfirmOutcome::Promoted {
        template_path: target,
        suffixed,
    })
}

/// Reject one staged draft: drop it from the queue and quarantine its shape
/// signature in the rejected ledger (design §6; AC#4).
///
/// Holds the [`acquire_induction_lock`] flock across both read-modify-writes.
/// A signature already recorded in the ledger removes the queue entry but
/// does not duplicate the ledger row.
pub async fn reject_workflow(
    logs_dir: &Path,
    draft_name: &str,
    now: i64,
) -> Result<RejectOutcome, InductionError> {
    let _lock = acquire_induction_lock(logs_dir)?;
    let mut drafts = read_drafts(logs_dir);
    let Some(pos) = drafts.iter().position(|draft| draft.name == draft_name) else {
        return Err(InductionError::UnknownDraft(draft_name.to_string()));
    };
    let draft = drafts[pos].clone();
    let mut rejected = read_rejected_for_update(logs_dir)?;
    drafts.remove(pos);
    write_drafts(logs_dir, &drafts)?;

    if rejected
        .iter()
        .any(|entry| entry.shape_signature == draft.shape_signature)
    {
        warn!(
            draft = draft_name,
            signature = %draft.shape_signature,
            "workflow shape is already quarantined; queue entry only removed"
        );
        return Ok(RejectOutcome::AlreadyRejected {
            shape_signature: draft.shape_signature,
        });
    }
    rejected.push(RejectedSignature {
        shape_signature: draft.shape_signature.clone(),
        rejected_at: utc_timestamp(now),
        source_plan_ids: draft.source_plan_ids,
    });
    write_rejected(logs_dir, &rejected)?;
    info!(
        draft = %draft.name,
        signature = %draft.shape_signature,
        "workflow shape quarantined"
    );
    Ok(RejectOutcome::Rejected {
        shape_signature: draft.shape_signature,
    })
}

fn template_dir(vault_root: &Path) -> PathBuf {
    vault_root.join(WORKFLOW_TEMPLATE_SUBDIR)
}

/// Base template path first, then the deterministic collision-suffixed
/// variant — the on-disk evidence that a draft was already promoted.
fn promoted_template_path(template_dir: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
        return None;
    }
    let base = template_dir.join(format!("{name}.md"));
    if base.is_file() {
        return Some(base);
    }
    let prefix = format!("{name}-");
    for entry in std::fs::read_dir(template_dir).ok()?.flatten() {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if let Some(stem) = file_name.strip_prefix(&prefix)
            && let Some(hex) = stem.strip_suffix(".md")
            && hex.len() == 8
            && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Some(entry.path());
        }
    }
    None
}

/// Mutation-path ledger read: unlike the fail-open [`read_rejected`], a
/// corrupt ledger is an error so a reject never overwrites existing
/// quarantines it could not parse.
fn read_rejected_for_update(logs_dir: &Path) -> Result<Vec<RejectedSignature>, InductionError> {
    let path = logs_dir.join(REJECTED_FILE);
    match std::fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).map_err(InductionError::from),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(io_err(&path, e)),
    }
}

fn write_rejected(logs_dir: &Path, rejected: &[RejectedSignature]) -> Result<(), InductionError> {
    std::fs::create_dir_all(logs_dir).map_err(|e| io_err(logs_dir, e))?;
    let json = serde_json::to_string_pretty(rejected)?;
    let path = logs_dir.join(REJECTED_FILE);
    zen_core::atomic_file::write_atomic(&path, json.as_bytes()).map_err(|e| io_err(&path, e))
}

/// Render the promoted wisdom template (design §6; frontmatter, tasks table
/// and evidence trail, mirroring `render_skill_md`'s shape). Deliberately a
/// pure function of the draft — no confirm-time field — so re-confirmation
/// compares byte-identical and stays a no-op (AC#3).
fn render_workflow_template(draft: &WorkflowDraft) -> String {
    let stats = &draft.success_stats;
    let mut md = String::from("---\n");
    md.push_str("type: workflow-template\n");
    md.push_str(&format!("name: {}\n", yaml_string(&draft.name)));
    md.push_str(&format!(
        "shape_signature: {}\n",
        yaml_string(&draft.shape_signature)
    ));
    md.push_str(&format!("induced_at: {}\n", yaml_string(&draft.induced_at)));
    md.push_str(&format!("cluster_size: {}\n", stats.cluster_size));
    md.push_str(&format!("success_rate: {:.2}\n", stats.success_rate));
    md.push_str(&format!("avg_duration_ms: {}\n", stats.avg_duration_ms));
    md.push_str(&format!("avg_cost_usd: {:.2}\n", stats.avg_cost_usd));
    md.push_str("advisory_only: true\n");
    md.push_str("---\n\n");
    md.push_str(&format!("# {}\n\n", draft.name));

    md.push_str("## Tasks\n\n");
    md.push_str("| id | agent | role | depends_on | typical_prompt_skeleton |\n");
    md.push_str("|---|---|---|---|---|\n");
    for task in &draft.tasks {
        let depends_on = if task.depends_on.is_empty() {
            "—".to_string()
        } else {
            task.depends_on.join(", ")
        };
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            cell(&task.id),
            cell(&task.agent),
            cell(&task.role),
            cell(&depends_on),
            cell(&task.typical_prompt_skeleton),
        ));
    }

    md.push_str("\n## Evidence\n\n");
    for (index, plan_id) in draft.source_plan_ids.iter().enumerate() {
        match draft.source_plan_dates.get(index) {
            Some(date) => md.push_str(&format!("- {} ({})\n", cell(plan_id), cell(date))),
            None => md.push_str(&format!("- {}\n", cell(plan_id))),
        }
    }
    md.push_str(&format!(
        "\ncluster_size: {}\nsuccess_rate: {:.2}\navg_duration_ms: {}\navg_cost_usd: {:.2}\n",
        stats.cluster_size, stats.success_rate, stats.avg_duration_ms, stats.avg_cost_usd,
    ));
    md.push_str(
        "\nThis template is advisory-only: the plan executor never auto-runs it. \
         Cite its shape when drafting a future `plan.execute`.\n",
    );
    md
}

/// JSON string escaping is valid YAML double-quoting, so a signature with
/// `:`/`;` can never change the frontmatter parse.
fn yaml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

/// One table cell: no row breaks, no column separators.
fn cell(value: &str) -> String {
    value
        .replace(['\n', '\r'], " ")
        .replace('|', "\\|")
        .trim()
        .to_string()
}

/// Runs one workflow-induction pass over the live plan corpus.
///
/// The caller supplies the logs directory (worker: `ZenPaths::logs()`) and
/// the state.db client — no global path is resolved here, so tests isolate
/// with plain temp dirs.
pub struct WorkflowInduction {
    logs_dir: PathBuf,
}

impl WorkflowInduction {
    pub fn new(logs_dir: PathBuf) -> Self {
        Self { logs_dir }
    }

    /// Detect shape clusters in the 90-day corpus and overwrite the draft
    /// queue. A zero-corpus run writes ONLY the last-run sidecar (W14.5
    /// amended AC#2): no drafts file — the report can distinguish "never
    /// ran" from "ran, nothing to do". Any non-empty corpus overwrites the
    /// drafts file atomically — including with an empty array when no
    /// cluster forms — under a flock that covers only the quarantine
    /// re-read and the file writes (W14.4: clustering computes outside).
    pub async fn run(
        &self,
        client: &SqliteClient,
        now: i64,
    ) -> Result<InductionRun, InductionError> {
        let cutoff = now - INDUCTION_WINDOW_DAYS * SECS_PER_DAY;
        let rows = WorkflowRepo::new(client).induction_corpus(cutoff).await?;
        if rows.is_empty() {
            // W14.5 (eng-review #10): a zero-corpus run still records the
            // last-run sidecar so `zen discover report` can distinguish
            // "never ran" (no sidecar) from "ran, nothing to do". No
            // drafts file is written.
            let run = InductionRun {
                drafts_written: 0,
                skipped_corrupt: 0,
                skipped_titles_only: 0,
                insufficient: Some(InsufficientReason::NoPlansInWindow),
            };
            let _lock = acquire_induction_lock(&self.logs_dir)?;
            if let Err(e) = write_last_run(&self.logs_dir, &run, now) {
                warn!(error = %e, "workflow induction: last-run sidecar write failed — report stays stale");
            }
            return Ok(run);
        }

        let mut skipped_corrupt = 0usize;
        let mut skipped_titles_only = 0usize;
        let mut incomplete_excluded = 0usize;
        let mut corpus: Vec<CorpusPlan> = Vec::new();
        for row in &rows {
            let (signature, tasks) = match shape_signature_from_spec_json(&row.spec_json) {
                Ok(SpecShape::Dag { signature, tasks }) => (signature, tasks),
                Ok(SpecShape::TitlesOnly) => {
                    skipped_titles_only += 1;
                    continue;
                }
                Err(e) => {
                    debug!(plan = %row.plan_id, error = %e, "workflow induction: corrupt spec_json skipped");
                    skipped_corrupt += 1;
                    continue;
                }
            };
            // W14.3 (eng-review #3/#4): admission honesty — only a plan the
            // engine signed off (`status = 'completed'`) with a FULL settled
            // row set contributes an observation. Partial executions and
            // cancelled rows never induce a shape; an ATTACHED row (both
            // `items` and merged `tasks`) is governed by the status gate
            // alone (its rows are ledger items, not batch tasks).
            let partial_fresh = !has_items_key(&row.spec_json) && row.tasks.len() < tasks.len();
            if row.status != "completed" || !fully_completed(&row.tasks) || partial_fresh {
                incomplete_excluded += 1;
                continue;
            }
            corpus.push(CorpusPlan {
                plan_id: row.plan_id.clone(),
                name: row.name.clone(),
                created_at: row.created_at,
                signature,
                tasks,
                task_rows: row.tasks.clone(),
            });
        }

        let plan_count = rows.len();
        let insufficient = if corpus.is_empty() {
            // W14.5 (eng-review #11): titles-only dominance is its own
            // honest state — not "insufficient data" (which invites waiting
            // for data that cannot arrive) but "no DAG-shaped plans yet".
            if skipped_titles_only > 0 && incomplete_excluded == 0 {
                Some(InsufficientReason::TitlesOnlyCorpus)
            } else if incomplete_excluded > 0 {
                Some(InsufficientReason::NoFullyCompletedPlans)
            } else {
                Some(InsufficientReason::InsufficientCompletedPlans)
            }
        } else if corpus.len() < MIN_OBSERVATIONS {
            Some(InsufficientReason::InsufficientCompletedPlans)
        } else {
            None
        };

        // W14.4 (eng-review #5): clustering runs OUTSIDE the flock — the
        // critical section shrinks to the quarantine re-read plus the two
        // atomic file writes. The re-read closes the pre-lock window: a
        // reject landing while clustering still quarantines this run's
        // drafts (one-run lag otherwise).
        let rejected_pre: HashSet<String> = read_rejected(&self.logs_dir)
            .into_iter()
            .map(|r| r.shape_signature)
            .collect();
        let (mut drafts, cluster_formed) = if insufficient.is_some() {
            (Vec::new(), false)
        } else {
            cluster_drafts(&corpus, &rejected_pre, now)
        };
        let _lock = acquire_induction_lock(&self.logs_dir)?;
        let rejected_now: HashSet<String> = read_rejected(&self.logs_dir)
            .into_iter()
            .map(|r| r.shape_signature)
            .collect();
        drafts.retain(|draft| !rejected_now.contains(&draft.shape_signature));
        write_drafts(&self.logs_dir, &drafts)?;

        let insufficient = if drafts.is_empty() && !cluster_formed && insufficient.is_none() {
            Some(InsufficientReason::NoSimilarCluster)
        } else {
            insufficient
        };

        let run = InductionRun {
            drafts_written: drafts.len(),
            skipped_corrupt,
            skipped_titles_only,
            insufficient,
        };
        if let Err(e) = write_last_run(&self.logs_dir, &run, now) {
            warn!(error = %e, "workflow induction: last-run sidecar write failed — report stays stale");
        }

        debug!(
            plans = plan_count,
            drafts = run.drafts_written,
            skipped_corrupt,
            skipped_titles_only,
            "workflow induction run complete"
        );
        Ok(run)
    }
}

struct CorpusPlan {
    plan_id: String,
    name: Option<String>,
    created_at: i64,
    signature: String,
    tasks: Vec<ShapeTask>,
    task_rows: Vec<InductionTaskRow>,
}

fn fully_completed(tasks: &[InductionTaskRow]) -> bool {
    !tasks.is_empty()
        && tasks
            .iter()
            .all(|t| matches!(t.status.as_str(), "completed" | "skipped"))
}

/// W14.3: an attached plan row carries BOTH `items` (the mint's ledger)
/// and the merged `tasks` (W14.1) — its settled rows are ledger items, so
/// the spec-task-count gate exempts it (the `completed` status gate
/// governs). F12 (W15): parsed-key check, not a substring probe — a task
/// id or any other string VALUE equal to `items` must not fake the key.
fn has_items_key(spec_json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(spec_json)
        .ok()
        .and_then(|v| v.as_object().map(|o| o.contains_key("items")))
        .unwrap_or(false)
}

/// Greedy seed-around-first-plan clustering (skill precipitation
/// precedent). Consumed plans never re-seed — including the members of a
/// rejected cluster, so a human-rejected shape cannot re-emerge through a
/// sibling seed. Returns the drafts and whether ANY qualifying cluster
/// formed (pre-quarantine) for the honest insufficiency state.
fn cluster_drafts(
    corpus: &[CorpusPlan],
    rejected: &HashSet<String>,
    now: i64,
) -> (Vec<WorkflowDraft>, bool) {
    let mut consumed = vec![false; corpus.len()];
    let mut drafts = Vec::new();
    let mut cluster_formed = false;
    for seed in 0..corpus.len() {
        if consumed[seed] {
            continue;
        }
        let members: Vec<usize> = (0..corpus.len())
            .filter(|&idx| {
                !consumed[idx]
                    && trigram_jaccard(&corpus[seed].signature, &corpus[idx].signature)
                        >= OBSERVATION_SIMILARITY
            })
            .collect();
        if members.len() < MIN_OBSERVATIONS {
            continue;
        }
        for &idx in &members {
            consumed[idx] = true;
        }
        cluster_formed = true;
        let signature = corpus[seed].signature.clone();
        if rejected.contains(&signature) {
            debug!(signature = %signature, "workflow induction: rejected signature quarantined");
            continue;
        }
        drafts.push(build_draft(corpus, seed, &members, signature, now));
    }
    (drafts, cluster_formed)
}

fn build_draft(
    corpus: &[CorpusPlan],
    seed: usize,
    members: &[usize],
    signature: String,
    now: i64,
) -> WorkflowDraft {
    let name =
        common_name(corpus, members).unwrap_or_else(|| format!("shape-{}", shape_sha8(&signature)));
    let tasks = corpus[seed]
        .tasks
        .iter()
        .map(|task| DraftTaskTemplate {
            id: task.id.clone(),
            agent: task.agent.clone(),
            role: role_for(task.in_degree, task.out_degree).to_string(),
            typical_prompt_skeleton: median_prompt(corpus, members, task),
            depends_on: task.depends_on.clone(),
        })
        .collect();

    let mut duration_sum = 0u64;
    let mut duration_count = 0u64;
    let mut cost_sum = 0.0f64;
    let mut cost_count = 0u64;
    for &idx in members {
        for row in &corpus[idx].task_rows {
            if let Some(ms) = row.duration_ms {
                duration_sum += u64::try_from(ms).unwrap_or_default();
                duration_count += 1;
            }
            if let Some(cost) = row.cost_usd {
                cost_sum += cost;
                cost_count += 1;
            }
        }
    }

    let completed = members
        .iter()
        .filter(|&&idx| fully_completed(&corpus[idx].task_rows))
        .count();

    WorkflowDraft {
        name,
        shape_signature: signature,
        source_plan_ids: members
            .iter()
            .map(|&idx| corpus[idx].plan_id.clone())
            .collect(),
        source_plan_dates: members
            .iter()
            .map(|&idx| utc_date(corpus[idx].created_at))
            .collect(),
        tasks,
        success_stats: DraftSuccessStats {
            cluster_size: members.len(),
            avg_duration_ms: duration_sum.checked_div(duration_count).unwrap_or(0),
            avg_cost_usd: if cost_count == 0 {
                0.0
            } else {
                cost_sum / cost_count as f64
            },
            success_rate: completed as f64 / members.len() as f64,
        },
        induced_at: utc_timestamp(now),
    }
}

/// Source (in=0), sink (out=0), otherwise intermediate. A lone task is
/// both — source wins (checked first, deterministic).
fn role_for(in_degree: usize, out_degree: usize) -> &'static str {
    if in_degree == 0 {
        "source"
    } else if out_degree == 0 {
        "sink"
    } else {
        "intermediate"
    }
}

/// Literal median prompt for one task slot (design §5 deviation 2): the
/// cluster's prompts for the seed task's `(agent, in_degree, out_degree)`
/// tuple, ordered by length, take the median, truncate at a char boundary.
fn median_prompt(corpus: &[CorpusPlan], members: &[usize], seed_task: &ShapeTask) -> String {
    let mut prompts: Vec<&str> = Vec::new();
    for &idx in members {
        for task in &corpus[idx].tasks {
            if task.agent == seed_task.agent
                && task.in_degree == seed_task.in_degree
                && task.out_degree == seed_task.out_degree
            {
                prompts.push(task.prompt.as_str());
            }
        }
    }
    if prompts.is_empty() {
        prompts.push(seed_task.prompt.as_str());
    }
    prompts.sort_by_key(|prompt| prompt.chars().count());
    truncate_chars(prompts[prompts.len() / 2], PROMPT_SKELETON_MAX_CHARS)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated: String = text.chars().take(max_chars).collect();
    truncated.push_str("...");
    truncated
}

/// Most common plan name in the cluster, slugified; ties break
/// lexicographically so the result is corpus-order independent.
fn common_name(corpus: &[CorpusPlan], members: &[usize]) -> Option<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for &idx in members {
        if let Some(name) = corpus[idx].name.as_deref() {
            let slug = slugify(name);
            if !slug.is_empty() {
                *counts.entry(slug).or_insert(0) += 1;
            }
        }
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map(|(name, _)| name)
}

fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut pending_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(c.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    slug
}

/// First 8 hex of `sha256(signature)` — the `shape-<hash>` draft name and
/// the collision-suffix key (design §5).
pub fn shape_sha8(signature: &str) -> String {
    let digest = Sha256::digest(signature.as_bytes());
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

fn utc_date(epoch_secs: i64) -> String {
    DateTime::<Utc>::from_timestamp(epoch_secs, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn utc_timestamp(epoch_secs: i64) -> String {
    DateTime::<Utc>::from_timestamp(epoch_secs, 0)
        .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Secs, true))
        .unwrap_or_default()
}

fn write_last_run(logs_dir: &Path, run: &InductionRun, now: i64) -> Result<(), InductionError> {
    std::fs::create_dir_all(logs_dir).map_err(|e| io_err(logs_dir, e))?;
    let summary = InductionRunSummary {
        drafts_written: run.drafts_written,
        skipped_corrupt: run.skipped_corrupt,
        skipped_titles_only: run.skipped_titles_only,
        insufficient: run.insufficient.is_some(),
        missing: run
            .insufficient
            .map(InsufficientReason::missing)
            .map(str::to_string),
        induced_at: utc_timestamp(now),
    };
    let json = serde_json::to_string_pretty(&summary)?;
    let path = logs_dir.join(LAST_RUN_FILE);
    zen_core::atomic_file::write_atomic(&path, json.as_bytes()).map_err(|e| io_err(&path, e))
}

fn write_drafts(logs_dir: &Path, drafts: &[WorkflowDraft]) -> Result<(), InductionError> {
    std::fs::create_dir_all(logs_dir).map_err(|e| io_err(logs_dir, e))?;
    let json = serde_json::to_string_pretty(drafts)?;
    let path = logs_dir.join(DRAFTS_FILE);
    zen_core::atomic_file::write_atomic(&path, json.as_bytes()).map_err(|e| io_err(&path, e))
}
