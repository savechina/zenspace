// Real-file mount point for the formal/ Kahn proof module: inline `mod`
// blocks get synthetic directories that #[path] resolution cannot walk
// (the plan_task/tests prefix does not exist on disk), so this physical
// file re-exports the canonical source instead. One physical source, two
// consumers (verus verifies formal/; this test binary congruence-pins it).
#[path = "../../../../../formal/src/plan_layers_verify.rs"]
mod plan_layers_verify;

pub(crate) use plan_layers_verify::*;
