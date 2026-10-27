// Integration-test target for the Verus layer (bbtree Guard-3 Wave B).
//
// `cargo build`/`cargo clippy` NEVER compile this target (test binaries are
// test-only), so the production dependency graph stays identical — vstd is
// a [dev-dependencies] entry only.
//
// The mount points at the REAL proof file (formal/src/, zero copy): the
// runtime pin below cross-checks the verified reference impl against the
// runtime grammar used by SecretMap (format!-based) — congruence pinned,
// floatering the two would fail this test under plain `bin/test`.
#[path = "../../../formal/src/secrets_verify.rs"]
mod secrets_verify;

#[test]
fn verified_reference_matches_runtime_grammar() {
    // Verified reference impl (proof: out@ == render_spec(n))
    let made = secrets_verify::make_placeholder(7);
    // Real SecretMap grammar from secrets_guard.rs (runtime truth)
    let runtime = format!(
        "{}7{}",
        zen_core::secrets_guard::PLACEHOLDER_PREFIX,
        zen_core::secrets_guard::PLACEHOLDER_SUFFIX
    );
    assert_eq!(made, runtime);
}
