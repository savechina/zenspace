use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use zen_core::paths::{ZenPaths, agents_skills_under, user_agents_skills_dir};

/// A skill definition parsed from a Markdown file with YAML frontmatter.
///
/// File format (`~/.zen/skills/<name>.md` or `~/.zen/skills/<name>/SKILL.md`,
/// the D02 precipitation layout):
/// ```text
/// ---
/// name: weekly-review
/// description: Run a weekly knowledge review
/// triggers: [weekly review, review my week]
/// auto_route: true
/// tools: [search, wiki]
/// context_files:
///   - wiki/review-template.md
/// prompt: "Review the past week..."
/// ---
/// # Skill body markdown
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillDefinition {
    pub name: String,
    pub description: String,
    /// Pi-style trigger phrases matched by SkillHitRouter (FR-037).
    #[serde(default)]
    pub triggers: Vec<String>,
    /// Per-skill auto-route opt-out (`[skills.auto_route]` global switch).
    /// Absent → inherit the global enabled default (true).
    #[serde(default)]
    pub auto_route: Option<bool>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub context_files: Vec<PathBuf>,
    #[serde(default)]
    pub prompt: String,
    pub body: String,
}

impl SkillDefinition {
    /// Effective per-skill auto-route eligibility: explicit frontmatter
    /// `auto_route: false` opts out; absent/true stays eligible.
    pub fn is_auto_route_enabled(&self) -> bool {
        self.auto_route.unwrap_or(true)
    }
}

/// Loads skill definitions from Markdown files across ordered discovery
/// roots (Phase 31 T212, owner decision D-E — zen-native first):
///
/// 1. `~/.zen/skills` (zen-native: precipitated + exported skills),
/// 2. project `.agents/skills` — walked UP from the workspace root,
///    closest-to-cwd first, stopping at the repo root (a directory
///    containing `.git`) or the filesystem root,
/// 3. `~/.agents/skills` (user scope — the vendor-neutral directory
///    codex-style and pi-style CLIs share).
///
/// Same-name collisions across roots resolve FIRST-WINS with a
/// `tracing::warn!` naming both paths (pi precedent) — an override is
/// always observable, never silent.
pub struct SkillLoader {
    roots: Vec<PathBuf>,
}

/// Collect the project-scope `.agents/skills` roots by walking UP from
/// `start`: closest-to-cwd first, stopping after the first directory
/// containing a `.git` marker (the repo root) or at the filesystem root.
/// Non-existent candidates are kept — consumers skip them — so a later
/// `mkdir` needs no loader rebuild.
fn project_skill_roots(start: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut dir = Some(start);
    while let Some(d) = dir {
        roots.push(agents_skills_under(d));
        if d.join(".git").exists() {
            break;
        }
        dir = d.parent();
    }
    roots
}

/// Build the ordered discovery roots (D-E precedence). Pure — pinned by
/// tests; `SkillLoader::new` injects the live home directory.
fn build_roots(
    zen_skills: PathBuf,
    workspace_root: Option<&PathBuf>,
    user_home_agents: Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut roots = vec![zen_skills];
    if let Some(ws) = workspace_root {
        roots.extend(project_skill_roots(ws));
    }
    if let Some(agents) = user_home_agents {
        roots.push(agents);
    }
    roots
}

impl SkillLoader {
    pub fn new(paths: &ZenPaths) -> Self {
        Self {
            roots: build_roots(
                paths.skills(),
                paths.workspace_root(),
                user_agents_skills_dir(),
            ),
        }
    }

    /// Loader over an explicit SINGLE directory (report-only surfaces such
    /// as the skill trigger grader that evaluate zen-confirmed skills only —
    /// deliberately NOT multi-root, Phase 31 T212).
    pub fn new_from_dir(skills_dir: PathBuf) -> Self {
        Self {
            roots: vec![skills_dir],
        }
    }

