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

#[test]
fn account_views_offer_table_json_and_compatible_tsv() {
    let home = TempDir::new().unwrap();
    write_sample_profiles(&home).unwrap();
    for command in ["list", "pool", "status"] {
        let json_output = Command::new(cargo_bin("codex").unwrap())
            .env("CODEX_HOME", home.path())
            .args(["account", command, "--format", "json"])
            .output()
            .unwrap();
        assert!(
            json_output.status.success(),
            "{}",
            String::from_utf8_lossy(&json_output.stderr)
        );
        let inventory: serde_json::Value = serde_json::from_slice(&json_output.stdout).unwrap();
        assert_eq!(inventory["accounts"].as_array().unwrap().len(), 4);
        assert_eq!(inventory["accounts"][3]["availability"], json!("disabled"));
        assert_eq!(inventory["accounts"][0]["login"], json!("missing"));
        let stdout = String::from_utf8(json_output.stdout).unwrap();
        assert!(!stdout.contains("credential_home"));

        let table = Command::new(cargo_bin("codex").unwrap())
            .env("CODEX_HOME", home.path())
            .args(["account", command, "--format", "table"])
            .output()
            .unwrap();
        assert!(
            table.status.success(),
            "{}",
            String::from_utf8_lossy(&table.stderr)
        );
        let stdout = String::from_utf8(table.stdout).unwrap();
        assert!(!stdout.contains('\t'));
        assert!(stdout.contains("Parked"));
        assert!(stdout.contains("disabled"));
        assert!(stdout.contains("missing"));

        let tsv = Command::new(cargo_bin("codex").unwrap())
            .env("CODEX_HOME", home.path())
            .args(["account", command, "--format", "tsv", "--show-profile"])
            .output()
            .unwrap();
        assert!(
            tsv.status.success(),
            "{}",
            String::from_utf8_lossy(&tsv.stderr)
        );
        let stdout = String::from_utf8(tsv.stdout).unwrap();
        let header = stdout
            .lines()
            .find(|line| line.starts_with("ACTIVE\t"))
            .unwrap();
        assert_eq!(
            header.split('\t').take(3).collect::<Vec<_>>(),
            vec!["ACTIVE", "PRIORITY", "PROFILE"]
        );
        let row = stdout
            .lines()
            .find(|line| line.split('\t').nth(2) == Some("first"))
            .unwrap();
        assert_eq!(
            row.split('\t').count(),
            if command == "list" { 8 } else { 14 }
        );
    }
}

