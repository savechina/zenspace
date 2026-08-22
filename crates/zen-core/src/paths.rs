use chrono::{DateTime, Utc};
use std::env;
use std::path::PathBuf;
use std::sync::LazyLock;

use crate::constants::{
    CACHE_DIR, CONFIG_FILE, DB_DIR, FINANCE_DIR, IDENTITY_DIR, INBOX_DIR, LOGS_DIR, MEMORY_DIR,
    OUTPUT_DIR, PLUGINS_DIR, RAW_DIR, SESSIONS_DIR, SKILLS_DIR, VAULT_DIR,
    WIKI_COMPILER_STATE_FILE, WIKI_DIR, ZEN_HOME_ENV,
};
use crate::errors::PathError;

#[allow(dead_code)]
static USER_ROOT: LazyLock<PathBuf> = LazyLock::new(|| -> PathBuf {
    env::var(ZEN_HOME_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| home::home_dir().map(|h| h.join(".zen")).unwrap_or_default())
});

pub fn user_root() -> PathBuf {
    // In test mode, read env var directly to allow test isolation
    #[cfg(test)]
    {
        env::var(ZEN_HOME_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|_| home::home_dir().map(|h| h.join(".zen")).unwrap_or_default())
    }
    #[cfg(not(test))]
    {
        USER_ROOT.clone()
    }
}

pub struct ZenPaths {
    global_root: PathBuf,
    workspace_root: Option<PathBuf>,
}

impl ZenPaths {
    pub fn detect() -> Result<Self, PathError> {
        let global_root = user_root();
        if global_root == PathBuf::default() {
            return Err(PathError::HomeDirNotFound);
        }

        let workspace_root = Self::find_workspace_root(&global_root);

        Ok(Self {
            global_root,
            workspace_root,
        })
    }

