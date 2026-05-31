use clap::Subcommand;
use colored::Colorize;
use tracing::debug;

use zen_auth::{AuthError, Keychain};
use zen_core::constants::SUPPORTED_LLM_PROVIDERS;
use zen_core::errors::ZenError;

// ---------------------------------------------------------------------------
// Auth subcommands — Keychain credential management (FR-061)
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub enum AuthCommands {
    /// List providers and credentials (alias: ls)
    #[command(visible_alias = "ls")]
    List,
    /// Log in to a provider (stores API key in Keychain)
    Login {
        /// Provider name (e.g., "openai", "aliyun", "deepseek")
        provider: String,
        /// API key to store (will be prompted if not provided)
        #[arg(short, long)]
        key: Option<String>,
    },
    /// Log out from a configured provider
    Logout {
        /// Provider name to logout
        #[arg(short, long)]
        provider: Option<String>,
    },
    /// Show authentication status
    Status,
}

/// Service naming convention: zen-{provider}-api-key
fn service_name(provider: &str) -> String {
    format!("zen-{}-api-key", provider.to_lowercase())
}

pub fn execute_command(operation: &AuthCommands) -> Result<(), ZenError> {
    match operation {
        AuthCommands::List => {
            debug!("listing all stored credentials");
            println!("{}", "🔑 Providers & Credentials".bold());

            let providers = SUPPORTED_LLM_PROVIDERS;

            let mut found_count = 0;

            for provider in providers {
                let service = service_name(provider);
                let keychain_status = match Keychain::retrieve(&service, "zen") {
                    Ok(key) => {
                        found_count += 1;
                        let masked = if key.len() > 12 {
                            format!("{}...{}", &key[..8], &key[key.len() - 4..])
                        } else {
                            "****".to_string()
                        };
                        format!("{} {}", "✓".green(), masked.dimmed())
                    }
                    Err(AuthError::CredentialNotFound { .. }) => {
                        let env_var = format!("{}_API_KEY", provider.to_uppercase());
                        if std::env::var(&env_var).is_ok() {
                            found_count += 1;
                            format!("{} {}", "⚠".yellow(), format!("env:{}", env_var).dimmed())
                        } else {
                            format!("{} {}", "✗".bright_black(), "no credential".dimmed())
                        }
                    }
                    Err(_) => format!("{} {}", "✗".red(), "access denied".dimmed()),
                };

                println!("  {} {}", provider.cyan().bold(), keychain_status);
            }

            println!();
            println!(
                "{}",
                format!(
                    "  {} providers checked, {} with credentials",
                    providers.len(),
                    found_count
                )
                .dimmed()
            );

            Ok(())
        }

        AuthCommands::Login { provider, key } => {
            debug!("logging in to provider: {}", provider);

            let service = service_name(provider);
            let api_key = match key {
                Some(k) => k.clone(),
                None => {
                    println!("{}", "🔑 Enter API key (input hidden):".yellow());
                    let input = rpassword::read_password().map_err(|e| {
                        ZenError::Message(format!("Failed to read password: {}", e))
                    })?;
                    if input.is_empty() {
                        return Err(ZenError::Message("API key cannot be empty".to_string()));
                    }
                    input
                }
            };

            Keychain::store(&service, "zen", &api_key).map_err(|e| map_auth_error(e, provider))?;

            println!(
                "{} Logged in to {}",
                "✓".green().bold(),
                provider.cyan().bold()
            );
            println!(
                "  Credential stored in macOS Keychain ({})",
                service.dimmed()
            );

            Ok(())
        }

        AuthCommands::Logout { provider } => {
            match provider {
                Some(p) => {
                    debug!("logging out from provider: {}", p);
                    let service = service_name(p);

                    Keychain::delete(&service, "zen").map_err(|e| map_auth_error(e, p))?;

                    println!("{} Logged out from {}", "✓".green().bold(), p.cyan().bold());
                    println!("  Credential removed from Keychain");
                }
                None => {
                    debug!("logging out from all providers");
                    let providers = SUPPORTED_LLM_PROVIDERS;
                    let mut count = 0;

                    for p in providers {
                        let service = service_name(p);
                        if Keychain::delete(&service, "zen").is_ok() {
                            count += 1;
                            println!("{} Logged out from {}", "✓".green(), p.cyan());
                        }
                    }

                    println!();
                    println!(
                        "{}",
                        format!("Removed {} credentials from Keychain", count).dimmed()
                    );
                }
            }

            Ok(())
        }

        AuthCommands::Status => {
            debug!("showing auth status");
            println!("{}", "🔐 Authentication Status".bold());

            let providers = SUPPORTED_LLM_PROVIDERS;
            let mut any_found = false;

            for provider in providers {
                let service = service_name(provider);
                match Keychain::retrieve(&service, "zen") {
                    Ok(_) => {
                        any_found = true;
                        println!(
                            "  {} {} — {}",
                            "✓".green(),
                            provider.cyan(),
                            "logged in (Keychain)".dimmed()
                        );
                    }
                    Err(AuthError::CredentialNotFound { .. }) => {
                        let env_var = format!("{}_API_KEY", provider.to_uppercase());
                        if std::env::var(&env_var).is_ok() {
                            any_found = true;
                            println!(
                                "  {} {} — {}",
                                "⚠".yellow(),
                                provider.cyan(),
                                format!("env var {} set", env_var).dimmed()
                            );
                        }
                    }
                    Err(e) => {
                        println!(
                            "  {} {} — {}",
                            "✗".red(),
                            provider.cyan(),
                            e.to_string().dimmed()
                        );
                    }
                }
            }

            if !any_found {
                println!(
                    "{}",
                    "  No credentials found. Use 'zen auth login <provider>' to authenticate."
                        .dimmed()
                );
            }

            Ok(())
        }
    }
}

fn map_auth_error(e: AuthError, provider: &str) -> ZenError {
    match e {
        AuthError::KeychainAccessDenied { service } => ZenError::Message(format!(
            "Keychain access denied for service '{}'. Check macOS Keychain permissions.",
            service
        )),
        AuthError::CredentialNotFound { service, .. } => ZenError::Message(format!(
            "No credential found for provider '{}' (service: '{}'). Use 'zen auth login {} --key <key>' to store.",
            provider, service, provider
        )),
        AuthError::Keychain(msg) => ZenError::Message(format!("Keychain error: {}", msg)),
        AuthError::EnvVarNotSet(var) => {
            ZenError::Message(format!("Environment variable '{}' not set", var))
        }
        AuthError::ResolutionFailed { reason } => {
            ZenError::Message(format!("Credential resolution failed: {}", reason))
        }
        AuthError::KeychainUnavailable { platform, message } => ZenError::Message(format!(
            "Keychain unavailable on {}. {}. Use SecretRef::Env or api_key_env fallback.",
            platform, message
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_name_format() {
        assert_eq!(service_name("openai"), "zen-openai-api-key");
        assert_eq!(service_name("ALIYUN"), "zen-aliyun-api-key");
        assert_eq!(service_name("DeepSeek"), "zen-deepseek-api-key");
    }
}
