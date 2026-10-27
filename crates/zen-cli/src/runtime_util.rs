//! Feature-routed runtime seams for the shuttle DST build.
//!
//! Feature OFF (default): every seam forwards to the real tokio API —
//! byte-identical behavior. Feature ON (`--features shuttle`): seams take
//! the DST-build branch, because shuttle-tokio omits multi-threaded-runtime
//! primitives and the cross-crate gateway/agents calls are never driven
//! under schedule exploration.

/// `tokio::task::block_in_place` has no shuttle counterpart (the shuttle
/// runtime is single-threaded and cooperative — blocking inline is the
/// norm there, so the closure simply runs).
pub(crate) fn block_in_place<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    #[cfg(feature = "shuttle")]
    {
        f()
    }
    #[cfg(not(feature = "shuttle"))]
    {
        tokio::task::block_in_place(f)
    }
}
