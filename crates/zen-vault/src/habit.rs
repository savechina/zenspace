use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;

/// A habit definition stored in `~/.zen/habits.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Habit {
    pub name: String,
    pub frequency: String,
    pub target: Option<String>,
    pub reminders_enabled: bool,
    pub created_at: String,
}

/// A check-in record stored in `~/.zen/habits-checkins.jsonl` (append-only).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HabitCheckIn {
    pub habit_name: String,
    pub timestamp: String,
    pub note: Option<String>,
}

/// Top-level TOML structure for `habits.toml`.
#[derive(Debug, Serialize, Deserialize)]
struct HabitsToml {
    habits: Vec<Habit>,
}

pub struct HabitService {
    habits_path: PathBuf,
    checkins_path: PathBuf,
}

impl HabitService {
    pub fn new(paths: &ZenPaths) -> Self {
        Self {
            habits_path: paths.global_root().join("habits.toml"),
            checkins_path: paths.global_root().join("habits-checkins.jsonl"),
        }
    }

    /// Create a service with custom paths (for testing).
    pub fn with_paths(habits_path: PathBuf, checkins_path: PathBuf) -> Self {
        Self {
            habits_path,
            checkins_path,
        }
    }

    /// Parse `habits.toml` and return the list of habits.
    pub fn load_habits(&self) -> Result<Vec<Habit>, ZenError> {
        if !self.habits_path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.habits_path).map_err(ZenError::Io)?;
        let parsed: HabitsToml = toml::from_str(&content)
            .map_err(|e| ZenError::Message(format!("failed to parse habits.toml: {e}")))?;
        Ok(parsed.habits)
    }

    /// Add a habit to `habits.toml`.
    pub fn add_habit(&self, habit: Habit) -> Result<(), ZenError> {
        let mut habits = self.load_habits()?;
        if habits.iter().any(|h| h.name == habit.name) {
            return Err(ZenError::Message(format!(
                "habit '{}' already exists",
                habit.name
            )));
        }
        habits.push(habit);
        self.write_habits(&habits)
    }

    /// Remove a habit by name from `habits.toml`.
    pub fn remove_habit(&self, name: &str) -> Result<(), ZenError> {
        let mut habits = self.load_habits()?;
        let before = habits.len();
        habits.retain(|h| h.name != name);
        if habits.len() == before {
            return Err(ZenError::Message(format!("habit '{name}' not found")));
        }
        self.write_habits(&habits)
    }

    pub fn check_in(&self, name: &str, note: Option<String>) -> Result<(), ZenError> {
        let habits = self.load_habits()?;
        if !habits.iter().any(|h| h.name == name) {
            return Err(ZenError::Message(format!("habit '{name}' not found")));
        }

        let record = HabitCheckIn {
            habit_name: name.to_string(),
            timestamp: Utc::now().to_rfc3339(),
            note,
        };

        let line = serde_json::to_string(&record).map_err(ZenError::Serialization)?;

        if let Some(parent) = self.checkins_path.parent() {
            fs::create_dir_all(parent).map_err(ZenError::Io)?;
        }

        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.checkins_path)
            .map_err(ZenError::Io)?;

        writeln!(file, "{line}").map_err(ZenError::Io)
    }

    /// Read all check-ins and filter by habit name.
    pub fn get_checkins(&self, name: &str) -> Result<Vec<HabitCheckIn>, ZenError> {
        if !self.checkins_path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(&self.checkins_path).map_err(ZenError::Io)?;
        let reader = BufReader::new(file);

        let mut checkins = Vec::new();
        for line_result in reader.lines() {
            let line = line_result.map_err(ZenError::Io)?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(record) = serde_json::from_str::<HabitCheckIn>(trimmed)
                && record.habit_name == name
            {
                checkins.push(record);
            }
        }
        Ok(checkins)
    }

    /// Calculate the current streak (consecutive days) for a habit.
    ///
    /// A streak counts backward from today. Each day that has at least one
    /// check-in counts toward the streak.
    pub fn get_streak(&self, name: &str) -> Result<u32, ZenError> {
        let checkins = self.get_checkins(name)?;
        if checkins.is_empty() {
            return Ok(0);
        }

        let mut dates: Vec<chrono::NaiveDate> = checkins
            .iter()
            .filter_map(|c| chrono::DateTime::parse_from_rfc3339(&c.timestamp).ok())
            .map(|dt| dt.date_naive())
            .collect();
        dates.sort_unstable();
        dates.dedup();

        let today = Utc::now().date_naive();

        let streak_start = if dates.last() == Some(&today) {
            today
        } else if dates.last() == Some(&(today - Duration::days(1))) {
            today - Duration::days(1)
        } else {
            return Ok(0);
        };

        let mut streak: u32 = 0;
        let mut expected = streak_start;
        for date in dates.iter().rev() {
            if *date == expected {
                streak += 1;
                expected -= Duration::days(1);
            } else if *date < expected {
                break;
            }
        }

        Ok(streak)
    }

    /// Calculate the completion rate over the last N days.
    ///
    /// Returns a value between 0.0 and 1.0 representing what fraction of days
    /// had at least one check-in.
    pub fn get_completion_rate(&self, name: &str, days: u32) -> Result<f64, ZenError> {
        let checkins = self.get_checkins(name)?;
        let today = Utc::now().date_naive();

        let dates: Vec<chrono::NaiveDate> = checkins
            .iter()
            .filter_map(|c| chrono::DateTime::parse_from_rfc3339(&c.timestamp).ok())
            .map(|dt| dt.date_naive())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let mut checked_in_days = 0u32;
        for offset in 0..days {
            let day = today - Duration::days(i64::from(offset));
            if dates.contains(&day) {
                checked_in_days += 1;
            }
        }

        Ok(checked_in_days as f64 / days as f64)
    }

    fn write_habits(&self, habits: &[Habit]) -> Result<(), ZenError> {
        let toml_struct = HabitsToml {
            habits: habits.to_vec(),
        };
        let content = toml::to_string_pretty(&toml_struct)
            .map_err(|e| ZenError::Message(format!("failed to serialize habits: {e}")))?;

        if let Some(parent) = self.habits_path.parent() {
            fs::create_dir_all(parent).map_err(ZenError::Io)?;
        }

        fs::write(&self.habits_path, content).map_err(ZenError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_service() -> (HabitService, TempDir) {
        let tmp = TempDir::new().unwrap();
        let habits_path = tmp.path().join("habits.toml");
        let checkins_path = tmp.path().join("checkins.jsonl");
        let service = HabitService::with_paths(habits_path, checkins_path);
        (service, tmp)
    }

    fn sample_habit(name: &str) -> Habit {
        Habit {
            name: name.to_string(),
            frequency: "daily".to_string(),
            target: Some("30 minutes exercise".to_string()),
            reminders_enabled: true,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn load_empty_when_no_file() {
        let (service, _tmp) = setup_service();
        let habits = service.load_habits().unwrap();
        assert!(habits.is_empty());
    }

    #[test]
    fn add_and_load_habit() {
        let (service, _tmp) = setup_service();
        let habit = sample_habit("exercise");
        service.add_habit(habit.clone()).unwrap();

        let habits = service.load_habits().unwrap();
        assert_eq!(habits.len(), 1);
        assert_eq!(habits[0].name, "exercise");
        assert_eq!(habits[0].frequency, "daily");
    }

    #[test]
    fn reject_duplicate_habit() {
        let (service, _tmp) = setup_service();
        service.add_habit(sample_habit("exercise")).unwrap();
        let result = service.add_habit(sample_habit("exercise"));
        assert!(result.is_err());
    }

    #[test]
    fn remove_habit() {
        let (service, _tmp) = setup_service();
        service.add_habit(sample_habit("exercise")).unwrap();
        service.remove_habit("exercise").unwrap();

        let habits = service.load_habits().unwrap();
        assert!(habits.is_empty());
    }

    #[test]
    fn remove_nonexistent_habit_errors() {
        let (service, _tmp) = setup_service();
        let result = service.remove_habit("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn check_in_and_get_records() {
        let (service, _tmp) = setup_service();
        service.add_habit(sample_habit("meditate")).unwrap();

        service
            .check_in("meditate", Some("felt calm".to_string()))
            .unwrap();
        service.check_in("meditate", None).unwrap();

        let checkins = service.get_checkins("meditate").unwrap();
        assert_eq!(checkins.len(), 2);
        assert_eq!(checkins[0].note.as_deref(), Some("felt calm"));
        assert_eq!(checkins[1].note, None);
    }

    #[test]
    fn check_in_nonexistent_habit_errors() {
        let (service, _tmp) = setup_service();
        let result = service.check_in("nope", None);
        assert!(result.is_err());
    }

    #[test]
    fn get_checkins_returns_empty_for_unknown() {
        let (service, _tmp) = setup_service();
        let checkins = service.get_checkins("unknown").unwrap();
        assert!(checkins.is_empty());
    }

    #[test]
    fn multiple_habits_coexist() {
        let (service, _tmp) = setup_service();
        service.add_habit(sample_habit("exercise")).unwrap();
        service.add_habit(sample_habit("reading")).unwrap();

        let habits = service.load_habits().unwrap();
        assert_eq!(habits.len(), 2);
    }

    #[test]
    fn habit_serde_roundtrip() {
        let habit = sample_habit("test");
        let toml_str = toml::to_string_pretty(&habit).unwrap();
        let deserialized: Habit = toml::from_str(&toml_str).unwrap();
        assert_eq!(habit, deserialized);
    }

    #[test]
    fn streak_zero_when_no_checkins() {
        let (service, _tmp) = setup_service();
        service.add_habit(sample_habit("running")).unwrap();
        let streak = service.get_streak("running").unwrap();
        assert_eq!(streak, 0);
    }
}