    /// The ordered discovery roots (highest precedence first).
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// List all available skill names across every root.
    ///
    /// Discovers both the flat layout (`<name>.md`) and the D02 precipitation
    /// layout (`<name>/SKILL.md`); the name is the file stem / directory name.
    /// Roots are merged in precedence order with first-wins dedup: a name
    /// shadowed by an earlier root keeps that root and the collision is
    /// warned with both paths (D-E).
    pub fn list_skills(&self) -> anyhow::Result<Vec<String>> {
        let mut skills = Vec::new();
        let mut winner: HashMap<String, &Path> = HashMap::new();

        for root in &self.roots {
            if !root.is_dir() {
                continue;
            }
            for entry in fs::read_dir(root)? {
                let entry = entry?;
                let path = entry.path();

                let name = if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .map(str::to_string)
                } else if path.is_dir() && path.join("SKILL.md").is_file() {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .map(str::to_string)
                } else {
                    None
                };
                let Some(name) = name else { continue };

                match winner.get(&name) {
                    Some(kept) => {
                        warn!(
                            skill = %name,
                            kept = %kept.display(),
                            shadowed = %root.display(),
                            "skill name collision across roots — first root wins (D-E)"
                        );
                    }
                    None => {
                        winner.insert(name.clone(), root.as_path());
                        skills.push(name);
                    }
                }
            }
        }

