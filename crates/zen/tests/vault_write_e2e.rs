//! T104 (T064 contract): vault-write e2e — `AtomicWikiWriter` CAS lands
//! vault-relative pages in the vault (never cwd/tmp) and conditional
//! writes only commit on version match.

use std::path::Path;
use tempfile::tempdir;
use zen_vault::wiki::AtomicWikiWriter;

#[test]
fn vault_relative_write_lands_in_vault_not_cwd() {
    let vault = tempdir().unwrap();
    let elsewhere = tempdir().unwrap();
    let writer = AtomicWikiWriter::new(vault.path());

    writer
        .write(Path::new("resources/x.md"), "# X\n")
        .expect("vault write must succeed");

    let landed = vault.path().join("resources/x.md");
    assert_eq!(std::fs::read_to_string(&landed).unwrap(), "# X\n");
    assert!(
        !elsewhere.path().join("resources/x.md").exists(),
        "write must not leak outside the vault base"
    );
}

#[test]
fn conditional_write_commits_on_match_conflicts_on_drift() {
    let vault = tempdir().unwrap();
    let writer = AtomicWikiWriter::new(vault.path());
    let page = Path::new("notes/cas.md");

    assert!(
        writer.write_conditional(page, None, "v1").unwrap(),
        "absent file must commit against None"
    );
    assert!(
        !writer.write_conditional(page, None, "v2").unwrap(),
        "existing file must conflict against None"
    );
    assert!(
        !writer
            .write_conditional(page, Some("stale-version"), "v2")
            .unwrap(),
        "drifted version must conflict"
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join(page)).unwrap(),
        "v1",
        "conflicted writes must leave content untouched"
    );
}
