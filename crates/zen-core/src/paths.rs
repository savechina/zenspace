use chrono::{DateTime, Utc};
use std::env;
use std::path::PathBuf;
use std::sync::LazyLock;

use crate::constants::{
    CACHE_DIR, CONFIG_FILE, DB_DIR, FINANCE_DIR, IDENTITY_DIR, INBOX_DIR, KNOWLEDGE_DIR, LOGS_DIR,
    MEMORY_DIR, OUTPUT_DIR, PLUGINS_DIR, RAW_DIR, SESSIONS_DIR, SKILLS_DIR, WIKI_DIR, ZEN_HOME_ENV,
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

    pub fn knowledge(&self) -> PathBuf {
        self.user_data(KNOWLEDGE_DIR)
    }

    pub fn inbox(&self) -> PathBuf {
        self.knowledge().join(INBOX_DIR)
    }

    pub fn raw(&self) -> PathBuf {
        self.knowledge().join(RAW_DIR)
    }

    pub fn wiki(&self) -> PathBuf {
        self.knowledge().join(WIKI_DIR)
    }

    pub fn skills(&self) -> PathBuf {
        self.user_data(SKILLS_DIR)
    }

    pub fn db(&self) -> PathBuf {
        self.global_root.join(DB_DIR)
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

    pub fn memory(&self) -> PathBuf {
        self.global_root.join(MEMORY_DIR)
    }

    pub fn identity(&self) -> PathBuf {
        self.global_root.join(IDENTITY_DIR)
    }

    pub fn logs(&self) -> PathBuf {
        self.global_root.join(LOGS_DIR)
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
        let mut current = env::current_dir().ok()?;

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
