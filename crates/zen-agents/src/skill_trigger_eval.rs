//! Report-only dual-metric grader for confirmed skills (compile-hygiene ⑤).
//!
//! Measures, without any labelled data (the T173 lesson: labels are the
//! bottleneck, so this surface stays observational):
//!
//! - **trigger accuracy** — per trigger, the best [`score_trigger`] match
//!   against the skill's own evidence observations (the `## Evidence`
//!   bullets the precipitation flow rendered into SKILL.md). Precision
//!   direction: does each trigger fire on content like the work that
//!   actually created the skill?
//! - **body alignment** — per observation, the best trigger match against
//!   it. Recall direction: how much of the proven work the trigger set
//!   covers. A skill whose evidence no trigger covers will silently never
//!   auto-route on similar future input.
//!
//! Every confirmed skill ends up with both scores or an explicit
//! `NotEvaluated` reason (acceptance: 100% accounted for). The report is
//! pure — it writes nothing and gates nothing.

use std::path::Path;

use serde::Serialize;
use tracing::info;

use crate::skill_hit_router::score_trigger;
use crate::skill_loader::{SkillDefinition, SkillLoader};
use zen_repo::normalize_alias;

/// Rendered-evidence section header written by `render_skill_md`.
const EVIDENCE_HEADER: &str = "## Evidence";

/// One evaluated skill: both metric directions, with sample counts.
#[derive(Debug, Clone, Serialize)]
pub struct SkillEval {
    pub skill: String,
    /// mean over triggers of the best trigger→observation match.
    pub trigger_accuracy: Option<f64>,
    /// mean over observations of the best observation→trigger match.
    pub body_alignment: Option<f64>,
    pub triggers: usize,
    pub observations: usize,
}

/// A skill the grader refused to score, with the reason spelled out.
#[derive(Debug, Clone, Serialize)]
pub struct SkillNotEvaluated {
    pub skill: String,
    pub reason: String,
}

/// Aggregate report consumed by `zen discover report`.
#[derive(Debug, Clone, Serialize)]
pub struct SkillEvalReport {
    pub confirmed_skills: usize,
    pub evaluated: Vec<SkillEval>,
    pub not_evaluated: Vec<SkillNotEvaluated>,
}

/// Grade every confirmed skill under `skills_dir`. Read-only: missing or
/// unparsable skills are reported as `NotEvaluated`, never an error — a
/// broken SKILL.md must not take down `zen discover report`.
pub fn compute(skills_dir: &Path) -> SkillEvalReport {
    let loader = SkillLoader::new_from_dir(skills_dir.to_path_buf());
    let mut report = SkillEvalReport {
        confirmed_skills: 0,
        evaluated: Vec::new(),
        not_evaluated: Vec::new(),
    };

    let Ok(names) = loader.list_skills() else {
        info!("skill trigger eval: skills dir unreadable — nothing to grade");
        return report;
    };
    report.confirmed_skills = names.len();

    for name in names {
        match loader.load_skill(&name) {
            Ok(def) => match grade(&def) {
                Ok(eval) => report.evaluated.push(eval),
                Err(reason) => report.not_evaluated.push(SkillNotEvaluated {
                    skill: def.name,
                    reason,
                }),
            },
            Err(e) => report.not_evaluated.push(SkillNotEvaluated {
                skill: name,
                reason: format!("unparsable SKILL.md: {e}"),
            }),
        }
    }

    info!(
        confirmed = report.confirmed_skills,
        evaluated = report.evaluated.len(),
        not_evaluated = report.not_evaluated.len(),
        "skill trigger eval complete"
    );
    report
}

/// Split the rendered body into evidence observations.
///
/// Observations live under the `## Evidence` header as `- ` bullets — the
/// exact format `render_skill_md` emits, so the grader and the writer share
/// one contract. A hand-written skill without that section simply has no
/// observations (trigger accuracy degrades to body grounding, body
/// alignment becomes `None`).
fn observations_from_body(body: &str) -> Vec<String> {
    let mut in_evidence = false;
    let mut out = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed == EVIDENCE_HEADER {
            in_evidence = true;
            continue;
        }
        if in_evidence && trimmed.starts_with("## ") {
            break;
        }
        if in_evidence && let Some(bullet) = trimmed.strip_prefix("- ") {
            out.push(bullet.trim().to_string());
        }
    }
    out
}

