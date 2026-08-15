use std::path::Path;

use zen_plugin::registry::{PluginState, is_valid_plugin_id};
use zen_plugin::{Lifecycle, PluginEntry, PluginKind, PluginRegistry, PluginRegistryError};

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

fn write_plugin(dir: &Path, id: &str, manifest_toml: &str, entry_bytes: &[u8]) {
    let plugin_dir = dir.join(id);
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("manifest.toml"), manifest_toml).unwrap();
    std::fs::write(plugin_dir.join("entry.wasm"), entry_bytes).unwrap();
}

fn manifest_toml(id: &str, sha256: Option<&str>) -> String {
    let hash_line = match sha256 {
        Some(h) => format!("sha256 = \"{h}\"\n"),
        None => String::new(),
    };
    format!(
        "id = \"{id}\"\nname = \"Test Plugin\"\nversion = \"1.0.0\"\ntype = \"tool\"\npermissions = []\nentry = \"entry.wasm\"\n{hash_line}"
    )
}

fn registry_for(dir: &Path) -> PluginRegistry {
    PluginRegistry::with_plugin_dir(dir.to_path_buf())
}

// ---------------------------------------------------------------------------
// T110 — strict hash (FR-049a)
// ---------------------------------------------------------------------------

#[test]
fn missing_sha256_wasm_rejected() {
    let dir = tempfile::tempdir().unwrap();
    write_plugin(
        dir.path(),
        "nohash",
        &manifest_toml("nohash", None),
        b"(module)",
    );

    let entry =
        PluginEntry::from_manifest_path(&dir.path().join("nohash").join("manifest.toml")).unwrap();
    let err = entry.verify_integrity().unwrap_err();
    assert!(
        matches!(err, PluginRegistryError::MissingHash { .. }),
        "expected MissingHash, got: {err}"
    );

    let mut registry = registry_for(dir.path());
    let count = registry.discover().unwrap();
    assert_eq!(count, 0, "wasm plugin without sha256 must not load");
    assert_eq!(registry.get("nohash").unwrap().lifecycle, Lifecycle::Failed);
}

#[test]
fn tampered_wasm_hash_mismatches_with_rehash_hint() {
    let dir = tempfile::tempdir().unwrap();
    let entry_bytes = b"(module)";
    write_plugin(
        dir.path(),
        "tampered",
        &manifest_toml("tampered", Some("deadbeef")),
        entry_bytes,
    );

    let entry = PluginEntry::from_manifest_path(&dir.path().join("tampered").join("manifest.toml"))
        .unwrap();
    let err = entry.verify_integrity().unwrap_err();
    assert!(
        matches!(err, PluginRegistryError::HashMismatch { .. }),
        "expected HashMismatch, got: {err}"
    );

    let msg = err.to_string();
    assert!(
        msg.contains("zen plugin rehash tampered"),
        "HashMismatch must point at `zen plugin rehash <id>`, got: {msg}"
    );
    assert!(
        msg.ends_with("after deliberate updates"),
        "HashMismatch must end with the recovery hint, got: {msg}"
    );

    let mut registry = registry_for(dir.path());
    registry.discover().unwrap();
    assert_eq!(
        registry.get("tampered").unwrap().lifecycle,
        Lifecycle::Failed
    );
}

#[test]
fn valid_hash_loads() {
    let dir = tempfile::tempdir().unwrap();
    let entry_bytes = b"(module)";
    let hash = sha256_hex(entry_bytes);
    write_plugin(
        dir.path(),
        "valid",
        &manifest_toml("valid", Some(&hash)),
        entry_bytes,
    );

    let entry =
        PluginEntry::from_manifest_path(&dir.path().join("valid").join("manifest.toml")).unwrap();
    assert!(entry.verify_integrity().is_ok());

    let mut registry = registry_for(dir.path());
    assert_eq!(registry.discover().unwrap(), 1);
    assert_eq!(registry.get("valid").unwrap().lifecycle, Lifecycle::Built);
}

