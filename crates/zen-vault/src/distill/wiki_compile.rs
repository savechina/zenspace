use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use rig_compose::context::InvestigationContext;
use rig_compose::registry::{KernelError, ToolRegistry};
use rig_compose::skill::{Skill, SkillOutcome};
use tracing::info;
use zen_repo::normalize_alias;

use crate::note::Note;
use crate::notion::NotionData;
use crate::wiki::{WikiIndex, WikiLog, WikiPage, WikiStructure};

/// Policy for `[[wikilink]]` targets missing from the compile whitelist
/// (compile-hygiene ②).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GhostlinkPolicy {
    /// Rewrite ghost `[[target]]` links to their plain text (default).
    #[default]
    Strip,
    /// Keep ghost links in the emitted page, counting them only.
    Warn,
}

impl GhostlinkPolicy {
    /// Parse a `[agentic.compile] ghostlink_enforcement` value; unknown
    /// values degrade to [`GhostlinkPolicy::Strip`] with a warn.
    pub fn parse(raw: &str) -> Self {
        if raw.eq_ignore_ascii_case("warn") {
            GhostlinkPolicy::Warn
        } else {
            if !raw.eq_ignore_ascii_case("strip") && !raw.is_empty() {
                tracing::warn!(value = raw, "unknown ghostlink policy; using strip");
            }
            GhostlinkPolicy::Strip
        }
    }

    /// Resolve the policy from `[agentic.compile] ghostlink_enforcement`,
    /// failing open to [`GhostlinkPolicy::Strip`] when config is unreadable.
    pub fn from_env_config() -> Self {
        zen_core::config::load_config()
            .map(|c| GhostlinkPolicy::parse(c.agentic.compile.ghostlink_enforcement_or_default()))
            .unwrap_or_default()
    }
}

/// Per-run ghostlink counters (compile-hygiene ②), carried on
/// `ScopedRunOutcome` and surfaced via the `loop.compile.ghostlinks` audit
/// line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GhostlinkReport {
    /// Link instances rewritten to plain text.
    pub stripped: usize,
    /// Link instances kept under `Warn` policy.
    pub warned: usize,
}

/// Per-run compile counters (compile-hygiene ②+E5), carried on
/// `ScopedRunOutcome` and surfaced via the `loop.compile.ghostlinks` /
/// `loop.page.iterations` audit lines.
#[derive(Debug, Clone, Default)]
pub struct CompileReport {
    /// Ghost `[[wikilink]]` counters for this run.
    pub ghostlinks: GhostlinkReport,
    /// Prior page versions captured before overwrites (E5). The captured
    /// files ride along so the caller can `txn.track_path()` them.
    pub iterations: Vec<crate::wiki::CapturedIteration>,
}

impl GhostlinkReport {
    pub fn total(&self) -> usize {
        self.stripped + self.warned
    }
}

/// Known technology keywords for notion classification.
const TECH_KEYWORDS: &[&str] = &[
    "rust",
    "python",
    "javascript",
    "typescript",
    "go",
    "sqlite",
    "postgresql",
    "redis",
    "docker",
    "kubernetes",
    "react",
    "vue",
    "tokio",
    "async",
    "llm",
    "ai",
    "mcp",
    "wasm",
    "rig-core",
    "ratatui",
    "grpc",
    "http",
    "tcp",
    "udp",
    "nginx",
    "apache",
    "git",
    "linux",
    "macos",
    "windows",
    "aws",
    "gcp",
    "azure",
    "kafka",
    "rabbitmq",
    "graphql",
    "rest",
];

/// Categorization result for a note's notion type.
#[derive(Debug, Clone, PartialEq)]
enum NoteCategory {
    /// Technology notion → goes under `notions/technology/`
    Technology,
    /// Concept/page → goes under `concepts/`
    Concept,
}

impl NoteCategory {
    fn directory_name(&self) -> &str {
        match self {
            NoteCategory::Technology => "notions/technology",
            NoteCategory::Concept => "notions/concepts",
        }
    }
}

/// WikiCompiler converts [`Note`] objects into [`WikiPage`] objects,
/// writes them to disk, and maintains the wiki index and log.
pub struct WikiCompiler;

impl WikiCompiler {
    /// Create a new `WikiCompiler`.
    pub fn new() -> Self {
        Self
    }

    /// Compile a batch of notes into wiki pages.
    ///
    /// For each note:
    /// 1. Extract title (first `# Heading` or fallback to note id)
    /// 2. Extract content (strip frontmatter if present)
    /// 3. Extract wikilinks using `[[...]]` pattern
    /// 4. Categorize (Technology → `notions/technology/`, Concept → `concepts/`)
    /// 5. Write to disk under `wiki_dir`
    ///
    /// Wikilink contract (compile-hygiene ②): every emitted `[[link]]` target
    /// must exist — pages already under `wiki_dir` plus pages created in this
    /// run. Ghost targets (LLM-hallucinated names) are stripped to plain text
    /// under the default [`GhostlinkPolicy::Strip`]; use
    /// [`compile_with_policy`](WikiCompiler::compile_with_policy) for the
    /// `Warn` variant or to receive the per-run [`GhostlinkReport`].
    ///
    /// After processing all notes:
    /// - Creates wiki directory structure via [`WikiStructure`]
    /// - Generates `index.md` via [`WikiIndex`]
    /// - Logs operations via [`WikiLog`]
    pub fn compile(&self, notes: &[Note], wiki_dir: &Path) -> Result<Vec<WikiPage>> {
        let (pages, _) = self.compile_with_policy(notes, wiki_dir, GhostlinkPolicy::Strip, None)?;
        Ok(pages)
    }

