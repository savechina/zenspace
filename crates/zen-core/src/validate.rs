use std::path::{Path, PathBuf};

const PROTECTED_NAMES: &[&str] = &["SOUL.md", "AGENTS.md", "MEMORY.md"];

const PROTECTED_DIRS: &[&str] = &["wiki", "skills", "plugins"];

const PROTECTED_FILES: &[&str] = &["config.toml"];

const RESTRICTED_COMMANDS: &[&str] = &["qq_bot_capture", "qq_bot_edit", "cli_note", "note_edit"];

#[derive(Debug, Clone, PartialEq)]
pub enum SensitivityLevel {
    Safe,
    Warning,
    Protected,
}

#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub allowed: bool,
    pub sensitivity: SensitivityLevel,
    pub reason: Option<String>,
    pub matched_rule: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("safety block: {reason}")]
    SafetyBlock { reason: String },

    #[error("validation error: {reason}")]
    Invalid { reason: String },
}

pub struct RoleSeparationValidator {
    zen_root: PathBuf,
}

impl Default for RoleSeparationValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl RoleSeparationValidator {
    pub fn new() -> Self {
        Self {
            zen_root: home::home_dir().unwrap_or_default().join(".zen"),
        }
    }

    pub fn with_zen_root(zen_root: PathBuf) -> Self {
        Self { zen_root }
    }

    pub fn zen_root(&self) -> &Path {
        &self.zen_root
    }

    fn is_protected_path(&self, path: &Path) -> bool {
        let canonical = path.to_string_lossy();

        for name in PROTECTED_NAMES {
            if canonical.ends_with(name) && self.is_in_zen_tree(path) {
                return true;
            }
        }

        for dirname in PROTECTED_DIRS {
            if (canonical.contains(&format!("/{dirname}/"))
                || canonical.ends_with(&format!("/{}", dirname)))
                && self.is_in_zen_tree(path)
            {
                return true;
            }
        }

        for fname in PROTECTED_FILES {
            if canonical.ends_with(fname) && self.is_in_zen_tree(path) {
                return true;
            }
        }

        false
    }

    fn is_in_zen_tree(&self, path: &Path) -> bool {
        path.starts_with(&self.zen_root)
    }

    pub fn check_path_modification(&self, path: &Path) -> ValidationResult {
        if self.is_protected_path(path) {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.to_string_lossy().to_string());

            return ValidationResult {
                allowed: false,
                sensitivity: SensitivityLevel::Protected,
                reason: Some(format!(
                    "path '{name}' is protected by role separation policy"
                )),
                matched_rule: Some(format!("protected_path:{name}")),
            };
        }

        let sensitivity = if self.is_in_zen_tree(path) {
            SensitivityLevel::Warning
        } else {
            SensitivityLevel::Safe
        };

        ValidationResult {
            allowed: true,
            sensitivity,
            reason: None,
            matched_rule: None,
        }
    }

    pub fn check_command_allowed(&self, command: &str) -> ValidationResult {
        if RESTRICTED_COMMANDS
            .iter()
            .any(|rc| command.to_lowercase().contains(rc))
        {
            return ValidationResult {
                allowed: false,
                sensitivity: SensitivityLevel::Protected,
                reason: Some(format!(
                    "command '{command}' is restricted by role separation policy"
                )),
                matched_rule: Some(format!("restricted_command:{command}")),
            };
        }

        ValidationResult {
            allowed: true,
            sensitivity: SensitivityLevel::Safe,
            reason: None,
            matched_rule: None,
        }
    }

    pub fn validate_path_modification(
        &self,
        path: &Path,
    ) -> Result<ValidationResult, ValidationError> {
        let result = self.check_path_modification(path);
        if !result.allowed {
            Err(ValidationError::SafetyBlock {
                reason: result.reason.clone().unwrap_or_default(),
            })
        } else {
            Ok(result)
        }
    }

    pub fn validate_command(&self, command: &str) -> Result<ValidationResult, ValidationError> {
        let result = self.check_command_allowed(command);
        if !result.allowed {
            Err(ValidationError::SafetyBlock {
                reason: result.reason.clone().unwrap_or_default(),
            })
        } else {
            Ok(result)
        }
    }
}
