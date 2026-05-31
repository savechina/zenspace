use zen_core::types::Sensitivity;

use crate::session::RetrievedNote;

/// Compute the maximum sensitivity level from retrieved knowledge notes.
///
/// Per FR-077: orchestrator computes `max(notes.map(|n| n.sensitivity))`
/// before routing to LLM. Returns `Private` when no notes are retrieved
/// (safe default per FR-071).
pub fn compute_max_sensitivity(notes: &[RetrievedNote]) -> Sensitivity {
    if notes.is_empty() {
        return Sensitivity::Private;
    }
    Sensitivity::max_of(&notes.iter().map(|n| n.sensitivity).collect::<Vec<_>>())
}

/// Validate that a provider is allowed for the given sensitivity level.
///
/// Returns `Ok(())` if the provider can handle data at this sensitivity,
/// or `Err` with a descriptive message if the routing would violate
/// sensitivity constraints.
pub fn validate_provider_for_sensitivity(
    provider: &str,
    sensitivity: Sensitivity,
    local_providers: &[&str],
) -> Result<(), String> {
    match sensitivity {
        Sensitivity::Public => Ok(()),
        Sensitivity::Private | Sensitivity::Confidential => {
            if local_providers.contains(&provider) {
                Ok(())
            } else {
                Err(format!(
                    "Local LLM unavailable. Start Ollama or configure a local provider. \
                     {sensitivity} data cannot be routed to cloud."
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_note(sensitivity: Sensitivity) -> RetrievedNote {
        RetrievedNote {
            path: "test.md".to_string(),
            content: "test".to_string(),
            sensitivity,
            relevance: 1.0,
        }
    }

    #[test]
    fn empty_notes_returns_private() {
        assert_eq!(compute_max_sensitivity(&[]), Sensitivity::Private);
    }

    #[test]
    fn single_public_note() {
        let notes = vec![make_note(Sensitivity::Public)];
        assert_eq!(compute_max_sensitivity(&notes), Sensitivity::Public);
    }

    #[test]
    fn single_private_note() {
        let notes = vec![make_note(Sensitivity::Private)];
        assert_eq!(compute_max_sensitivity(&notes), Sensitivity::Private);
    }

    #[test]
    fn mixed_sensitivity_returns_max() {
        let notes = vec![
            make_note(Sensitivity::Public),
            make_note(Sensitivity::Confidential),
            make_note(Sensitivity::Private),
        ];
        assert_eq!(compute_max_sensitivity(&notes), Sensitivity::Confidential);
    }

    #[test]
    fn validate_public_allows_any_provider() {
        assert!(validate_provider_for_sensitivity("openai", Sensitivity::Public, &[]).is_ok());
    }

    #[test]
    fn validate_private_rejects_cloud_provider() {
        let result = validate_provider_for_sensitivity("openai", Sensitivity::Private, &["ollama"]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Local LLM unavailable"));
    }

    #[test]
    fn validate_private_allows_local_provider() {
        assert!(
            validate_provider_for_sensitivity("ollama", Sensitivity::Private, &["ollama"]).is_ok()
        );
    }
}
