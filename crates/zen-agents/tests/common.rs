//! Shared test-support harness for zen-agents integration tests (T111/T118).
//!
//! PURPOSE: One frozen ZEN_HOME per test binary. `user_root()` is a
//! process-once `LazyLock` in non-test zen-core builds, so integration
//! tests must pick ONE temp home before the first `ZenPaths::detect()`
//! and never mutate it per-test (per-test env changes do not take effect
//! after first resolution).
//!
//! USAGE: `let _guard = common::begin();` as the first line of every test
//! that constructs an orchestrator / runs turn flows / emits audit lines.
//!
//! EXPECTED: audit writes land in `<temp>/logs/audit.jsonl`, never the
//! developer's real `~/.zen`.
//!
//! ERRORS: tests share one process-frozen ZEN_HOME and serialize on a
//! mutex (SQLite single-writer); a failure here usually means env
//! ordering, not product code.

use std::sync::{Mutex, MutexGuard, Once, OnceLock};
use tempfile::TempDir;

static INIT: Once = Once::new();
static HOME: OnceLock<TempDir> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());

/// Hold for the whole test body: freezes ZEN_HOME to a temp dir on first
/// call and serializes audit-emitting tests (state.db is single-writer).
pub fn begin() -> MutexGuard<'static, ()> {
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    INIT.call_once(|| {
        let home = HOME.get_or_init(|| TempDir::new().expect("temp ZEN_HOME"));
        // SAFETY: single-threaded at this point (INIT once under LOCK); no
        // other thread reads the env concurrently, and value is valid UTF-8.
        unsafe { std::env::set_var("ZEN_HOME", home.path()) };
        zen_core::config::invalidate_config_cache();
    });
    guard
}
