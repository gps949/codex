use std::io::Write;
use std::process::Command;
use std::process::Stdio;

use codex_utils_cargo_bin::cargo_bin;
use pretty_assertions::assert_eq;
use serde_json::json;

fn run_manager(home: &std::path::Path, language: &str, input: &str) -> anyhow::Result<String> {
    let mut child = Command::new(cargo_bin("codex")?)
        .env("CODEX_HOME", home)
        .args(["account", "manage", "--tui", "--lang", language])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("Account manager did not open its input stream"))?
        .write_all(input.as_bytes())?;
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.replace("\u{1b}[2J\u{1b}[H", ""))
}

#[test]
fn account_manager_edits_preserve_raw_names_and_explicit_clear_restores_email() -> anyhow::Result<()>
{
    let mut transcript = String::new();
    for custom_label in [None, Some("fixture")] {
        let home = tempfile::TempDir::new()?;
        std::fs::write(home.path().join("config.toml"), "")?;
        let manifest_path = home.path().join("account-profiles.json");
        std::fs::write(
            &manifest_path,
            json!({"version": 1, "profiles": [{
                "id": "fixture", "label": custom_label, "priority": 10,
                "credential_location": "managed_profile", "state": "ready", "disabled": false
            }]})
            .to_string(),
        )?;
        let credential_home = home.path().join("auth-profiles/fixture");
        std::fs::create_dir_all(&credential_home)?;
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &json!({"email": "alice@example.com", "https://api.openai.com/auth": {
                "chatgpt_plan_type": "pro", "chatgpt_user_id": "fixture-owner", "chatgpt_account_id": "fixture-account"
            }}),
            &jsonwebtoken::EncodingKey::from_secret(b"synthetic-test-key"),
        )?;
        std::fs::write(credential_home.join("auth.json"), json!({"tokens": {
            "id_token": token, "access_token": "synthetic-access", "refresh_token": "synthetic-refresh"
        }, "last_refresh": "2099-01-01T00:00:00Z"}).to_string())?;
        let edited = run_manager(home.path(), "en", "1\ne\n\n11\nq\n")?;
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
        assert_eq!(
            (
                manifest["profiles"][0]["label"].clone(),
                manifest["profiles"][0]["priority"].clone()
            ),
            (json!(custom_label), json!(11))
        );
        if custom_label.is_none() {
            transcript.push_str(&format!("Automatic name, priority edit:\n{edited}\n"));
        }
        let cleared = run_manager(home.path(), "zh-CN", "1\nn\nq\n")?;
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
        assert_eq!(manifest["profiles"][0]["label"], serde_json::Value::Null);
        assert!(cleared.contains("alice@example.com"));
        if custom_label.is_some() {
            transcript.push_str(&format!("Explicit name matching ID, clear:\n{cleared}"));
        }
    }
    insta::assert_snapshot!(transcript);
    Ok(())
}
