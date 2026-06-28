const CONTENT_START: &str = "[USER_CONTENT_START]";
const CONTENT_END: &str = "[USER_CONTENT_END]";

#[derive(Debug, Clone)]
pub struct SanitizedContent {
    pub original: String,
    pub sanitized: String,
    pub stripped_patterns: Vec<String>,
}

pub struct InputSanitizer {
    custom_patterns: Vec<(String, String)>,
}

#[derive(Debug, thiserror::Error)]
pub enum SanitizeError {
    #[error("content blocked by safety filter: {reason}")]
    SafetyBlock { reason: String },
}

impl Default for InputSanitizer {
    fn default() -> Self {
        Self::new()
    }
}

fn has_double_dot_slash(content: &str) -> bool {
    content.contains("../") && {
        let parts: Vec<&str> = content.split("../").collect();
        parts.len() > 2
    }
}

fn strip_system_tags(content: &str) -> (String, bool) {
    let lower = content.to_lowercase();
    let mut stripped = false;

    let mut result = content.to_string();
    if lower.contains("<system>") || lower.contains("</system>") {
        result = result.replace("<system>", "");
        result = result.replace("</system>", "");
        result = result.replace("<SYSTEM>", "");
        result = result.replace("</SYSTEM>", "");
        result = result.replace("<System>", "");
        result = result.replace("</System>", "");
        stripped = true;
    }

    if result.contains('<') && result.to_lowercase().contains("system>") {
        let mut cleaned = String::new();
        let mut in_tag = false;
        let mut tag_buf = String::new();
        for ch in result.chars() {
            if ch == '<' {
                in_tag = true;
                tag_buf.clear();
                continue;
            }
            if in_tag {
                tag_buf.push(ch);
                if ch == '>' {
                    if tag_buf.to_lowercase().contains("system") {
                        stripped = true;
                        tag_buf.clear();
                    } else {
                        cleaned.push('<');
                        cleaned.push_str(&tag_buf);
                        tag_buf.clear();
                    }
                    in_tag = false;
                }
                continue;
            }
            cleaned.push(ch);
        }
        result = cleaned;
    }

    (result, stripped)
}

fn strip_role_overrides(content: &str) -> (String, bool) {
    let lower = content.to_lowercase();
    let role_prefixes = [
        "# system:",
        "# assistant:",
        "# developer:",
        "system:",
        "assistant:",
        "developer:",
        "### system",
        "### assistant",
        "### developer",
    ];

    let mut stripped = false;
    let mut result = content.to_string();

    for prefix in &role_prefixes {
        if lower.contains(prefix) {
            let lines: Vec<&str> = result.lines().collect();
            let filtered: Vec<&str> = lines
                .iter()
                .filter(|line| !line.to_lowercase().starts_with(prefix))
                .copied()
                .collect();
            result = filtered.join("\n");
            stripped = true;
        }
    }

    (result, stripped)
}

fn strip_shell_injection(content: &str) -> (String, bool) {
    let lower = content.to_lowercase();
    let mut stripped = false;
    let mut result = content.to_string();

    let shell_patterns = [
        "rm -rf",
        "sudo |",
        "wget |",
        "curl |",
        "wget |sh",
        "curl |sh",
        "wget |bash",
        "curl |bash",
    ];

    for pattern in &shell_patterns {
        if lower.contains(pattern) {
            stripped = true;
            result = result.replace(pattern, "");
        }
    }

    let piping_patterns = ["| sh", "|bash", "| sh\n"];

    for pattern in &piping_patterns {
        if result.contains(pattern) {
            stripped = true;
            result = result.replace(pattern, "");
        }
    }

    (result, stripped)
}

fn strip_privilege_escalation(content: &str) -> (String, bool) {
    let lower = content.to_lowercase();
    let mut stripped = false;
    let mut result = content.to_string();

    let priv_patterns = [
        "sudo passwd",
        "sudo visudo",
        "sudo usermod",
        "sudo chmod",
        "chmod 777 /",
        "/etc/passwd",
        "/etc/shadow",
        "visudo",
    ];

    for pattern in &priv_patterns {
        if lower.contains(pattern) {
            stripped = true;
            result = result.replace(pattern, "");
        }
    }

    (result, stripped)
}

