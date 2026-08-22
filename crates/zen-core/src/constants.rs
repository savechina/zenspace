use include_dir::{Dir, include_dir};

pub static ASSETS: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../assets");
pub static TEMPLATES: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../templates");

pub const USER_ROOT: &str = ".zen";

pub const ASSETS_DIR: &str = "assets/";
pub const TEMPLATES_DIR: &str = "templates/";
pub const DB_DIR: &str = "data/";
pub const SESSIONS_DIR: &str = "sessions/";

pub const VAULT_DIR: &str = "vault/";
pub const INBOX_DIR: &str = "inbox/";
pub const RAW_DIR: &str = "raw/";
pub const WIKI_DIR: &str = "wiki/";
pub const WIKI_NOTIONS_DIR: &str = "wiki/notions/";
pub const WIKI_TOPICS_DIR: &str = "wiki/topics/";
pub const WIKI_SUBJECTS_DIR: &str = "wiki/subjects/";
pub const WIKI_WISDOM_DIR: &str = "wiki/wisdom/";
pub const WIKI_VIRTUES_DIR: &str = "wiki/virtues/";
pub const SKILLS_DIR: &str = "skills/";
pub const FINANCE_DIR: &str = "finance/";
pub const PROJECTS_DIR: &str = "vault/projects/";
pub const AREAS_DIR: &str = "vault/areas/";
pub const RESOURCES_DIR: &str = "vault/resources/";
pub const ARCHIVE_DIR: &str = "vault/archive/";

pub const CACHE_DIR: &str = "cache/";
/// Root directory for cumulative memory artifacts (journal entries, logs).
pub const MEMORY_DIR: &str = "memories/";
pub const MEMVID_STORE_FILE: &str = "mem1.mv2";
pub const WIKI_COMPILER_STATE_FILE: &str = ".wiki_compiler_state.json";
/// Directory holding identity definition files (SOUL.md, MEMORY.md, AGENTS.md).
///
/// **Design Note**: Intentionally co-located with `MEMORY_DIR` ("memories/").
/// Identity files ARE memory artifacts — SOUL.md defines personality,
/// MEMORY.md is the cumulative record, AGENTS.md captures agent behavior.
/// Both `ZenPaths::identity()` and `ZenPaths::memory()` resolve to the same
/// physical directory; the distinct accessors preserve semantic intent at the
/// API level without fragmenting the filesystem. See `memory-architecture/spec.md`.
pub const IDENTITY_DIR: &str = "memories/";
pub const PROMPTS_DIR: &str = "prompts/";

pub const LOGS_DIR: &str = "logs/";
pub const OUTPUT_DIR: &str = "output/";
pub const PLUGINS_DIR: &str = "plugins/";
pub const TODOS_DIR: &str = "todos/";
pub const PLANS_DIR: &str = "plans/";

pub const CONFIG_FILE: &str = "config.toml";
pub const HISTORY_FILE: &str = "history.jsonl";

pub const AGENTS_FILE: &str = "AGENTS.md";

pub const HISTORY_DEFAULT_MAX_BYTES: u64 = 1_048_576; // 1MB
pub const HISTORY_SOFT_CAP_RATIO: f64 = 0.8; // trim to 80% when exceeded

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

// ============================================
// Provider Type Identifiers
// ============================================
pub const PROVIDER_OLLAMA: &str = "ollama";
pub const PROVIDER_OPENAI: &str = "openai";
pub const PROVIDER_ANTHROPIC: &str = "anthropic";
pub const PROVIDER_COHERE: &str = "cohere";
pub const PROVIDER_GEMINI: &str = "gemini";
pub const PROVIDER_MISTRAL: &str = "mistral";
pub const PROVIDER_OPENAI_COMPATIBLE: &str = "openai-compatible";
pub const PROVIDER_ANTHROPIC_COMPATIBLE: &str = "anthropic-compatible";
pub const PROVIDER_MOCK: &str = "mock";

// ============================================
// Provider Default Base URLs
// ============================================
pub const OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434";
pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const OPENAI_API_URL: &str = "https://api.openai.com";
pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
pub const MISTRAL_BASE_URL: &str = "https://api.mistral.ai/v1";
pub const MISTRAL_API_URL: &str = "https://api.mistral.ai";
pub const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
pub const QQBOT_BASE_URL: &str = "https://api.sgroup.qq.com";

// ============================================
// Provider Default Models (Commonly Used)
// ============================================
pub const OLLAMA_DEFAULT_MODEL: &str = "qwen3-coder";
pub const OPENAI_DEFAULT_MODEL: &str = "gpt-4o-mini";
pub const ANTHROPIC_DEFAULT_MODEL: &str = "claude-haiku-4-5";
pub const COHERE_DEFAULT_MODEL: &str = "command-r";
pub const GEMINI_DEFAULT_MODEL: &str = "gemini-2.0-flash";
pub const MISTRAL_DEFAULT_MODEL: &str = "mistral-large-latest";
pub const DEEPSEEK_DEFAULT_MODEL: &str = "deepseek-v4-flash";
pub const ALIYUN_DEFAULT_MODEL: &str = "qwen3.5-plus";
pub const GROQ_DEFAULT_MODEL: &str = "llama-3.3-70b-versatile";
pub const MOONSHOT_DEFAULT_MODEL: &str = "kimi-k2.5";
pub const XAI_DEFAULT_MODEL: &str = "grok-beta";
pub const PERPLEXITY_DEFAULT_MODEL: &str = "sonar";

// ============================================
// Default Provider (Fallback)
// ============================================
pub const DEFAULT_PROVIDER: &str = "ollama";
pub const DEFAULT_MODEL: &str = "gpt-4o-mini";
