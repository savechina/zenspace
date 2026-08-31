use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use tracing::{debug, info, warn};

use zen_core::paths::ZenPaths;
use zen_memory::belief::{Belief, slugify_proposition};
use zen_memory::commitment::Commitment;
use zen_memory::correction::Correction;
use zen_memory::decision::{CostBreakdown, Decision};
use zen_memory::dream::{ExtractedSignals, update_memory_from_facts};
use zen_memory::fact::Fact;
use zen_memory::feedback_signal::Feedback;

use super::super::{WorkerContext, WorkerReport, ZenWorker};
use super::marker_state::JournalEntryState;

pub struct MemoryCurator {
    scheduled: Option<&'static str>,
}

impl MemoryCurator {
    pub fn new() -> Self {
        Self { scheduled: None }
    }

    pub fn with_schedule(mut self, expr: &str) -> Self {
        self.scheduled = Some(Box::leak(expr.to_string().into_boxed_str()));
        self
    }
}

impl Default for MemoryCurator {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ZenWorker for MemoryCurator {
    fn id(&self) -> &'static str {
        "memory-curator"
    }

    fn description(&self) -> &'static str {
        "Route typed signals from journal entries to wiki/wisdom + memories surfaces, update MEMORY.md"
    }

    fn schedule(&self) -> &'static str {
        self.scheduled.unwrap_or("0 */5 * * * *")
    }

    async fn execute(&self, _ctx: &WorkerContext) -> Result<WorkerReport> {
        let start = std::time::Instant::now();
        let paths = ZenPaths::detect()?;

        let journal_dir = paths.journal_entries();
        if !journal_dir.is_dir() {
            debug!("journal entries directory does not exist, skipping");
            return Ok(WorkerReport {
                worker_id: self.id().to_string(),
                success: true,
                fact_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
                llm_cost_usd: 0.0,
            });
        }

        let mut all_signals = ExtractedSignals::default();
        let mut typed_all = TypedJournalSignals::default();
        let mut to_mark: Vec<std::path::PathBuf> = Vec::new();

        for entry in fs::read_dir(&journal_dir)
            .with_context(|| format!("failed to read journal entries: {}", journal_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();

            if !path.is_file() || path.extension().is_none_or(|ext| ext != "md") {
                continue;
            }

            if !is_journaled(&path) {
                continue;
            }
            if has_memory_updated_marker(&path) {
                continue;
            }

            match extract_signals_from_journal(&path) {
                Ok(signals) => {
                    // Marker only when BOTH extraction passes succeed — a
                    // failed typed extraction must retry next cycle, not be
                    // skipped forever.
                    match extract_typed_signals_from_journal(&path) {
                        Ok(typed) => {
                            typed_all.extend(typed);
                            to_mark.push(path.clone());
                        }
                        Err(e) => {
                            warn!(path = %path.display(), error = %e, "typed signal extraction failed; journal stays unmarked for retry");
                        }
                    }
                    debug!(
                        path = %path.display(),
                        facts = signals.facts.len(),
                        reflections = signals.reflections.len(),
                        commitments = signals.commitments.len(),
                        "signals extracted from journal entry"
                    );
                    all_signals.facts.extend(signals.facts);
                    all_signals.reflections.extend(signals.reflections);
                    all_signals.commitments.extend(signals.commitments);
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "failed to extract signals from journal entry");
                }
            }
        }

        let routed = route_typed_signals(&paths, &typed_all);

        if !all_signals.facts.is_empty() {
            update_memory_from_facts(&paths, &all_signals.facts, "Session")?;
        }
        if !all_signals.reflections.is_empty() {
            update_memory_from_facts(&paths, &all_signals.reflections, "Reflection")?;
        }
        if !all_signals.commitments.is_empty() {
            update_memory_from_facts(&paths, &all_signals.commitments, "Commitment")?;
        }

        let total = all_signals.total();
        if total > 0 || routed > 0 {
            info!(
                facts = all_signals.facts.len(),
                reflections = all_signals.reflections.len(),
                commitments = all_signals.commitments.len(),
                routed,
                "MEMORY.md updated and typed signals routed from journal entries"
            );
        }

        for path in &to_mark {
            if let Err(e) = append_memory_updated_marker(path) {
                warn!(path = %path.display(), error = %e, "failed to mark journal entry as memory-updated");
            }
        }

        Ok(WorkerReport {
            worker_id: self.id().to_string(),
            success: true,
            fact_count: total + routed,
            duration_ms: start.elapsed().as_millis() as u64,
            llm_cost_usd: 0.0,
        })
    }
}