#[test]
fn paused_pool_keeps_pending_and_disabled_profiles_visible() {
    let home = TempDir::new().unwrap();
    write_sample_profiles(&home).unwrap();
    let path = home.path().join("account-profiles.json");
    let mut profiles: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    profiles["profiles"][1]["state"] = json!("pending_login");
    std::fs::write(path, profiles.to_string()).unwrap();
    codex_login::AccountPoolRuntime::suspend_home(home.path()).unwrap();

    let output = Command::new(cargo_bin("codex").unwrap())
        .env("CODEX_HOME", home.path())
        .args(["account", "pool", "--format", "json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let inventory: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        (
            inventory["suspended"].clone(),
            inventory["accounts"].as_array().unwrap().len(),
            inventory["accounts"][1]["login"].clone(),
            inventory["accounts"][3]["availability"].clone(),
        ),
        (json!(true), 4, json!("pending"), json!("disabled")),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cached_account_views_do_not_refresh_disabled_credentials() {
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;

    let home = TempDir::new().unwrap();
    write_sample_profiles(&home).unwrap();
    let credential_home = home.path().join("auth-profiles/parked");
    std::fs::create_dir_all(&credential_home).unwrap();
    let auth_path = credential_home.join("auth.json");
    let credentials = json!({
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": "eyJhbGciOiJub25lIn0.e30.c2ln",
            "access_token": "fixture-expired-access",
            "refresh_token": "fixture-refresh",
            "account_id": "fixture-account"
        },
        "last_refresh": "2000-01-01T00:00:00Z"
    })
    .to_string();
    std::fs::write(&auth_path, &credentials).unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    for command in ["list", "pool"] {
        let output = Command::new(cargo_bin("codex").unwrap())
            .env("CODEX_HOME", home.path())
            .env(
                "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
                format!("{}/oauth/token", server.uri()),
            )
            .args(["account", command, "--format", "json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let inventory: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(inventory["accounts"][3]["login"], json!("cached"));
        assert_eq!(std::fs::read_to_string(&auth_path).unwrap(), credentials);
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
    server.verify().await;
}

#[test]
fn account_config_controls_preserve_comments_and_disable_early_rotation() {
    let home = TempDir::new().unwrap();
    let config_path = home.path().join("config.toml");
    std::fs::write(
        &config_path,
        "# Personal settings\n[account_pool]\n# Keep this note\npreemptive_switch_percent = 95.0\nreturn_to_preferred = false\n",
    ).unwrap();
    for args in [
        vec!["set-preemptive-switch-percent", "0"],
        vec!["set-window-warmup", "false"],
        vec!["set-resume-after-reset", "true"],
        vec!["set-max-reset-wait-minutes", "120"],
        vec!["set-auto-reset-credits", "when_pool_exhausted"],
    ] {
        let output = Command::new(cargo_bin("codex").unwrap())
            .env("CODEX_HOME", home.path())
            .args(["account", "config"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let saved = std::fs::read_to_string(&config_path).unwrap();
    assert!(saved.contains("# Personal settings"));
    assert!(saved.contains("# Keep this note"));
    let config: toml::Value = toml::from_str(&saved).unwrap();
    assert_eq!(
        config["account_pool"].clone(),
        toml::Value::Table(toml::toml! {
            preemptive_switch_percent = 0.0
            return_to_preferred = false
            window_warmup = false
            resume_after_reset = true
            max_reset_wait_minutes = 120
            auto_reset_credits = "when_pool_exhausted"
        })
    );
    let show = Command::new(cargo_bin("codex").unwrap())
        .env("CODEX_HOME", home.path())
        .args(["account", "config", "show"])
        .output()
        .unwrap();
    assert!(show.status.success());
    let text = String::from_utf8_lossy(&show.stdout);
    assert!(
        text.contains("preemptive_switch_percent=disabled"),
        "{text}"
    );
    assert!(text.contains("window_warmup=false"), "{text}");
    assert!(text.contains("max_reset_wait_minutes=120"), "{text}");
    let before = std::fs::read(&config_path).unwrap();
    for args in [
        ["set-preemptive-switch-percent", "NaN"],
        ["set-max-reset-wait-minutes", "1441"],
    ] {
        let invalid = Command::new(cargo_bin("codex").unwrap())
            .env("CODEX_HOME", home.path())
            .args(["account", "config"])
            .args(args)
            .output()
            .unwrap();
        assert!(!invalid.status.success());
        assert_eq!(std::fs::read(&config_path).unwrap(), before);
    }
}

#[test]
fn account_auto_clears_manual_pin_resumes_pool_and_supports_least_used() {
    let home = TempDir::new().unwrap();
    write_sample_profiles(&home).unwrap();
    for (id, used_weekly) in [("first", 90.0), ("second", 15.0), ("third", 50.0)] {
        let cred_dir = home.path().join("auth-profiles").join(id);
        std::fs::create_dir_all(&cred_dir).unwrap();
        let auth = json!({
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": "eyJhbGciOiJub25lIn0.e30.c2ln",
                "access_token": format!("access-{id}"),
                "refresh_token": format!("refresh-{id}"),
                "account_id": format!("acct-{id}")
            },
            "last_refresh": "2026-01-01T00:00:00Z"
        });
        std::fs::write(cred_dir.join("auth.json"), auth.to_string()).unwrap();
        let _ = used_weekly;
    }

    let set_strategy = Command::new(cargo_bin("codex").unwrap())
        .env("CODEX_HOME", home.path())
        .args(["account", "config", "set-rotation-strategy", "least_used"])
        .output()
        .unwrap();
    assert!(
        set_strategy.status.success(),
        "{}",
        String::from_utf8_lossy(&set_strategy.stderr)
    );

    // Manually pin `third` and suspend the pool.
    let manual = Command::new(cargo_bin("codex").unwrap())
        .env("CODEX_HOME", home.path())
        .args(["account", "use", "third"])
        .output()
        .unwrap();
    assert!(manual.status.success());
    let state_path = home.path().join("account-runtime-state.json");
    let manual_state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(manual_state["active_profile_id"], json!("third"));
    assert_eq!(manual_state["manual_selection"], json!(true));

    // Seed cached quota observations so `second` has the lowest constraining usage.
    let now = chrono::Utc::now();
    let seeded = json!({
        "version": 1,
        "active_profile_id": "third",
        "manual_selection": true,
        "selection_revision": 3,
        "profiles": [
            {
                "profile_id": "first",
                "rate_limits": {
                    "primary": {"used_percent": 10.0, "resets_at": now + chrono::Duration::hours(1), "window_minutes": 300},
                    "secondary": {"used_percent": 90.0, "resets_at": now + chrono::Duration::days(3), "window_minutes": 10080},
                    "observed_at": now
                }
            },
            {
                "profile_id": "second",
                "rate_limits": {
                    "primary": {"used_percent": 20.0, "resets_at": now + chrono::Duration::hours(4), "window_minutes": 300},
                    "secondary": {"used_percent": 15.0, "resets_at": now + chrono::Duration::days(5), "window_minutes": 10080},
                    "observed_at": now
                }
            },
            {
                "profile_id": "third",
                "rate_limits": {
                    "primary": {"used_percent": 40.0, "resets_at": now + chrono::Duration::hours(2), "window_minutes": 300},
                    "secondary": {"used_percent": 50.0, "resets_at": now + chrono::Duration::days(4), "window_minutes": 10080},
                    "observed_at": now
                }
            }
        ]
    });
    std::fs::write(&state_path, seeded.to_string()).unwrap();
    codex_login::AccountPoolRuntime::suspend_home(home.path()).unwrap();

    let auto = Command::new(cargo_bin("codex").unwrap())
        .env("CODEX_HOME", home.path())
        .args(["account", "auto"])
        .output()
        .unwrap();
    assert!(
        auto.status.success(),
        "{}",
        String::from_utf8_lossy(&auto.stderr)
    );
    assert!(!codex_login::AccountPoolRuntime::is_home_suspended(
        home.path()
    ));
    let after: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    assert_eq!(after["active_profile_id"], json!("second"));
    assert!(
        after.get("manual_selection").is_none() || after["manual_selection"] == json!(false),
        "{after}"
    );

    // `codex account refresh` rejects disabled or ambiguous selectors before any network traffic.
    for bad_selector in ["Parked", "Shared", "missing"] {
        let refresh = Command::new(cargo_bin("codex").unwrap())
            .env("CODEX_HOME", home.path())
            .args(["account", "refresh", bad_selector])
            .output()
            .unwrap();
        assert!(!refresh.status.success());
    }
}
