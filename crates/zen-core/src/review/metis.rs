use crate::types::Task;

#[derive(Debug, Clone)]
pub struct MetisFinding {
    pub finding_type: MetisFindingType,
    pub description: String,
    pub severity: FindingSeverity,
    pub suggestion: String,
}

#[derive(Debug, Clone)]
pub enum MetisFindingType {
    LogicGap,
    MissingAssumption,
    TacticalFeasibility,
    PathOptimization,
}

#[derive(Debug, Clone)]
pub enum FindingSeverity {
    Low,
    Medium,
    High,
}

#[derive(Debug)]
pub struct MetisReview {
    pub findings: Vec<MetisFinding>,
    pub optimization_score: f64,
}

pub struct MetisReviewer;

impl MetisReviewer {
    pub fn new() -> Self {
        Self
    }

    pub fn review_plan(&self, task: &Task, plan: &str) -> MetisReview {
        let mut findings = Vec::new();
        findings.extend(self.analyze_logic_gaps(plan));
        findings.extend(self.validate_assumptions(plan));
        findings.extend(self.assess_tactical_feasibility(plan));
        findings.extend(self.suggest_optimizations(plan));

        let optimization_score = if findings.is_empty() {
            1.0
        } else {
            let penalty: f64 = findings
                .iter()
                .map(|f| match f.severity {
                    FindingSeverity::Low => 0.05,
                    FindingSeverity::Medium => 0.15,
                    FindingSeverity::High => 0.3,
                })
                .sum();
            (1.0 - penalty).max(0.0)
        };

        tracing::debug!(
            task_id = %task.id,
            findings = findings.len(),
            score = optimization_score,
            "MetisReview completed"
        );

        MetisReview {
            findings,
            optimization_score,
        }
    }

    pub fn analyze_logic_gaps(&self, plan: &str) -> Vec<MetisFinding> {
        let mut findings = Vec::new();
        if plan.trim().is_empty() {
            return findings;
        }

        let sentences: Vec<&str> = plan
            .split(['.', '\n'])
            .filter(|s| !s.trim().is_empty())
            .collect();

        for (i, sentence) in sentences.iter().enumerate() {
            let lower = sentence.to_lowercase();
            if lower.contains("then") && i > 0 && !sentences[i - 1].contains('.') {
                findings.push(MetisFinding {
                    finding_type: MetisFindingType::LogicGap,
                    description: format!("Unsequenced 'then' at position {i} without preceding step"),
                    severity: FindingSeverity::Medium,
                    suggestion: "Ensure each step has a clear predecessor".to_string(),
                });
            }
            if lower.contains("TODO") || lower.contains("FIXME") || lower.contains("placeholder") {
                findings.push(MetisFinding {
                    finding_type: MetisFindingType::LogicGap,
                    description: format!("Unresolved marker found: {}", sentence.trim()),
                    severity: FindingSeverity::High,
                    suggestion: "Replace all TODO/FIXME/placeholder markers with concrete actions".to_string(),
                });
            }
        }

        let has_undefined_abbreviations = sentences.iter().any(|s| {
            s.chars().filter(|c| c.is_ascii_uppercase()).count() >= 3
                && !s.contains("TODO")
                && !s.contains("FIXME")
        });
        if has_undefined_abbreviations && sentences.len() > 3 {
            findings.push(MetisFinding {
                finding_type: MetisFindingType::LogicGap,
                description: "Possible undefined abbreviations detected in plan".to_string(),
                severity: FindingSeverity::Low,
                suggestion: "Define all abbreviations on first use".to_string(),
            });
        }

        findings
    }

    pub fn validate_assumptions(&self, plan: &str) -> Vec<MetisFinding> {
        let mut findings = Vec::new();
        if plan.trim().is_empty() {
            return findings;
        }

        let assumption_indicators = [
            ("assume", "Implicit assumption detected — make it explicit"),
            ("obviously", "Assumed triviality — verify the assumption"),
            ("naturally", "Assumed naturalness — verify the assumption"),
            ("clearly", "Assumed clarity — verify the assumption"),
            ("everyone knows", "Shared-knowledge assumption — state it openly"),
        ];

        for indicator in &assumption_indicators {
            if plan.to_lowercase().contains(indicator.0) {
                findings.push(MetisFinding {
                    finding_type: MetisFindingType::MissingAssumption,
                    description: format!("Assumption indicator '{}' found in plan", indicator.0),
                    severity: FindingSeverity::Medium,
                    suggestion: indicator.1.to_string(),
                });
            }
        }

        if !plan.to_lowercase().contains("prerequisite")
            && !plan.to_lowercase().contains("requires")
            && plan.split_whitespace().count() > 20
        {
            findings.push(MetisFinding {
                finding_type: MetisFindingType::MissingAssumption,
                description: "No prerequisites or requirements section detected".to_string(),
                severity: FindingSeverity::Low,
                suggestion: "Add a prerequisites section for plans longer than 20 words".to_string(),
            });
        }

        findings
    }