    /// [`compile`](WikiCompiler::compile) with an explicit ghost policy
    /// and optional E5 page-iteration capture, returning the per-run
    /// [`CompileReport`] alongside the pages.
    ///
    /// `page_iterations` (`Some`) captures the prior bytes of every page
    /// this run overwrites into the `vault/iterations` store before the
    /// write lands; the caller must `txn.track_path()` the captured files
    /// (see [`CapturedIteration`]) so CAS rollback and crash replay cover
    /// them. `None` disables capture (tests, `compile`).
    pub fn compile_with_policy(
        &self,
        notes: &[Note],
        wiki_dir: &Path,
        policy: GhostlinkPolicy,
        page_iterations: Option<&crate::wiki::PageIterations>,
    ) -> Result<(Vec<WikiPage>, CompileReport)> {
        let structure = WikiStructure::new(wiki_dir);
        structure
            .ensure_directories()
            .context("ensure wiki directories")?;

        let log = WikiLog::new(wiki_dir);
        log.append("compile_start", &format!("compiling {} notes", notes.len()))?;

        // Pass 1: convert every note before writing anything, so the
        // whitelist covers same-run creates (page A may legitimately link
        // page B compiled later in the batch).
        let mut pages = Vec::with_capacity(notes.len());
        for note in notes {
            pages.push(self.note_to_page(note)?);
        }

        // Whitelist: pre-existing page names ∪ this run's titles/slugs,
        // compared through the canonical NFC alias normalization. Existing
        // stems are slugified filenames, so both the dashed form and the
        // de-slugified form must resolve to the same page title.
        let mut whitelist: HashSet<String> = crate::graph_verify::wiki_page_inventory(wiki_dir)
            .into_iter()
            .flat_map(|(stem, _)| {
                [
                    normalize_alias(&stem),
                    normalize_alias(&stem.replace('-', " ")),
                ]
            })
            .collect();
        for page in &pages {
            whitelist.insert(normalize_alias(&page.title));
            whitelist.insert(normalize_alias(&slugify(&page.title)));
        }

        let mut report = GhostlinkReport::default();
        for page in &mut pages {
            enforce_ghostlinks(page, &whitelist, policy, &mut report);
        }

        let mut written = 0usize;
        let mut iterations = Vec::new();
        for page in &pages {
            let full_path = wiki_dir.join(&page.path);

            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create dir: {}", parent.display()))?;
            }

            let rendered = self.render_page(page);
            // E5: protect the prior version before the pipeline overwrites
            // a page a human may have hand-tuned.
            if let Some(store) = page_iterations
                && let Some(captured) = store
                    .capture(wiki_dir, &full_path, &rendered)
                    .with_context(|| format!("capture iteration: {}", full_path.display()))?
            {
                iterations.push(captured);
            }
            zen_core::atomic_file::write_atomic(&full_path, rendered.as_bytes())
                .with_context(|| format!("write wiki page: {}", full_path.display()))?;

            info!(
                title = %page.title,
                path = %page.path.display(),
                wikilinks = page.wikilinks.len(),
                "wiki page written"
            );

            log.append(
                "page_created",
                &format!("{} -> {}", page.title, page.path.display()),
            )?;

            written += 1;
        }

        if !pages.is_empty() {
            let index = WikiIndex::new(wiki_dir);
            index.generate(&pages).context("generate index")?;
            log.append("index_generated", &format!("{} pages indexed", pages.len()))?;
        }

        log.append(
            "compile_complete",
            &format!(
                "{} pages written from {} notes; ghostlinks stripped={} warned={}",
                written,
                notes.len(),
                report.stripped,
                report.warned
            ),
        )?;

