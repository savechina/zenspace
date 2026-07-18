use clap::Subcommand;
use colored::Colorize;
use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_vault::goal::{Goal, GoalService};

#[derive(Subcommand)]
pub enum GoalCommands {
    /// Set or update a goal
    Set {
        name: String,
        #[arg(short, long)]
        target: String,
        #[arg(short, long)]
        by: Option<String>,
        #[arg(short = 'H', long)]
        habits: Option<Vec<String>>,
    },
    /// Show goal progress
    Progress {
        name: String,
        #[arg(short, long)]
        progress: Option<f64>,
    },
    /// List all goals
    List,
    /// Mark goal as completed
    Complete { name: String },
}

pub fn execute_command(cmd: &GoalCommands) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
    let service = GoalService::new(&paths);

    match cmd {
        GoalCommands::Set {
            name,
            target,
            by,
            habits,
        } => {
            let goal = Goal {
                name: name.clone(),
                target: target.clone(),
                deadline: by.clone(),
                linked_habits: habits.clone().unwrap_or_default(),
                linked_skills: Vec::new(),
                created_at: chrono::Utc::now().to_rfc3339(),
                status: "active".to_string(),
                progress: 0.0,
            };
            service.set_goal(goal)?;
            println!(
                "{} Set goal '{}' — {}",
                "✓".green().bold(),
                name.cyan(),
                target
            );
            if let Some(deadline) = by {
                println!("  Deadline: {deadline}");
            }
            Ok(())
        }

        GoalCommands::Progress { name, progress } => {
            if let Some(p) = progress {
                service.update_progress(name, *p)?;
                println!(
                    "{} Updated '{}' progress to {:.0}%",
                    "✓".green().bold(),
                    name.cyan(),
                    p * 100.0
                );
                return Ok(());
            }

            let goal = service
                .get_goal(name)?
                .ok_or_else(|| ZenError::Message(format!("goal '{name}' not found")))?;

            println!("{}", format!("Goal '{}'", goal.name).bold());
            println!("  Target:    {}", goal.target);
            println!("  Status:    {}", goal.status);
            println!(
                "  Progress:  {:.0}{}",
                goal.progress * 100.0,
                "%".green()
            );
            if let Some(ref deadline) = goal.deadline {
                println!("  Deadline:  {deadline}");
            }
            if !goal.linked_habits.is_empty() {
                println!("  Habits:    {}", goal.linked_habits.join(", "));
            }
            Ok(())
        }

        GoalCommands::List => {
            let goals = service.load_goals()?;
            if goals.is_empty() {
                println!("No goals defined. Use {} to add one.", "zen goal set".dimmed());
                return Ok(());
            }

            println!(
                "{:<20} {:<15} {:<10} {}",
                "GOAL".bold(),
                "STATUS".bold(),
                "PROGRESS".bold(),
                "TARGET".bold()
            );
            println!("{}", "-".repeat(60));

            for goal in &goals {
                let status_color = match goal.status.as_str() {
                    "completed" => goal.status.green(),
                    "abandoned" => goal.status.red(),
                    _ => goal.status.yellow(),
                };
                println!(
                    "{:<20} {:<15} {:<10} {}",
                    goal.name.cyan(),
                    status_color,
                    format!("{:.0}%", goal.progress * 100.0),
                    goal.target
                );
            }
            println!("\n{} goal(s) total.", goals.len());
            Ok(())
        }

        GoalCommands::Complete { name } => {
            service.complete_goal(name)?;
            println!(
                "{} Completed goal '{}'",
                "✓".green().bold(),
                name.cyan()
            );
            Ok(())
        }
    }
}