fn extract_signals_from_journal(path: &std::path::Path) -> Result<ExtractedSignals> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read journal entry: {}", path.display()))?;

    let mut signals = ExtractedSignals::default();
    let mut current_section: Option<&str> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == "## Facts" {
            current_section = Some("facts");
            continue;
        } else if trimmed == "## Reflections" {
            current_section = Some("reflections");
            continue;
        } else if trimmed == "## Commitments" {
            current_section = Some("commitments");
            continue;
        } else if trimmed.starts_with("## ") {
            current_section = None;
            continue;
        }

        if let Some(section) = current_section
            && let Some(item) = trimmed.strip_prefix("- ")
        {
            let item = item.trim().to_string();
            let is_placeholder = item.starts_with("_(no ");
            if !item.is_empty() && !is_placeholder {
                match section {
                    "facts" => signals.facts.push(item),
                    "reflections" => signals.reflections.push(item),
                    "commitments" => signals.commitments.push(item),
                    _ => {}
                }
            }
        }
    }

    Ok(signals)
}

// ─── T024: typed-signal routing (FR-021) ───────────────────────────────

/// Typed signal buckets routed to M3/M4 surfaces (FR-021, data-model §9).
///
/// Payload lines follow the `session_journaler` `a|||b|||c` convention:
/// - decisions:     `text|||context|||expected_value`
/// - corrections:   `error|||fix|||cost`
/// - feedback:      `target|||content|||sentiment`
/// - beliefs:       `statement|||confidence`
/// - anti-patterns: `pattern|||trigger|||avoidance`
/// - mental models: `model|||application`
/// - virtue logs:   `domain_slug|||kept|broken|partial`
/// - facts / commitments: bare text.
#[derive(Default)]
struct TypedJournalSignals {
    facts: Vec<String>,
    commitments: Vec<String>,
    anti_patterns: Vec<String>,
    mental_models: Vec<String>,
    virtue_logs: Vec<String>,
    decisions: Vec<String>,
    corrections: Vec<String>,
    feedback: Vec<String>,
    beliefs: Vec<String>,
}

impl TypedJournalSignals {
    fn extend(&mut self, other: TypedJournalSignals) {
        self.facts.extend(other.facts);
        self.commitments.extend(other.commitments);
        self.anti_patterns.extend(other.anti_patterns);
        self.mental_models.extend(other.mental_models);
        self.virtue_logs.extend(other.virtue_logs);
        self.decisions.extend(other.decisions);
        self.corrections.extend(other.corrections);
        self.feedback.extend(other.feedback);
        self.beliefs.extend(other.beliefs);
    }
}

/// Routing bucket a journal line currently belongs to during extraction.
#[derive(Clone, Copy, Debug, PartialEq)]
enum TypedBucket {
    Facts,
    Commitments,
    AntiPatterns,
    MentalModels,
    VirtueLogs,
    Decisions,
    Corrections,
    Feedback,
    Beliefs,
}

/// Normalize a `kind:` tag value to a routing bucket.
///
/// Accepts singular/plural and `snake_case`/`kebab-case` spellings, e.g.
/// `decision`, `decisions`, `anti_pattern`, `anti-pattern`, `mentalmodel`.
/// `reflection` maps to `None` — reflections stay in MEMORY.md (FR-021).
fn kind_to_bucket(value: &str) -> Option<TypedBucket> {
    let normalized: String = value
        .trim()
        .trim_matches('"')
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let normalized = normalized.strip_suffix('s').unwrap_or(&normalized);
    match normalized {
        "fact" => Some(TypedBucket::Facts),
        "commitment" => Some(TypedBucket::Commitments),
        "antipattern" => Some(TypedBucket::AntiPatterns),
        "mentalmodel" => Some(TypedBucket::MentalModels),
        "virtuelog" => Some(TypedBucket::VirtueLogs),
        "decision" => Some(TypedBucket::Decisions),
        "correction" => Some(TypedBucket::Corrections),
        "feedback" => Some(TypedBucket::Feedback),
        "belief" => Some(TypedBucket::Beliefs),
        _ => None,
    }
}