    pub fn assess_tactical_feasibility(&self, plan: &str) -> Vec<MetisFinding> {
        let mut findings = Vec::new();
        if plan.trim().is_empty() {
            return findings;
        }

        let has_actionable_terms = ["create", "update", "delete", "remove", "add", "replace", "deploy", "build", "test", "verify"].iter()
            .any(|t| plan.to_lowercase().contains(t));
        let is_vague = ["should", "might", "could", "probably", "eventually", "hopefully"]
            .iter()
            .any(|t| plan.to_lowercase().contains(t));

        if !has_actionable_terms && plan.split_whitespace().count() > 5 {
            findings.push(MetisFinding {
                finding_type: MetisFindingType::TacticalFeasibility,
                description: "Plan lacks concrete actionable verbs".to_string(),
                severity: FindingSeverity::High,
                suggestion: "Replace abstract language with specific actionable steps".to_string(),
            });
        }

        if is_vague {
            findings.push(MetisFinding {
                finding_type: MetisFindingType::TacticalFeasibility,
                description: "Plan contains non-committal language".to_string(),
                severity: FindingSeverity::Medium,
                suggestion: "Replace hedging words with definitive action descriptions".to_string(),
            });
        }

        let step_count = plan.split(['\n', '.']).filter(|s| !s.trim().is_empty()).count();
        if step_count < 3 && plan.split_whitespace().count() > 15 {
            findings.push(MetisFinding {
                finding_type: MetisFindingType::TacticalFeasibility,
                description: format!("Plan has only {step_count} step(s) but {len} words — consider decomposition", len = plan.split_whitespace().count()),
                severity: FindingSeverity::Medium,
                suggestion: "Break large plan into smaller, verifiable steps".to_string(),
            });
        }

        findings
    }

    pub fn suggest_optimizations(&self, plan: &str) -> Vec<MetisFinding> {
        let mut findings = Vec::new();
        if plan.trim().is_empty() {
            return findings;
        }

        let word_count = plan.split_whitespace().count();
        if word_count > 200 {
            findings.push(MetisFinding {
                finding_type: MetisFindingType::PathOptimization,
                description: format!("Plan is verbose ({word_count} words) — consider summarization"),
                severity: FindingSeverity::Low,
                suggestion: "Condense plan to essential steps; move details to supporting docs".to_string(),
            });
        }

        if plan.to_lowercase().matches("and").count() > 3 {
            findings.push(MetisFinding {
                finding_type: MetisFindingType::PathOptimization,
                description: "Multiple compound steps detected — parallelization may be possible".to_string(),
                severity: FindingSeverity::Low,
                suggestion: "Consider splitting compound steps for parallel execution".to_string(),
            });
        }

        findings
    }
}

impl Default for MetisReviewer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Task, TaskType};

    #[test]
    fn metis_reviewer_returns_non_empty_review() {
        let reviewer = MetisReviewer::new();
        let task = Task::new("test task", 0.5, TaskType::Code);
        let review = reviewer.review_plan(&task, "test plan");
        assert!(review.optimization_score >= 0.0);
        assert!(review.optimization_score <= 1.0);
    }

    #[test]
    fn metis_analysis_methods_return_findings_for_plans_with_issues() {
        let reviewer = MetisReviewer::new();

        let gap_findings = reviewer.analyze_logic_gaps("step one. then step two. TODO: implement this.");
        assert!(gap_findings.iter().any(|f| matches!(f.finding_type, MetisFindingType::LogicGap)));

        let assumption_findings = reviewer.validate_assumptions("obviously this works and we assume it's fine");
        assert!(!assumption_findings.is_empty());
    }

    #[test]
    fn metis_analysis_methods_return_empty_for_clean_short_plans() {
        let reviewer = MetisReviewer::new();
        let gap_findings = reviewer.analyze_logic_gaps("clean plan");
        assert!(gap_findings.is_empty());
        let assumption_findings = reviewer.validate_assumptions("clean plan");
        assert!(assumption_findings.is_empty());
        let tactical = reviewer.assess_tactical_feasibility("clean plan");
        assert!(tactical.is_empty());
        let optimizations = reviewer.suggest_optimizations("clean plan");
        assert!(optimizations.is_empty());
    }

    #[test]
    fn metis_handles_complex_task_types() {
        let reviewer = MetisReviewer::new();
        let task = Task::new("complex task", 0.8, TaskType::Text);
        let review = reviewer.review_plan(&task, "complex plan");
        assert!(review.optimization_score >= 0.0);
    }
}
