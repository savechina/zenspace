use std::path::Path;

use zen_plugin::{Lifecycle, PluginEntry, PluginRegistry, PluginRegistryError};

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

#[test]
fn tampered_wasm_hash_mismatches() {
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

    let mut registry = PluginRegistry::with_plugin_dir(dir.path().to_path_buf());
    registry.discover().unwrap();
    let loaded = registry.get("tampered").unwrap();
    assert_eq!(loaded.lifecycle, Lifecycle::Failed);
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

    let mut registry = PluginRegistry::with_plugin_dir(dir.path().to_path_buf());
    registry.discover().unwrap();
    let loaded = registry.get("valid").unwrap();
    assert_eq!(loaded.lifecycle, Lifecycle::Built);
}

#[test]
fn missing_sha256_warns_but_loads() {
    let dir = tempfile::tempdir().unwrap();
    write_plugin(
        dir.path(),
        "nohash",
        &manifest_toml("nohash", None),
        b"(module)",
    );

    let entry =
        PluginEntry::from_manifest_path(&dir.path().join("nohash").join("manifest.toml")).unwrap();
    assert!(entry.verify_integrity().is_ok());

    let mut registry = PluginRegistry::with_plugin_dir(dir.path().to_path_buf());
    registry.discover().unwrap();
    let loaded = registry.get("nohash").unwrap();
    assert_eq!(loaded.lifecycle, Lifecycle::Built);
}
