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
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
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
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Service naming convention: zen-{provider}-api-key
fn service_name(provider: &str) -> String {
    format!("zen-{}-api-key", provider.to_lowercase())
}

/// Mask a stored secret for display: only the final 4 characters are
/// revealed (e.g. `••••••••ab12`); keys of 8 characters or fewer render
/// as `set` — the key body is never shown (CSO audit: first-8 leak).
fn mask_secret_tail(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 8 {
        return "set".to_string();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("••••••••{tail}")
}

/// Per-provider credential probe shared by the JSON and human render paths.
/// `source` is "keychain" | "env:<VAR>" | "access_denied" | None; `masked`
/// carries the display tail only (never the key body).
struct CredProbe {
    provider: &'static str,
    source: Option<String>,
    masked: Option<String>,
}

pub fn execute_command(operation: &AuthCommands) -> Result<(), ZenError> {
    match operation {
        AuthCommands::List { json } => {
            debug!("listing all stored credentials");

            let providers = SUPPORTED_LLM_PROVIDERS;

            let mut probes: Vec<CredProbe> = Vec::new();
            for provider in providers {
                let service = service_name(provider);
                let probe = match Keychain::retrieve(&service, "zen") {
                    Ok(key) => CredProbe {
                        provider,
                        source: Some("keychain".to_string()),
                        masked: Some(mask_secret_tail(&key)),
                    },
                    Err(AuthError::CredentialNotFound { .. }) => {
                        let env_var = format!("{}_API_KEY", provider.to_uppercase());
                        if std::env::var(&env_var).is_ok() {
                            CredProbe {
                                provider,
                                source: Some(format!("env:{env_var}")),
                                masked: None,
                            }
                        } else {
                            CredProbe {
                                provider,
                                source: None,
                                masked: None,
                            }
                        }
                    }
                    Err(_) => CredProbe {
                        provider,
                        source: Some("access_denied".to_string()),
                        masked: None,
                    },
                };
                probes.push(probe);
            }

            if *json {
                let payload = serde_json::json!({
                    "providers": probes
                        .iter()
                        .map(|p| serde_json::json!({
                            "provider": p.provider,
                            "source": p.source,
                            "masked": p.masked,
                        }))
                        .collect::<Vec<_>>(),
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload)
                        .map_err(|e| ZenError::Message(format!("json: {e}")))?
                );
                return Ok(());
            }

            println!("{}", "🔑 Providers & Credentials".bold());

            let found_count = probes.iter().filter(|p| p.source.is_some()).count();

            for p in &probes {
                let keychain_status = match (&p.source, &p.masked) {
                    (Some(_), Some(masked)) => {
                        format!("{} {}", "✓".green(), masked.dimmed())
                    }
                    (Some(src), None) if src.starts_with("env:") => {
                        format!("{} {}", "⚠".yellow(), src.dimmed())
                    }
                    (Some(_), None) => format!("{} {}", "✗".red(), "access denied".dimmed()),
                    (None, _) => format!("{} {}", "✗".bright_black(), "no credential".dimmed()),
                };
                println!("  {} {}", p.provider.cyan().bold(), keychain_status);
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

        AuthCommands::Status { json } => {
            debug!("showing auth status");

            let providers = SUPPORTED_LLM_PROVIDERS;
            let mut any_found = false;

            let mut probes: Vec<CredProbe> = Vec::new();
            for provider in providers {
                let service = service_name(provider);
                let probe = match Keychain::retrieve(&service, "zen") {
                    Ok(key) => {
                        any_found = true;
                        CredProbe {
                            provider,
                            source: Some("keychain".to_string()),
                            masked: Some(mask_secret_tail(&key)),
                        }
                    }
                    Err(AuthError::CredentialNotFound { .. }) => {
                        let env_var = format!("{}_API_KEY", provider.to_uppercase());
                        if std::env::var(&env_var).is_ok() {
                            any_found = true;
                            CredProbe {
                                provider,
                                source: Some(format!("env:{env_var}")),
                                masked: None,
                            }
                        } else {
                            CredProbe {
                                provider,
                                source: None,
                                masked: None,
                            }
                        }
                    }
                    Err(e) => CredProbe {
                        provider,
                        source: Some("error".to_string()),
                        masked: Some(e.to_string()),
                    },
                };
                probes.push(probe);
            }

            if *json {
                let payload = serde_json::json!({
                    "authenticated": any_found,
                    "providers": probes
                        .iter()
                        .map(|p| serde_json::json!({
                            "provider": p.provider,
                            "source": p.source,
                            "masked": p.masked,
                        }))
                        .collect::<Vec<_>>(),
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload)
                        .map_err(|e| ZenError::Message(format!("json: {e}")))?
                );
                return Ok(());
            }

            println!("{}", "🔐 Authentication Status".bold());

            for p in &probes {
                match (&p.source, &p.masked) {
                    (Some(src), Some(masked)) if src == "keychain" => println!(
                        "  {} {} — {}",
                        "✓".green(),
                        p.provider.cyan(),
                        format!("logged in (Keychain), {masked}").dimmed()
                    ),
                    (Some(src), None) if src.starts_with("env:") => {
                        println!(
                            "  {} {} — {}",
                            "⚠".yellow(),
                            p.provider.cyan(),
                            format!("{src} var set").dimmed()
                        )
                    }
                    (Some(_), Some(msg)) => {
                        println!("  {} {} — {}", "✗".red(), p.provider.cyan(), msg.dimmed())
                    }
                    _ => {}
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

    #[test]
    fn mask_secret_tail_shows_only_last_four() {
        assert_eq!(mask_secret_tail("sk-abcdefghijklmnop"), "••••••••mnop");
        assert_eq!(mask_secret_tail("123456789"), "••••••••6789");
    }

    #[test]
    fn mask_secret_tail_short_keys_render_set() {
        assert_eq!(mask_secret_tail(""), "set");
        assert_eq!(mask_secret_tail("short"), "set");
        assert_eq!(mask_secret_tail("12345678"), "set");
    }
}
