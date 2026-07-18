use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::debug;
use zen_core::paths::ZenPaths;

/// A skill definition parsed from a Markdown file with YAML frontmatter.
///
/// File format (`~/.zen/skills/<name>.md`):
/// ```text
/// ---
/// name: weekly-review
/// description: Run a weekly knowledge review
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
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub context_files: Vec<PathBuf>,
    #[serde(default)]
    pub prompt: String,
    pub body: String,
}

/// Loads skill definitions from Markdown files in `~/.zen/skills/`.
pub struct SkillLoader {
    skills_dir: PathBuf,
}

impl SkillLoader {
    pub fn new(paths: &ZenPaths) -> Self {
        Self {
            skills_dir: paths.skills(),
        }
    }

    /// List all available skill names (file stems of `.md` files in the skills directory).
    pub fn list_skills(&self) -> anyhow::Result<Vec<String>> {
        let mut skills = Vec::new();

        if !self.skills_dir.is_dir() {
            return Ok(skills);
        }

        for entry in fs::read_dir(&self.skills_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|e| e.to_str()) == Some("md")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                skills.push(stem.to_string());
            }
        }

        skills.sort();
        debug!(count = skills.len(), "listed skills");
        Ok(skills)
    }

    /// Load a skill definition by name (without `.md` extension).
    pub fn load_skill(&self, name: &str) -> anyhow::Result<SkillDefinition> {
        let path = self.skills_dir.join(format!("{name}.md"));
        let content = fs::read_to_string(&path)?;

        parse_skill_file(name, &content)
    }

    /// Check if a skill file exists.
    pub fn skill_exists(&self, name: &str) -> bool {
        self.skills_dir.join(format!("{name}.md")).is_file()
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
        .ok_or_else(|| anyhow::anyhow!("skill file '{name}' missing opening --- frontmatter delimiter"))?
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
        fs::write(skills_dir.join("alpha.md"), "---\nname: alpha\ndescription: A test\n---\nBody").unwrap();
        fs::write(skills_dir.join("beta.md"), "---\nname: beta\ndescription: B test\n---\nBody").unwrap();
        // Non-.md file should be ignored
        fs::write(skills_dir.join("gamma.txt"), "not a skill").unwrap();

        let loader = SkillLoader { skills_dir: skills_dir.to_path_buf() };
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

        let loader = SkillLoader { skills_dir: skills_dir.to_path_buf() };
        let skill = loader.load_skill("weekly-review").unwrap();

        assert_eq!(skill.name, "weekly-review");
        assert_eq!(skill.description, "Run a weekly knowledge review");
        assert_eq!(skill.tools, vec!["search", "wiki"]);
        assert_eq!(skill.context_files, vec![PathBuf::from("wiki/review-template.md")]);
        assert_eq!(skill.prompt, "Review the past week...");
        assert!(skill.body.contains("# Skill body"));
    }
}
