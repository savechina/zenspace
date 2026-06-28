use std::fs::read_to_string;

use anyhow::Result;
use futures::stream::StreamExt;
use rig_compose::ContextPackConfig;
use rig_compose::agent::{Agent, GenericAgent};
use rig_compose::context::{Evidence, InvestigationContext, Signal};
use rig_core::completion::CompletionModel;
use rig_core::streaming::StreamedAssistantContent;
use rig_memvid::{CardSelection, MemoryCardContext};
use serde_json::json;
use tracing::{instrument, warn};
use zen_core::paths::ZenPaths;
use zen_core::types::SessionContext;
use zen_provider::DefaultRouter;

use crate::completion_model::ZenCompletionModel;
pub use crate::wiring::ZenWiring;

/// Context loaded from identity files: SOUL.md, AGENTS.md, MEMORY.md.
///
/// Re-exported from `zen_memory::IdentityContext` (canonical type).
/// Field access via `.soul_content()`, `.agents_content()`, `.memory_content()`
/// which return `&str` (empty when the corresponding file is absent).
pub use zen_memory::IdentityContext;

/// Self-learning signals loaded from memory stores, injected into agent prompts.
///
/// Each field is a pre-formatted string suitable for `PromptAssembly` injection.
/// Empty string means "no data available" — the section will be skipped.
#[derive(Debug, Clone, Default)]
pub struct SelfLearningSignals {
    /// Loss-aversion guard: recent corrections to avoid repeating mistakes.
    pub corrections: String,
    /// Quality-filtered feedback archive.
    pub feedback: String,
    /// Top-5 low-confidence beliefs needing evidence.
    pub beliefs: String,
    /// Weekly virtue tracking (三省吾身).
    pub virtue_logs: String,
    /// Daily prompt-injected reflections from wiki.
    pub reflections: String,
    /// Relevant mental models from wiki.
    pub mental_models: String,
    /// Decisions under review.
    pub decisions: String,
    /// Priority scoring from beliefs × commitments.
    pub priority_items: String,
}

impl SelfLearningSignals {
    /// Load all self-learning signals from ZenPaths vault/memory directories.
    ///
    /// Degrades gracefully — missing files or parse errors yield empty strings.
    /// Never panics; uses `tracing::warn` for errors.
    ///
    /// Also wires `ReinforcementTracker` to record retrieval hit-counts for
    /// each loaded entity (§8.3.3 reinforcement mechanism).
    pub fn load(zen_paths: &ZenPaths) -> Self {
        use std::path::PathBuf;
        use zen_memory::priority::ReinforcementTracker;

        let wiki_dir = zen_paths.wiki();
        let memories_dir = zen_paths.vault().join("memories");
        let reinforcement_path: PathBuf =
            zen_paths.global_root().join("memories/.reinforcement.json");
        let mut tracker = ReinforcementTracker::new(reinforcement_path);

        let corrections = load_corrections(&wiki_dir.join("wisdom/corrections"), &mut tracker);
        let feedback = load_feedback(&wiki_dir.join("wisdom/feedback"), &mut tracker);
        let beliefs = load_beliefs(&wiki_dir.join("wisdom/beliefs"), &mut tracker);
        let virtue_logs = load_virtue_logs(&memories_dir.join("virtue_logs"));
        let reflections = load_reflections(&wiki_dir.join("wisdom/reflections"));
        let mental_models = load_mental_models(&wiki_dir.join("wisdom/models"));
        let decisions = load_decisions(&wiki_dir.join("wisdom/decisions"), &mut tracker);
        let priority_items = load_priority_items(
            &wiki_dir.join("wisdom/beliefs"),
            &memories_dir.join("commitments"),
        );

        let _ = tracker.save();

        Self {
            corrections,
            feedback,
            beliefs,
            virtue_logs,
            reflections,
            mental_models,
            decisions,
            priority_items,
        }
    }

    /// Returns true if all signal fields are empty.
    pub fn is_empty(&self) -> bool {
        self.corrections.is_empty()
            && self.feedback.is_empty()
            && self.beliefs.is_empty()
            && self.virtue_logs.is_empty()
            && self.reflections.is_empty()
            && self.mental_models.is_empty()
            && self.decisions.is_empty()
            && self.priority_items.is_empty()
    }
}

