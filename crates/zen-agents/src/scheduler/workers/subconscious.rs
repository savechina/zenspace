use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use chrono::NaiveDate;
use tracing::{debug, info};

use zen_core::paths::ZenPaths;
use zen_memory::journal::Journal;
use zen_memory::memory_service::IdentityContext;

use super::super::{WorkerContext, WorkerReport, ZenWorker};

#[derive(Debug, Clone)]
pub enum MicroAction {
    Remind(String),
    Suggest(String),
    Log(String),
    Organize(String),
}

pub struct SubconsciousWorker {
    scheduled: Option<&'static str>,
}

impl SubconsciousWorker {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

#[async_trait::async_trait]
impl ZenWorker for SubconsciousWorker {
    fn id(&self) -> &'static str {
          "subconscious"
      }

     /// Human-readable description of what this worker does.
    fn description(&self) -> &'static str {
          "Evaluate workspace state, log micro-actions (remind, suggest, organize)"
      }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 */5 * * * *")
      }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;
        let today = chrono::Utc::now().naive_utc().date();

        let actions = evaluate_tick(&paths, today)?;

        if !actions.is_empty() {
            append_subconscious_log(&paths, &actions)?;
            info!("subconscious tick: {} actions decided", actions.len());
         }

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: actions.len(),
            duration_ms: start.elapsed().as_millis() as u64,
         })
     }
}

fn evaluate_tick(zen_paths: &ZenPaths, date: NaiveDate) -> Result<Vec<MicroAction>> {
    let mut actions = Vec::new();

    let today_entries = Journal::read_entries(zen_paths, date)?;

    if today_entries.is_empty() {
        actions.push(MicroAction::Remind(
             "No log entries today. Consider recording your activities.".to_string(),
         ));
     }

    match load_identity(zen_paths) {
        Ok(ctx) if ctx.has_content() => {
            actions.push(MicroAction::Log(format!(
                 "Identity loaded: {} files",
                ctx.file_count()
             )));
         }
        Ok(_) => {
            actions.push(MicroAction::Suggest(
                 "No identity context found. Consider creating SOUL.md / MEMORY.md.".to_string(),
             ));
         }
        Err(e) => {
            actions.push(MicroAction::Log(format!("Identity load note: {e}")));
         }
     }

    let pending_notes = count_inbox_notes(zen_paths);
    if pending_notes > 0 {
        actions.push(MicroAction::Organize(format!(
             "{pending_notes} inbox notes are pending consolidation"
         )));
     }

    actions.push(MicroAction::Log(format!(
         "Tick complete at {date}: {} log entries, {pending_notes} inbox notes",
        today_entries.len()
     )));

    Ok(actions)
}

fn load_identity(zen_paths: &ZenPaths) -> Result<IdentityContext> {
    zen_memory::memory_service::load_all(zen_paths)
}

fn count_inbox_notes(zen_paths: &ZenPaths) -> usize {
    let inbox = zen_paths.inbox();
    if !inbox.is_dir() {
        return 0;
     }
    std::fs::read_dir(&inbox)
         .map(|entries| {
            entries
                 .filter_map(|e| e.ok())
                 .filter(|e| {
                    e.path()
                         .extension()
                         .is_some_and(|ext| ext == "md" || ext == "txt")
                 })
                 .count()
         })
         .unwrap_or(0)
}

fn subconscious_log_path(zen_paths: &ZenPaths) -> PathBuf {
    zen_paths.logs().join("subconscious.md")
}

fn append_subconscious_log(zen_paths: &ZenPaths, actions: &[MicroAction]) -> Result<()> {
    let path = subconscious_log_path(zen_paths);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
     }

    let now = chrono::Utc::now().naive_utc();
    let header = format!(
         "\n## {} — Subconscious Tick\n\n",
        now.format("%Y-%m-%d %H:%M:%S")
     );

    let body: String = actions
         .iter()
         .map(|a| match a {
            MicroAction::Remind(msg) => format!("- **Remind**: {msg}"),
            MicroAction::Suggest(msg) => format!("- **Suggest**: {msg}"),
            MicroAction::Log(msg) => format!("- **Log**: {msg}"),
            MicroAction::Organize(msg) => format!("- **Organize**: {msg}"),
         })
         .collect::<Vec<_>>()
         .join("\n");

    let mut file = std::fs::OpenOptions::new()
         .create(true)
         .append(true)
         .open(&path)?;
    writeln!(file, "{header}{body}")?;

    debug!("subconscious log appended to {}", path.display());
    Ok(())
}