fn strip_zero_width_chars(content: &str) -> (String, bool) {
    let mut stripped = false;
    let result: String = content
        .chars()
        .filter(|c| {
            matches!(
                *c,
                '\u{200B}'  // zero-width space
                | '\u{200C}' // zero-width non-joiner
                | '\u{200D}' // zero-width joiner
                | '\u{FEFF}' // BOM / zero-width no-break space
                | '\u{2060}' // word joiner
                | '\u{00AD}' // soft hyphen
            )
            .then(|| {
                stripped = true;
            })
            .is_none()
        })
        .collect();
    (result, stripped)
}

fn strip_html_dangerous(content: &str) -> (String, bool) {
    let lower = content.to_lowercase();
    let mut stripped = false;
    let mut result = content.to_string();

    let dangerous_tags = [
        "<script",
        "</script>",
        "<iframe",
        "</iframe>",
        "<embed",
        "</embed>",
        "<object",
        "</object>",
        "<style",
        "</style>",
        "<link",
        "<meta",
    ];

    for tag in &dangerous_tags {
        if lower.contains(tag) {
            stripped = true;
            let mut buf = result.clone();
            loop {
                let buf_lower = buf.to_lowercase();
                if let Some(start) = buf_lower.find(tag) {
                    let end = buf[start..].find('>').map(|i| start + i + 1);
                    if let Some(end) = end {
                        buf = format!("{}{}", &buf[..start], &buf[end..]);
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            result = buf;
        }
    }

    let event_handlers = ["onerror=", "onload=", "onclick=", "onmouseover="];
    for handler in &event_handlers {
        if result.to_lowercase().contains(handler) {
            stripped = true;
            let mut buf = result.clone();
            loop {
                let buf_lower = buf.to_lowercase();
                if let Some(pos) = buf_lower.find(handler) {
                    let after = &buf[pos + handler.len()..];
                    let trimmed = after.trim_start();
                    let skip = if let Some(rest) = trimmed.strip_prefix('"') {
                        rest.find('"').map(|i| i + 1).unwrap_or(0) + 1
                    } else if let Some(rest) = trimmed.strip_prefix('\'') {
                        rest.find('\'').map(|i| i + 1).unwrap_or(0) + 1
                    } else {
                        trimmed
                            .find(|c: char| c.is_whitespace() || c == '>')
                            .unwrap_or(trimmed.len())
                    };
                    let ws_len = after.len() - trimmed.len();
                    buf = format!("{}{}", &buf[..pos], &after[skip + ws_len..]);
                } else {
                    break;
                }
            }
            result = buf;
        }
    }

    let lower_result = result.to_lowercase();
    if lower_result.contains("javascript:") || lower_result.contains("javascript :") {
        stripped = true;
        result = result
            .replace("javascript:", "")
            .replace("javascript :", "");
    }

    (result, stripped)
}

fn strip_code_execution(content: &str) -> (String, bool) {
    let lower = content.to_lowercase();
    let mut stripped = false;
    let mut result = content.to_string();

    let exec_patterns = [
        "base64 -d",
        "eval(",
        "exec(",
        "system(",
        "runtime.exec",
        "processbuilder",
    ];

    for pattern in &exec_patterns {
        if lower.contains(pattern) {
            stripped = true;
            result = result.replace(pattern, "");
        }
    }

    (result, stripped)
}

impl InputSanitizer {
    pub fn new() -> Self {
        Self {
            custom_patterns: Vec::new(),
        }
    }

    pub fn add_pattern(&mut self, pattern: &str, name: &str) -> Result<(), SanitizeError> {
        if pattern.is_empty() {
            return Err(SanitizeError::SafetyBlock {
                reason: "pattern must not be empty".into(),
            });
        }
        self.custom_patterns
            .push((pattern.to_string(), name.to_string()));
        Ok(())
    }

    pub fn sanitize(&self, content: &str) -> Result<SanitizedContent, SanitizeError> {
        if content.len() > 1_000_000 {
            return Err(SanitizeError::SafetyBlock {
                reason: "content exceeds maximum size (1MB)".into(),
            });
        }

        let mut stripped_patterns = Vec::new();
        let mut text = content.to_string();

        let (t, s) = strip_zero_width_chars(&text);
        text = t;
        if s {
            stripped_patterns.push("zero_width_chars".into());
        }

        let (t, s) = strip_html_dangerous(&text);
        text = t;
        if s {
            stripped_patterns.push("html_injection".into());
        }

        let (t, s) = strip_system_tags(&text);
        text = t;
        if s {
            stripped_patterns.push("system_tag".into());
        }

        let (t, s) = strip_role_overrides(&text);
        text = t;
        if s {
            stripped_patterns.push("role_override".into());
        }

        if text.to_lowercase().contains("ignore previous")
            || text.to_lowercase().contains("disregard")
        {
            stripped_patterns.push("prompt_injection".into());
        }

        let (t, s) = strip_shell_injection(&text);
        text = t;
        if s {
            stripped_patterns.push("shell_injection".into());
        }

        if has_double_dot_slash(&text) {
            stripped_patterns.push("path_traversal".into());
        }

        let (t, s) = strip_privilege_escalation(&text);
        text = t;
        if s {
            stripped_patterns.push("privilege_escalation".into());
        }

        let (t, s) = strip_code_execution(&text);
        text = t;
        if s {
            stripped_patterns.push("code_execution".into());
        }

        if (text.to_lowercase().contains("wget https")
            || text.to_lowercase().contains("curl https"))
            && (text.contains(" >") || text.contains(" >>") || text.contains("/dev/null"))
        {
            stripped_patterns.push("network_exfiltration".into());
        }

        for (pattern, name) in &self.custom_patterns {
            if text.to_lowercase().contains(&pattern.to_lowercase()) {
                stripped_patterns.push(name.clone());
            }
        }

        stripped_patterns.sort();
        stripped_patterns.dedup();

        text = format!("{CONTENT_START}{text}{CONTENT_END}");

        Ok(SanitizedContent {
            original: content.to_string(),
            sanitized: text,
            stripped_patterns,
        })
    }

    pub fn strip_dangerous_patterns(&self, content: &str) -> String {
        let mut text = content.to_string();

        let (t, _) = strip_zero_width_chars(&text);
        text = t;

        let (t, _) = strip_html_dangerous(&text);
        text = t;

        let (t, _) = strip_system_tags(&text);
        text = t;

        let (t, _) = strip_role_overrides(&text);
        text = t;

        let (t, _) = strip_shell_injection(&text);
        text = t;

        let (t, _) = strip_privilege_escalation(&text);
        text = t;

        let (t, _) = strip_code_execution(&text);
        text = t;

        text
    }

    pub fn contains_dangerous_pattern(&self, content: &str) -> Vec<String> {
        let mut detected = Vec::new();

        let lower = content.to_lowercase();

        if content.contains('\u{200B}')
            || content.contains('\u{200C}')
            || content.contains('\u{200D}')
            || content.contains('\u{FEFF}')
            || content.contains('\u{2060}')
            || content.contains('\u{00AD}')
        {
            detected.push("zero_width_chars".into());
        }
        if lower.contains("<script")
            || lower.contains("<iframe")
            || lower.contains("<embed")
            || lower.contains("<object")
            || lower.contains("onerror=")
            || lower.contains("onload=")
            || lower.contains("javascript:")
        {
            detected.push("html_injection".into());
        }
        if lower.contains("<system>") || lower.contains("</system>") || lower.contains("<system ") {
            detected.push("system_tag".into());
        }
        if lower.contains("# system:")
            || lower.contains("# assistant:")
            || lower.contains("### system")
        {
            detected.push("role_override".into());
        }
        if lower.contains("rm -rf")
            || lower.contains("wget |")
            || lower.contains("curl |")
            || lower.contains("sudo |")
        {
            detected.push("shell_injection".into());
        }
        if has_double_dot_slash(content) {
            detected.push("path_traversal".into());
        }
        if lower.contains("sudo passwd")
            || lower.contains("sudo visudo")
            || lower.contains("/etc/shadow")
        {
            detected.push("privilege_escalation".into());
        }
        if lower.contains("base64 -d")
            || lower.contains("eval(")
            || lower.contains("runtime.exec")
            || lower.contains("processbuilder")
        {
            detected.push("code_execution".into());
        }
        if (lower.contains("wget https") || lower.contains("curl https"))
            && (content.contains(" >") || content.contains(" >>") || content.contains("/dev/null"))
        {
            detected.push("network_exfiltration".into());
        }

        for (pattern, name) in &self.custom_patterns {
            if lower.contains(&pattern.to_lowercase()) {
                detected.push(name.clone());
            }
        }

        detected.sort();
        detected.dedup();
        detected
    }
}
