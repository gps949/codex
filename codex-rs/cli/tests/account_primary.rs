use std::fs;
use std::path::Path;
use std::process::Command;
use std::process::Output;

use codex_utils_cargo_bin::cargo_bin;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;

fn command(home: &Path, args: &[&str]) -> anyhow::Result<Output> {
    Ok(Command::new(cargo_bin("codex")?)
        .env("CODEX_HOME", home)
        .env_remove("CODEX_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_ACCESS_TOKEN")
        .env_remove("OPENAI_FEDERATION_RULE_ID")
        .env_remove("OPENAI_IDENTITY_TOKEN_FILE")
        .env_remove("OPENAI_WORKLOAD_IDENTITY_CONTEXT")
        .env(
            "CODEX_REVOKE_TOKEN_URL_OVERRIDE",
            "http://127.0.0.1:9/oauth/revoke",
        )
        .args(args)
        .output()?)
}

fn run(home: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = command(home, args)?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn fixture() -> anyhow::Result<TempDir> {
    let home = TempDir::new()?;
    fs::write(
        home.path().join("config.toml"),
        "auth_credentials_store = \"file\"\n",
    )?;
    fs::write(home.path().join("account-profiles.json"), json!({"version": 1, "profiles": [
        {"id": "seat-a", "label": "Remote seat", "priority": 0, "credential_location": "managed_profile", "state": "ready", "disabled": true},
        {"id": "seat-b", "label": null, "priority": 1, "credential_location": "managed_profile", "state": "ready", "disabled": false}
    ]}).to_string())?;
    for (directory, user, workspace) in [
        (home.path().to_path_buf(), "root", "personal"),
        (
            home.path().join("auth-profiles/seat-a"),
            "alice",
            "business",
        ),
        (home.path().join("auth-profiles/seat-b"), "bob", "business"),
    ] {
        fs::create_dir_all(&directory)?;
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &json!({"email": format!("{user}@example.com"), "https://api.openai.com/auth": {
                "chatgpt_user_id": user, "chatgpt_account_id": workspace, "chatgpt_plan_type": "pro"
            }}),
            &jsonwebtoken::EncodingKey::from_secret(b"synthetic-primary-test"),
        )?;
        fs::write(
            directory.join("auth.json"),
            json!({"auth_mode": "chatgpt", "tokens": {
            "id_token": token, "access_token": format!("synthetic-access-{user}"),
            "refresh_token": format!("synthetic-refresh-{user}"), "account_id": workspace
        }, "last_refresh": "2099-01-01T00:00:00Z"})
            .to_string(),
        )?;
    }
    run(home.path(), &["account", "use", "seat-b"])?;
    Ok(home)
}

fn inference_files(home: &Path) -> anyhow::Result<Vec<Vec<u8>>> {
    [
        "account-profiles.json",
        "account-runtime-state.json",
        "auth.json",
        "auth-profiles/seat-a/auth.json",
        "auth-profiles/seat-b/auth.json",
    ]
    .into_iter()
    .map(|path| fs::read(home.join(path)).map_err(Into::into))
    .collect()
}

#[test]
fn primary_source_operations_preserve_inference_and_credentials() -> anyhow::Result<()> {
    let home = fixture()?;
    let before = inference_files(home.path())?;
    let mut transcript = String::new();
    for (args, source) in [
        (
            vec!["account", "primary", "use", "Remote seat"],
            json!({"type": "profile", "profile_id": "seat-a"}),
        ),
        (
            vec!["account", "primary", "use", "bob@example.com"],
            json!({"type": "profile", "profile_id": "seat-b"}),
        ),
        (
            vec!["account", "primary", "logout"],
            json!({"type": "signed_out"}),
        ),
        (
            vec!["account", "primary", "root"],
            json!({"type": "root_login"}),
        ),
    ] {
        transcript.push_str(&format!(
            "$ codex {}\n{}\n",
            args.join(" "),
            run(home.path(), &args)?
        ));
        let state: Value =
            serde_json::from_slice(&fs::read(home.path().join(".primary-login.json"))?)?;
        let mut actual = state["source"].clone();
        if let Some(fields) = actual.as_object_mut() {
            fields.remove("owner_hash");
        }
        assert_eq!(actual, source);
        assert_eq!(inference_files(home.path())?, before);
    }
    insta::assert_snapshot!(transcript);
    Ok(())
}

#[test]
fn selected_primary_profile_cannot_be_removed_before_revocation() -> anyhow::Result<()> {
    let home = fixture()?;
    run(home.path(), &["account", "primary", "use", "Remote seat"])?;
    let before = inference_files(home.path())?;
    for args in [
        vec!["account", "remove", "seat-a"],
        vec!["account", "remove", "seat-a", "--keep-credentials"],
    ] {
        let output = command(home.path(), &args)?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("selected for host sign-in"));
        assert_eq!(inference_files(home.path())?, before);
    }
    run(home.path(), &["account", "primary", "logout"])?;
    run(
        home.path(),
        &["account", "remove", "seat-a", "--keep-credentials"],
    )?;
    let profiles: Value =
        serde_json::from_slice(&fs::read(home.path().join("account-profiles.json"))?)?;
    assert_eq!(
        profiles["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .map(|profile| profile["id"].clone())
            .collect::<Vec<_>>(),
        vec![json!("seat-b")]
    );
    assert_eq!(
        fs::read(home.path().join("auth-profiles/seat-a/auth.json"))?,
        before[3]
    );
    Ok(())
}

#[test]
fn corrupt_primary_metadata_does_not_revoke_root_credentials_on_logout() -> anyhow::Result<()> {
    let home = fixture()?;
    let before = inference_files(home.path())?;
    fs::write(home.path().join(".primary-login.json"), "{")?;
    let output = command(home.path(), &["logout"])?;
    assert!(
        !output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(inference_files(home.path())?, before);
    Ok(())
}

#[test]
fn host_managed_workload_selection_does_not_mutate_primary_metadata() -> anyhow::Result<()> {
    let home = fixture()?;
    let before = inference_files(home.path())?;
    for args in [
        vec!["account", "primary", "use", "seat-b"],
        vec!["account", "primary", "logout"],
    ] {
        let output = Command::new(cargo_bin("codex")?)
            .env("CODEX_HOME", home.path())
            .env("OPENAI_FEDERATION_RULE_ID", "synthetic-workload-marker")
            .env_remove("CODEX_API_KEY")
            .env_remove("CODEX_ACCESS_TOKEN")
            .args(args)
            .output()?;
        assert!(!output.status.success());
        assert!(!home.path().join(".primary-login.json").exists());
        assert_eq!(inference_files(home.path())?, before);
    }
    Ok(())
}
