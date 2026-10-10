//! Phase 30 G1: `daemon.pid` version/protocol fields are additive —
//! legacy-shaped records must parse (version unknown, never a parse
//! error) and new-shaped records must round-trip.

use zen_gateway::{read_pid_record, write_pid_for};

#[test]
fn legacy_json_record_parses_with_unknown_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.pid");
    std::fs::write(&path, r#"{"pid":4242,"start":"Mon Jan 1 00:00:00 2026"}"#).unwrap();

    let record = read_pid_record(&path).expect("legacy record must parse");
    assert_eq!(record.pid, 4242);
    assert_eq!(record.start.as_deref(), Some("Mon Jan 1 00:00:00 2026"));
    assert_eq!(record.version, None, "legacy daemon ⇒ version unknown");
    assert_eq!(record.protocol, None);
}

#[test]
fn legacy_bare_pid_record_parses_with_unknown_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.pid");
    std::fs::write(&path, "4242\n").unwrap();

    let record = read_pid_record(&path).expect("bare-pid record must parse");
    assert_eq!(record.pid, 4242);
    assert_eq!(record.start, None);
    assert_eq!(record.version, None);
    assert_eq!(record.protocol, None);
}

#[test]
fn new_record_round_trips_version_and_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.pid");
    let pid = std::process::id();

    write_pid_for(&path, pid).expect("write record");
    let record = read_pid_record(&path).expect("record must parse");

    assert_eq!(record.pid, pid);
    assert_eq!(record.version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    assert_eq!(
        record.protocol.as_deref(),
        Some(zen_gateway::protocol::SERVER_PROTOCOL_VERSION)
    );
    assert!(record.start.is_some(), "own pid has a start token");
}

#[test]
fn partial_record_missing_only_protocol_tolerates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.pid");
    std::fs::write(&path, r#"{"pid":7,"version":"0.0.8"}"#).unwrap();

    let record = read_pid_record(&path).expect("partial record must parse");
    assert_eq!(record.pid, 7);
    assert_eq!(record.version.as_deref(), Some("0.0.8"));
    assert_eq!(record.protocol, None);
}
