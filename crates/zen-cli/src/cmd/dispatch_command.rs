use std::path::PathBuf;

use clap::Subcommand;
use colored::Colorize;
use tracing::info;

use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;
use zen_vault::dispatch::{DispatchService, DispatchTarget};

#[derive(Subcommand)]
pub enum DispatchCommands {
    /// Dispatch a task to a sub-agent (codex/opencode)
    Run {
        /// Task description
        task: String,
        /// Target sub-agent
        #[arg(short, long, default_value = "codex")]
        to: String,
        /// Context files to inject (paths relative to CWD or absolute)
        #[arg(short, long)]
        context: Option<Vec<String>>,
        /// Timeout in seconds
        #[arg(short, long, default_value = "300")]
        timeout: u64,
    },
    /// Check status of a dispatch task
    Status {
        /// Task ID
        task_id: String,
    },
    /// List all dispatch tasks
    List,
    /// Cancel a dispatch task
    Cancel {
        /// Task ID
        task_id: String,
    },
}

pub async fn execute_command(cmd: &DispatchCommands) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;
    let service = DispatchService::from_paths(&paths);

    match cmd {
        DispatchCommands::Run {
            task,
            to,
            context,
            timeout,
        } => {
            let target = DispatchTarget::from_str(to)
                .map_err(|e| ZenError::Message(format!("invalid target '{to}': {e}")))?;

            let context_files: Vec<PathBuf> = context
                .as_ref()
                .map(|files| files.iter().map(PathBuf::from).collect())
                .unwrap_or_default();

            info!(task = task.as_str(), target = %target, "dispatching task");

            println!(
                "{} Dispatching to {}...",
                "→".cyan().bold(),
                target.to_string().cyan()
            );

            let result = service
                .dispatch(task, target, &context_files, *timeout)
                .await
                .map_err(|e| ZenError::Message(format!("dispatch failed: {e}")))?;

            println!(
                "\n{} Task {} — {}",
                "✓".green().bold(),
                result.short_id().cyan(),
                result.status.to_string().bold()
            );
            println!("  ID: {}", result.id);

            if !result.files_changed.is_empty() {
                println!("\n  Files Changed:");
                for f in &result.files_changed {
                    println!("    {} {}", "•".dimmed(), f);
                }
            }
            if !result.key_decisions.is_empty() {
                println!("\n  Key Decisions:");
                for d in &result.key_decisions {
                    println!("    {} {}", "•".dimmed(), d);
                }
            }
            if let Some(err) = &result.error {
                println!("\n  {} {}", "Error:".red().bold(), err);
            }

            Ok(())
        }

        DispatchCommands::Status { task_id } => {
            let task = service
                .load_task(task_id)
                .map_err(|e| ZenError::Message(format!("task not found: {e}")))?;

            println!(
                "Task {} — {}",
                task.short_id().cyan(),
                task.status.to_string().bold()
            );
            println!("  Target:     {}", task.target);
            println!("  Created:    {}", task.created_at);
            if let Some(completed) = &task.completed_at {
                println!("  Completed:  {}", completed);
            }
            println!("  Task:       {}", task.task_description);

            if !task.files_changed.is_empty() {
                println!("\n  Files Changed:");
                for f in &task.files_changed {
                    println!("    {} {}", "•".dimmed(), f);
                }
            }
            if !task.key_decisions.is_empty() {
                println!("\n  Key Decisions:");
                for d in &task.key_decisions {
                    println!("    {} {}", "•".dimmed(), d);
                }
            }
            if !task.lessons_learned.is_empty() {
                println!("\n  Lessons Learned:");
                for l in &task.lessons_learned {
                    println!("    {} {}", "•".dimmed(), l);
                }
            }
            if let Some(err) = &task.error {
                println!("\n  {} {}", "Error:".red().bold(), err);
            }

            Ok(())
        }

        DispatchCommands::List => {
            let tasks = service.list_tasks();

            if tasks.is_empty() {
                println!("No dispatch tasks found.");
                return Ok(());
            }

            println!("{:<10} {:<10} {:<12} CREATED", "ID", "TARGET", "STATUS");
            println!("{}", "-".repeat(60));

            for task in &tasks {
                println!(
                    "{:<10} {:<10} {:<12} {}",
                    task.short_id(),
                    task.target,
                    task.status,
                    &task.created_at[..10]
                );
            }

            println!("\n{} task(s).", tasks.len());
            Ok(())
        }
        DispatchCommands::Cancel { task_id } => {
            let task = service
                .load_task(task_id)
                .map_err(|e| ZenError::Message(format!("task not found: {e}")))?;

            println!("Task {} — {}", task.short_id().cyan(), "cancelled".bold());
            println!("  Target:     {}", task.target);
            println!("  Task:       {}", task.task_description);
            Ok(())
        }
    }
}
