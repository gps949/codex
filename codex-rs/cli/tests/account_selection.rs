use std::process::Command;

use codex_utils_cargo_bin::cargo_bin;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

fn write_sample_profiles(home: &TempDir) -> std::io::Result<()> {
    let profiles = json!({"version": 1, "profiles": [
        {"id": "first", "label": "Work Pro", "priority": 0, "credential_location": "managed_profile", "state": "ready", "disabled": false},
        {"id": "second", "label": "Shared", "priority": 1, "credential_location": "managed_profile", "state": "ready", "disabled": false},
        {"id": "third", "label": "Shared", "priority": 2, "credential_location": "managed_profile", "state": "ready", "disabled": false},
        {"id": "parked", "label": "Parked", "priority": 3, "credential_location": "managed_profile", "state": "ready", "disabled": true}
    ]});
    std::fs::write(
        home.path().join("account-profiles.json"),
        profiles.to_string(),
    )?;
    std::fs::write(home.path().join("config.toml"), "")?;
    Ok(())
}

#[test]
fn account_use_resolves_labels_and_rejects_ambiguous_or_disabled_profiles() {
    let home = TempDir::new().unwrap();
    write_sample_profiles(&home).unwrap();
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

#[test]
fn account_set_resolves_labels_and_list_hides_profile_by_default() {
    let home = TempDir::new().unwrap();
    write_sample_profiles(&home).unwrap();

    let set = Command::new(cargo_bin("codex").unwrap())
        .env("CODEX_HOME", home.path())
        .args(["account", "set", "Work Pro", "--priority", "5"])
        .output()
        .unwrap();
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    let profiles: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.path().join("account-profiles.json")).unwrap())
            .unwrap();
    let first = profiles["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|profile| profile["id"] == "first")
        .unwrap();
    assert_eq!(first["priority"], json!(5));

    let list = Command::new(cargo_bin("codex").unwrap())
        .env("CODEX_HOME", home.path())
        .args(["account", "list"])
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_stdout.starts_with("ACTIVE\tPRIORITY\tSTATE\tPLAN\tEMAIL\tCOOLDOWN\tLABEL"),
        "{list_stdout}"
    );
    assert!(!list_stdout.contains("\tPROFILE\t"), "{list_stdout}");
    assert!(!list_stdout.contains("first"), "{list_stdout}");

    let list_with_profile = Command::new(cargo_bin("codex").unwrap())
        .env("CODEX_HOME", home.path())
        .args(["account", "list", "--show-profile"])
        .output()
        .unwrap();
    assert!(
        list_with_profile.status.success(),
        "{}",
        String::from_utf8_lossy(&list_with_profile.stderr)
    );
    let list_with_profile_stdout = String::from_utf8_lossy(&list_with_profile.stdout);
    assert!(
        list_with_profile_stdout
            .starts_with("ACTIVE\tPRIORITY\tPROFILE\tSTATE\tPLAN\tEMAIL\tCOOLDOWN\tLABEL"),
        "{list_with_profile_stdout}"
    );
    assert!(
        list_with_profile_stdout.contains("first"),
        "{list_with_profile_stdout}"
    );
}
