use clap::Subcommand;
use colored::Colorize;
use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_vault::habit::{Habit, HabitService};

#[derive(Subcommand)]
pub enum HabitCommands {
    /// Record a habit check-in
    CheckIn {
        name: String,
        #[arg(short, long)]
        note: Option<String>,
    },
    /// List all habits with streaks
    List,
    /// Show habit statistics
    Stats {
        name: String,
        #[arg(short = 'd', long, default_value = "30")]
        days: u32,
    },
    /// Add a new habit definition
    Add {
        name: String,
        #[arg(short = 'f', long, default_value = "daily")]
        frequency: String,
        #[arg(short, long)]
        target: Option<String>,
    },
    /// Remove a habit
    Remove { name: String },
}

pub fn execute_command(cmd: &HabitCommands) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
    let service = HabitService::new(&paths);

    match cmd {
        HabitCommands::CheckIn { name, note } => {
            service.check_in(name, note.clone())?;
            println!(
                "{} Checked in to habit '{}'",
                "✓".green().bold(),
                name.cyan()
            );
            Ok(())
        }

        HabitCommands::List => {
            let habits = service.load_habits()?;
            if habits.is_empty() {
                println!("No habits defined. Use {} to add one.", "zen habit add".dimmed());
                return Ok(());
            }

            println!(
                "{:<20} {:<10} {:<10} {}",
                "HABIT".bold(),
                "FREQ".bold(),
                "STREAK".bold(),
                "TARGET".bold()
            );
            println!("{}", "-".repeat(55));

            for habit in &habits {
                let streak = service.get_streak(&habit.name)?;
                let target_display = habit.target.as_deref().unwrap_or("-");
                println!(
                    "{:<20} {:<10} {:<10} {}",
                    habit.name.cyan(),
                    habit.frequency,
                    format!("{streak}d").green(),
                    target_display
                );
            }
            println!("\n{} habit(s) total.", habits.len());
            Ok(())
        }

        HabitCommands::Stats { name, days } => {
            let habit = service
                .load_habits()?
                .into_iter()
                .find(|h| h.name == *name)
                .ok_or_else(|| ZenError::Message(format!("habit '{name}' not found")))?;

            let streak = service.get_streak(&habit.name)?;
            let rate = service.get_completion_rate(&habit.name, *days)?;
            let checkins = service.get_checkins(&habit.name)?;

            println!("{}", format!("Stats for '{}'", habit.name).bold());
            println!("  Frequency:  {}", habit.frequency);
            if let Some(ref target) = habit.target {
                println!("  Target:     {target}");
            }
            println!("  Streak:     {} days", streak.to_string().green());
            println!(
                "  Completion: {:.0}% (last {} days)",
                rate * 100.0,
                days
            );
            println!("  Check-ins:  {}", checkins.len());

            Ok(())
        }

        HabitCommands::Add {
            name,
            frequency,
            target,
        } => {
            let habit = Habit {
                name: name.clone(),
                frequency: frequency.clone(),
                target: target.clone(),
                reminders_enabled: true,
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            service.add_habit(habit)?;
            println!(
                "{} Added habit '{}' (frequency: {})",
                "✓".green().bold(),
                name.cyan(),
                frequency
            );
            Ok(())
        }

        HabitCommands::Remove { name } => {
            service.remove_habit(name)?;
            println!(
                "{} Removed habit '{}'",
                "✓".green().bold(),
                name.cyan()
            );
            Ok(())
        }
    }
}
