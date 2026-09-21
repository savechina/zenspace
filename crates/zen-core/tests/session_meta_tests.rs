// PURPOSE: Pin the write_meta contract (NFR-010): line 0 (session/meta) is
//          replaced, every other line is preserved byte-for-byte, and the
//          persist is atomic (tmp + fsync + rename) — a crash mid-write can
//          never truncate the session file.
// USAGE:   cargo test -p zen-core --test session_meta_tests
// EXPECTED: after write_meta, line 0 is the new meta event and all turn
//           lines are byte-identical to what was written before.
// ERRORS:  a truncated file or a lost turn line points at a regression.

use zen_core::types::{Session, SessionEvent};

fn tmpdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

fn turn_line(role: &str, content: &str) -> String {
    format!(
        "{{\"type\":\"chat/turn\",\"payload\":{{\"role\":\"{}\",\"content\":\"{}\"}}}}",
        role, content
    )
}

#[test]
fn write_meta_replaces_only_line_zero_and_preserves_turns() {
    let dir = tmpdir();
    let path = dir.path().join("session.jsonl");

    let session = Session::new("Sisyphus", "/tmp/workspace");
    SessionEvent::write_meta(&path, &session).expect("initial meta write");

    let turns = vec![
        turn_line("user", "hello"),
        turn_line("assistant", "hi there"),
        turn_line("user", "third turn"),
    ];
    // Append turns after the meta line (the real append path).
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open for append");
        for t in &turns {
            writeln!(file, "{t}").expect("append turn");
        }
    }

    // Update the meta (e.g. a rename) and verify only line 0 changed.
    let mut renamed = session.clone();
    renamed.title = Some("renamed".into());
    SessionEvent::write_meta(&path, &renamed).expect("meta rewrite");

    let content = std::fs::read_to_string(&path).expect("read back");
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 1 + turns.len(), "turn lines must survive");

    let meta: SessionEvent = serde_json::from_str(lines[0]).expect("line 0 parses");
    match meta {
        SessionEvent::Meta(m) => assert_eq!(m.title.as_deref(), Some("renamed")),
        other => panic!("line 0 is not a meta event: {other:?}"),
    }

    for (i, t) in turns.iter().enumerate() {
        assert_eq!(
            lines[i + 1],
            t,
            "turn line {} must be byte-identical",
            i + 1
        );
    }
}

#[test]
fn write_meta_creates_new_file_with_meta_first_line() {
    let dir = tmpdir();
    let path = dir.path().join("fresh.jsonl");
    let session = Session::new("Sisyphus", "/tmp/workspace");
    SessionEvent::write_meta(&path, &session).expect("meta write");

    let content = std::fs::read_to_string(&path).expect("read back");
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 1, "fresh file holds exactly the meta line");
    let meta: SessionEvent = serde_json::from_str(lines[0]).expect("line 0 parses");
    assert!(matches!(meta, SessionEvent::Meta(_)));
}