        info!(
            "wiki compile complete: {written} pages from {} notes; ghostlinks stripped={} warned={}",
            notes.len(),
            report.stripped,
            report.warned
        );
        Ok((
            pages,
            CompileReport {
                ghostlinks: report,
                iterations,
            },
        ))
    }

    /// Compile notion data into wiki pages under `wiki/notions/technology/`.
    /// Relationships rendered as `[[wikilinks]]` for cross-linking.
    pub fn compile_from_entities(&self, notions: &[NotionData], wiki_dir: &Path) -> Result<usize> {
        let structure = WikiStructure::new(wiki_dir);
        structure.ensure_directories()?;

        let log = WikiLog::new(wiki_dir);
        log.append(
            "entity_compile_start",
            &format!("compiling {} notion pages", notions.len()),
        )?;

        let mut written = 0usize;

        for data in notions {
            let slug = slugify(&data.notion.name);
            let rel_path = format!("notions/technology/{slug}.md");
            let full_path = wiki_dir.join(&rel_path);

            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create dir: {}", parent.display()))?;
            }

            let md = self.render_entity_page(data);
            std::fs::write(&full_path, &md)
                .with_context(|| format!("write notion wiki: {}", full_path.display()))?;

            info!(notion = %data.notion.name, path = %rel_path, "notion wiki page written");
            written += 1;
        }

        log.append(
            "entity_compile_complete",
            &format!("{written} notion pages compiled"),
        )?;

        self.generate_entity_index(notions, wiki_dir)?;

        Ok(written)
    }

    /// Generate OKF v0.1 §6 compliant `index.md` — no frontmatter,
    /// standard markdown links with descriptions for progressive disclosure.
    fn generate_entity_index(&self, notions: &[NotionData], wiki_dir: &Path) -> Result<()> {
        let index_path = wiki_dir.join("index.md");
        let mut md = String::new();

        // OKF §6: index files contain no frontmatter.
        md.push_str("# Knowledge Index\n\n## Entities\n\n");

        let mut sorted: Vec<&NotionData> = notions.iter().collect();
        sorted.sort_by_key(|data| data.notion.name.to_lowercase());

        for data in &sorted {
            let slug = slugify(&data.notion.name);
            let desc = data
                .facts
                .first()
                .map(|f| truncate_for_description(f))
                .unwrap_or_else(|| format!("{:?} notion", data.notion.kind));
            // OKF §6: "* [Title](relative-url) - description"
            md.push_str(&format!(
                "* [{}]({}.md) - {}\n",
                data.notion.name, slug, desc
            ));
        }

        std::fs::write(&index_path, &md)
            .with_context(|| format!("write notion index: {}", index_path.display()))?;
        Ok(())
    }

    /// Render an notion page as OKF v0.1 compliant markdown.
    ///
    /// Frontmatter follows §4.1: `type` is required; `title`, `description`,
    /// `tags`, `timestamp` are recommended. `created_at` is a zen extension
    /// (§4.1 allows producer-defined keys).
    ///
    /// Cross-links use standard markdown `[text](/path.md)` per §5.1 (absolute,
    /// bundle-relative), not `[[wikilinks]]`.
    fn render_entity_page(&self, data: &NotionData) -> String {
        let mut md = String::new();

        let type_str = format!("{:?}", data.notion.kind);
        let now = chrono::Utc::now();

        let description = data
            .facts
            .first()
            .map(|f| truncate_for_description(f))
            .unwrap_or_else(|| format!("{} notion", data.notion.name));

        // OKF §4.1 frontmatter
        let mut fm = format!(
            "---\n\
             type: {type_str}\n\
             title: {name}\n\
             description: {description}\n\
             tags: [{tag}]\n",
            name = data.notion.name,
            tag = type_str.to_lowercase(),
        );

        if let Some(ref domain) = data.notion.domain {
            fm.push_str(&format!("domain: {domain}\n"));
        }
        if !data.notion.aliases.is_empty() {
            let aliases_str = data
                .notion
                .aliases
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            fm.push_str(&format!("aliases: [{aliases_str}]\n"));
        }
        fm.push_str(&format!(
            "timestamp: {ts}\n\
             created_at: {created}\n\
             ---\n\n",
            ts = now.to_rfc3339(),
            created = data.notion.created_at.to_rfc3339(),
        ));
        md.push_str(&fm);

        md.push_str(&format!("# {}\n\n", data.notion.name));

        if !data.facts.is_empty() {
            md.push_str("## Facts\n\n");
            for fact in &data.facts {
                md.push_str(&format!("- {fact}\n"));
            }
            md.push('\n');
        }

        if !data.relationships.is_empty() {
            md.push_str("## Relationships\n\n");
            for (target, rel) in &data.relationships {
                let rel_str = format!("{rel:?}");
                let target_slug = slugify(target);
                // OKF §5.1: absolute bundle-relative links
                md.push_str(&format!(
                    "- [{target}](/notions/technology/{target_slug}.md) — {rel_str}\n"
                ));
            }
        }

        md
    }

    // ── Internal helpers ─────────────────────────────────────────

    /// Convert a single [`Note`] into a [`WikiPage`].
    fn note_to_page(&self, note: &Note) -> Result<WikiPage> {
        let title = extract_title(&note.content).unwrap_or_else(|| slugify(&note.id));
        let content = strip_frontmatter(&note.content);
        let wikilinks = WikiPage::extract_wikilinks(&content);
        let category = classify_note(&note.content, &note.tags);

        let slug = slugify(&title);
        let path = PathBuf::from(format!("{}/{slug}.md", category.directory_name()));

        Ok(WikiPage {
            title,
            path,
            created_at: note.created_at,
            updated_at: note.updated_at,
            tags: note.tags.clone(),
            wikilinks,
            para: note.para.clone(),
            // OKF conformance (compile-hygiene ①): the lint requires BOTH
            // `type` and `description` on every page, so the compiler must
            // always supply both — a page that emitted neither was reported
            // as non-conforming on every single lint run, forever.
            //
            // `type` falls back to the note's own classification — the same
            // taxonomy that chose the output directory, so the value is not
            // invented. `description` prefers the note's own frontmatter and
            // otherwise derives one bounded line from the body.
            okf_type: Some(
                note.okf_type
                    .clone()
                    .unwrap_or_else(|| format!("{:?}", category)),
            ),
            description: note
                .description
                .clone()
                .or_else(|| first_substantive_line(&content)),
            // E2 provenance: the original source file. The archive hook
            // redirects this to the durable archive dest in the same cycle.
            sources: note
                .file_path
                .as_ref()
                .map(|p| vec![p.to_string_lossy().to_string()])
                .unwrap_or_default(),
            content,
        })
    }

    /// Render a [`WikiPage`] back to markdown with front matter header.
    fn render_page(&self, page: &WikiPage) -> String {
        let tags_str = if page.tags.is_empty() {
            "[]".to_string()
        } else {
            format!(
                "[{}]",
                page.tags
                    .iter()
                    .map(|t| format!("\"{t}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        let links_str = if page.wikilinks.is_empty() {
            "[]".to_string()
        } else {
            format!(
                "[{}]",
                page.wikilinks
                    .iter()
                    .map(|l| format!("\"{l}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        let mut fm = format!(
            "---\ntitle: \"{}\"\ntags: {}\nwikilinks: {}",
            page.title, tags_str, links_str,
        );

        if let Some(ref para) = page.para {
            fm.push_str(&format!("\npara: \"{}\"", para));
        }
        if let Some(ref okf_type) = page.okf_type {
            fm.push_str(&format!("\ntype: \"{}\"", okf_type));
        }
        if let Some(ref description) = page.description
            && !description.trim().is_empty()
        {
            fm.push_str(&format!("\ndescription: \"{description}\""));
        }
        if !page.sources.is_empty() {
            let sources_str = page
                .sources
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", ");
            fm.push_str(&format!("\nsources: [{sources_str}]"));
        }

        fm.push_str(&format!(
            "\ncreated_at: \"{}\"\nupdated_at: \"{}\"\n---\n\n{}",
            page.created_at.to_rfc3339(),
            page.updated_at.to_rfc3339(),
            page.content,
        ));

        fm
    }
}

impl Default for WikiCompiler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Skill for WikiCompiler {
    fn id(&self) -> &str {
        "zen-wiki-compilation"
    }

    fn description(&self) -> &str {
        "Compile notes into structured wiki pages with headings, wikilinks, and categorized directories"
    }

    fn applies(&self, _ctx: &InvestigationContext) -> bool {
        true
    }

    async fn execute(
        &self,
        ctx: &mut InvestigationContext,
        _tools: &ToolRegistry,
    ) -> Result<SkillOutcome, KernelError> {
        let wiki_dir_str = ctx
            .evidence
            .iter()
            .filter_map(|ev| {
                ev.detail
                    .get("wiki_dir")
                    .and_then(|d| d.as_str())
                    .map(String::from)
            })
            .next();

        let wiki_dir = match wiki_dir_str {
            Some(dir) => PathBuf::from(dir),
            None => {
                return Ok(SkillOutcome::noop());
            }
        };

        let notes_val = ctx
            .evidence
            .iter()
            .filter_map(|ev| ev.detail.get("notes").cloned())
            .next();

        let pages = if let Some(notes_json) = notes_val {
            let notes = Self::notes_from_json(&notes_json)
                .map_err(|e| KernelError::SkillFailed(e.to_string()))?;
            let structure = WikiStructure::new(&wiki_dir);
            structure
                .ensure_directories()
                .map_err(|e| KernelError::SkillFailed(e.to_string()))?;

            let log = WikiLog::new(&wiki_dir);
            log.append(
                "skill_compile_start",
                &format!("compiling {} notes", notes.len()),
            )
            .map_err(|e| KernelError::SkillFailed(e.to_string()))?;

            let mut compiled = Vec::with_capacity(notes.len());
            for note in &notes {
                let page = self
                    .note_to_page(note)
                    .map_err(|e| KernelError::SkillFailed(e.to_string()))?;
                let full_path = wiki_dir.join(&page.path);
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| KernelError::SkillFailed(e.to_string()))?;
                }
                let rendered = self.render_page(&page);
                std::fs::write(&full_path, &rendered)
                    .map_err(|e| KernelError::SkillFailed(e.to_string()))?;
                compiled.push(page);
            }

            if !compiled.is_empty() {
                let index = WikiIndex::new(&wiki_dir);
                index
                    .generate(&compiled)
                    .map_err(|e| KernelError::SkillFailed(e.to_string()))?;
            }

            log.append(
                "skill_compile_complete",
                &format!(
                    "{} pages written from {} notes",
                    compiled.len(),
                    notes.len()
                ),
            )
            .map_err(|e| KernelError::SkillFailed(e.to_string()))?;

            compiled
        } else {
            Vec::new()
        };

        let page_count = pages.len();
        info!(page_count, "Wiki compilation skill complete");

        Ok(SkillOutcome::noop().with_delta(if page_count > 0 { 0.05 } else { 0.0 }))
    }
}

// ── Notes-from-JSON helper ────────────────────────────────────────

impl WikiCompiler {
    fn notes_from_json(notes_val: &serde_json::Value) -> Result<Vec<Note>> {
        let notes_array = notes_val
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("expected notes array"))?;

        let mut notes = Vec::new();
        for note_val in notes_array {
            let note: Note = serde_json::from_value(note_val.clone())
                .map_err(|e| anyhow::anyhow!("failed to parse note: {e}"))?;
            notes.push(note);
        }
        Ok(notes)
    }
}

fn extract_title(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('#') {
            let heading = trimmed.trim_start_matches('#').trim();
            if !heading.is_empty() {
                return Some(heading.to_string());
            }
        }
    }
    None
}