#[test]
fn native_entry_without_hash_still_loads() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_dir = dir.path().join("native");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        "id = \"native\"\nname = \"Native\"\nversion = \"1.0.0\"\ntype = \"tool\"\npermissions = []\nentry = \"entry.so\"\n",
    )
    .unwrap();
    std::fs::write(plugin_dir.join("entry.so"), b"\x7fELF").unwrap();

    let entry = PluginEntry::from_manifest_path(&plugin_dir.join("manifest.toml")).unwrap();
    assert!(entry.verify_integrity().is_ok());

    let mut registry = registry_for(dir.path());
    assert_eq!(registry.discover().unwrap(), 1);
    assert_eq!(registry.get("native").unwrap().lifecycle, Lifecycle::Built);
}

// ---------------------------------------------------------------------------
// T111 — id charset validation (FR-050a)
// ---------------------------------------------------------------------------

#[test]
fn plugin_id_validation_vectors() {
    for bad in ["Bad Id!", "UPPER", "a/b", ""] {
        assert!(!is_valid_plugin_id(bad), "{bad:?} must be rejected");
    }
    for good in ["echo", "my-plugin_2", "a-b_9"] {
        assert!(is_valid_plugin_id(good), "{good:?} must be accepted");
    }
}

#[test]
fn discover_rejects_invalid_plugin_ids() {
    let dir = tempfile::tempdir().unwrap();
    let entry_bytes = b"(module)";
    let hash = sha256_hex(entry_bytes);
    for bad_id in ["Bad Id!", "UPPER", "a/b"] {
        let plugin_dir = dir.path().join(bad_id.replace([' ', '!'], "_"));
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("manifest.toml"),
            manifest_toml(bad_id, Some(&hash)),
        )
        .unwrap();
        std::fs::write(plugin_dir.join("entry.wasm"), entry_bytes).unwrap();
    }

    let mut registry = registry_for(dir.path());
    let count = registry.discover().unwrap();
    assert_eq!(count, 0, "invalid ids must not load");
    assert!(registry.get("Bad Id!").is_none());
    assert!(registry.get("UPPER").is_none());
    assert!(registry.get("a/b").is_none());
}

// ---------------------------------------------------------------------------
// T108 — state.json round-trip (FR-047)
// ---------------------------------------------------------------------------

fn write_two_valid_plugins(dir: &Path) -> String {
    let entry_bytes = b"(module)";
    let hash = sha256_hex(entry_bytes);
    write_plugin(
        dir,
        "alpha",
        &manifest_toml("alpha", Some(&hash)),
        entry_bytes,
    );
    write_plugin(
        dir,
        "beta",
        &manifest_toml("beta", Some(&hash)),
        entry_bytes,
    );
    hash
}

#[test]
fn state_json_disable_enable_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    write_two_valid_plugins(dir.path());

    let mut registry = registry_for(dir.path());
    assert_eq!(registry.discover().unwrap(), 2);

    // Disable alpha → state.json written → a fresh registry skips it.
    let mut state = PluginState::load(dir.path());
    state.disable("alpha");
    state.save(dir.path()).unwrap();

    let raw = std::fs::read_to_string(dir.path().join("state.json")).unwrap();
    serde_json::from_str::<serde_json::Value>(&raw)
        .expect("state.json must hold valid JSON after save");
    assert!(
        !dir.path().join("state.json.tmp").exists(),
        "atomic save must not leave the temp file behind"
    );

    let mut registry = registry_for(dir.path());
    assert_eq!(registry.discover().unwrap(), 1);
    assert!(
        registry.get("alpha").is_none(),
        "disabled plugin is skipped"
    );
    assert!(registry.get("beta").is_some());

    // Re-enable → the plugin returns at the next discovery.
    let mut state = PluginState::load(dir.path());
    state.enable("alpha");
    state.save(dir.path()).unwrap();

    let mut registry = registry_for(dir.path());
    assert_eq!(registry.discover().unwrap(), 2);
    assert!(registry.get("alpha").is_some());
}

#[test]
fn state_disable_is_idempotent() {
    let mut state = PluginState::default();
    state.disable("dup");
    state.disable("dup");
    assert_eq!(state.disabled, vec!["dup".to_string()]);

    state.enable("dup");
    state.enable("dup");
    assert!(state.disabled.is_empty());
}

