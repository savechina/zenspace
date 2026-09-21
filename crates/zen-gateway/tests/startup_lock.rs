// PURPOSE: Pin the startup-lock contract that makes the socket hand-off
//          (exists → probe → remove-stale → bind) exclusive across processes.
// USAGE:   cargo test -p zen-gateway --test startup_lock
// EXPECTED: a second holder is refused while the first lives, and the lock is
//           available again once the first is dropped (crash-release parity).
// ERRORS:  a second successful acquire means two daemons can race the socket.

use zen_gateway::{StartupLock, startup_lock_path};

fn tmpdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

#[test]
fn a_second_holder_is_refused_and_release_restores_availability() {
    let dir = tmpdir();
    let socket = dir.path().join("gateway.sock");
    let lock_path = startup_lock_path(&socket);
    assert_eq!(
        lock_path.file_name().and_then(|n| n.to_str()),
        Some("gateway.lock"),
        "the lock lives beside the socket, so one ZEN_HOME has one lock"
    );

    let first = StartupLock::acquire(&lock_path).expect("first acquire");
    let refused = StartupLock::acquire(&lock_path);
    let message = format!("{}", refused.expect_err("second acquire must be refused"));
    assert!(
        message.contains("another zen serve is starting or running"),
        "the refusal names the cause, not a raw io error: {message}"
    );

    drop(first);
    StartupLock::acquire(&lock_path).expect("the lock is released with its holder");
}

#[test]
fn the_lock_is_scoped_to_the_data_directory() {
    let dir = tmpdir();
    assert_eq!(
        startup_lock_path(&dir.path().join("a.sock")),
        startup_lock_path(&dir.path().join("b.sock")),
        "one ZEN_HOME owns one daemon, whichever socket name its config uses"
    );

    let other = tmpdir();
    let _held = StartupLock::acquire(&startup_lock_path(&dir.path().join("a.sock"))).expect("a");
    StartupLock::acquire(&startup_lock_path(&other.path().join("a.sock")))
        .expect("an unrelated ZEN_HOME has its own lock");
}