fn load_corrections(
    dir: &std::path::Path,
    tracker: &mut zen_memory::priority::ReinforcementTracker,
) -> String {
    let corrections = match zen_memory::Correction::load_all(dir) {
        Ok(c) => c,
        Err(e) => {
            warn!(dir = %dir.display(), error = %e, "failed to load corrections");
            return String::new();
        }
    };

    if corrections.is_empty() {
        return String::new();
    }

    for c in &corrections {
        let _ = tracker.record_retrieval(&c.id);
    }

    let mut sorted = corrections;
    sorted.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    let top: Vec<_> = sorted.into_iter().take(3).collect();

    let mut out = String::from("⚠️ Past errors to avoid:\n");
    for c in &top {
        let cost_info = if c.cost.economic > 0.0 || c.cost.time_hours > 0.0 {
            format!(", cost: ${:.0}/{}h", c.cost.economic, c.cost.time_hours)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "- \"{}\" → fix: {}{}\n",
            c.error_ref, c.fix, cost_info
        ));
    }
    out
}

fn load_feedback(
    dir: &std::path::Path,
    tracker: &mut zen_memory::priority::ReinforcementTracker,
) -> String {
    let feedbacks = match zen_memory::Feedback::load_all(dir) {
        Ok(f) => f,
        Err(e) => {
            warn!(dir = %dir.display(), error = %e, "failed to load feedback");
            return String::new();
        }
    };

    if feedbacks.is_empty() {
        return String::new();
    }

    for f in &feedbacks {
        let _ = tracker.record_retrieval(&f.id);
    }

    let mut sorted = feedbacks;
    sorted.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    let top: Vec<_> = sorted.into_iter().take(3).collect();

    let mut out = String::from("📋 Recent feedback:\n");
    for f in &top {
        out.push_str(&format!(
            "- [{}] {}: \"{}\"\n",
            f.disposition, f.source, f.content
        ));
    }
    out
}

fn load_beliefs(
    dir: &std::path::Path,
    tracker: &mut zen_memory::priority::ReinforcementTracker,
) -> String {
    let mut beliefs = match zen_memory::Belief::load_all(dir) {
        Ok(b) => b,
        Err(e) => {
            warn!(dir = %dir.display(), error = %e, "failed to load beliefs");
            return String::new();
        }
    };

    if beliefs.is_empty() {
        return String::new();
    }

    let now = chrono::Utc::now();
    let pruned = zen_memory::priority::prune_beliefs(&mut beliefs, now);
    if !pruned.is_empty() {
        tracing::debug!(count = pruned.len(), "pruned stale low-confidence beliefs");
    }

    let reinforced_ids: std::collections::HashSet<String> = {
        let reinforced = zen_memory::priority::reinforce_beliefs(&beliefs, tracker);
        reinforced.into_iter().map(|b| b.id.clone()).collect()
    };

    for b in &mut beliefs {
        let _ = tracker.record_retrieval(&b.id);
        b.reinforce();
    }

    for b in &beliefs {
        if let Err(e) = b.save(dir) {
            warn!(belief_id = %b.id, error = %e, "failed to save reinforced belief");
        }
    }

    let mut sorted = beliefs;
    sorted.sort_by(|a, b| {
        a.posterior
            .partial_cmp(&b.posterior)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let top: Vec<_> = sorted.into_iter().take(5).collect();

    let mut out = String::from("🔍 Low-confidence beliefs (need evidence):\n");
    for b in &top {
        let marker = if reinforced_ids.contains(b.id.as_str()) {
            " [REINFORCED]"
        } else {
            ""
        };
        out.push_str(&format!(
            "- \"{}\"{} (confidence: {:.0}%)\n",
            b.proposition,
            marker,
            b.posterior * 100.0
        ));
    }
    out
}

fn load_virtue_logs(dir: &std::path::Path) -> String {
    let logs = match zen_memory::VirtueLog::load_all(dir) {
        Ok(l) => l,
        Err(e) => {
            warn!(dir = %dir.display(), error = %e, "failed to load virtue logs");
            return String::new();
        }
    };

    if logs.is_empty() {
        return String::new();
    }

    use std::collections::HashMap;
    let mut latest: HashMap<zen_memory::VirtueDomain, &zen_memory::VirtueLog> = HashMap::new();
    for log in &logs {
        let entry = latest.entry(log.virtue).or_insert(log);
        if log.date > entry.date {
            *entry = log;
        }
    }

    let mut out = String::from("🧘 Virtue tracking:\n");
    for (domain, log) in &latest {
        out.push_str(&format!(
            "- {}: {} (streak: {} days)\n",
            domain, log.status, log.streak
        ));
    }
    out
}

fn load_reflections(dir: &std::path::Path) -> String {
    if !dir.is_dir() {
        return String::new();
    }

    let mut files: Vec<_> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect(),
        Err(e) => {
            warn!(dir = %dir.display(), error = %e, "failed to read reflections dir");
            return String::new();
        }
    };

    files.sort_by(|a, b| {
        let ta = a
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let tb = b
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        tb.cmp(&ta)
    });

    let top: Vec<_> = files.into_iter().take(3).collect();
    if top.is_empty() {
        return String::new();
    }

    let mut out = String::from("📝 Recent reflections:\n");
    for entry in &top {
        let path = entry.path();
        if let Ok(content) = std::fs::read_to_string(&path) {
            let first_para = extract_first_paragraph(&content);
            if !first_para.is_empty() {
                let title = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("reflection");
                out.push_str(&format!("- **{}**: {}\n", title, first_para));
            }
        }
    }
    out
}

fn load_mental_models(dir: &std::path::Path) -> String {
    if !dir.is_dir() {
        return String::new();
    }

    let files: Vec<_> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect(),
        Err(e) => {
            warn!(dir = %dir.display(), error = %e, "failed to read mental models dir");
            return String::new();
        }
    };

    if files.is_empty() {
        return String::new();
    }

    let mut out = String::from("🧠 Mental models:\n");
    for entry in &files {
        let path = entry.path();
        if let Ok(content) = std::fs::read_to_string(&path) {
            let first_para = extract_first_paragraph(&content);
            if !first_para.is_empty() {
                let title = path.file_stem().and_then(|s| s.to_str()).unwrap_or("model");
                out.push_str(&format!("- **{}**: {}\n", title, first_para));
            }
        }
    }
    out
}

