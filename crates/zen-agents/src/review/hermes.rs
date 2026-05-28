use zen_core::types::Task;

#[derive(Debug, Clone)]
pub struct HermesFinding {
    pub finding_type: HermesFindingType,
    pub description: String,
    pub requires_revision: bool,
}

#[derive(Debug, Clone)]
pub enum HermesFindingType {
    FactCheckFailure,
    FormatNonCompliance,
    ToneMismatch,
    DeliveryReadinessIssue,
}

#[derive(Debug)]
pub struct HermesValidation {
    pub findings: Vec<HermesFinding>,
    pub delivery_ready: bool,
    pub revision_count: u8,
}

pub struct HermesValidator;

impl HermesValidator {
    pub fn new() -> Self {
        Self
    }

    pub fn validate_deliverable(&self, task: &Task, deliverable: &str) -> HermesValidation {
        let mut findings = Vec::new();

        let mut revision_count = 0u8;
        if let Some(rev_str) = task.metadata.get("revision_count") {
            revision_count = rev_str.parse().unwrap_or(0);
        }

        findings.extend(self.fact_check(deliverable, &task.user_input));
        findings.extend(self.check_format_compliance(deliverable));

        let delivery_ready = findings.is_empty();

        tracing::debug!(
            task_id = %task.id,
            findings = findings.len(),
            delivery_ready,
            revision_count,
            "HermesValidation completed"
        );

        HermesValidation {
            findings,
            delivery_ready,
            revision_count,
        }
    }

    pub fn fact_check(&self, deliverable: &str, blueprint: &str) -> Vec<HermesFinding> {
        let mut findings = Vec::new();
        if deliverable.trim().is_empty() {
            return findings;
        }

        let blueprint_words: Vec<&str> = blueprint.split_whitespace().collect();
        let significant_words: Vec<&str> = blueprint_words
            .iter()
            .filter(|w| w.len() > 4 && !["this", "that", "which", "there", "their"].contains(w))
            .copied()
            .collect();

        let lower_deliverable = deliverable.to_lowercase();
        for word in &significant_words {
            if !lower_deliverable.contains(word.to_lowercase().as_str()) {
                findings.push(HermesFinding {
                    finding_type: HermesFindingType::FactCheckFailure,
                    description: format!("Blueprint key term '{word}' not found in deliverable"),
                    requires_revision: true,
                });
            }
        }

        findings
    }

    pub fn check_format_compliance(&self, deliverable: &str) -> Vec<HermesFinding> {
        let mut findings = Vec::new();
        if deliverable.trim().is_empty() {
            return findings;
        }

        let lines: Vec<&str> = deliverable.lines().collect();
        let has_any_structure = lines.iter().any(|l| {
            let t = l.trim();
            t.starts_with("#") || t.starts_with("-") || t.starts_with("*") || t.chars().next().is_some_and(|c| c.is_ascii_digit())
        });

        if lines.len() > 10 && !has_any_structure {
            findings.push(HermesFinding {
                finding_type: HermesFindingType::FormatNonCompliance,
                description: "Long deliverable lacks structural elements (headings, lists, numbered steps)".to_string(),
                requires_revision: true,
            });
        }

        if deliverable.as_bytes().iter().filter(|b| **b == b'`').count() % 2 != 0 {
            findings.push(HermesFinding {
                finding_type: HermesFindingType::FormatNonCompliance,
                description: "Unmatched backtick characters detected in formatting".to_string(),
                requires_revision: false,
            });
        }

        findings
    }

    pub fn check_tone_match(&self, _deliverable: &str, _expected_tone: &str) -> Vec<HermesFinding> {
        Vec::new()
    }

    pub fn can_push(&self, validation: &HermesValidation) -> bool {
        validation.delivery_ready && validation.findings.is_empty()
    }
}

impl Default for HermesValidator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zen_core::types::{Task, TaskType};

    #[test]
    fn hermes_approves_valid_deliverables() {
        let validator = HermesValidator::new();
        let task = Task::new("build a feature", 0.5, TaskType::Code);
        let validation = validator.validate_deliverable(&task, "build a feature with tests");
        assert!(validation.delivery_ready);
        assert!(validator.can_push(&validation));
    }

    #[test]
    fn hermes_detects_fact_check_failures() {
        let validator = HermesValidator::new();
        let task = Task::new("implement authentication middleware", 0.7, TaskType::Code);
        let validation = validator.validate_deliverable(&task, "simple code output");
        assert!(!validation.delivery_ready);
        assert!(validation.findings.iter().any(|f| matches!(f.finding_type, HermesFindingType::FactCheckFailure)));
    }

    #[test]
    fn hermes_can_push_returns_true_when_delivery_ready() {
        let validator = HermesValidator::new();
        let task = Task::new("test", 0.3, TaskType::Text);
        let validation = validator.validate_deliverable(&task, "ready content");
        assert!(validator.can_push(&validation));
    }
}
