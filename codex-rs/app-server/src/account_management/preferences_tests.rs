use super::*;
use pretty_assertions::assert_eq;

#[test]
fn manager_language_roundtrips_without_account_configuration() -> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    assert_eq!(read(home.path()), ManagerPreferences::default());
    let chinese = ManagerPreferences {
        language: ManagerLanguage::SimplifiedChinese,
    };
    write(home.path(), chinese)?;
    assert_eq!(read(home.path()), chinese);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(home.path().join(FILE_NAME))?)?,
        serde_json::json!({"schemaVersion":1,"language":"zh-CN"})
    );
    write(home.path(), ManagerPreferences::default())?;
    assert_eq!(read(home.path()), ManagerPreferences::default());
    assert!(!home.path().join("config.toml").exists());
    Ok(())
}

#[test]
fn invalid_or_oversized_preferences_do_not_block_management() -> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    for bytes in [
        b"{".as_slice(),
        b"{\"schemaVersion\":2,\"language\":\"zh-CN\"}",
        b"{\"schemaVersion\":1,\"language\":\"other\"}",
    ] {
        std::fs::write(home.path().join(FILE_NAME), bytes)?;
        assert_eq!(read(home.path()), ManagerPreferences::default());
    }
    std::fs::write(
        home.path().join(FILE_NAME),
        vec![b' '; MAX_BYTES as usize + 1],
    )?;
    assert_eq!(read(home.path()), ManagerPreferences::default());
    write(
        home.path(),
        ManagerPreferences {
            language: ManagerLanguage::SimplifiedChinese,
        },
    )?;
    assert_eq!(
        read(home.path()).language,
        ManagerLanguage::SimplifiedChinese
    );
    Ok(())
}