    pub fn for_testing(global_root: PathBuf) -> Self {
        Self {
            global_root,
            workspace_root: None,
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.workspace_root
            .as_ref()
            .map(|w| w.join(CONFIG_FILE))
            .unwrap_or_else(|| self.global_root.join(CONFIG_FILE))
    }

    pub fn user_data(&self, domain: &str) -> PathBuf {
        match &self.workspace_root {
            Some(w) => w.join(domain),
            None => self.global_root.join(domain),
        }
    }

    pub fn cache(&self, domain: &str) -> PathBuf {
        self.global_root.join(CACHE_DIR).join(domain)
    }

    pub fn vault(&self) -> PathBuf {
        self.user_data(VAULT_DIR)
    }

    pub fn inbox(&self) -> PathBuf {
        self.vault().join(INBOX_DIR)
    }

    pub fn raw(&self) -> PathBuf {
        self.vault().join(RAW_DIR)
    }

    pub fn wiki(&self) -> PathBuf {
        self.vault().join(WIKI_DIR)
    }

    pub fn skills(&self) -> PathBuf {
        self.user_data(SKILLS_DIR)
    }

    pub fn db(&self) -> PathBuf {
        self.global_root.join(DB_DIR)
    }

    pub fn data(&self) -> PathBuf {
        self.global_root.join(DB_DIR)
    }

    pub fn wiki_compiler_state(&self) -> PathBuf {
        self.data().join(WIKI_COMPILER_STATE_FILE)
    }

    pub fn sessions(&self) -> PathBuf {
        self.global_root.join(SESSIONS_DIR)
    }

    /// Return the date-separated session directory for a given date.
    /// `~/.zen/sessions/YYYY/MM/DD/`
    pub fn session_dir_for_date(&self, date: DateTime<Utc>) -> PathBuf {
        self.sessions()
            .join(date.format("%Y").to_string())
            .join(date.format("%m").to_string())
            .join(date.format("%d").to_string())
    }

    pub fn finance(&self) -> PathBuf {
        self.user_data(FINANCE_DIR)
    }

    pub fn projects(&self) -> PathBuf {
        self.vault().join("projects")
    }

    pub fn areas(&self) -> PathBuf {
        self.vault().join("areas")
    }

    pub fn resources_dir(&self) -> PathBuf {
        self.vault().join("resources")
    }

    pub fn archive(&self) -> PathBuf {
        self.vault().join("archive")
    }

    pub fn memory(&self) -> PathBuf {
        self.global_root.join(MEMORY_DIR)
    }

    /// Legacy alias: memvid store now lives at `memories/` (same directory).
    pub fn memvid_dir(&self) -> PathBuf {
        self.memory()
    }

    pub fn identity(&self) -> PathBuf {
        self.global_root.join(IDENTITY_DIR)
    }

    pub fn ensure_identity_files(&self) -> std::io::Result<()> {
        let identity_dir = self.identity();
        std::fs::create_dir_all(&identity_dir)?;

        let soul_path = identity_dir.join("SOUL.md");
        if !soul_path.exists() {
            std::fs::write(&soul_path, DEFAULT_SOUL_MD)?;
        }

        let memory_path = identity_dir.join("MEMORY.md");
        if !memory_path.exists() {
            std::fs::write(&memory_path, DEFAULT_MEMORY_MD)?;
        }

        let agents_path = identity_dir.join("AGENTS.md");
        if !agents_path.exists() {
            std::fs::write(&agents_path, DEFAULT_AGENTS_MD)?;
        }

        Ok(())
    }

    pub fn ensure_runtime_dirs(&self) -> std::io::Result<()> {
        for dir in &[
            self.data(),
            self.sessions(),
            self.memory(),
            self.logs(),
            self.identity(),
            self.cache(""),
            self.vault(),
            self.inbox(),
            self.raw(),
            self.wiki(),
            self.skills(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    pub fn logs(&self) -> PathBuf {
        self.global_root.join(LOGS_DIR)
    }

    /// Journal entries directory under memories.
    /// `~/.zen/memories/journal/`
    pub fn journal_entries(&self) -> PathBuf {
        self.memory().join("journal")
    }

    pub fn output(&self) -> PathBuf {
        self.workspace_root
            .as_ref()
            .map(|w| w.join(OUTPUT_DIR))
            .unwrap_or_else(|| self.global_root.join(OUTPUT_DIR))
    }

    pub fn plugins(&self) -> PathBuf {
        self.global_root.join(PLUGINS_DIR)
    }

    pub fn global_root(&self) -> &PathBuf {
        &self.global_root
    }

    pub fn workspace_root(&self) -> Option<&PathBuf> {
        self.workspace_root.as_ref()
    }

    fn find_workspace_root(global_root: &PathBuf) -> Option<PathBuf> {
        if let Ok(path) = env::var("ZEN_WORKSPACE") {
            let candidate = PathBuf::from(path);
            if candidate.join(".zen").is_dir() || candidate.is_dir() {
                return Some(candidate);
            }
        }

        let home = home::home_dir()?;
        // When ZEN_HOME is set, start the walk-up from global_root's parent
        // instead of current_dir().  This prevents the real repo's .zen/ from
        // leaking into tests that set ZEN_HOME to an isolated temp directory.
        let mut current = if env::var("ZEN_HOME").is_ok() {
            global_root.parent()?.to_path_buf()
        } else {
            env::current_dir().ok()?
        };

        loop {
            if current == home || &current == global_root {
                break;
            }

            let candidate = current.join(".zen");
            if candidate.is_dir() {
                return Some(current);
            }
            if !current.pop() {
                break;
            }
        }
        None
    }
}

const DEFAULT_SOUL_MD: &str = "# Soul\n\nYou are Zen, a personal AI agentic workspace assistant.\nYou help capture notes, build knowledge, and learn from every interaction.\n\n## Core Values\n\n- Honesty over comfort\n- Simplicity over cleverness\n- Action over analysis paralysis\n";

const DEFAULT_MEMORY_MD: &str = "# Memory\n\n## Identity\n\n## Active Commitments\n\n## Stop-Doing Ledger\n\n## Continue-Doing Ledger\n\n## Active Mental Models\n\n## Recent Wisdom\n";

const DEFAULT_AGENTS_MD: &str = "# Agent Behavior\n\n- Be concise and direct\n- Ask for clarification when requirements are ambiguous\n- Follow existing codebase patterns\n- Surface assumptions explicitly\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_para_path_accessors() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ZenPaths::for_testing(tmp.path().to_path_buf());

        assert_eq!(paths.projects(), tmp.path().join("vault/projects"));
        assert_eq!(paths.areas(), tmp.path().join("vault/areas"));
        assert_eq!(paths.resources_dir(), tmp.path().join("vault/resources"));
        assert_eq!(paths.archive(), tmp.path().join("vault/archive"));
    }

    #[test]
    fn fr052_output_dir_prefers_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ZenPaths::for_testing(tmp.path().to_path_buf());
        assert_eq!(paths.output(), tmp.path().join("output"));
    }

    #[test]
    fn fr052_output_dir_falls_back_to_global() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = ZenPaths {
            global_root: tmp.path().to_path_buf(),
            workspace_root: None,
        };
        assert_eq!(paths.output(), tmp.path().join("output"));
    }
}