pub(crate) fn strip_frontmatter(content: &str) -> String {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content.to_string();
    }

    let rest = &trimmed[3..];
    if let Some(end_pos) = rest.find("---") {
        let body = &rest[end_pos + 3..];
        body.trim_start().to_string()
    } else {
        content.to_string()
    }
}

fn classify_note(content: &str, tags: &[String]) -> NoteCategory {
    let content_lower = content.to_lowercase();

    for tag in tags {
        let tag_lower = tag.to_lowercase();
        if TECH_KEYWORDS.iter().any(|k| k.to_lowercase() == tag_lower) {
            return NoteCategory::Technology;
        }
    }

    for word in TECH_KEYWORDS {
        if content_lower.contains(&word.to_lowercase()) {
            return NoteCategory::Technology;
        }
    }

    NoteCategory::Concept
}

/// First substantive line of a note body, used as a compiled page's OKF
/// `description` when the source note carries none.
///
/// Skips lines that carry no prose — blank lines, ATX headings, fences,
/// thematic breaks, block quotes, tables and images — and collapses
/// internal whitespace so a wrapped sentence stays one line. The bound and
/// quote-escaping come from [`truncate_for_description`], shared with the
/// entity renderer so both description sources truncate identically.
///
/// Returns `None` when the body has no prose at all (an image-only or
/// table-only note): the field is then left empty rather than filled with a
/// placeholder, which is the honest signal the OKF lint reports on.
fn first_substantive_line(body: &str) -> Option<String> {
    body.lines()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && !line.starts_with('#')
                && !line.starts_with("```")
                && !line.starts_with("---")
                && !line.starts_with('>')
                && !line.starts_with('|')
                && !line.starts_with("![")
        })
        .map(|line| {
            let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
            truncate_for_description(&collapsed)
        })
}

fn truncate_for_description(s: &str) -> String {
    const MAX: usize = 100;
    if s.len() <= MAX {
        s.replace('"', "'").replace('\n', " ")
    } else {
        let truncated = s[..s
            .char_indices()
            .take(MAX)
            .last()
            .map(|(i, _)| i)
            .unwrap_or(MAX)]
            .replace('"', "'")
            .replace('\n', " ");
        format!("{truncated}…")
    }
}

