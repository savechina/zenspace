//! Agent tools for zen (feature 003-agentic-plugin).
//!
//! All tools implement `rig_compose::tool::Tool` and are registered centrally
//! in `ZenWiring::new()` via the [`register_all`] helper.
//!
//! # Tool catalogue
//!
//! | Tool          | File           | FR        | Sensitivity |
//! |---------------|----------------|-----------|-------------|
//! | `fs.read`     | `fs_read`      | FR-001    | Public      |
//! | `fs.write`    | `fs_write`     | FR-002    | Private     |
//! | `fs.edit`     | `fs_edit`      | FR-021    | Private     |
//! | `fs.delete`   | `fs_delete`    | FR-022    | Private     |
//! | `fs.move`     | `fs_move`      | FR-022    | Private     |
//! | `fs.copy`     | `fs_copy`      | FR-022    | Private     |
//! | `fs.list`     | `fs_list`      | FR-003    | Public      |
//! | `fs.grep`     | `fs_grep`      | —         | Public      |
//! | `fs.glob`     | `fs_glob`      | —         | Public      |
//! | `web.fetch`   | `web_fetch`    | FR-010    | Private     |
//! | `web.search`  | `web_search`   | FR-006    | Private     |
//!
//! Dispatch hooks (FR-018/019/020):
//! - `audit_hook` — writes `ToolInvocationRecord` to `logs/audit.jsonl` after each call.
//! - `approval_hook` — prompts the user before mutations when `SandboxMode::Ask` is active.

pub mod approval_hook;
pub mod audit_hook;
pub mod confidentiality_hook;
pub mod fs_copy;
pub mod fs_delete;
pub mod fs_edit;
pub mod fs_glob;
pub mod fs_grep;
pub mod fs_list;
pub mod fs_move;
pub mod fs_read;
pub mod fs_write;
pub mod mcp_client;
pub mod shell_exec;
pub mod web_fetch;
pub mod web_search;

pub use shell_exec::ShellExecTool;