        skills.sort();
        debug!(
            count = skills.len(),
            roots = self.roots.len(),
            "listed skills"
        );
        Ok(skills)
    }

    /// Resolve the on-disk file of `name` by probing roots in precedence
    /// order: within each root the flat `<name>.md` layout wins over the D02
    /// `<name>/SKILL.md` layout (the pre-T212 single-root order).
    fn find_skill_file(&self, name: &str) -> Option<PathBuf> {
        self.roots.iter().find_map(|root| {
            let flat = root.join(format!("{name}.md"));
            if flat.is_file() {
                return Some(flat);
            }
            let nested = root.join(name).join("SKILL.md");
            nested.is_file().then_some(nested)
        })
    }

    /// Load a skill definition by name (first root containing it wins).
    ///
    /// Tries the flat `<name>.md` first, then the D02 `<name>/SKILL.md`.
    pub fn load_skill(&self, name: &str) -> anyhow::Result<SkillDefinition> {
        let path = self.find_skill_file(name).ok_or_else(|| {
            anyhow::anyhow!(
                "skill '{name}' not found in any root ({})",
                self.roots
                    .iter()
                    .map(|r| r.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        let content = fs::read_to_string(&path)?;

        parse_skill_file(name, &content)
    }

    /// Check if a skill file exists (either layout, any root).
    pub fn skill_exists(&self, name: &str) -> bool {
        self.find_skill_file(name).is_some()
    }
}

/// Parse a skill file's content into a `SkillDefinition`.
///
/// Splits on `---` delimiters to separate YAML-like frontmatter from the Markdown body.
/// Parses frontmatter key-value pairs manually (avoids a YAML dependency).
fn parse_skill_file(name: &str, content: &str) -> anyhow::Result<SkillDefinition> {
    let trimmed = content.trim();

    // Find the opening `---`
    let after_open = trimmed
        .strip_prefix("---")
        .ok_or_else(|| {
            anyhow::anyhow!("skill file '{name}' missing opening --- frontmatter delimiter")
        })?
        .trim_start();

    // Find the closing `---`
    let (frontmatter_str, body) = match after_open.find("\n---") {
        Some(idx) => {
            let fm = &after_open[..idx];
            let rest = after_open[idx + 4..].trim_start();
            (fm, rest.to_string())
        }
        None => {
            // Try at the very start (empty frontmatter case)
            return Ok(SkillDefinition {
                name: name.to_string(),
                description: String::new(),
                triggers: Vec::new(),
                auto_route: None,
                tools: Vec::new(),
                context_files: Vec::new(),
                prompt: String::new(),
                body: trimmed.to_string(),
            });
        }
    };

    // Parse frontmatter key-value pairs
    let mut fm_name = None;
    let mut fm_description = String::new();
    let mut fm_triggers: Vec<String> = Vec::new();
    let mut fm_auto_route: Option<bool> = None;
    let mut fm_tools = Vec::new();
    let mut fm_context_files = Vec::new();
    let mut fm_prompt = String::new();

    let mut current_key: Option<String> = None;

    for line in frontmatter_str.lines() {
        let trimmed_line = line.trim();

        // Skip blank lines
        if trimmed_line.is_empty() {
            continue;
        }

        // Check for a top-level key: value pair
        if let Some(colon_pos) = trimmed_line.find(':') {
            let key = trimmed_line[..colon_pos].trim().to_string();
            let value = trimmed_line[colon_pos + 1..].trim().to_string();

            // If this is a new key-value pair (not an indented list item)
            if !trimmed_line.starts_with(' ') && !trimmed_line.starts_with('-') {
                // Flush previous key's multiline content
                if let Some("prompt") = current_key.as_deref() {}

                current_key = Some(key.clone());

                match key.as_str() {
                    "name" => fm_name = Some(value.trim_matches('"').to_string()),
                    "description" => fm_description = value.trim_matches('"').to_string(),
                    "triggers" => {
                        // Parse `[a, b]` syntax; empty value → indented list below
                        let triggers = parse_array_bracket(&value);
                        if !triggers.is_empty() {
                            fm_triggers = triggers;
                        }
                    }
                    "auto_route" => {
                        fm_auto_route = value.parse::<bool>().ok();
                    }
                    "tools" => {
                        // Parse `[a, b]` syntax
                        let tools = parse_array_bracket(&value);
                        if !tools.is_empty() {
                            fm_tools = tools;
                        }
                        // If empty value, expect indented list items below
                    }
                    "context_files" => {
                        // If empty value, expect indented list items below
                    }
                    "prompt" => {
                        fm_prompt = value.trim_matches('"').to_string();
                    }
                    _ => {}
                }
                continue;
            }
        }

        // Handle indented list items (e.g., "  - wiki/review-template.md")
        if let Some(item) = trimmed_line.strip_prefix("- ") {
            let item_value = item.trim().trim_matches('"').to_string();
            match current_key.as_deref() {
                Some("triggers") => {
                    fm_triggers.push(item_value);
                }
                Some("tools") => {
                    fm_tools.push(item_value);
                }
                Some("context_files") => {
                    fm_context_files.push(PathBuf::from(item_value));
                }
                Some("prompt") => {
                    if !fm_prompt.is_empty() {
                        fm_prompt.push('\n');
                    }
                    fm_prompt.push_str(trimmed_line);
                }
                _ => {}
            }
            continue;
        }

        // Handle continuation lines for prompt (indented, non-list)
        if !trimmed_line.is_empty() {
            match current_key.as_deref() {
                Some("prompt") => {
                    if !fm_prompt.is_empty() {
                        fm_prompt.push(' ');
                    }
                    fm_prompt.push_str(trimmed_line);
                }
                Some("description") => {
                    if fm_description.is_empty() {
                        fm_description = trimmed_line.to_string();
                    } else {
                        fm_description.push(' ');
                        fm_description.push_str(trimmed_line);
                    }
                }
                _ => {}
            }
        }
    }

    Ok(SkillDefinition {
        name: fm_name.unwrap_or_else(|| name.to_string()),
        description: fm_description,
        triggers: fm_triggers,
        auto_route: fm_auto_route,
        tools: fm_tools,
        context_files: fm_context_files,
        prompt: fm_prompt,
        body,
    })
}

/// Parse a bracket-delimited array like `[search, wiki]` into a Vec of strings.
fn parse_array_bracket(value: &str) -> Vec<String> {
    let stripped = value.trim();
    let inner = match stripped.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        Some(inner) => inner,
        None => return Vec::new(),
    };

    inner
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_list_skills() {
        let dir = tempdir().unwrap();
        let skills_dir = dir.path();

        // Create some skill files
        fs::write(
            skills_dir.join("alpha.md"),
            "---\nname: alpha\ndescription: A test\n---\nBody",
        )
        .unwrap();
        fs::write(
            skills_dir.join("beta.md"),
            "---\nname: beta\ndescription: B test\n---\nBody",
        )
        .unwrap();
        // Non-.md file should be ignored
        fs::write(skills_dir.join("gamma.txt"), "not a skill").unwrap();

        let loader = SkillLoader::new_from_dir(skills_dir.to_path_buf());
        let mut skills = loader.list_skills().unwrap();

        skills.sort();
        assert_eq!(skills, vec!["alpha", "beta"]);
    }

    #[test]
    fn test_load_skill_with_frontmatter() {
        let dir = tempdir().unwrap();
        let skills_dir = dir.path();
        fs::create_dir_all(skills_dir).unwrap();

        let skill_content = r#"---
name: weekly-review
description: Run a weekly knowledge review
tools: [search, wiki]
context_files:
  - wiki/review-template.md
prompt: "Review the past week..."
---
# Skill body

This is the skill body in Markdown."#;

        fs::write(skills_dir.join("weekly-review.md"), skill_content).unwrap();

        let loader = SkillLoader::new_from_dir(skills_dir.to_path_buf());
        let skill = loader.load_skill("weekly-review").unwrap();

        assert_eq!(skill.name, "weekly-review");
        assert_eq!(skill.description, "Run a weekly knowledge review");
        assert_eq!(skill.tools, vec!["search", "wiki"]);
        assert_eq!(
            skill.context_files,
            vec![PathBuf::from("wiki/review-template.md")]
        );
        assert_eq!(skill.prompt, "Review the past week...");
        assert!(skill.body.contains("# Skill body"));
    }

    fn write_skill(dir: &Path, name: &str, description: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join(format!("{name}.md")),
            format!("---\nname: {name}\ndescription: {description}\n---\nBody"),
        )
        .unwrap();
    }

    #[test]
    fn build_roots_orders_zen_native_first_project_closest_user_last() {
        let zen = PathBuf::from("/tmp/zen-skills");
        let ws = PathBuf::from("/tmp/ws-root");
        let user = PathBuf::from("/tmp/user-agents-skills");
        let roots = build_roots(zen.clone(), Some(&ws), Some(user.clone()));
        assert_eq!(roots[0], zen);
        // Project roots walk UP from the workspace root, closest first.
        assert_eq!(roots[1], ws.join(".agents").join("skills"));
        assert_eq!(*roots.last().unwrap(), user);
        // No workspace root ⇒ only zen-native + user scope.
        let roots = build_roots(zen.clone(), None, Some(user.clone()));
        assert_eq!(roots, vec![zen.clone(), user]);
        // No home ⇒ no user-scope root.
        let roots = build_roots(zen.clone(), None, None);
        assert_eq!(roots, vec![zen]);
    }

    #[test]
    fn project_walk_stops_at_repo_root_git_marker() {
        let base = tempdir().unwrap();
        let repo = base.path().join("repo");
        let nested = repo.join("crates").join("app");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap();

        let roots = project_skill_roots(&nested);
        assert_eq!(
            roots,
            vec![
                nested.join(".agents").join("skills"),
                repo.join("crates").join(".agents").join("skills"),
                repo.join(".agents").join("skills"),
            ],
            "walk must include the repo root itself and never its parent"
        );
    }

    #[test]
    fn project_walk_without_git_marker_stops_at_filesystem_root() {
        let base = tempdir().unwrap();
        let deep = base.path().join("a").join("b");
        fs::create_dir_all(&deep).unwrap();
        let roots = project_skill_roots(&deep);
        assert_eq!(
            roots.last().map(PathBuf::as_path),
            Some(Path::new("/").join(".agents").join("skills").as_path())
        );
        assert!(roots.len() >= 3);
    }

    #[test]
    fn multi_root_merge_is_first_wins_with_later_roots_still_discovered() {
        let zen_root = tempdir().unwrap();
        let project_root = tempdir().unwrap();
        let user_root = tempdir().unwrap();
        write_skill(zen_root.path(), "shared", "from-zen-native");
        write_skill(project_root.path(), "shared", "from-project");
        write_skill(project_root.path(), "project-only", "p");
        write_skill(user_root.path(), "user-only", "u");

        let loader = SkillLoader {
            roots: vec![
                zen_root.path().to_path_buf(),
                project_root.path().to_path_buf(),
                user_root.path().to_path_buf(),
            ],
        };
        let names = loader.list_skills().unwrap();
        assert_eq!(names, vec!["project-only", "shared", "user-only"]);
        // Collision: the FIRST root wins (D-E, pi precedent; warned at merge).
        let shared = loader.load_skill("shared").unwrap();
        assert_eq!(shared.description, "from-zen-native");
        assert!(loader.skill_exists("user-only"));
    }

    #[test]
    fn later_root_nested_skill_layout_is_discovered() {
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        let nested = second.path().join("zen-wiki");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("SKILL.md"),
            "---\nname: zen-wiki\ndescription: exported KB\n---\nBody",
        )
        .unwrap();

        let loader = SkillLoader {
            roots: vec![first.path().to_path_buf(), second.path().to_path_buf()],
        };
        assert_eq!(loader.list_skills().unwrap(), vec!["zen-wiki"]);
        assert_eq!(
            loader.load_skill("zen-wiki").unwrap().description,
            "exported KB"
        );
    }

    #[test]
    fn flat_layout_wins_over_nested_within_one_root() {
        let root = tempdir().unwrap();
        write_skill(root.path(), "dup", "flat");
        let nested = root.path().join("dup");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("SKILL.md"),
            "---\nname: dup\ndescription: nested\n---\nBody",
        )
        .unwrap();
        let loader = SkillLoader::new_from_dir(root.path().to_path_buf());
        assert_eq!(loader.load_skill("dup").unwrap().description, "flat");
    }

    #[test]
    fn new_from_dir_stays_single_root() {
        let own = tempdir().unwrap();
        let other = tempdir().unwrap();
        write_skill(own.path(), "alpha", "a");
        write_skill(other.path(), "beta", "b");

        let loader = SkillLoader::new_from_dir(own.path().to_path_buf());
        assert_eq!(
            loader.roots(),
            std::slice::from_ref(&own.path().to_path_buf())
        );
        assert_eq!(loader.list_skills().unwrap(), vec!["alpha"]);
        assert!(!loader.skill_exists("beta"));
    }

    #[test]
    fn load_missing_skill_error_names_every_probed_root() {
        let a = tempdir().unwrap();
        let b = tempdir().unwrap();
        let loader = SkillLoader {
            roots: vec![a.path().to_path_buf(), b.path().to_path_buf()],
        };
        let err = loader.load_skill("nope").unwrap_err().to_string();
        assert!(err.contains(&a.path().display().to_string()), "{err}");
        assert!(err.contains(&b.path().display().to_string()), "{err}");
    }

    #[test]
    fn missing_roots_are_skipped_not_fatal() {
        let real = tempdir().unwrap();
        write_skill(real.path(), "alpha", "a");
        let loader = SkillLoader {
            roots: vec![
                real.path().join("does-not-exist"),
                real.path().to_path_buf(),
            ],
        };
        assert_eq!(loader.list_skills().unwrap(), vec!["alpha"]);
    }
}
