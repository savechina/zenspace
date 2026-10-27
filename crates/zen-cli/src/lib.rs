mod cli;
mod cmd;
mod runtime_util;
mod session;
// `bench` feature exposes tui to external bench targets (benches/ dirs are
// separate binaries and see only `pub`); lint's --all-features keeps it
// compiled, default builds stay crate-private byte-identical.
#[cfg(feature = "bench")]
#[doc(hidden)]
pub mod tui;
#[cfg(not(feature = "bench"))]
pub(crate) mod tui;

pub use cli::shell;