#[test]
fn corrupt_state_json_fails_open() {
    let dir = tempfile::tempdir().unwrap();
    write_two_valid_plugins(dir.path());

    std::fs::write(dir.path().join("state.json"), "{ not valid json !!").unwrap();

    let mut registry = registry_for(dir.path());
    let count = registry.discover().unwrap();
    assert_eq!(
        count, 2,
        "corrupt state.json must fail open: all plugins stay enabled"
    );
    assert!(registry.get("alpha").is_some());
    assert!(registry.get("beta").is_some());
}

// ---------------------------------------------------------------------------
// rehash / install auto-hash (FR-049b / FR-049c)
// ---------------------------------------------------------------------------

#[test]
fn rehash_manifest_recovers_tampered_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let entry_bytes = b"(module)";
    write_plugin(
        dir.path(),
        "drifted",
        &manifest_toml("drifted", Some("deadbeef")),
        entry_bytes,
    );

    let entry =
        PluginEntry::from_manifest_path(&dir.path().join("drifted").join("manifest.toml")).unwrap();
    assert!(matches!(
        entry.verify_integrity().unwrap_err(),
        PluginRegistryError::HashMismatch { .. }
    ));

    let hash = entry.rehash_manifest().unwrap();
    assert_eq!(hash, sha256_hex(entry_bytes));

    let reloaded =
        PluginEntry::from_manifest_path(&dir.path().join("drifted").join("manifest.toml")).unwrap();
    assert!(reloaded.verify_integrity().is_ok());

    let mut registry = registry_for(dir.path());
    assert_eq!(registry.discover().unwrap(), 1);
    assert_eq!(registry.get("drifted").unwrap().lifecycle, Lifecycle::Built);
}

#[test]
fn rehash_manifest_fills_missing_hash() {
    let dir = tempfile::tempdir().unwrap();
    write_plugin(
        dir.path(),
        "nohash2",
        &manifest_toml("nohash2", None),
        b"(module)",
    );

    let entry =
        PluginEntry::from_manifest_path(&dir.path().join("nohash2").join("manifest.toml")).unwrap();
    assert!(matches!(
        entry.verify_integrity().unwrap_err(),
        PluginRegistryError::MissingHash { .. }
    ));

    let hash = entry.rehash_manifest().unwrap();
    assert_eq!(hash, sha256_hex(b"(module)"));

    let reloaded =
        PluginEntry::from_manifest_path(&dir.path().join("nohash2").join("manifest.toml")).unwrap();
    assert!(reloaded.verify_integrity().is_ok());

    let mut registry = registry_for(dir.path());
    assert_eq!(registry.discover().unwrap(), 1);
    assert_eq!(registry.get("nohash2").unwrap().lifecycle, Lifecycle::Built);
}

#[test]
fn rehash_manifest_preserves_other_fields() {
    let dir = tempfile::tempdir().unwrap();
    let plugin_dir = dir.path().join("schema-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("entry.wasm"), b"(module)").unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "schema-plugin"
name = "Schema Plugin"
version = "2.3.4"
type = "hook"
permissions = ["net.fetch"]
entry = "entry.wasm"

[config_schema]
type = "object"
properties = { url = { type = "string" } }
"#,
    )
    .unwrap();

    let entry = PluginEntry::from_manifest_path(&plugin_dir.join("manifest.toml")).unwrap();
    let hash = entry.rehash_manifest().unwrap();
    assert_eq!(hash, sha256_hex(b"(module)"));

    let reloaded = PluginEntry::from_manifest_path(&plugin_dir.join("manifest.toml")).unwrap();
    assert_eq!(reloaded.manifest.id, "schema-plugin");
    assert_eq!(reloaded.manifest.name, "Schema Plugin");
    assert_eq!(reloaded.manifest.version, "2.3.4");
    assert_eq!(reloaded.manifest.kind, PluginKind::Hook);
    assert_eq!(reloaded.manifest.permissions, vec!["net.fetch".to_string()]);
    assert!(
        reloaded.manifest.config_schema.is_some(),
        "config_schema must survive the round-trip"
    );
    assert!(reloaded.verify_integrity().is_ok());
}