fn load_decisions(
    dir: &std::path::Path,
    tracker: &mut zen_memory::priority::ReinforcementTracker,
) -> String {
    let decisions = match zen_memory::Decision::load_all(dir) {
        Ok(d) => d,
        Err(e) => {
            warn!(dir = %dir.display(), error = %e, "failed to load decisions");
            return String::new();
        }
    };

    if decisions.is_empty() {
        return String::new();
    }

    for d in &decisions {
        let _ = tracker.record_retrieval(&d.id);
    }

    let mut open: Vec<_> = decisions
        .into_iter()
        .filter(|d| d.closed_at.is_none())
        .collect();
    open.sort_by_key(|b| std::cmp::Reverse(b.decided_at));
    let top: Vec<_> = open.into_iter().take(3).collect();

    if top.is_empty() {
        return String::new();
    }

    let mut out = String::from("⚡ Decisions under review:\n");
    for d in &top {
        out.push_str(&format!(
            "- \"{}\" (decided: {}, domain: {})\n",
            d.title,
            d.decided_at.format("%Y-%m-%d"),
            d.domain
        ));
    }
    out
}

fn load_priority_items(beliefs_dir: &std::path::Path, commitments_dir: &std::path::Path) -> String {
    let beliefs = match zen_memory::Belief::load_all(beliefs_dir) {
        Ok(b) => b,
        Err(e) => {
            warn!(dir = %beliefs_dir.display(), error = %e, "failed to load beliefs for priority");
            return String::new();
        }
    };
    let commitments = match zen_memory::Commitment::load_all(commitments_dir) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                dir = %commitments_dir.display(),
                error = %e,
                "failed to load commitments for priority"
            );
            return String::new();
        }
    };

    let scores = zen_memory::priority::top_n_by_priority(&beliefs, &commitments, 5);
    zen_memory::priority::format_priority_for_prompt(&scores)
}

fn extract_first_paragraph(content: &str) -> String {
    let mut lines = content.lines();
    let mut paragraph = String::new();
    let mut found_first = false;
    let mut in_frontmatter = false;

    for line in lines.by_ref() {
        let trimmed = line.trim();

        if !found_first && trimmed == "---" {
            if in_frontmatter {
                in_frontmatter = false;
                continue;
            }
            in_frontmatter = true;
            continue;
        }

        if in_frontmatter {
            continue;
        }

        if !found_first && trimmed.is_empty() {
            continue;
        }

        if !found_first && trimmed.starts_with('#') {
            continue;
        }

        found_first = true;

        if trimmed.is_empty() {
            break;
        }

        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(trimmed);
    }

    paragraph.chars().take(200).collect()
}

/// Load identity files from the Zen home directory (~/.zen/).
///
/// Each file is optional — missing or unreadable files yield `None`
/// with a warning logged. Falls back to `<workspace>/AGENTS.md` if
/// the identity-dir copy is absent.
pub fn load_identity_files(zen_paths: &ZenPaths) -> IdentityContext {
    let identity_dir = zen_paths.identity();

    let soul_path = identity_dir.join("SOUL.md");
    let soul = read_to_string(&soul_path)
        .map_err(|e| {
            tracing::warn!(path = ?soul_path, error = %e, "SOUL.md not found or unreadable");
            e
        })
        .ok();

    let agents_path = identity_dir.join("AGENTS.md");
    let agents = read_to_string(&agents_path).ok().or_else(|| {
        if let Some(ws) = zen_paths.workspace_root() {
            let ws_agents = ws.join("AGENTS.md");
            match read_to_string(&ws_agents) {
                Ok(content) => Some(content),
                Err(e) => {
                    tracing::warn!(path = ?ws_agents, error = %e, "AGENTS.md not found in workspace root either");
                    None
                }
            }
        } else {
            tracing::warn!(path = ?agents_path, "AGENTS.md not found (no workspace root detected)");
            None
        }
    });

    let memory_path = identity_dir.join("MEMORY.md");
    let memory = read_to_string(&memory_path)
        .map_err(|e| {
            tracing::warn!(path = ?memory_path, error = %e, "MEMORY.md not found or unreadable");
            e
        })
        .ok();

    IdentityContext {
        soul,
        memory,
        agents,
    }
}

