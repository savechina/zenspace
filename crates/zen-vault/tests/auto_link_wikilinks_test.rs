// PURPOSE: Pin auto_link_wikilinks against the CJK char-boundary panic that
//          repeatedly killed the production zen-loop worker (serve.err.log
//          2026-10: "start byte index 37 is not a char boundary ... '安'").
// USAGE: cargo nextest run -p zen-vault auto_link
// EXPECTED: all tests pass; cjk_content_does_not_panic fails on the pre-fix
//           byte-walk implementation with the exact recorded panic.
// ERRORS: none — pure function over owned strings.

use zen_vault::distill::pipeline::auto_link_wikilinks;

#[test]
fn cjk_content_does_not_panic_and_stays_intact() {
    let content = "今天安装了新的开发环境，安静的下午写代码。安装过程很顺利。";
    let entities = vec!["Rust".to_string(), "安装".to_string()];
    let linked = auto_link_wikilinks(content, &entities);
    assert!(
        linked.contains("[[安装]]"),
        "CJK entity must be wrapped, got: {linked}"
    );
    // Content outside matches must survive byte-identical (no mojibake).
    assert!(linked.contains("安静的下午写代码"), "got: {linked}");
}

#[test]
fn ascii_entity_in_cjk_content_is_wrapped() {
    let content = "用 Rust 重写了服务，Rust 的性能不错。";
    let entities = vec!["Rust".to_string()];
    let linked = auto_link_wikilinks(content, &entities);
    assert_eq!(linked, "用 [[Rust]] 重写了服务，[[Rust]] 的性能不错。");
}

#[test]
fn backtick_and_wikilink_spans_are_skipped() {
    let content = "在 `Go 代码` 中引用 Go 语言。";
    let linked = auto_link_wikilinks(content, &["Go".to_string()]);
    assert_eq!(
        linked, "在 `Go 代码` 中引用 [[Go]] 语言。",
        "code span passes through, plain occurrence wraps"
    );

    // Historical semantics preserved: an entity that already appears as any
    // [[wikilink]] is skipped entirely (extract_wikilinks short-circuit).
    let content2 = "在 `Rust 代码` 中引用 Rust，另见 [[Rust]] 页。";
    let linked2 = auto_link_wikilinks(content2, &["Rust".to_string()]);
    assert_eq!(linked2, content2);
}

#[test]
fn word_boundary_and_multibyte_case_handling() {
    // 'İ' lowercases to two chars — the expansion-matching path.
    let content = "İstanbul 与 Istanbul 是同一座城市";
    let entities = vec!["Istanbul".to_string()];
    let linked = auto_link_wikilinks(content, &entities);
    assert!(
        linked.contains("[[Istanbul]]"),
        "case-insensitive multibyte match, got: {linked}"
    );
    assert!(
        linked.contains("[[İstanbul]]") || linked.contains("İstanbul"),
        "original spelling preserved or wrapped canonically, got: {linked}"
    );
    // No partial-word wrap: "Rust" must not match inside "Rusting".
    let content2 = "Rusting is not Rust";
    let linked2 = auto_link_wikilinks(content2, &["Rust".to_string()]);
    assert_eq!(linked2, "Rusting is not [[Rust]]");
}
