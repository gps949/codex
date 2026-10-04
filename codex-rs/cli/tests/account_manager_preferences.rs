use std::io::Write;
use std::process::Command;
use std::process::Stdio;

use codex_utils_cargo_bin::cargo_bin;
use pretty_assertions::assert_eq;

fn run_manager(
    home: &std::path::Path,
    launch_language: Option<&str>,
    input: &str,
) -> anyhow::Result<String> {
    let mut command = Command::new(cargo_bin("codex")?);
    command
        .env("CODEX_HOME", home)
        .args(["account", "manage", "--tui"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(language) = launch_language {
        command.args(["--lang", language]);
    }
    let mut child = command.spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("manager stdin is piped"))?
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
fn account_manager_language_persists_and_launch_override_is_session_only() -> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    std::fs::write(home.path().join("config.toml"), "")?;
    let first = run_manager(home.path(), /*launch_language*/ None, "g\nq\n")?;
    assert!(first.contains("Codex Accounts"));
    assert!(first.contains("Codex 账号管理"));
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(
        home.path().join(".account-manager-ui.json"),
    )?)?;
    assert_eq!(
        stored,
        serde_json::json!({"schemaVersion":1,"language":"zh-CN"})
    );
    let remembered = run_manager(home.path(), /*launch_language*/ None, "q\n")?;
    assert!(remembered.contains("Codex 账号管理"));
    assert!(!remembered.contains("Codex Accounts"));
    let override_english = run_manager(home.path(), Some("en"), "q\n")?;
    assert!(override_english.contains("Codex Accounts"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(
            home.path().join(".account-manager-ui.json")
        )?)?,
        stored
    );
    let after_override = run_manager(home.path(), /*launch_language*/ None, "q\n")?;
    assert!(after_override.contains("Codex 账号管理"));
    Ok(())
}
