//! Shared backoff schedule for transient failures (HTTP 429/5xx, network).
//!
//! Single home for the 1s→2s→4s exponential backoff schedule so HTTP tools
//! (web.search, MCP reconnect) don't drift into divergent copies. The async
//! retry loop that consumes this schedule lives in zen-plugin (this crate is
//! sync-only; `tokio` is a dev-dependency here).

/// Exponential backoff schedule (one delay per attempt): 1s, 2s, 4s.
pub const BACKOFF_SECS: &[u64] = &[1, 2, 4];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_is_one_two_four() {
        assert_eq!(BACKOFF_SECS, &[1, 2, 4]);
    }
}