/// A Zen-tailored agent combining rig_compose's skill-driver [`GenericAgent`]
/// with a [`ZenCompletionModel`] for direct LLM routing.
pub struct ZenAgent {
    pub generic: GenericAgent,
    pub completion_model: ZenCompletionModel,
    identity: Option<IdentityContext>,
    signals: Option<SelfLearningSignals>,
    memvid_store: Option<rig_memvid::MemvidStore>,
}

impl ZenAgent {
    /// Create a [`ZenAgentBuilder`] for this agent name.
    pub fn builder(name: &str) -> ZenAgentBuilder {
        ZenAgentBuilder::new(name)
    }

    /// Access the agent's identity context (SOUL.md/MEMORY.md/AGENTS.md).
    pub fn identity(&self) -> &Option<IdentityContext> {
        &self.identity
    }

    /// Access loaded self-learning signals (if any).
    pub fn signals(&self) -> &Option<SelfLearningSignals> {
        &self.signals
    }

    fn push_self_learning_evidence(&self, ctx: &mut InvestigationContext) {
        let Some(ref signals) = self.signals else {
            return;
        };
        if signals.is_empty() {
            return;
        }

        let mut pushed = 0;
        if !signals.corrections.is_empty() {
            ctx.evidence.push(
                Evidence::new("self-learning", "corrections")
                    .with_detail(json!({ "summary": signals.corrections })),
            );
            pushed += 1;
        }
        if !signals.feedback.is_empty() {
            ctx.evidence.push(
                Evidence::new("self-learning", "feedback")
                    .with_detail(json!({ "summary": signals.feedback })),
            );
            pushed += 1;
        }
        if !signals.beliefs.is_empty() {
            ctx.evidence.push(
                Evidence::new("self-learning", "beliefs")
                    .with_detail(json!({ "summary": signals.beliefs })),
            );
            pushed += 1;
        }
        if !signals.virtue_logs.is_empty() {
            ctx.evidence.push(
                Evidence::new("self-learning", "virtue-logs")
                    .with_detail(json!({ "summary": signals.virtue_logs })),
            );
            pushed += 1;
        }
        if !signals.reflections.is_empty() {
            ctx.evidence.push(
                Evidence::new("self-learning", "reflections")
                    .with_detail(json!({ "summary": signals.reflections })),
            );
            pushed += 1;
        }
        if !signals.mental_models.is_empty() {
            ctx.evidence.push(
                Evidence::new("self-learning", "mental-models")
                    .with_detail(json!({ "summary": signals.mental_models })),
            );
            pushed += 1;
        }
        if !signals.decisions.is_empty() {
            ctx.evidence.push(
                Evidence::new("self-learning", "decisions")
                    .with_detail(json!({ "summary": signals.decisions })),
            );
            pushed += 1;
        }
        if !signals.priority_items.is_empty() {
            ctx.evidence.push(
                Evidence::new("self-learning", "priority-items")
                    .with_detail(json!({ "summary": signals.priority_items })),
            );
            pushed += 1;
        }

        if pushed > 0 {
            tracing::info!(
                signal_types = pushed,
                "Self-learning signals injected as evidence"
            );
        }
    }

    fn push_conversation_evidence(&self, ctx: &mut InvestigationContext, session: &SessionContext) {
        if session.conversation.is_empty() {
            return;
        }
        let history: String = session
            .conversation
            .iter()
            .map(|turn| format!("[{}] {}", turn.role, turn.content))
            .collect::<Vec<_>>()
            .join("\n\n");
        ctx.evidence.push(
            Evidence::new("conversation", "session").with_detail(json!({ "summary": history })),
        );
    }

