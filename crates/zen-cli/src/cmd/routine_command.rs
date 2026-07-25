use clap::Subcommand;
use tracing::info;

use zen_agents::scheduler::create_default_scheduler;
use zen_core::errors::ZenError;

#[derive(Subcommand)]
pub enum RoutineCommands {
    /// List all registered scheduler workers
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
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
        RoutineCommands::List { json } => {
            let scheduler = create_default_scheduler();
            let workers = scheduler.list();

            if workers.is_empty() {
                if *json {
                    println!("[]");
                } else {
                    println!("No workers registered.");
                }
                return Ok(());
            }

            if *json {
                let json_arr: Vec<serde_json::Value> = workers
                    .iter()
                    .map(|w| {
                        serde_json::json!({
                            "id": w.id,
                            "schedule": w.schedule,
                            "description": w.description,
                            "enabled": w.enabled,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json_arr).unwrap_or_default()
                );
            } else {
                println!(
                    "{:<15} {:<22} {:<7} DESCRIPTION",
                    "WORKER", "SCHEDULE", "ENABLED"
                );
                println!("{}", "-".repeat(80));
                for w in &workers {
                    let status = if w.enabled { "✓" } else { "✗" };
                    println!(
                        "{:<15} {:<22} {:<7} {}",
                        w.id, w.schedule, status, w.description
                    );
                }
                println!("\n{} worker(s) registered.", workers.len());
            }
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
            let mut scheduler = create_default_scheduler();
            match scheduler.enable(name) {
                Ok(()) => {
                    println!("✓ Worker '{name}' enabled");
                    Ok(())
                }
                Err(_) => Err(ZenError::Message(format!(
                    "worker '{name}' not registered — run `zen routine list` to see registered workers"
                ))),
            }
        }

        RoutineCommands::Disable { name } => {
            let mut scheduler = create_default_scheduler();
            match scheduler.disable(name) {
                Ok(()) => {
                    println!("✓ Worker '{name}' disabled");
                    Ok(())
                }
                Err(_) => Err(ZenError::Message(format!(
                    "worker '{name}' not registered — run `zen routine list` to see registered workers"
                ))),
            }
        }
    }
}