pub(crate) fn slugify(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    let mut prev_dash = false;

    for c in title.to_lowercase().chars() {
        if c.is_alphanumeric() {
            slug.push(c);
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }

    slug.trim_matches('-').to_string()
}

/// Enforce the compile whitelist on one page (compile-hygiene ②): every
/// `[[target]]` not in `whitelist` is counted and — under
/// [`GhostlinkPolicy::Strip`] — rewritten to its plain text. Mutates the
/// page body and its `wikilinks` frontmatter list so the two stay
/// consistent.
fn enforce_ghostlinks(
    page: &mut WikiPage,
    whitelist: &HashSet<String>,
    policy: GhostlinkPolicy,
    report: &mut GhostlinkReport,
) {
    let original_links = std::mem::take(&mut page.wikilinks);
    let mut kept = Vec::with_capacity(original_links.len());

    for target in original_links {
        if whitelist.contains(&normalize_alias(&target)) {
            kept.push(target);
            continue;
        }
        match policy {
            GhostlinkPolicy::Warn => {
                report.warned += 1;
                kept.push(target);
            }
            GhostlinkPolicy::Strip => {
                let needle = format!("[[{target}]]");
                let occurrences = page.content.matches(&needle).count();
                if occurrences > 0 {
                    page.content = page.content.replace(&needle, &target);
                }
                report.stripped += occurrences.max(1);
            }
        }
    }

    page.wikilinks = kept;
}

#[cfg(test)]
mod tests {
    use super::*;
    use zen_core::types::Sensitivity;

    fn make_test_note(content: &str, tags: Vec<String>) -> Note {
        let now = chrono::Utc::now();
        Note {
            id: uuid::Uuid::now_v7().to_string(),
            tags,
            source: "test".to_string(),
            source_id: None,
            sensitivity: Sensitivity::Private,
            created_at: now,
            updated_at: now,
            domain: vec![],
            project: None,
            para: None,
            okf_type: None,
            description: None,
            content: content.to_string(),
            file_path: None,
        }
    }

    fn make_test_note_with_id(id: &str, content: &str, tags: Vec<String>) -> Note {
        let now = chrono::Utc::now();
        Note {
            id: id.to_string(),
            tags,
            source: "test".to_string(),
            source_id: None,
            sensitivity: Sensitivity::Private,
            created_at: now,
            updated_at: now,
            domain: vec![],
            project: None,
            para: None,
            okf_type: None,
            description: None,
            content: content.to_string(),
            file_path: None,
        }
    }

    #[test]
    fn test_compile_creates_wiki_directory_structure() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note("# Hello World\n\nContent", vec![])];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert_eq!(pages.len(), 1);

        assert!(dir.path().join("notions/technology").exists());
        assert!(dir.path().join("notions/concepts").exists());
        assert!(dir.path().join("sources").exists());
    }

    #[test]
    fn test_compile_extracts_title_from_heading() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note("# My Page Title\n\nBody text", vec![])];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert_eq!(pages[0].title, "My Page Title");
    }

    #[test]
    fn test_compile_falls_back_to_id_when_no_heading() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let note = make_test_note_with_id("abc-123", "Just plain content", vec![]);
        let notes = vec![note];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert_eq!(pages[0].title, "abc-123");
    }

    #[test]
    fn test_compile_extracts_wikilinks() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        // Targets must exist (this run's creates) — compile-hygiene ②
        // strips links whose targets are absent from the whitelist.
        let notes = vec![
            make_test_note("See [[Rust]] and [[Tokio]] for details.", vec![]),
            make_test_note("# Rust\n\nContent.", vec![]),
            make_test_note("# Tokio\n\nContent.", vec![]),
        ];

        let (pages, report) = compiler
            .compile_with_policy(&notes, dir.path(), GhostlinkPolicy::Strip, None)
            .unwrap();
        assert_eq!(report.ghostlinks.total(), 0);
        assert_eq!(pages[0].wikilinks, vec!["Rust", "Tokio"]);
    }

    #[test]
    fn test_compile_copies_tags_from_note() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note(
            "# Tagged Note\n\nContent",
            vec!["tag1".into(), "tag2".into()],
        )];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert_eq!(pages[0].tags, vec!["tag1", "tag2"]);
    }

    #[test]
    fn test_compile_copies_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let now = chrono::Utc::now();
        let note = Note {
            id: "ts-note".to_string(),
            tags: vec![],
            source: "test".to_string(),
            source_id: None,
            sensitivity: Sensitivity::Private,
            created_at: now,
            updated_at: now,
            domain: vec![],
            project: None,
            para: None,
            okf_type: None,
            description: None,
            content: "# TS Note\n\nBody".to_string(),
            file_path: None,
        };

        let pages = compiler.compile(&[note], dir.path()).unwrap();
        assert_eq!(pages[0].created_at, now);
        assert_eq!(pages[0].updated_at, now);
    }

    #[test]
    fn test_compile_classifies_tech_entity() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note(
            "# Rust Performance\n\nRust is fast.",
            vec![],
        )];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert!(
            pages[0]
                .path
                .to_string_lossy()
                .starts_with("notions/technology/")
        );
    }

    #[test]
    fn test_compile_classifies_concept() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note(
            "# Weekly Reflection\n\nThinking about life.",
            vec![],
        )];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert!(
            pages[0]
                .path
                .to_string_lossy()
                .starts_with("notions/concepts/")
        );
    }

    #[test]
    fn test_compile_classifies_by_tag_keyword() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note(
            "# Some General Note\n\nContent here.",
            vec!["rust".into()],
        )];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert!(
            pages[0]
                .path
                .to_string_lossy()
                .starts_with("notions/technology/")
        );
    }

    #[test]
    fn test_compile_empty_notes_returns_empty_vec() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let pages = compiler.compile(&[], dir.path()).unwrap();
        assert!(pages.is_empty());
    }

    #[test]
    fn test_compile_strips_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let content = "---\nid: \"existing\"\n---\n\n# After Frontmatter\n\nBody text.";
        let notes = vec![make_test_note(content, vec![])];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert!(!pages[0].content.contains("---"));
        assert!(pages[0].content.contains("After Frontmatter"));
    }

    #[test]
    fn test_compile_writes_files_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note("# Disk Page\n\nContent", vec![])];

        compiler.compile(&notes, dir.path()).unwrap();

        // Find the written file (path depends on classification)
        for entry in walkdir::WalkDir::new(dir.path())
            .min_depth(2)
            .max_depth(3)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.path().extension().is_some_and(|e| e == "md") {
                let content = std::fs::read_to_string(entry.path()).unwrap();
                assert!(content.contains("Disk Page"));
                assert!(content.contains("---")); // frontmatter written by render
                return;
            }
        }
        panic!("No wiki .md file found on disk");
    }

    #[test]
    fn test_compile_generates_index() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![
            make_test_note("# Page One\n\nOne", vec![]),
            make_test_note("# Page Two\n\nTwo", vec![]),
        ];

        compiler.compile(&notes, dir.path()).unwrap();

        let index_path = dir.path().join("index.md");
        assert!(index_path.exists());

        let index_content = std::fs::read_to_string(&index_path).unwrap();
        assert!(index_content.contains("Knowledge Index"));
        assert!(index_content.contains("Page One"));
        assert!(index_content.contains("Page Two"));
    }

    #[test]
    fn test_compile_appends_to_log() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note("# Log Test\n\nContent", vec![])];

        compiler.compile(&notes, dir.path()).unwrap();

        let log_path = dir.path().join("log.md");
        assert!(log_path.exists());

        let log_content = std::fs::read_to_string(&log_path).unwrap();
        assert!(log_content.contains("compile_start"));
        assert!(log_content.contains("page_created"));
        assert!(log_content.contains("compile_complete"));
    }

    #[test]
    fn test_compile_multiple_notes_to_different_categories() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![
            make_test_note("# Rust Guide\n\nRust is great", vec![]),
            make_test_note("# Life Lessons\n\nJust thinking", vec![]),
        ];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert_eq!(pages.len(), 2);

        let has_entity = pages
            .iter()
            .any(|p| p.path.to_string_lossy().starts_with("notions/technology/"));
        let has_concept = pages
            .iter()
            .any(|p| p.path.to_string_lossy().starts_with("notions/concepts/"));
        assert!(has_entity, "should have an notion page");
        assert!(has_concept, "should have a concept page");
    }

    // ── Helper function tests ───────────────────────────────────

    #[test]
    fn test_extract_title_single_heading() {
        assert_eq!(
            extract_title("# My Title\n\nBody"),
            Some("My Title".to_string())
        );
    }

    #[test]
    fn test_extract_title_h2_fallback() {
        assert_eq!(
            extract_title("## Secondary Title\n\nBody"),
            Some("Secondary Title".to_string())
        );
    }

    #[test]
    fn test_extract_title_no_heading() {
        assert_eq!(extract_title("Just a paragraph"), None);
    }

    #[test]
    fn test_extract_title_empty_heading() {
        assert_eq!(extract_title("# \n\nBody"), None);
    }

    #[test]
    fn test_strip_frontmatter_with_fm() {
        let input = "---\nid: \"abc\"\ntitle: \"test\"\n---\n\nBody content here.";
        let result = strip_frontmatter(input);
        assert_eq!(result, "Body content here.");
        assert!(!result.contains("---"));
    }

    #[test]
    fn test_strip_frontmatter_no_fm() {
        let input = "No frontmatter here.\nJust markdown.";
        let result = strip_frontmatter(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_strip_frontmatter_unclosed() {
        let input = "---\nid: unclosed\nBody without close";
        let result = strip_frontmatter(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_classify_note_technology_by_keyword() {
        assert_eq!(
            classify_note("I love using Rust for systems.", &[]),
            NoteCategory::Technology
        );
    }

    #[test]
    fn test_classify_note_technology_by_tag() {
        assert_eq!(
            classify_note("Some content here.", &["docker".to_string()]),
            NoteCategory::Technology
        );
    }

    #[test]
    fn test_classify_note_concept_fallback() {
        assert_eq!(
            classify_note("Reflecting on personal growth.", &["journal".to_string()]),
            NoteCategory::Concept
        );
    }

    #[test]
    fn test_slugify_simple() {
        assert_eq!(slugify("Hello World"), "hello-world");
    }

    #[test]
    fn test_slugify_special_chars() {
        assert_eq!(slugify("What's New in 2024?"), "what-s-new-in-2024");
    }

    #[test]
    fn test_slugify_collapse_dashes() {
        assert_eq!(slugify("Hello   World"), "hello-world");
    }

    #[test]
    fn test_slugify_trim_edges() {
        assert_eq!(slugify("!Hello World!"), "hello-world");
    }

    #[test]
    fn test_slugify_uuid_fallback() {
        assert_eq!(slugify("abc-123-def"), "abc-123-def");
    }

    #[test]
    fn test_note_to_page_basic() {
        let compiler = WikiCompiler::new();
        let note = make_test_note("# Test Page\n\nContent here.", vec!["tag1".into()]);
        let page = compiler.note_to_page(&note).unwrap();

        assert_eq!(page.title, "Test Page");
        assert_eq!(page.tags, vec!["tag1"]);
        assert!(!page.content.contains("---"));
    }

    /// Compile-hygiene ① acceptance, pinned at the source: a page compiled from
    /// an ordinary note must carry BOTH OKF required keys. Before the fix the
    /// compiler emitted `description` never and `type` only when the source
    /// note had one, so every compiled page was reported as non-conforming on
    /// every lint run.
    #[test]
    fn compiled_note_page_always_carries_okf_type_and_description() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let note = make_test_note(
            "---\nid: \"n1\"\n---\n\n# Heading\n\nThe first real sentence carries the summary.\n",
            vec![],
        );

        let pages = compiler.compile(&[note], dir.path()).unwrap();
        let page = &pages[0];

        let okf_type = page.okf_type.as_deref().expect("type always set");
        assert!(!okf_type.trim().is_empty(), "type must be non-empty");
        assert_eq!(
            page.description.as_deref(),
            Some("The first real sentence carries the summary."),
            "description is the first substantive line — the heading is skipped"
        );

        let rendered = compiler.render_page(page);
        assert!(rendered.contains(&format!("\ntype: \"{okf_type}\"")));
        assert!(
            rendered.contains("\ndescription: \"The first real sentence carries the summary.\"")
        );
    }

    #[test]
    fn note_frontmatter_type_and_description_win_over_derivation() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        // Parsed, not hand-built: this also pins the frontmatter arm.
        let note = crate::note::parse_frontmatter(
            "---\nid: \"n2\"\ntype: \"reference\"\ndescription: \"Hand-authored summary.\"\n---\n\nBody text here.\n",
        )
        .unwrap();

        let pages = compiler.compile(&[note], dir.path()).unwrap();
        assert_eq!(pages[0].okf_type.as_deref(), Some("reference"));
        assert_eq!(
            pages[0].description.as_deref(),
            Some("Hand-authored summary."),
            "an explicit description must not be overwritten by the derived one"
        );
    }

    #[test]
    fn body_only_note_still_gets_a_description() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let note = make_test_note("Bare prose with no frontmatter at all.", vec![]);

        let pages = compiler.compile(&[note], dir.path()).unwrap();
        assert_eq!(
            pages[0].description.as_deref(),
            Some("Bare prose with no frontmatter at all.")
        );
    }

    #[test]
    fn freshly_compiled_pages_pass_the_okf_conformance_scan() {
        // The design record's acceptance metric, asserted against the real
        // lint rather than the renderer's own output.
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let note = make_test_note(
            "---\nid: \"n4\"\n---\n\nA plain note about a compiled topic.\n",
            vec![],
        );
        compiler.compile(&[note], dir.path()).unwrap();

        let result = crate::tindy::lint::Linter::new()
            .run_with_okf(dir.path(), true)
            .unwrap();
        assert!(
            result.okf_missing.is_empty(),
            "a freshly compiled page must satisfy the OKF required keys, got: {:?}",
            result.okf_missing
        );
    }

    #[test]
    fn test_render_page_includes_frontmatter() {
        let compiler = WikiCompiler::new();
        let page = WikiPage {
            title: "Rendered Page".to_string(),
            path: PathBuf::from("concepts/rendered-page.md"),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            tags: vec!["tag1".into()],
            wikilinks: vec!["Link1".into()],
            para: None,
            okf_type: None,
            description: None,
            sources: Vec::new(),
            content: "Body content".to_string(),
        };

        let rendered = compiler.render_page(&page);
        assert!(rendered.contains("title: \"Rendered Page\""));
        assert!(rendered.contains("tags: [\"tag1\"]"));
        assert!(rendered.contains("wikilinks: [\"Link1\"]"));
        assert!(rendered.contains("Body content"));
    }

    // ── Ghostlink whitelist (compile-hygiene ②) ─────────────────

    #[test]
    fn ghost_stripped_to_plain_text_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note(
            "# Source\n\nSee [[Hallucinated Thing]] for details.",
            vec![],
        )];

        let (pages, report) = compiler
            .compile_with_policy(&notes, dir.path(), GhostlinkPolicy::Strip, None)
            .unwrap();
        assert_eq!(report.ghostlinks.stripped, 1);
        assert_eq!(report.ghostlinks.warned, 0);
        assert!(pages[0].wikilinks.is_empty());
        assert!(
            pages[0]
                .content
                .contains("See Hallucinated Thing for details.")
        );
        assert!(!pages[0].content.contains("[["));
    }

    #[test]
    fn link_to_same_run_create_survives() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![
            make_test_note("# Pointer\n\nRead [[Rust Guide]] first.", vec![]),
            make_test_note("# Rust Guide\n\nRust content here.", vec![]),
        ];

        let (pages, report) = compiler
            .compile_with_policy(&notes, dir.path(), GhostlinkPolicy::Strip, None)
            .unwrap();
        assert_eq!(report.ghostlinks.total(), 0, "report: {report:?}");
        let pointer = pages
            .iter()
            .find(|p| p.title == "Pointer")
            .expect("pointer page");
        assert_eq!(pointer.wikilinks, vec!["Rust Guide"]);
    }

    #[test]
    fn link_to_preexisting_page_survives() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();

        // Pre-existing page (from an earlier compile run), slugified stem.
        let prev_dir = dir.path().join("notions/concepts");
        std::fs::create_dir_all(&prev_dir).unwrap();
        std::fs::write(prev_dir.join("rust-guide.md"), "# Rust Guide\n\nold").unwrap();

        let notes = vec![make_test_note(
            "# New Note\n\nBack to [[Rust Guide]].",
            vec![],
        )];
        let (pages, report) = compiler
            .compile_with_policy(&notes, dir.path(), GhostlinkPolicy::Strip, None)
            .unwrap();
        assert_eq!(
            report.ghostlinks.total(),
            0,
            "de-slugified title link must survive"
        );
        assert_eq!(pages[0].wikilinks, vec!["Rust Guide"]);

        let notes2 = vec![make_test_note(
            "# New Note\n\nBack to [[rust-guide]].",
            vec![],
        )];
        let (pages2, report2) = compiler
            .compile_with_policy(&notes2, dir.path(), GhostlinkPolicy::Strip, None)
            .unwrap();
        assert_eq!(
            report2.ghostlinks.total(),
            0,
            "dashed stem link must survive"
        );
        assert_eq!(pages2[0].wikilinks, vec!["rust-guide"]);
    }

    #[test]
    fn nfc_variant_of_valid_target_not_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();

        // Create "Café" (precomposed NFC) this run; link with the
        // decomposed NFD spelling — normalize_alias NFCs both sides.
        let nfc = "Caf\u{e9}";
        let nfd = "Cafe\u{301}";
        let notes = vec![
            make_test_note(&format!("# Note\n\nSee [[{nfd}]] now."), vec![]),
            make_test_note(&format!("# {nfc}\n\nCoffee content."), vec![]),
        ];

        let (pages, report) = compiler
            .compile_with_policy(&notes, dir.path(), GhostlinkPolicy::Strip, None)
            .unwrap();
        assert_eq!(
            report.ghostlinks.total(),
            0,
            "NFC equivalence must hold: {report:?}"
        );
        let pointer = pages.iter().find(|p| p.title == "Note").unwrap();
        assert_eq!(pointer.wikilinks.len(), 1);
    }

    #[test]
    fn warn_policy_keeps_link_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note(
            "# Source\n\nSee [[Hallucinated Thing]] for details.",
            vec![],
        )];

        let (pages, report) = compiler
            .compile_with_policy(&notes, dir.path(), GhostlinkPolicy::Warn, None)
            .unwrap();
        assert_eq!(report.ghostlinks.warned, 1);
        assert_eq!(report.ghostlinks.stripped, 0);
        assert_eq!(pages[0].wikilinks, vec!["Hallucinated Thing"]);
        assert!(pages[0].content.contains("[[Hallucinated Thing]]"));
    }

    #[test]
    fn bare_compile_defaults_to_strip() {
        let dir = tempfile::tempdir().unwrap();
        let compiler = WikiCompiler::new();
        let notes = vec![make_test_note("# Source\n\nSee [[Ghost]] now.", vec![])];

        let pages = compiler.compile(&notes, dir.path()).unwrap();
        assert!(pages[0].wikilinks.is_empty());
        assert!(pages[0].content.contains("See Ghost now."));
    }

    #[test]
    fn e2_rendered_page_carries_sources_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let source = inbox.join("provenanced.md");
        std::fs::write(&source, "# Provenanced\n\nBody text.").unwrap();

        let mut note = make_test_note("# Provenanced\n\nBody text.", vec![]);
        note.file_path = Some(source.clone());

        let compiler = WikiCompiler::new();
        let (pages, _) = compiler
            .compile_with_policy(
                &[note],
                dir.path().join("wiki").as_path(),
                GhostlinkPolicy::Strip,
                None,
            )
            .unwrap();
        assert_eq!(pages[0].sources, vec![source.to_string_lossy().to_string()]);

        // A note without a file path carries no provenance — no sources key.
        let anon = make_test_note("# Anonymous\n\nBody.", vec![]);
        let (pages2, _) = compiler
            .compile_with_policy(
                &[anon],
                dir.path().join("wiki2").as_path(),
                GhostlinkPolicy::Strip,
                None,
            )
            .unwrap();
        assert!(pages2[0].sources.is_empty());
        let rendered = compiler.render_page(&pages2[0]);
        assert!(!rendered.contains("sources:"));
    }

    #[test]
    fn e5_compile_capture_overwrites_not_creates() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = dir.path().join("wiki");
        std::fs::create_dir_all(&wiki).unwrap();
        let store = crate::wiki::PageIterations::new(dir.path().join("iterations"));

        // Pre-existing page: the recompile must capture its prior bytes.
        let existing_rel = PathBuf::from("notions/concepts").join("rendered-page.md");
        let existing = wiki.join(&existing_rel);
        std::fs::create_dir_all(existing.parent().unwrap()).unwrap();
        std::fs::write(&existing, "# Rendered Page\n\nhand-tuned\n").unwrap();

        // A page that does not exist yet must NOT be captured (a create
        // has nothing to protect).
        let compiler = WikiCompiler::new();
        let note = make_test_note("# Rendered Page\n\npipeline v2\n", vec![]);
        let (pages, report) = compiler
            .compile_with_policy(&[note], &wiki, GhostlinkPolicy::Strip, Some(&store))
            .unwrap();

        assert_eq!(report.iterations.len(), 1, "only the overwrite captured");
        assert_eq!(pages[0].path, existing_rel);
        let captured = &report.iterations[0];
        assert_eq!(captured.page_rel, existing_rel);
        let stored = std::fs::read_to_string(&captured.md_path).unwrap();
        assert_eq!(stored, "# Rendered Page\n\nhand-tuned\n");
        assert!(
            captured.diff_path.as_ref().is_some_and(|d| d.exists()),
            "diff sidecar written"
        );
        // The store lives OUTSIDE the wiki dir — inventory consumers
        // (whitelist/lint/CAS) must never enumerate iteration files.
        assert!(store.root().starts_with(dir.path()));
        assert!(!store.root().starts_with(&wiki));
    }

    #[test]
    fn ghost_policy_parse_degrades_to_strip() {
        assert_eq!(GhostlinkPolicy::parse("strip"), GhostlinkPolicy::Strip);
        assert_eq!(GhostlinkPolicy::parse("WARN"), GhostlinkPolicy::Warn);
        assert_eq!(GhostlinkPolicy::parse(""), GhostlinkPolicy::Strip);
        assert_eq!(GhostlinkPolicy::parse("stip"), GhostlinkPolicy::Strip);
    }
}