    /// Retrieve memories from the memvid store for this session.
    ///
    /// Uses per-session scoping (D7): the session_id from SessionContext
    /// isolates each conversation's memory namespace.
    #[instrument(skip(self), fields(session_id))]
    fn retrieve_memories(&self, session_id: &str) -> Option<Vec<String>> {
        self.memvid_store.as_ref().and_then(|store| {
            let mut all_cards = Vec::new();

            if let Ok(session_cards) = store.entity_memories(session_id) {
                all_cards.extend(session_cards);
            }

            if let Ok(user_cards) = store.entity_memories("user") {
                all_cards.extend(user_cards);
            }

            if all_cards.is_empty() {
                tracing::debug!(session_id, "No memories retrieved");
                return None;
            }

            tracing::info!(
                session_id,
                count = all_cards.len(),
                "Memories retrieved (session + user)"
            );
            Some(
                all_cards
                    .into_iter()
                    .filter(|c| {
                        c.confidence.unwrap_or(1.0) >= zen_memory::memvid::TRIPLET_MIN_CONFIDENCE
                    })
                    .map(|c| format!("[{}] {}={}: {}", c.kind, c.entity, c.slot, c.value))
                    .collect(),
            )
        })
    }

    #[instrument(skip(self), fields(session_id, query_len = query.len()))]
    fn retrieve_memories_structured(&self, session_id: &str, query: &str) -> Option<Vec<String>> {
        self.memvid_store.as_ref().and_then(|store| {
            let ctx = MemoryCardContext::new(
                store.clone(),
                CardSelection::ForPrincipal(session_id.to_string()),
            );

            match ctx.select(query) {
                Ok(cards) if !cards.is_empty() => {
                    tracing::info!(
                        session_id,
                        count = cards.len(),
                        "Structured memory cards retrieved"
                    );
                    Some(
                        cards
                            .into_iter()
                            .filter(|c| {
                                c.confidence.unwrap_or(1.0)
                                    >= zen_memory::memvid::TRIPLET_MIN_CONFIDENCE
                            })
                            .map(|c| format!("[{}] {}={}: {}", c.kind, c.entity, c.slot, c.value))
                            .collect(),
                    )
                }
                Ok(_) => {
                    tracing::debug!(session_id, "No structured cards found");
                    None
                }
                Err(e) => {
                    tracing::warn!(session_id, error = %e, "Failed structured memory retrieval");
                    None
                }
            }
        })
    }

    /// Persist a conversation turn to the memvid store with per-session scoping.
    ///
    /// Replaces the former inline `put_text()` calls. The orchestrator calls
    /// this after execution completes, keeping the write concern at the
    /// orchestrator level (D2). Uses `uri = session_id` for scope isolation
    /// (D7) and `extract_triplets(false)` for Phase 1 (D9).
    #[instrument(skip(self), fields(session_id, response_len = assistant_response.len()))]
    pub fn persist_turn(&self, session_id: &str, user_query: &str, assistant_response: &str) {
        if let Some(ref store) = self.memvid_store {
            let zen_store = zen_memory::memvid::ZenMemvidStore::from_store(store.clone());
            let content = format!("User: {user_query}\nAssistant: {assistant_response}");

            if let Err(e) = zen_store.persist_structured_turn(session_id, "user", &content) {
                tracing::warn!(session_id, error = %e, "Failed to persist turn to memvid");
            }
        }
    }

    /// Execute a user query. Async-native, no nested runtime creation.
    #[instrument(skip(self, session), fields(session_id = %session.session_id, query_len = query.len()))]
    pub async fn execute(&self, query: &str, session: &mut SessionContext) -> Result<String> {
        let session_id = session.session_id.to_string();
        let mut ctx = InvestigationContext::new(&session_id, "query");

        ctx.evidence
            .push(Evidence::new("user-input", "query").with_detail(json!({ "summary": query })));

        if let Some(ref identity) = self.identity {
            ctx.evidence.push(
                Evidence::new("identity", "soul")
                    .with_detail(json!({ "summary": identity.soul_content() })),
            );
            ctx.evidence.push(
                Evidence::new("identity", "agents")
                    .with_detail(json!({ "summary": identity.agents_content() })),
            );
            ctx.evidence.push(
                Evidence::new("identity", "memory")
                    .with_detail(json!({ "summary": identity.memory_content() })),
            );
        }

        for note in &session.knowledge {
            ctx.evidence.push(
                Evidence::new("knowledge", "wiki")
                    .with_detail(json!({ "summary": note.content, "path": note.path })),
            );
        }

        self.push_conversation_evidence(&mut ctx, session);

        let memories = self
            .retrieve_memories_structured(&session_id, query)
            .or_else(|| self.retrieve_memories(&session_id));

        if let Some(ref memories) = memories {
            let memory_text = memories.join("\n");
            ctx.evidence.push(
                Evidence::new("retrieved-memory", "memvid")
                    .with_detail(json!({ "summary": memory_text })),
            );
        }

        self.push_self_learning_evidence(&mut ctx);

        ctx.signals.push(Signal::new("knowledge-query"));

        let step_result = self.generic.step(&mut ctx).await?;

        tracing::info!(
            skills_run = ?step_result.skills_run,
            confidence = step_result.confidence,
            concluded = step_result.concluded,
            "ZenAgent::execute: skills completed"
        );

        let system_prompt = self.build_system_prompt_with_assembly(session, memories.as_deref());
        let dynamic_context = self.build_prompt(query, &ctx);
        let user_message = if dynamic_context.is_empty() || dynamic_context == query {
            query.to_string()
        } else {
            format!(
                "## Retrieved Context\n\n{}\n\n## Current Query\n{}",
                dynamic_context, query
            )
        };

        let response = self
            .call_llm_with_assembly(query, &system_prompt, &user_message, session)
            .await?;

        tracing::info!(
            response_len = response.len(),
            "ZenAgent::execute: LLM response received"
        );

        session.add_turn("user", query);
        session.add_turn("assistant", &response);

        Ok(response)
    }

