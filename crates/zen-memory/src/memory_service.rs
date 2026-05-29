use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result};
use tracing::debug;
use zen_core::paths::ZenPaths;

/// Consolidated identity context loaded from SOUL.md, MEMORY.md, AGENTS.md.
///
/// Each field contains the raw markdown content of the corresponding file.
/// Fields are optional because not all identity files may exist.
#[derive(Debug, Clone, Default)]
pub struct IdentityContext {
    pub soul: Option<String>,
    pub memory: Option<String>,
    pub agents: Option<String>,
}

impl IdentityContext {
    /// Returns true if any identity file content was loaded.
    pub fn has_content(&self) -> bool {
        self.soul.is_some() || self.memory.is_some() || self.agents.is_some()
    }

    /// Returns the number of identity files that contain data.
    pub fn file_count(&self) -> usize {
        self.soul.is_some() as usize
            + self.memory.is_some() as usize
            + self.agents.is_some() as usize
    }

    /// Merge another IdentityContext into this one, with `other` taking
    /// precedence for any non-None fields (shallow union).
    pub fn merge(&mut self, other: IdentityContext) {
        if other.soul.is_some() {
            self.soul = other.soul;
        }
        if other.memory.is_some() {
            self.memory = other.memory;
        }
        if other.agents.is_some() {
            self.agents = other.agents;
        }
    }
}

/// Loads identity files from a single directory that may contain
/// SOUL.md / MEMORY.md / AGENTS.md.  Used for both system-templates and
/// user-templates lookup.
fn load_identity_from_dir(dir: &Path) -> Result<IdentityContext> {
    let mut ctx = IdentityContext::default();

    if dir.is_dir() {
        if let Ok(soul) = read_identity_file(&dir.join("SOUL.md")) {
            ctx.soul = Some(soul);
        }
        if let Ok(memory) = read_identity_file(&dir.join("MEMORY.md")) {
            ctx.memory = Some(memory);
        }
        if let Ok(agents) = read_identity_file(&dir.join("AGENTS.md")) {
            ctx.agents = Some(agents);
        }
    }

    Ok(ctx)
}

fn read_identity_file(path: &Path) -> Result<String> {
    if !path.exists() {
        anyhow::bail!("identity file not found: {}", path.display());
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read identity file: {}", path.display()))?;
    Ok(content)
}

// ─── Error type ────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("identity file not found: {0}")]
    NotFound(String),

    #[error("failed to read file: {0}")]
    ReadError(String),
}

// ─── Public API ────────────────────────────────────────────────────────

/// Load a single identity file by its absolute path, returning the raw content.
pub fn load_soul(path: &Path) -> std::result::Result<String, MemoryError> {
    read_identity_file(path).map_err(|e| MemoryError::ReadError(e.to_string()))
}

/// Load `MEMORY.md` by path.
pub fn load_memory(path: &Path) -> std::result::Result<String, MemoryError> {
    read_identity_file(path).map_err(|e| MemoryError::ReadError(e.to_string()))
}

/// Load `AGENTS.md` by path.
pub fn load_agents(path: &Path) -> std::result::Result<String, MemoryError> {
    read_identity_file(path).map_err(|e| MemoryError::ReadError(e.to_string()))
}

/// Load the full identity context using two tiers:
/// 1. User files in `~/.zen/identity/` (takes precedence)
/// 2. System defaults from `~/.zen/` (fallback)
///
/// If neither tier provides a file, the corresponding field is `None`.
pub fn load_all(zen_paths: &ZenPaths) -> Result<IdentityContext> {
    let mut ctx = IdentityContext::default();

    // Tier 2: global ~/.zen/ level (AGENTS.md at root)
    let global_identity = load_identity_from_dir(zen_paths.global_root())?;
    ctx.merge(global_identity);

    // Tier 1: user identity directory ~/.zen/identity/
    let identity_dir = zen_paths.identity();
    let user_identity = load_identity_from_dir(&identity_dir)?;
    ctx.merge(user_identity);

    debug!(
        "loaded identity context: {} files present",
        ctx.file_count()
    );

    Ok(ctx)
}

/// Load identity from a workspace-level directory (e.g., `<workspace>/.zen/`).
/// Used when the workspace provides its own identity files.
pub fn load_from_workspace(workspace_root: &Path) -> Result<IdentityContext> {
    if !workspace_root.is_dir() {
        return Ok(IdentityContext::default());
    }

    let ctx = load_identity_from_dir(workspace_root)?;

    if ctx.has_content() {
        debug!("workspace identity loaded: {} files", ctx.file_count());
    }

    Ok(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// Helper: write a set of files into a directory and return the path.
    fn setup_identity_dir(files: &[(&str, &str)]) -> PathBuf {
        let dir = tempdir().unwrap();
        for (name, content) in files {
            let path = dir.path().join(name);
            let mut f = fs::File::create(&path).unwrap();
            f.write_all(content.as_bytes()).unwrap();
        }
        dir.keep()
    }

    #[test]
    fn test_load_single_file() {
        let dir = setup_identity_dir(&[("SOUL.md", "personality: friendly\n")]);
        let soul = load_soul(&dir.join("SOUL.md")).unwrap();
        assert_eq!(soul, "personality: friendly\n");
    }

    #[test]
    fn test_load_missing_file_returns_error() {
        let result = load_soul(Path::new("/nonexistent/SOUL.md"));
        assert!(result.is_err());
    }

    #[test]
    fn test_identity_context_default() {
        let ctx = IdentityContext::default();
        assert!(!ctx.has_content());
        assert_eq!(ctx.file_count(), 0);
    }

    #[test]
    fn test_identity_context_has_content() {
        let ctx = IdentityContext {
            soul: Some("test".into()),
            ..Default::default()
        };
        assert!(ctx.has_content());
        assert_eq!(ctx.file_count(), 1);
    }

    #[test]
    fn test_identity_context_merge() {
        let mut a = IdentityContext {
            soul: Some("a_soul".into()),
            ..Default::default()
        };
        let b = IdentityContext {
            memory: Some("b_memory".into()),
            ..Default::default()
        };
        a.merge(b);
        assert_eq!(a.soul, Some("a_soul".into()));
        assert_eq!(a.memory, Some("b_memory".into()));
        assert_eq!(a.file_count(), 2);
    }

    #[test]
    fn test_identity_context_merge_overrides() {
        let mut a = IdentityContext {
            soul: Some("old".into()),
            ..Default::default()
        };
        let b = IdentityContext {
            soul: Some("new".into()),
            ..Default::default()
        };
        a.merge(b);
        assert_eq!(a.soul, Some("new".into()));
    }

    #[test]
    fn test_load_all_partially_populated() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();

        // Write only AGENTS.md at global level
        fs::write(root.join("AGENTS.md"), "rules: be kind\n").unwrap();

        // Build ZenPaths-like struct via workspace_root pattern doesn't work here,
        // so we test load_from_workspace
        let ctx = load_from_workspace(root).unwrap();
        assert!(ctx.has_content());
        assert_eq!(ctx.file_count(), 1);
        assert_eq!(ctx.agents, Some("rules: be kind\n".into()));
    }
}
