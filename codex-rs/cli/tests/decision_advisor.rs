use codex_utils_cargo_bin::cargo_bin;
use pretty_assertions::assert_eq;
use std::process::Command;

#[test]
fn decision_advisor_status_is_read_only_and_off_by_default() -> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    std::fs::write(home.path().join("config.toml"), "")?;
    let output = Command::new(cargo_bin("codex")?)
        .env("CODEX_HOME", home.path())
        .env_remove("TYPESAFE_API_KEY")
        .args(["decision-advisor", "status"])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    insta::assert_snapshot!(String::from_utf8(output.stdout)?);
    assert_eq!(
        std::fs::read_to_string(home.path().join("config.toml"))?,
        ""
    );
    let disabled = Command::new(cargo_bin("codex")?)
        .env("CODEX_HOME", home.path())
        .args([
            "decision-advisor",
            "probe",
            "--query",
            "weather",
            "--catalog",
            "unused.json",
        ])
        .output()?;
    assert!(!disabled.status.success());
    assert!(String::from_utf8(disabled.stderr)?.contains("The decision advisor is off"));
    Ok(())
}
