use clap::Subcommand;
use colored::Colorize;
use zen_agents::skill_history::{SkillExecutionRecord, SkillHistory};
use zen_agents::skill_loader::SkillLoader;
use zen_core::errors::ZenError;
use zen_core::paths::ZenPaths;

#[derive(Subcommand)]
pub enum SkillCommands {
    /// List all available skills
    List,
    /// Run a skill by name
    Run {
        /// Skill name
        name: String,
        /// Optional input to include in context
        #[arg(short, long)]
        input: Option<String>,
        /// Rate execution quality (0-10) after run
        #[arg(long)]
        rate: Option<u8>,
    },
    /// Show execution stats for a skill
    Progress {
        /// Skill name
        name: String,
    },
    /// Show full skill definition
    Show {
        /// Skill name
        name: String,
    },
}

pub async fn execute_command(cmd: &SkillCommands) -> Result<(), ZenError> {
    let paths = ZenPaths::detect().map_err(|e| ZenError::Message(e.to_string()))?;

    match cmd {
        SkillCommands::List => {
            let loader = SkillLoader::new(&paths);
            let skills = loader
                .list_skills()
                .map_err(|e| ZenError::Message(e.to_string()))?;

            if skills.is_empty() {
                println!("No skills found in {}", paths.skills().display());
                return Ok(());
            }

            println!("{:<20} DESCRIPTION", "SKILL");
            println!("{}", "-".repeat(50));

            for name in &skills {
                match loader.load_skill(name) {
                    Ok(def) => {
                        println!("{:<20} {}", name.green().bold(), def.description);
                    }
                    Err(_) => {
                        println!("{:<20} (parse error)", name.green().bold());
                    }
                }
            }

            println!("\n{} skill(s) found.", skills.len());
            Ok(())
        }

        SkillCommands::Run { name, input, rate } => {
            let loader = SkillLoader::new(&paths);

            if !loader.skill_exists(name) {
                return Err(ZenError::Message(format!("skill '{name}' not found")));
            }

            let def = loader
                .load_skill(name)
                .map_err(|e| ZenError::Message(e.to_string()))?;

            println!("{} {}", "Skill:".bold(), def.name.green().bold());
            println!("{}\n", def.description.dimmed());

            for ctx_file in &def.context_files {
                let full_path = paths.skills().join(ctx_file);
                if full_path.is_file() {
                    match std::fs::read_to_string(&full_path) {
                        Ok(content) => {
                            println!(
                                "{} {}",
                                "Context:".bold(),
                                ctx_file.display().to_string().cyan()
                            );
                            println!("{content}\n");
                        }
                        Err(e) => {
                            println!(
                                "{} failed to read {}: {e}",
                                "Warning:".yellow(),
                                ctx_file.display()
                            );
                        }
                    }
                }
            }

            if let Some(user_input) = input {
                println!("{} {user_input}\n", "Input:".bold());
            }

            println!("{}\n", def.prompt);
            println!("{}\n", def.body);

            let record = SkillExecutionRecord {
                skill_name: def.name.clone(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                duration_ms: 0,
                quality_rating: *rate,
                context_summary: input.clone().unwrap_or_default(),
                result_summary: "executed".to_string(),
            };

            let history = SkillHistory::new(&paths);
            history
                .log_execution(record)
                .map_err(|e| ZenError::Message(e.to_string()))?;

            println!("{}", "Execution logged.".dimmed());
            Ok(())
        }

        SkillCommands::Progress { name } => {
            let history = SkillHistory::new(&paths);
            let stats = history
                .get_stats(name)
                .map_err(|e| ZenError::Message(e.to_string()))?;

            if stats.total_runs == 0 {
                println!("No execution history for '{name}'.");
                return Ok(());
            }

            println!("{}", format!("Stats for '{name}':").bold());
            println!("  Total runs:     {}", stats.total_runs.to_string().bold());
            println!(
                "  Avg quality:    {:.1}",
                stats.avg_quality.to_string().bold()
            );
            println!(
                "  Total time:     {}ms",
                stats.total_time_ms.to_string().bold()
            );
            if let Some(ref last) = stats.last_run {
                println!("  Last run:       {}", last.cyan());
            }

            Ok(())
        }

        SkillCommands::Show { name } => {
            let loader = SkillLoader::new(&paths);

            if !loader.skill_exists(name) {
                return Err(ZenError::Message(format!("skill '{name}' not found")));
            }

            let def = loader
                .load_skill(name)
                .map_err(|e| ZenError::Message(e.to_string()))?;

            println!("{}", "─".repeat(50));
            println!("{} {}", "Skill:".bold(), def.name.green().bold());
            println!("{} {}", "Description:".bold(), def.description);

            if !def.tools.is_empty() {
                println!("{} {}", "Tools:".bold(), def.tools.join(", ").cyan());
            }

            if !def.context_files.is_empty() {
                let files: Vec<String> = def
                    .context_files
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                println!("{} {}", "Context files:".bold(), files.join(", ").cyan());
            }

            if !def.prompt.is_empty() {
                println!("{}\n{}", "Prompt:".bold(), def.prompt);
            }

            println!("{}\n{}", "Body:".bold(), def.body);
            println!("{}", "─".repeat(50));

            Ok(())
        }
    }
}
