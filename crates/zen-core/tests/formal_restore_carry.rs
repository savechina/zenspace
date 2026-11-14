// Congruence pin for the Verus-verified restore_incremental carry mirror
// (T278 goal 2; bbtree pattern — see formal/src/restore_carry_verify.rs).
//
// The Verus layer proves the two integer invariants (carry ≤ 26 bytes,
// emitted+carry conserves the restored length) over a CHAR-MODEL mirror.
// This test pins the mirror to the REAL production path — the mirror
// would silently drift if `partial_placeholder_suffix` ever changed its
// scan order, lookback constant, or predicate. Covers the ⟦-boundary
// multi-byte char, the exact k == 26 lookback boundary, the k == 27
// exhaustion edge, CJK, and genuine redact→restore maps.
#[path = "../../../formal/src/restore_carry_verify.rs"]
mod carry_verify;

use carry_verify::carry_split_exec;
use zen_core::secrets_guard::{redact, restore, restore_incremental};

fn pin_fixture(text: &str, map: &zen_core::secrets_guard::SecretMap, label: &str) {
    let restored = restore(text, map).text;
    let (emit, carry) = restore_incremental(text, map);
    let chars: Vec<char> = restored.chars().collect();
    let (mirror_emitted, mirror_carry) = carry_split_exec(&chars);

    // Theorem 1 (runtime echo): the withheld suffix never exceeds the
    // lookback (26 bytes = 10-byte opener + 16).
    assert!(
        carry.len() <= 26,
        "{label}: carry {} exceeds lookback",
        carry.len()
    );

    // Theorem 2 (conservation): emit + carry accounts for every byte.
    assert_eq!(
        emit.len() + carry.len(),
        restored.len(),
        "{label}: split dropped bytes"
    );

    // Congruence: the mirror decides the same split as production.
    assert_eq!(
        mirror_emitted,
        restored.len() - carry.len(),
        "{label}: mirror emitted_len diverges"
    );
    if carry.is_empty() {
        assert!(
            mirror_carry.is_none(),
            "{label}: mirror withholds, production forwards"
        );
    } else {
        assert_eq!(
            mirror_carry,
            Some(carry.len()),
            "{label}: mirror carry_len diverges"
        );
    }
}

#[test]
fn carry_mirror_congruent_with_production_across_fixtures() {
    let corpus = vec!["ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij".to_string()];

    // Genuine redact→restore pairs (real maps, real placeholders).
    let genuine = [
        "token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij then text",
        "one ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij two ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij",
    ];
    for src in genuine {
        let whole = redact(src, &corpus);
        pin_fixture(&whole.text, &whole.map, "genuine");
    }

    // Synthetic partials through the no-map passthrough path.
    let synthetic = [
        "",
        "plain text without tokens",
        "done ⟦SECR",
        "⟦SECRET_",
        "⟦SECRET_12",
        "笔记⟦SECRET_",
        // exact lookback boundary: 10-byte opener + 16 digits = 26 bytes
        "x⟦SECRET_1234567890123456",
        // one past the boundary: opener start falls outside the lookback
        "x⟦SECRET_12345678901234567",
        // closed token restorable shape with a real map
        "k ⟦SECRET_1⟧ tail",
    ];
    let empty_map = redact("nothing secret here", &[]).map;
    for src in synthetic {
        pin_fixture(src, &empty_map, "synthetic");
    }
}