    fn tier_score(source_skill: &str, label: &str) -> f64 {
        match (source_skill, label) {
            ("user-input", _) => 1.00,
            ("conversation", _) => 0.98,
            ("identity", _) => 0.95,
            ("self-learning", _) => 0.85,
            ("retrieved-memory", _) => 0.80,
            ("knowledge", _) => 0.70,
            _ => 0.50,
        }
    }

    fn tier_score_from_source_id(source_id: &str) -> f64 {
        let (skill, label) = source_id.split_once('/').unwrap_or(("_", "_"));
        Self::tier_score(skill, label)
    }

    fn build_system_prompt_with_assembly(
        &self,
        session: &SessionContext,
        memories: Option<&[String]>,
    ) -> String {
        use zen_memory::PromptAssembly;

        let mut builder = PromptAssembly::builder().sensitivity(session.sensitivity_policy);

        if let Some(ref identity) = self.identity {
            if !identity.soul_content().is_empty() {
                builder = builder.intro(identity.soul_content());
            }
            if !identity.agents_content().is_empty() {
                builder = builder.claude_md(identity.agents_content());
            }
        }

        let mut knowledge: Vec<String> = session
            .knowledge
            .iter()
            .map(|n| n.content.clone())
            .collect();

        if let Some(memories) = memories {
            let memory_text = memories.join("\n");
            if !memory_text.is_empty() {
                knowledge.push(format!("## Retrieved Memories (Memvid)\n{}", memory_text));
            }
        }

        let history: Vec<(String, String)> = session
            .conversation
            .iter()
            .map(|turn| (turn.role.clone(), turn.content.clone()))
            .collect();

        builder = builder.memory_section(knowledge, history);
        builder = builder.env_info(PromptAssembly::build_env_info(session));
        builder = builder.blast_radius(session.sensitivity_policy);

        if let Some(ref signals) = self.signals {
            if !signals.corrections.is_empty() {
                builder = builder.corrections(&signals.corrections);
            }
            if !signals.feedback.is_empty() {
                builder = builder.feedback(&signals.feedback);
            }
            if !signals.beliefs.is_empty() {
                builder = builder.beliefs(&signals.beliefs);
            }
            if !signals.virtue_logs.is_empty() {
                builder = builder.virtue_logs(&signals.virtue_logs);
            }
            if !signals.reflections.is_empty() {
                builder = builder.reflections(&signals.reflections);
            }
            if !signals.mental_models.is_empty() {
                builder = builder.mental_models(&signals.mental_models);
            }
            if !signals.decisions.is_empty() {
                builder = builder.decisions(&signals.decisions);
            }
            if !signals.priority_items.is_empty() {
                builder = builder.priority_items(&signals.priority_items);
            }
        }

        let prompt = builder.build().assemble();

        tracing::info!(
            prompt_len = prompt.len(),
            has_cache_boundary = prompt.contains(zen_memory::SYSTEM_PROMPT_DYNAMIC_BOUNDARY),
            "build_system_prompt_with_assembly: assembled PromptAssembly"
        );

        prompt
    }

