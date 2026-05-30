use include_dir::{Dir, include_dir};

pub static ASSETS: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../assets");
pub static TEMPLATES: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../templates");

pub const USER_ROOT: &str = ".zen";

pub const ASSETS_DIR: &str = "assets/";
pub const TEMPLATES_DIR: &str = "templates/";
pub const DB_DIR: &str = "db/";
pub const SESSIONS_DIR: &str = "sessions/";
pub const KNOWLEDGE_DIR: &str = "knowledge/";
pub const INBOX_DIR: &str = "inbox/";
pub const RAW_DIR: &str = "raw/";
pub const WIKI_DIR: &str = "wiki/";
pub const SKILLS_DIR: &str = "skills/";
pub const FINANCE_DIR: &str = "finance/";
pub const CACHE_DIR: &str = "cache/";
pub const MEMORY_DIR: &str = "memory/";
pub const IDENTITY_DIR: &str = "identity/";
pub const LOGS_DIR: &str = "logs/";
pub const OUTPUT_DIR: &str = "output/";
pub const PLUGINS_DIR: &str = "plugins/";
pub const CONFIG_FILE: &str = "config.toml";

pub const ZEN_HOME_ENV: &str = "ZEN_HOME";

pub const SUPPORTED_LLM_PROVIDERS: &[&str] = &[
    "openai",
    "anthropic",
    "deepseek",
    "aliyun",
    "mistral",
    "groq",
    "moonshot",
    "xai",
    "perplexity",
    "gemini",
    "ollama",
    "qqbot",
];
