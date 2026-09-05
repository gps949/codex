use std::process::Command;

use codex_utils_cargo_bin::cargo_bin;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

#[test]
fn account_use_resolves_labels_and_rejects_ambiguous_or_disabled_profiles() {
    let home = TempDir::new().unwrap();
    let profiles = json!({"version": 1, "profiles": [
        {"id": "first", "label": "Work Pro", "priority": 0, "credential_location": "managed_profile", "state": "ready", "disabled": false},
        {"id": "second", "label": "Shared", "priority": 1, "credential_location": "managed_profile", "state": "ready", "disabled": false},
        {"id": "third", "label": "Shared", "priority": 2, "credential_location": "managed_profile", "state": "ready", "disabled": false},
        {"id": "parked", "label": "Parked", "priority": 3, "credential_location": "managed_profile", "state": "ready", "disabled": true}
    ]});
    std::fs::write(
        home.path().join("account-profiles.json"),
        profiles.to_string(),
    )
    .unwrap();
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    for (selector, expected) in [
        ("Work Pro", Some("first")),
        ("second", Some("second")),
        ("Shared", None),
        ("Parked", None),
        ("missing", None),
    ] {
        let state_path = home.path().join("account-runtime-state.json");
        let before = std::fs::read(&state_path).ok();
        let output = Command::new(cargo_bin("codex").unwrap())
            .env("CODEX_HOME", home.path())
            .args(["account", "use", selector])
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            expected.is_some(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(id) = expected {
            let state: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
            assert_eq!(state["active_profile_id"], json!(id));
        } else {
            assert_eq!(std::fs::read(&state_path).ok(), before);
        }
    }
}