/// Grade one skill. `Err(reason)` = explicit NotEvaluated.
fn grade(def: &SkillDefinition) -> Result<SkillEval, String> {
    let triggers: Vec<String> = def
        .triggers
        .iter()
        .map(|t| normalize_alias(t))
        .filter(|t| !t.is_empty())
        .collect();
    let observations = observations_from_body(&def.body);

    if triggers.is_empty() {
        return Err("no triggers declared — the skill can never auto-route".to_string());
    }

    // Grounding corpus: rendered evidence when present, else the body
    // itself (prompt + prose). Empty body AND empty evidence → the trigger
    // direction has nothing real to ground against.
    let mut grounding: Vec<String> = observations.clone();
    if grounding.is_empty() {
        let body = normalize_alias(&def.body);
        if !body.is_empty() {
            grounding.push(body);
        }
    }
    if grounding.is_empty() {
        return Err(
            "no evidence observations and empty body — nothing to grade against".to_string(),
        );
    }

    let mut trigger_scores = Vec::new();
    for trigger in &triggers {
        let best = grounding
            .iter()
            .map(|g| score_trigger(g, trigger))
            .fold(0.0f32, f32::max);
        trigger_scores.push(best);
    }
    let trigger_accuracy =
        Some(trigger_scores.iter().map(|s| *s as f64).sum::<f64>() / trigger_scores.len() as f64);

    let body_alignment = if observations.is_empty() {
        None
    } else {
        let mut obs_scores = Vec::new();
        for obs in &observations {
            let obs_norm = normalize_alias(obs);
            let best = triggers
                .iter()
                .map(|t| score_trigger(&obs_norm, t))
                .fold(0.0f32, f32::max);
            obs_scores.push(best);
        }
        Some(obs_scores.iter().map(|s| *s as f64).sum::<f64>() / obs_scores.len() as f64)
    };

    Ok(SkillEval {
        skill: def.name.clone(),
        trigger_accuracy,
        body_alignment,
        triggers: triggers.len(),
        observations: observations.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str, triggers: &[&str], body: &str) -> SkillDefinition {
        SkillDefinition {
            name: name.to_string(),
            description: String::new(),
            triggers: triggers.iter().map(|t| t.to_string()).collect(),
            auto_route: None,
            tools: Vec::new(),
            context_files: Vec::new(),
            prompt: String::new(),
            body: body.to_string(),
        }
    }

    fn skill_with_evidence(evidence: &[&str]) -> SkillDefinition {
        let mut body = String::from("# demo\n\nprompt text about deploying rust services\n");
        body.push_str(EVIDENCE_HEADER);
        body.push('\n');
        for e in evidence {
            body.push_str(&format!("- {e}\n"));
        }
        body.push_str("## Gotchas\n\nNone recorded\n");
        def("deploy-rust", &["deploy rust", "ship the service"], &body)
    }

    #[test]
    fn dual_metrics_discriminate_aligned_from_unrelated_skills() {
        let aligned = skill_with_evidence(&[
            "deployed the rust service to staging",
            "shipped the service after review",
        ]);
        let eval = grade(&aligned).unwrap();
        assert_eq!(eval.triggers, 2);
        assert_eq!(eval.observations, 2);
        assert!(eval.trigger_accuracy.unwrap() > 0.0);
        let ba_aligned = eval.body_alignment.unwrap();

        // Verbatim trigger mentions in the evidence must containment-score
        // to the ceiling; unrelated evidence must score strictly lower.
        let verbatim = skill_with_evidence(&[
            "used when the user says deploy rust",
            "triggered by ship the service requests",
        ]);
        let ba_verbatim = grade(&verbatim).unwrap().body_alignment.unwrap();
        assert!(
            ba_verbatim > ba_aligned,
            "verbatim evidence {ba_verbatim} must outscore paraphrase {ba_aligned}"
        );

        let unrelated =
            skill_with_evidence(&["gardening tips for the balcony", "soup recipes for winter"]);
        let ba_unrelated = grade(&unrelated).unwrap().body_alignment.unwrap();
        assert!(
            ba_unrelated < ba_aligned,
            "unrelated evidence {ba_unrelated} must score below paraphrase {ba_aligned}"
        );
    }

    #[test]
    fn no_triggers_is_explicit_not_evaluated() {
        let d = def("bare", &[], "some body");
        let reason = grade(&d).unwrap_err();
        assert!(reason.contains("no triggers"));
    }

    #[test]
    fn no_evidence_and_empty_body_is_not_evaluated() {
        let d = def("hollow", &["trigger"], "");
        let reason = grade(&d).unwrap_err();
        assert!(reason.contains("nothing to grade"));
    }

    #[test]
    fn no_evidence_degrades_body_alignment_to_none_but_keeps_trigger_score() {
        // Body present, no Evidence section: trigger accuracy grounds on
        // the body; body alignment has no observations to measure.
        let d = def("flat", &["rust deploy"], "how to deploy rust services");
        let eval = grade(&d).unwrap();
        assert!(eval.trigger_accuracy.unwrap() > 0.0);
        assert!(eval.body_alignment.is_none());
        assert_eq!(eval.observations, 0);
    }

    #[test]
    fn observations_parser_scops_to_evidence_section() {
        let d = skill_with_evidence(&["first finding", "second finding"]);
        let obs = observations_from_body(&d.body);
        assert_eq!(obs, vec!["first finding", "second finding"]);
        // Gotchas bullets (if any) must not leak into observations.
        assert!(!obs.iter().any(|o| o.contains("Gotchas")));
    }

    #[test]
    fn compute_reports_unparsable_and_missing_dirs_fail_open() {
        let tmp = tempfile::TempDir::new().unwrap();
        let report = compute(tmp.path());
        assert_eq!(report.confirmed_skills, 0);
        assert!(report.evaluated.is_empty());

        // A skill whose evidence is empty + empty body lands in
        // not_evaluated with its reason, keeping the rest gradeable.
        let skills = tmp.path().join("s");
        std::fs::create_dir_all(skills.join("broken")).unwrap();
        std::fs::write(
            skills.join("broken/SKILL.md"),
            "---\nname: broken\ntriggers: []\n---\n",
        )
        .unwrap();
        let report = compute(&skills);
        assert_eq!(report.confirmed_skills, 1);
        assert_eq!(report.not_evaluated.len(), 1);
        assert!(report.not_evaluated[0].reason.contains("no triggers"));
    }
}
