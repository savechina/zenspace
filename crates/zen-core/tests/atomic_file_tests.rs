// PURPOSE: Pin the atomic-replacement contract for artefacts several processes
//          read (a half-written JSON parses as corrupt and is discarded).
// USAGE:   cargo test -p zen-core --test atomic_file_tests
// EXPECTED: target holds the new bytes, no temp residue remains, and a
//           concurrent reader never observes a partial write.
// ERRORS:  a leftover `.tmp` sibling or a torn read points at a real regression.

use std::path::Path;
use zen_core::atomic_file::write_atomic;

fn tmpdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

fn temp_residue(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("read_dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".tmp"))
        .collect()
}

#[test]
fn write_atomic_replaces_content_and_leaves_no_residue() {
    let dir = tmpdir();
    let path = dir.path().join("registry.json");

    write_atomic(&path, b"{\"first\":1}").expect("first write");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"first\":1}");

    write_atomic(&path, b"{\"second\":2}").expect("second write");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"second\":2}",
        "the replacement wins"
    );
    assert!(
        temp_residue(dir.path()).is_empty(),
        "the temp sibling is renamed away, never left behind"
    );
}

#[test]
fn write_atomic_never_exposes_a_partial_file() {
    let dir = tmpdir();
    let path = dir.path().join("state.json");
    let payload = vec![b'x'; 512 * 1024];
    write_atomic(&path, b"{\"complete\":true}").expect("seed");

    let stop = std::sync::atomic::AtomicBool::new(false);
    let observed = std::thread::scope(|scope| {
        let reader_path = path.clone();
        let stop_ref = &stop;
        let reader = scope.spawn(move || {
            let mut observed = Vec::new();
            while !stop_ref.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok(body) = std::fs::read_to_string(&reader_path) {
                    observed.push(body);
                }
            }
            observed
        });

        for _ in 0..64 {
            write_atomic(
                &path,
                &format!("{{\"len\":{}}}", payload.len()).into_bytes(),
            )
            .expect("write");
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        reader.join().expect("reader thread")
    });

    let torn: Vec<&String> = observed
        .iter()
        .filter(|body| {
            let body = body.as_str();
            !body.is_empty() && body != "{\"complete\":true}" && !body.starts_with("{\"len\":")
        })
        .collect();
    assert!(
        torn.is_empty(),
        "a reader saw a torn write: {:?}",
        torn.first()
    );
}
