#[deprecated(
    since = "0.0.1",
    note = "Use rig_memvid::projection::MemoryContextPack + rig_compose::ContextPack instead. \
            ContextBudget will be removed in a future release."
)]
pub struct ContextBudget {
    pub max_tokens: usize,
}

#[allow(deprecated)]
impl ContextBudget {
    #[deprecated(
        since = "0.0.1",
        note = "Use rig_compose::ContextPackConfig for budget configuration instead."
    )]
    pub fn new(max_tokens: usize) -> Self {
        Self { max_tokens }
    }

    pub fn estimated_tokens(text: &str) -> usize {
        text.len() / 4 + 1
    }

    #[deprecated(
        since = "0.0.1",
        note = "Use rig_compose::ContextPack::pack() with ContextPackConfig instead. \
                ContextPack applies priority-based progressive compression."
    )]
    pub fn truncate_messages(
        messages: &mut Vec<(String, String)>,
        max_tokens: usize,
    ) -> Option<String> {
        let total: usize = messages
            .iter()
            .map(|(_, c)| Self::estimated_tokens(c))
            .sum();
        if total <= max_tokens {
            return None;
        }

        let mut current_tokens = total;
        let mut warning = None;

        while current_tokens > max_tokens && messages.len() > 1 {
            if messages[0].0 == "system" {
                if messages.len() <= 2 {
                    break;
                }
                messages.remove(1);
            } else {
                messages.remove(0);
            }
            current_tokens = messages
                .iter()
                .map(|(_, c)| Self::estimated_tokens(c))
                .sum();
        }

        if current_tokens > max_tokens {
            let oldest_content = &messages[0].1;
            let tokens_to_remove = current_tokens - max_tokens;
            let chars_to_remove = tokens_to_remove * 4;
            if chars_to_remove < oldest_content.len() {
                messages[0].1 = oldest_content[chars_to_remove..].to_string();
                warning =
                    Some("Context exceeded limit. Earlier conversation truncated.".to_string());
            }
        } else {
            warning = Some("Context exceeded limit. Earlier conversation summarized.".to_string());
        }

        warning
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;

    #[test]
    fn no_truncation_when_under_limit() {
        let mut messages = vec![
            ("system".to_string(), "You are helpful".to_string()),
            ("user".to_string(), "Hello".to_string()),
        ];
        let result = ContextBudget::truncate_messages(&mut messages, 1000);
        assert_eq!(result, None);
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn truncates_oldest_first() {
        let mut messages = vec![
            ("system".to_string(), "System prompt".to_string()),
            (
                "user".to_string(),
                "First message that is quite long and should be truncated first".to_string(),
            ),
            ("assistant".to_string(), "Second message".to_string()),
            ("user".to_string(), "Third message".to_string()),
        ];
        let result = ContextBudget::truncate_messages(&mut messages, 20);
        assert!(result.is_some());
        assert!(messages.len() < 4);
    }

    #[test]
    fn preserves_system_prompt() {
        let mut messages = vec![
            ("system".to_string(), "Critical system prompt".to_string()),
            ("user".to_string(), "User message one".to_string()),
            ("assistant".to_string(), "Assistant reply".to_string()),
        ];
        ContextBudget::truncate_messages(&mut messages, 10);
        assert_eq!(messages[0].0, "system");
        assert_eq!(messages[0].1, "Critical system prompt");
    }
}
