#[derive(Debug, PartialEq)]
pub enum QqBotCommand {
    Note { content: String },
    Search { query: String },
    Status,
}

pub fn parse_command(content: &str) -> Option<QqBotCommand> {
    let after_prefix = content.strip_prefix("@agentic")?;
    let rest = after_prefix.trim();

    if rest.starts_with("note ") || rest.starts_with("note\t") {
        let content = rest
            .trim_start_matches([' ', '\t'])
            .strip_prefix("note")
            .map(str::trim)?;
        if content.is_empty() {
            return None;
        }
        return Some(QqBotCommand::Note {
            content: content.to_string(),
        });
    }

    if rest.starts_with("search ") || rest.starts_with("search\t") {
        let query = rest
            .trim_start_matches([' ', '\t'])
            .strip_prefix("search")
            .map(str::trim)?;
        if query.is_empty() {
            return None;
        }
        return Some(QqBotCommand::Search {
            query: query.to_string(),
        });
    }

    if rest == "status" {
        return Some(QqBotCommand::Status);
    }

    if rest.is_empty() {
        return Some(QqBotCommand::Status);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_cmd() {
        let r = parse_command("@agentic note hello world");
        assert!(matches!(r, Some(QqBotCommand::Note { .. })));
        if let Some(QqBotCommand::Note { content }) = r {
            assert_eq!(content, "hello world");
        }
    }

    #[test]
    fn search_cmd() {
        let r = parse_command("@agentic search rust");
        assert!(matches!(r, Some(QqBotCommand::Search { .. })));
    }

    #[test]
    fn status_cmd() {
        assert_eq!(parse_command("@agentic"), Some(QqBotCommand::Status));
        assert_eq!(parse_command("@agentic status"), Some(QqBotCommand::Status));
    }

    #[test]
    fn no_match() {
        assert_eq!(parse_command("hello"), None);
        assert_eq!(parse_command("@agenticx"), None);
        assert_eq!(parse_command("@agentic unknown"), None);
    }
}