/// Extract typed signals from one journal entry.
///
/// Detection covers `kind: <x>` tags anywhere in the entry (frontmatter or
/// body — the same convention `zen_loop` greps via `kind: decision`) plus
/// the `## Facts` / `## Commitments` section headings, whose items route to
/// their M3/M4 surfaces in addition to MEMORY.md. Items under the current
/// context are `- ` list entries; placeholders (`_(no ...`) are skipped.
fn extract_typed_signals(content: &str) -> TypedJournalSignals {
    let mut typed = TypedJournalSignals::default();
    let mut current: Option<TypedBucket> = None;

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(rest) = trimmed.strip_prefix("kind:") {
            current = kind_to_bucket(rest);
            continue;
        }

        if trimmed.starts_with("## ") {
            current = match trimmed {
                "## Facts" => Some(TypedBucket::Facts),
                "## Commitments" => Some(TypedBucket::Commitments),
                _ => None,
            };
            continue;
        }

        if current.is_some()
            && let Some(item) = trimmed.strip_prefix("- ")
        {
            let item = item.trim().to_string();
            if item.is_empty() || item.starts_with("_(no ") {
                continue;
            }
            match current {
                Some(TypedBucket::Facts) => typed.facts.push(item),
                Some(TypedBucket::Commitments) => typed.commitments.push(item),
                Some(TypedBucket::AntiPatterns) => typed.anti_patterns.push(item),
                Some(TypedBucket::MentalModels) => typed.mental_models.push(item),
                Some(TypedBucket::VirtueLogs) => typed.virtue_logs.push(item),
                Some(TypedBucket::Decisions) => typed.decisions.push(item),
                Some(TypedBucket::Corrections) => typed.corrections.push(item),
                Some(TypedBucket::Feedback) => typed.feedback.push(item),
                Some(TypedBucket::Beliefs) => typed.beliefs.push(item),
                None => {}
            }
        }
    }

    typed
}

/// Read one journal entry and extract its typed signals.
fn extract_typed_signals_from_journal(path: &Path) -> Result<TypedJournalSignals> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read journal entry: {}", path.display()))?;
    Ok(extract_typed_signals(&content))
}

