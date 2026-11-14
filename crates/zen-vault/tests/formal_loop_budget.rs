// Congruence pin for the Verus-verified LoopBudget throughput gates
// (T278 goal 3; bbtree pattern — see formal/src/loop_budget_verify.rs).
//
// The Verus layer proves the ceiling contracts over EXEC MIRRORS; this
// test pins each mirror to the real `LoopBudget` methods — including the
// `saturating_add` overflow edge the mirror reproduces by hand (accept
// at u32::MAX iff max_tokens == u32::MAX) and the cycle-level
// grant-or-stall theorem. Mirror↔production drift on any decision or
// counter value fails here under plain `bin/test`.
#[path = "../../../formal/src/loop_budget_verify.rs"]
mod budget_verify;

use budget_verify::{budget_steps_exec, consume_step_exec, consume_tokens_exec, over_budget_exec};
use zen_vault::distill::types::LoopBudget;

fn pin_consume_step(max_steps: u32, consumed: u32) {
    let mut b = LoopBudget::with_limits(max_steps, 8_000);
    b.consumed_steps = consumed;
    let prod_allowed = b.consume_step();
    let (mirror_allowed, mirror_next) = consume_step_exec(max_steps, consumed);

    assert_eq!(
        prod_allowed, mirror_allowed,
        "consume_step decision diverges at ({max_steps}, {consumed})"
    );
    assert_eq!(
        b.consumed_steps, mirror_next,
        "consume_step counter diverges at ({max_steps}, {consumed})"
    );
    // Contract echo: acceptance never passes the ceiling; refusal pins
    // the counter at exactly the ceiling (only reachable from a valid
    // state — mirrors the `requires consumed <= max_steps` clause).
    if prod_allowed {
        assert!(b.consumed_steps <= max_steps);
        assert_eq!(b.consumed_steps, consumed + 1);
    } else if consumed <= max_steps {
        assert_eq!(b.consumed_steps, max_steps);
    }
}

fn pin_consume_tokens(max_tokens: u32, consumed: u32, tokens: u32) {
    let mut b = LoopBudget::with_limits(5, max_tokens);
    b.consumed_tokens = consumed;
    let prod_allowed = b.consume_tokens(tokens);
    let (mirror_allowed, mirror_next) = consume_tokens_exec(max_tokens, consumed, tokens);

    assert_eq!(
        prod_allowed, mirror_allowed,
        "consume_tokens decision diverges at ({max_tokens}, {consumed}, {tokens})"
    );
    assert_eq!(
        b.consumed_tokens, mirror_next,
        "consume_tokens counter diverges at ({max_tokens}, {consumed}, {tokens})"
    );
    if prod_allowed {
        assert!(b.consumed_tokens <= max_tokens);
    } else {
        assert_eq!(
            b.consumed_tokens, consumed,
            "refusal must not mutate the counter"
        );
    }
}

#[test]
fn budget_mirrors_congruent_with_production_across_grid() {
    // consume_step: below / at / (invalid-input probe stays decision-equal)
    for max in [0u32, 1, 5, u32::MAX] {
        for consumed in [0u32, 1, 4, 5, u32::MAX] {
            pin_consume_step(max, consumed);
        }
    }

    // consume_tokens: monotone accept, ceiling refuse, and the
    // saturating_add overflow edge (u32::MAX ceiling accepts at MAX).
    for max in [0u32, 1, 8_000, u32::MAX] {
        for consumed in [0u32, 1, 7_999, 8_000, u32::MAX] {
            for tokens in [0u32, 1, 2_000, u32::MAX] {
                pin_consume_tokens(max, consumed, tokens);
            }
        }
    }

    // over_budget: full 4-field predicate equivalence.
    for max_steps in [0u32, 5] {
        for max_tokens in [8_000u32, u32::MAX] {
            for cs in [0u32, 4, 5, 6] {
                for ct in [0u32, 7_999, 8_000, 8_001] {
                    let mut b = LoopBudget::with_limits(max_steps, max_tokens);
                    b.consumed_steps = cs;
                    b.consumed_tokens = ct;
                    assert_eq!(
                        b.over_budget(),
                        over_budget_exec(max_steps, max_tokens, cs, ct),
                        "over_budget diverges at ({max_steps}, {max_tokens}, {cs}, {ct})"
                    );
                }
            }
        }
    }

    // Cycle theorem vs production: driving N consume_step calls from a
    // zeroed budget grants exactly N or stalls at exactly the ceiling.
    for max in [0u32, 1, 3, 8] {
        for requested in [0u32, 1, 3, 7, 12] {
            let mut b = LoopBudget::with_limits(max, 8_000);
            let mut granted = 0u32;
            while granted < requested && b.consume_step() {
                granted += 1;
            }
            let mirror = budget_steps_exec(max, requested);
            assert_eq!(
                granted, mirror,
                "cycle grant count diverges at ({max}, {requested})"
            );
            assert!(granted <= max);
            assert!(granted == requested || granted == max);
        }
    }
}
