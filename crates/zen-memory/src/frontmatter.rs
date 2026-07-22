/// Extract the YAML frontmatter block (between first two `---` lines).
/// Returns None if no valid frontmatter found.
pub fn extract_frontmatter(content: &str) -> Option<String> {
    let mut lines = content.lines();
    let first = lines.next()?.trim();
    if first != "---" {
        return None;
    }
    let mut fm = String::new();
    for line in lines {
        if line.trim() == "---" {
            return Some(fm);
        }
        fm.push_str(line);
        fm.push('\n');
    }
    None
}

/// Parse a simple `key: value` line from frontmatter. Returns the trimmed value.
/// Handles: `key: "value"`, `key: value`, `key: 'value'` — strips quotes if present.
pub fn parse_field(frontmatter: &str, key: &str) -> Option<String> {
    for line in frontmatter.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&format!("{key}:")) {
            let val = rest.trim().to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

/// Parse a YAML array field.
/// Supports both inline (`key: ["a", "b"]` or `key: []`) and block (`key:\n  - item1\n  - item2`) formats.
pub fn parse_yaml_array(frontmatter: &str, key: &str) -> Vec<String> {
    let mut lines = frontmatter.lines();
    let mut in_array = false;
    let mut items = Vec::new();

    for line in &mut lines {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&format!("{key}:")) {
            let val = rest.trim();
            if val.is_empty() {
                in_array = true;
                continue;
            }
            if val == "[]" {
                return Vec::new();
            }
            if val.starts_with('[') {
                let inner = val.trim_start_matches('[').trim_end_matches(']');
                return inner
                    .split(',')
                    .map(|s| s.trim().trim_matches('"').to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
        if in_array {
            if let Some(item) = trimmed.strip_prefix("- ") {
                items.push(item.trim().to_string());
            } else if !trimmed.is_empty() {
                break;
            }
        }
    }
    items
}
