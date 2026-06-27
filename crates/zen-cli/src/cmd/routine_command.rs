use clap::Subcommand;
use tracing::info;

use zen_agents::scheduler::create_default_scheduler;
use zen_core::errors::ZenError;

#[derive(Subcommand)]
pub enum RoutineCommands {
    /// List all registered scheduler workers
    List,
    /// Trigger a worker by name immediately (e.g. "dream", "journal-worker", "subconscious")
    Trigger {
        /// Worker name
        name: String,
    },
    /// Enable a routine (not yet implemented)
    Enable {
        /// Routine name
        name: String,
    },
    /// Disable a routine (not yet implemented)
    Disable {
        /// Routine name
        name: String,
    },
}

pub async fn execute_command(cmd: &RoutineCommands) -> Result<(), ZenError> {
    match cmd {
        RoutineCommands::List => {
            let scheduler = create_default_scheduler();
            let workers = scheduler.list();

            if workers.is_empty() {
                println!("No workers registered.");
                return Ok(());
            }

            println!(
                "{:<15} {:<22} DESCRIPTION",
                "WORKER", "SCHEDULE"
            );
            println!("{}", "-".repeat(70));
            for w in &workers {
                println!("{:<15} {:<22} {}", w.id, w.schedule, w.description);
            }
            println!("\n{} worker(s) registered.", workers.len());
            Ok(())
        }

        RoutineCommands::Trigger { name } => {
            info!(worker = name.as_str(), "routine: manual trigger");
            let scheduler = create_default_scheduler();

            match scheduler.trigger(name).await {
                Ok(report) => {
                    println!(
                        "Worker '{}' completed — success={}, facts={}, duration={}ms",
                        report.worker_id, report.success, report.fact_count, report.duration_ms
                    );
                    Ok(())
                }
                Err(e) => Err(ZenError::Message(format!(
                    "failed to trigger worker '{name}': {e}"
                ))),
            }
        }

        RoutineCommands::Enable { name } => {
            println!("enable not yet implemented for worker '{name}'");
            Ok(())
        }

        RoutineCommands::Disable { name } => {
            println!("disable not yet implemented for worker '{name}'");
            Ok(())
        }
    }
}