    fn build_prompt(&self, query: &str, ctx: &InvestigationContext) -> String {
        let mut items = rig_resources::projection::evidence_to_context_items(ctx);

        items.sort_by(|a, b| {
            let ta = Self::tier_score_from_source_id(&a.source_id);
            let tb = Self::tier_score_from_source_id(&b.source_id);
            tb.partial_cmp(&ta).unwrap_or(std::cmp::Ordering::Equal)
        });
        for (rank, item) in items.iter_mut().enumerate() {
            item.rank = rank;
        }

        let config = ContextPackConfig::new(12288)
            .with_max_items(30)
            .with_reserve_chars(query.chars().count());

        let pack = rig_resources::projection::pack_resource_context(items, config);

        tracing::info!(
            selected = pack.selected.len(),
            omitted = pack.omitted.len(),
            "Context pack: selected {} items, omitted {}",
            pack.selected.len(),
            pack.omitted.len()
        );

        if !pack.omitted.is_empty() {
            for omitted in &pack.omitted {
                tracing::debug!(
                    source_id = %omitted.item.source_id,
                    reason = ?omitted.reason,
                    "Context item omitted from prompt"
                );
            }
        }

        pack.selected
            .iter()
            .map(|i| i.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    #[instrument(skip(self, system_prompt, user_message, session), fields(session_id = %session.session_id, query_len = query.len()))]
    async fn call_llm_with_assembly(
        &self,
        query: &str,
        system_prompt: &str,
        user_message: &str,
        session: &SessionContext,
    ) -> Result<String> {
        use rig_core::completion::CompletionRequest;
        use rig_core::message::Message;
        use rig_core::OneOrMany;
        use std::time::Instant;

        let messages_in = session.conversation.len() + 1;
        let model_name = self.completion_model.provider_name();
        let conversation_id = session.session_id.to_string();
        let start = Instant::now();

        crate::observability::emit_prompt_started(model_name, &conversation_id, messages_in);

        let mut history_messages: Vec<Message> = session
            .conversation
            .iter()
            .filter_map(|turn| match turn.role.as_str() {
                "user" => Some(Message::user(&turn.content)),
                "assistant" => Some(Message::assistant(&turn.content)),
                _ => None,
            })
            .collect();

        history_messages.push(Message::user(user_message));

        let chat_history = OneOrMany::many(history_messages)
            .unwrap_or_else(|_| OneOrMany::one(Message::user(user_message)));

        let request = CompletionRequest {
            model: None,
            preamble: Some(system_prompt.to_string()),
            chat_history,
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(2048),
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let result = self.completion_model.completion(request).await;
        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(response) => {
                crate::observability::emit_prompt_completed(
                    model_name,
                    &conversation_id,
                    Some(response.usage.input_tokens),
                    Some(response.usage.output_tokens),
                    Some(duration_ms),
                );
                match response.choice.first() {
                    rig_core::completion::AssistantContent::Text(t) => Ok(t.text.clone()),
                    other => Ok(format!("{other:?}")),
                }
            }
            Err(e) => {
                crate::observability::emit_prompt_failed(
                    model_name,
                    &conversation_id,
                    &e.to_string(),
                );
                Err(e.into())
            }
        }
    }

    #[instrument(skip(self, session, on_token), fields(session_id = %session.session_id, query_len = query.len()))]
    pub async fn execute_stream(
        &self,
        query: &str,
        session: &mut SessionContext,
        on_token: impl FnMut(&str),
    ) -> Result<String> {
        let session_id = session.session_id.to_string();
        let conv_len = session.conversation.len();
        tracing::info!(
            session_id = %session_id,
            conversation_turns = conv_len,
            "execute_stream: starting with PromptAssembly + rig projection merge"
        );

        let mut ctx = InvestigationContext::new(&session_id, "query");

        ctx.evidence
            .push(Evidence::new("user-input", "query").with_detail(json!({ "summary": query })));

        if let Some(ref identity) = self.identity {
            ctx.evidence.push(
                Evidence::new("identity", "soul")
                    .with_detail(json!({ "summary": identity.soul_content() })),
            );
            ctx.evidence.push(
                Evidence::new("identity", "agents")
                    .with_detail(json!({ "summary": identity.agents_content() })),
            );
            ctx.evidence.push(
                Evidence::new("identity", "memory")
                    .with_detail(json!({ "summary": identity.memory_content() })),
            );
        }

        self.push_conversation_evidence(&mut ctx, session);

        for note in &session.knowledge {
            ctx.evidence.push(
                Evidence::new("knowledge", "wiki")
                    .with_detail(json!({ "summary": note.content, "path": note.path })),
            );
        }

        let memories = self
            .retrieve_memories_structured(&session_id, query)
            .or_else(|| self.retrieve_memories(&session_id));

        if let Some(ref memories) = memories {
            let memory_text = memories.join("\n");
            ctx.evidence.push(
                Evidence::new("retrieved-memory", "memvid")
                    .with_detail(json!({ "summary": memory_text })),
            );
        }

        self.push_self_learning_evidence(&mut ctx);

        ctx.signals.push(Signal::new("knowledge-query"));

        let _step_result = self.generic.step(&mut ctx).await?;

        let system_prompt = self.build_system_prompt_with_assembly(session, memories.as_deref());

        let dynamic_context = self.build_prompt(query, &ctx);

        let user_message = if dynamic_context.is_empty() || dynamic_context == query {
            query.to_string()
        } else {
            format!(
                "## Retrieved Context\n\n{}\n\n## Current Query\n{}",
                dynamic_context, query
            )
        };

        let response = self
            .call_llm_stream_with_assembly(query, &system_prompt, &user_message, session, on_token)
            .await?;

        session.add_turn("user", query);
        session.add_turn("assistant", &response);

        Ok(response)
    }

    #[instrument(skip(self, system_prompt, user_message, session, on_token), fields(session_id = %session.session_id, query_len = query.len()))]
    async fn call_llm_stream_with_assembly(
        &self,
        query: &str,
        system_prompt: &str,
        user_message: &str,
        session: &SessionContext,
        mut on_token: impl FnMut(&str),
    ) -> Result<String> {
        use rig_core::completion::{CompletionModel, CompletionRequest};
        use rig_core::message::Message;
        use rig_core::OneOrMany;

        let model_name = self.completion_model.provider_name();
        let conversation_id = session.session_id.to_string();
        let messages_in = session.conversation.len() + 1;

        crate::observability::emit_prompt_started(model_name, &conversation_id, messages_in);

        let mut history_messages: Vec<Message> = session
            .conversation
            .iter()
            .filter_map(|turn| match turn.role.as_str() {
                "user" => Some(Message::user(&turn.content)),
                "assistant" => Some(Message::assistant(&turn.content)),
                _ => None,
            })
            .collect();

        tracing::info!(
            history_messages = history_messages.len(),
            system_prompt_len = system_prompt.len(),
            user_message_len = user_message.len(),
            "call_llm_stream_with_assembly: sending request"
        );

        history_messages.push(Message::user(user_message));

        let chat_history = OneOrMany::many(history_messages)
            .unwrap_or_else(|_| OneOrMany::one(Message::user(user_message)));

        let request = CompletionRequest {
            model: None,
            preamble: Some(system_prompt.to_string()),
            chat_history,
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(2048),
            tool_choice: None,
            additional_params: None,
            output_schema: None,
        };

        let mut stream = self.completion_model.stream(request).await?;
        let mut full_response = String::new();

        while let Some(item) = stream.next().await {
            match item {
                Ok(StreamedAssistantContent::Text(text)) => {
                    let token = text.text.clone();
                    full_response.push_str(&token);
                    on_token(&token);
                }
                Ok(StreamedAssistantContent::Final(_)) => break,
                Ok(_) => {}
                Err(e) => {
                    return Err(anyhow::anyhow!("streaming error: {}", e));
                }
            }
        }

        crate::observability::emit_prompt_completed(
            model_name,
            &conversation_id,
            None,
            None,
            None,
        );

        tracing::info!(
            response_len = full_response.len(),
            "call_llm_stream_with_assembly: response complete"
        );

        Ok(full_response)
    }
}

/// Builder for [`ZenAgent`].
pub struct ZenAgentBuilder {
    name: String,
    skill_ids: Vec<String>,
    tool_ids: Vec<String>,
    zen_paths: Option<ZenPaths>,
    memvid_store: Option<rig_memvid::MemvidStore>,
}

impl ZenAgentBuilder {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            skill_ids: Vec::new(),
            tool_ids: Vec::new(),
            zen_paths: None,
            memvid_store: None,
        }
    }

