use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;

/// A goal definition stored in `~/.zen/goals.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Goal {
    pub name: String,
    pub target: String,
    pub deadline: Option<String>,
    pub linked_habits: Vec<String>,
    pub linked_skills: Vec<String>,
    pub created_at: String,
    pub status: String,
    pub progress: f64,
}

/// Top-level TOML structure for `goals.toml`.
#[derive(Debug, Serialize, Deserialize)]
struct GoalsToml {
    goals: Vec<Goal>,
}

pub struct GoalService {
    goals_path: PathBuf,
}

impl GoalService {
    pub fn new(paths: &ZenPaths) -> Self {
        Self {
            goals_path: paths.global_root().join("goals.toml"),
        }
    }

    /// Create a service with a custom path (for testing).
    pub fn with_path(goals_path: PathBuf) -> Self {
        Self { goals_path }
    }

    /// Parse `goals.toml` and return the list of goals.
    pub fn load_goals(&self) -> Result<Vec<Goal>, ZenError> {
        if !self.goals_path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.goals_path).map_err(ZenError::Io)?;
        let parsed: GoalsToml = toml::from_str(&content)
            .map_err(|e| ZenError::Message(format!("failed to parse goals.toml: {e}")))?;
        Ok(parsed.goals)
    }

    /// Add or update a goal in `goals.toml`.
    ///
    /// If a goal with the same name exists it is replaced; otherwise a new
    /// goal is appended.
    pub fn set_goal(&self, goal: Goal) -> Result<(), ZenError> {
        let mut goals = self.load_goals()?;
        if let Some(idx) = goals.iter().position(|g| g.name == goal.name) {
            goals[idx] = goal;
        } else {
            goals.push(goal);
        }
        self.write_goals(&goals)
    }

    /// Update progress (0.0 — 1.0) for a goal by name.
    pub fn update_progress(&self, name: &str, progress: f64) -> Result<(), ZenError> {
        let mut goals = self.load_goals()?;
        let goal = goals
            .iter_mut()
            .find(|g| g.name == name)
            .ok_or_else(|| ZenError::Message(format!("goal '{name}' not found")))?;
        goal.progress = progress.clamp(0.0, 1.0);
        self.write_goals(&goals)
    }

    /// Mark a goal as completed.
    pub fn complete_goal(&self, name: &str) -> Result<(), ZenError> {
        let mut goals = self.load_goals()?;
        let goal = goals
            .iter_mut()
            .find(|g| g.name == name)
            .ok_or_else(|| ZenError::Message(format!("goal '{name}' not found")))?;
        goal.status = "completed".to_string();
        goal.progress = 1.0;
        self.write_goals(&goals)
    }

    /// Get a single goal by name.
    pub fn get_goal(&self, name: &str) -> Result<Option<Goal>, ZenError> {
        let goals = self.load_goals()?;
        Ok(goals.into_iter().find(|g| g.name == name))
    }

    fn write_goals(&self, goals: &[Goal]) -> Result<(), ZenError> {
        let toml_struct = GoalsToml {
            goals: goals.to_vec(),
        };
        let content = toml::to_string_pretty(&toml_struct)
            .map_err(|e| ZenError::Message(format!("failed to serialize goals: {e}")))?;

        if let Some(parent) = self.goals_path.parent() {
            fs::create_dir_all(parent).map_err(ZenError::Io)?;
        }

        fs::write(&self.goals_path, content).map_err(ZenError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_service() -> (GoalService, TempDir) {
        let tmp = TempDir::new().unwrap();
        let goals_path = tmp.path().join("goals.toml");
        let service = GoalService::with_path(goals_path);
        (service, tmp)
    }

    fn sample_goal(name: &str) -> Goal {
        Goal {
            name: name.to_string(),
            target: "lose 5kg".to_string(),
            deadline: Some("2026-12-31".to_string()),
            linked_habits: vec!["exercise".to_string()],
            linked_skills: vec![],
            created_at: "2026-01-01T00:00:00Z".to_string(),
            status: "active".to_string(),
            progress: 0.0,
        }
    }

    #[test]
    fn load_empty_when_no_file() {
        let (service, _tmp) = setup_service();
        let goals = service.load_goals().unwrap();
        assert!(goals.is_empty());
    }

    #[test]
    fn set_and_load_goal() {
        let (service, _tmp) = setup_service();
        let goal = sample_goal("fitness");
        service.set_goal(goal.clone()).unwrap();

        let goals = service.load_goals().unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].name, "fitness");
        assert_eq!(goals[0].progress, 0.0);
    }

    #[test]
    fn update_existing_goal() {
        let (service, _tmp) = setup_service();
        service.set_goal(sample_goal("fitness")).unwrap();

        let updated = Goal {
            progress: 0.5,
            ..sample_goal("fitness")
        };
        service.set_goal(updated).unwrap();

        let goals = service.load_goals().unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0].progress, 0.5);
    }

    #[test]
    fn update_progress() {
        let (service, _tmp) = setup_service();
        service.set_goal(sample_goal("fitness")).unwrap();
        service.update_progress("fitness", 0.75).unwrap();

        let goal = service.get_goal("fitness").unwrap().unwrap();
        assert_eq!(goal.progress, 0.75);
    }

    #[test]
    fn progress_clamped_to_range() {
        let (service, _tmp) = setup_service();
        service.set_goal(sample_goal("fitness")).unwrap();

        service.update_progress("fitness", 1.5).unwrap();
        assert_eq!(service.get_goal("fitness").unwrap().unwrap().progress, 1.0);

        service.update_progress("fitness", -0.5).unwrap();
        assert_eq!(service.get_goal("fitness").unwrap().unwrap().progress, 0.0);
    }

    #[test]
    fn complete_goal() {
        let (service, _tmp) = setup_service();
        service.set_goal(sample_goal("fitness")).unwrap();
        service.complete_goal("fitness").unwrap();

        let goal = service.get_goal("fitness").unwrap().unwrap();
        assert_eq!(goal.status, "completed");
        assert_eq!(goal.progress, 1.0);
    }

    #[test]
    fn update_nonexistent_goal_errors() {
        let (service, _tmp) = setup_service();
        let result = service.update_progress("nope", 0.5);
        assert!(result.is_err());
    }

    #[test]
    fn complete_nonexistent_goal_errors() {
        let (service, _tmp) = setup_service();
        let result = service.complete_goal("nope");
        assert!(result.is_err());
    }

    #[test]
    fn get_goal_returns_none_when_missing() {
        let (service, _tmp) = setup_service();
        assert!(service.get_goal("nonexistent").unwrap().is_none());
    }

    #[test]
    fn multiple_goals_coexist() {
        let (service, _tmp) = setup_service();
        service.set_goal(sample_goal("fitness")).unwrap();
        service
            .set_goal(Goal {
                name: "reading".to_string(),
                target: "read 12 books".to_string(),
                ..sample_goal("fitness")
            })
            .unwrap();

        let goals = service.load_goals().unwrap();
        assert_eq!(goals.len(), 2);
    }

    #[test]
    fn goal_serde_roundtrip() {
        let goal = sample_goal("test");
        let toml_str = toml::to_string_pretty(&goal).unwrap();
        let deserialized: Goal = toml::from_str(&toml_str).unwrap();
        assert_eq!(goal, deserialized);
    }
}