/// Route typed journal signals to their M3/M4 surfaces (FR-021):
///
/// - Fact/Decision/Correction/Feedback/Belief → `wiki/wisdom/{facts,decisions,
///   corrections,feedback,beliefs}/{slug}.md`
/// - Commitment → `memories/commitments/{slug}.md`
/// - AntiPattern → `wiki/wisdom/anti-patterns/{slug}.md`
/// - MentalModel → `wiki/wisdom/models/{slug}.md`
/// - VirtueLog → `memories/virtue_logs/{domain}/{date}.md`
/// - Reflection → stays in MEMORY.md (handled by `extract_signals_from_journal`)
///
/// Idempotency: entries are gated by the `memory_updated` sidecar marker, and
/// deterministic-slug surfaces are additionally skipped when the target file
/// already exists. Writers mirror `session_journaler::save_typed_signals`.
/// Returns the number of signals routed.
fn route_typed_signals(paths: &ZenPaths, typed: &TypedJournalSignals) -> usize {
    let vault = paths.vault();
    let mut routed = 0usize;

    for raw in &typed.decisions {
        let parts: Vec<&str> = raw.splitn(3, "|||").collect();
        let Some(text) = parts.first().map(|s| s.trim()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let context = parts.get(1).map(|s| s.trim()).unwrap_or("");
        let id = Decision::slugify_title(text);
        let dir = vault.join("wiki/wisdom/decisions");
        if dir.join(format!("{id}.md")).exists() {
            continue;
        }
        let mut decision = Decision::new(id, text.to_string(), "journal".to_string());
        decision.goal = context.to_string();
        match decision.save(&dir) {
            Ok(()) => routed += 1,
            Err(e) => warn!(error = %e, text = %text, "failed to route decision"),
        }
    }

    for raw in &typed.corrections {
        let parts: Vec<&str> = raw.splitn(3, "|||").collect();
        let Some(error_ref) = parts.first().map(|s| s.trim()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let fix = parts.get(1).map(|s| s.trim()).unwrap_or("");
        let correction = Correction::new(error_ref, fix, CostBreakdown::default());
        match correction.save(&vault.join("wiki/wisdom/corrections")) {
            Ok(_) => routed += 1,
            Err(e) => warn!(error = %e, error_ref = %error_ref, "failed to route correction"),
        }
    }

    for raw in &typed.feedback {
        let parts: Vec<&str> = raw.splitn(3, "|||").collect();
        let Some(target) = parts.first().map(|s| s.trim()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let content = parts.get(1).map(|s| s.trim()).unwrap_or("");
        let feedback = Feedback::new(target, content);
        match feedback.save(&vault.join("wiki/wisdom/feedback")) {
            Ok(_) => routed += 1,
            Err(e) => warn!(error = %e, target = %target, "failed to route feedback"),
        }
    }

    for raw in &typed.beliefs {
        let parts: Vec<&str> = raw.splitn(2, "|||").collect();
        let Some(statement) = parts.first().map(|s| s.trim()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let confidence = parts
            .get(1)
            .and_then(|s| s.trim().parse::<f64>().ok())
            .unwrap_or(0.5);
        let id = slugify_proposition(statement);
        let dir = vault.join("wiki/wisdom/beliefs");
        if dir.join(format!("{id}.md")).exists() {
            continue;
        }
        let mut belief = Belief::new(id, statement.to_string(), "journal".to_string());
        belief.posterior = confidence.clamp(0.01, 0.99);
        match belief.save(&dir) {
            Ok(()) => routed += 1,
            Err(e) => warn!(error = %e, statement = %statement, "failed to route belief"),
        }
    }

    for raw in &typed.facts {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let fact = Fact::new(trimmed, "journal", Vec::new());
        match fact.save(&vault.join("wiki/wisdom/facts")) {
            Ok(_) => routed += 1,
            Err(e) => warn!(error = %e, what = %trimmed, "failed to route fact"),
        }
    }

    for raw in &typed.commitments {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let commitment = Commitment::from_raw(trimmed);
        let dir = vault.join("memories/commitments");
        if dir.join(format!("{}.md", commitment.slug())).exists() {
            continue;
        }
        match commitment.save(&dir) {
            Ok(_) => routed += 1,
            Err(e) => warn!(error = %e, what = %trimmed, "failed to route commitment"),
        }
    }

    for raw in &typed.anti_patterns {
        let parts: Vec<&str> = raw.splitn(3, "|||").collect();
        let Some(pattern) = parts.first().map(|s| s.trim()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let signal = zen_memory::AntiPatternSignal {
            pattern: pattern.to_string(),
            trigger: parts
                .get(1)
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
            avoidance: parts
                .get(2)
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
            detected_in: vec!["journal".to_string()],
        };
        let dir = vault.join("wiki/wisdom/anti-patterns");
        if dir.join(format!("{}.md", signal.slug())).exists() {
            continue;
        }
        match signal.save(&dir) {
            Ok(_) => routed += 1,
            Err(e) => warn!(error = %e, pattern = %pattern, "failed to route anti-pattern"),
        }
    }

    for raw in &typed.mental_models {
        let parts: Vec<&str> = raw.splitn(2, "|||").collect();
        let Some(model) = parts.first().map(|s| s.trim()).filter(|s| !s.is_empty()) else {
            continue;
        };
        let signal = zen_memory::MentalModelSignal {
            model: model.to_string(),
            domain: "journal".to_string(),
            application: parts
                .get(1)
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
            source: "journal".to_string(),
        };
        let dir = vault.join("wiki/wisdom/models");
        if dir.join(format!("{}.md", signal.slug())).exists() {
            continue;
        }
        match signal.save(&dir) {
            Ok(_) => routed += 1,
            Err(e) => warn!(error = %e, model = %model, "failed to route mental model"),
        }
    }

    routed += route_virtue_logs(&vault.join("memories/virtue_logs"), &typed.virtue_logs);

    routed
}

/// Route virtue-log entries (`domain_slug|||status`) to
/// `memories/virtue_logs/{domain}/{date}.md`. Returns the count routed.
fn route_virtue_logs(dir: &Path, raws: &[String]) -> usize {
    use zen_memory::{VirtueDomain, VirtueLog, VirtueStatus};

    let mut routed = 0usize;
    for raw in raws {
        let parts: Vec<&str> = raw.splitn(2, "|||").collect();
        let Some(domain) = parts
            .first()
            .and_then(|s| VirtueDomain::from_slug(s.trim()))
        else {
            debug!(raw = %raw, "unparseable virtue log payload, skipping");
            continue;
        };
        let status = match parts.get(1).map(|s| s.trim()) {
            Some("broken") => VirtueStatus::Broken,
            Some("partial") => VirtueStatus::Partial,
            Some("kept") => VirtueStatus::Kept,
            _ => {
                debug!(raw = %raw, "unknown virtue status, skipping");
                continue;
            }
        };
        let log = VirtueLog::new(domain, status, chrono::Utc::now().date_naive());
        let target = dir
            .join(domain.slug())
            .join(format!("{}.md", log.date.format("%Y-%m-%d")));
        if target.exists() {
            continue;
        }
        match log.save(dir) {
            Ok(_) => routed += 1,
            Err(e) => warn!(error = %e, raw = %raw, "failed to route virtue log"),
        }
    }
    routed
}

fn is_journaled(path: &std::path::Path) -> bool {
    if JournalEntryState::is_journaled(path) {
        return true;
    }
    JournalEntryState::migrate_from_frontmatter(path) && JournalEntryState::is_journaled(path)
}

fn has_memory_updated_marker(path: &std::path::Path) -> bool {
    if JournalEntryState::has_memory_updated(path) {
        return true;
    }
    JournalEntryState::migrate_from_frontmatter(path) && JournalEntryState::has_memory_updated(path)
}

fn append_memory_updated_marker(path: &std::path::Path) -> Result<()> {
    let state = JournalEntryState {
        memory_updated_at: Some(chrono::Utc::now().to_rfc3339()),
        ..Default::default()
    };
    state.save(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_is_journaled_found() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "---\nsession_id: test\n---\n\n").unwrap();

        let state = JournalEntryState {
            journaled_at: Some("2026-06-20T14:30:00Z".to_string()),
            ..Default::default()
        };
        state.save(&path).unwrap();

        assert!(is_journaled(&path));
    }

    #[test]
    fn test_is_journaled_not_found() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "---\nsession_id: test\n---\n\ncontent\n").unwrap();
        assert!(!is_journaled(&path));
    }

    #[test]
    fn test_has_memory_updated_marker() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "---\nsession_id: test\n---\n\n").unwrap();

        let state = JournalEntryState {
            journaled_at: Some("2026-06-20T14:30:00Z".to_string()),
            memory_updated_at: Some("2026-06-20T14:35:00Z".to_string()),
            ..Default::default()
        };
        state.save(&path).unwrap();

        assert!(has_memory_updated_marker(&path));
    }

    #[test]
    fn test_has_memory_updated_marker_not_found() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(
            &path,
            "---\nsession_id: test\njournaled_at: 2026-06-20T14:30:00Z\n---\n\n",
        )
        .unwrap();
        assert!(!has_memory_updated_marker(&path));
    }

    #[test]
    fn test_extract_signals_from_journal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        let content = "---\nsession_id: test\njournaled_at: 2026-06-20T14:30:00Z\n---\n\n## Facts\n\n- completed auth module\n- fixed login bug\n\n## Other\n\n- not a fact\n";
        fs::write(&path, content).unwrap();

        let signals = extract_signals_from_journal(&path).unwrap();
        assert_eq!(signals.facts.len(), 2);
        assert!(signals.facts.contains(&"completed auth module".to_string()));
        assert!(signals.facts.contains(&"fixed login bug".to_string()));
    }

    #[test]
    fn test_extract_signals_empty_journal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "---\njournaled_at: 2026-06-20T14:30:00Z\n---\n\n## Facts\n\n_(no durable facts extracted)_\n").unwrap();

        let signals = extract_signals_from_journal(&path).unwrap();
        assert!(signals.is_empty());
    }

    #[test]
    fn test_extract_signals_all_three_sections() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        let content = "---\nsession_id: test\njournaled_at: 2026-06-20T14:30:00Z\n---\n\n## Facts\n\n- implemented JWT auth\n- fixed race condition\n\n## Reflections\n\n- login flow too complex\n- should have tested migration first\n\n## Commitments\n\n- simplify login by July\n- write integration tests this week\n";
        fs::write(&path, content).unwrap();

        let signals = extract_signals_from_journal(&path).unwrap();
        assert_eq!(signals.facts.len(), 2);
        assert_eq!(signals.reflections.len(), 2);
        assert_eq!(signals.commitments.len(), 2);
        assert!(signals.facts.contains(&"implemented JWT auth".to_string()));
        assert!(
            signals
                .reflections
                .contains(&"login flow too complex".to_string())
        );
        assert!(
            signals
                .commitments
                .contains(&"simplify login by July".to_string())
        );
    }

    #[test]
    fn test_kind_to_bucket_aliases() {
        assert_eq!(kind_to_bucket("decision"), Some(TypedBucket::Decisions));
        assert_eq!(kind_to_bucket("Decisions"), Some(TypedBucket::Decisions));
        assert_eq!(
            kind_to_bucket("anti_pattern"),
            Some(TypedBucket::AntiPatterns)
        );
        assert_eq!(
            kind_to_bucket("anti-patterns"),
            Some(TypedBucket::AntiPatterns)
        );
        assert_eq!(
            kind_to_bucket("mental-model"),
            Some(TypedBucket::MentalModels)
        );
        assert_eq!(kind_to_bucket("virtue_log"), Some(TypedBucket::VirtueLogs));
        assert_eq!(kind_to_bucket("reflection"), None);
        assert_eq!(kind_to_bucket("session"), None);
        assert_eq!(kind_to_bucket(""), None);
    }

    #[test]
    fn test_extract_typed_signals_frontmatter_kind() {
        let content = "---\nsession_id: s1\nkind: decision\n---\n\n- Use SQLite over Postgres|||offline-first|||lower ops cost\n- Ship dark mode next week|||user demand\n";
        let typed = extract_typed_signals(content);
        assert_eq!(typed.decisions.len(), 2);
        assert!(typed.decisions[0].starts_with("Use SQLite over Postgres"));
        assert!(typed.facts.is_empty());
    }

    #[test]
    fn test_extract_typed_signals_sections_and_inline_tags() {
        let content = "---\nsession_id: s1\n---\n\n## Facts\n\n- completed auth module\n\n## Reflections\n\n- should test more\n\nkind: belief\n\n- SQLite scales for single-user|||0.7\n\nkind: virtue_log\n\n- diligence|||kept\n\n## Other\n\n- not captured\n";
        let typed = extract_typed_signals(content);
        assert_eq!(typed.facts, vec!["completed auth module".to_string()]);
        assert_eq!(typed.beliefs.len(), 1);
        assert_eq!(typed.virtue_logs, vec!["diligence|||kept".to_string()]);
    }

    #[test]
    fn test_extract_typed_signals_skips_placeholders() {
        let content = "---\nkind: decisions\n---\n\n## Decisions\n\n_(no decisions extracted)_\n";
        let typed = extract_typed_signals(content);
        assert!(typed.decisions.is_empty());
    }

    #[test]
    fn test_route_typed_signals_writes_all_surfaces() {
        let tmp = tempdir().unwrap();
        let paths = ZenPaths::for_testing(tmp.path().to_path_buf());
        let typed = TypedJournalSignals {
            facts: vec!["completed auth module".to_string()],
            commitments: vec!["simplify login by July".to_string()],
            anti_patterns: vec![
                "Loss Aversion|||clinging to sunk cost|||set stop-loss".to_string(),
            ],
            mental_models: vec!["Circle of Competence|||stay in known domains".to_string()],
            virtue_logs: vec!["diligence|||kept".to_string()],
            decisions: vec![
                "Use SQLite over Postgres|||offline-first|||lower ops cost".to_string(),
            ],
            corrections: vec!["assumed env vars set|||validate at startup|||2h".to_string()],
            feedback: vec!["login-flow|||users abandon at step 3|||negative".to_string()],
            beliefs: vec!["SQLite is sufficient locally|||0.7".to_string()],
        };

        let routed = route_typed_signals(&paths, &typed);
        assert_eq!(routed, 9);

        let vault = tmp.path().join("vault");
        assert!(
            vault
                .join("wiki/wisdom/decisions/use-sqlite-over-postgres.md")
                .exists()
        );
        assert!(vault.join("wiki/wisdom/corrections").is_dir());
        assert!(vault.join("wiki/wisdom/feedback").is_dir());
        assert!(
            vault
                .join("wiki/wisdom/beliefs/sqlite-is-sufficient-locally.md")
                .exists()
        );
        assert!(vault.join("wiki/wisdom/facts").is_dir());
        assert!(
            vault
                .join("memories/commitments/simplify-login-by-july.md")
                .exists()
        );
        assert!(
            vault
                .join("wiki/wisdom/anti-patterns/loss-aversion.md")
                .exists()
        );
        assert!(
            vault
                .join("wiki/wisdom/models/circle-of-competence.md")
                .exists()
        );
        assert!(vault.join("memories/virtue_logs/diligence").is_dir());

        let again = route_typed_signals(&paths, &typed);
        assert_eq!(
            again, 3,
            "only uuid/timestamp-id surfaces (fact, correction, feedback) re-route"
        );
    }

    #[test]
    fn test_route_virtue_logs_skips_unknown_payloads() {
        let tmp = tempdir().unwrap();
        let raws = vec![
            "not-a-virtue|||kept".to_string(),
            "diligence|||unknown-status".to_string(),
        ];
        assert_eq!(route_virtue_logs(tmp.path(), &raws), 0);
    }
}