    pub fn with_skill(mut self, id: impl Into<String>) -> Self {
        self.skill_ids.push(id.into());
        self
    }

    pub fn with_tool(mut self, id: impl Into<String>) -> Self {
        self.tool_ids.push(id.into());
        self
    }

    pub fn with_paths(mut self, paths: ZenPaths) -> Self {
        self.zen_paths = Some(paths);
        self
    }

    pub fn with_memvid_store(mut self, store: rig_memvid::MemvidStore) -> Self {
        self.memvid_store = Some(store);
        self
    }

    pub fn build(self, wiring: &ZenWiring, router: &DefaultRouter) -> Result<ZenAgent> {
        let completion_model =
            ZenCompletionModel::new(router.clone(), router.default_provider_name());

        let generic = GenericAgent::builder(&self.name)
            .with_skills(self.skill_ids.clone())
            .with_tools(self.tool_ids.clone())
            .build(&wiring.skills, &wiring.tools)?;

        let identity = self.zen_paths.as_ref().map(load_identity_files);
        let signals = self.zen_paths.as_ref().map(SelfLearningSignals::load);

        Ok(ZenAgent {
            generic,
            completion_model,
            identity,
            signals,
            memvid_store: self.memvid_store,
        })
    }
}

#[cfg(test)]
mod chain_tests {
    use super::*;
    use zen_core::types::SessionContext;

    #[test]
    fn tier_score_includes_knowledge() {
        let score = ZenAgent::tier_score("knowledge", "wiki");
        assert_eq!(score, 0.70);
        assert!(score < ZenAgent::tier_score("identity", "soul"));
        assert!(score > ZenAgent::tier_score("skills", "other"));
    }
}
