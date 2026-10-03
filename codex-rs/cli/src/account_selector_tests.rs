use super::*;
use codex_login::AccountProfileStore;
use pretty_assertions::assert_eq;

fn save_email(record: &AccountProfileRecord, email: &str) -> anyhow::Result<()> {
    let token = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &serde_json::json!({"email": email}),
        &jsonwebtoken::EncodingKey::from_secret(b"synthetic-test-key"),
    )?;
    std::fs::write(
        record.profile.credential_home.join("auth.json"),
        serde_json::to_vec(&serde_json::json!({"tokens": {
            "id_token": token, "access_token": "synthetic-access", "refresh_token": "synthetic-refresh"
        }, "last_refresh": "2099-01-01T00:00:00Z"}))?,
    )?;
    Ok(())
}

#[tokio::test]
async fn unique_local_emails_and_trimmed_custom_labels_select_the_same_profile()
-> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let store = AccountProfileStore::new(home.path().to_path_buf());
    let profile = store.allocate_profile(Some("  Work  ".into()), /*priority*/ 10)?;
    store.complete_profile(&profile.id)?;
    let records = store.load_profile_records()?;
    save_email(&records[0], "alice@example.com")?;
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let selected = ["Work", " alice@example.com ", profile.id.as_str()].map(|selector| {
        resolve_account_with_config(&records, selector, &config.auth_config())
            .map(|record| record.profile.id.clone())
    });
    assert_eq!(
        selected,
        [
            Ok(profile.id.clone()),
            Ok(profile.id.clone()),
            Ok(profile.id)
        ]
    );
    Ok(())
}

#[tokio::test]
async fn shared_personal_and_business_email_requires_an_exact_id_or_unique_label()
-> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let store = AccountProfileStore::new(home.path().to_path_buf());
    for label in ["Personal", "Business"] {
        let profile = store.allocate_profile(Some(label.into()), /*priority*/ 10)?;
        store.complete_profile(&profile.id)?;
    }
    let records = store.load_profile_records()?;
    for record in &records {
        save_email(record, "shared@example.com")?;
    }
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    assert!(
        resolve_account_with_config(&records, "shared@example.com", &config.auth_config()).is_err()
    );
    for record in &records {
        assert_eq!(
            resolve_account_with_config(
                &records,
                record.profile.id.as_str(),
                &config.auth_config()
            )
            .map_err(anyhow::Error::msg)?,
            record
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_label_matching_another_accounts_email_is_ambiguous_even_when_label_is_first()
-> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let store = AccountProfileStore::new(home.path().to_path_buf());
    for label in ["bob@example.com", "Work"] {
        let profile = store.allocate_profile(Some(label.into()), /*priority*/ 10)?;
        store.complete_profile(&profile.id)?;
    }
    let records = store.load_profile_records()?;
    for record in &records {
        save_email(
            record,
            if record.profile.label.as_deref() == Some("Work") {
                "bob@example.com"
            } else {
                "alice@example.com"
            },
        )?;
    }
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    assert!(
        resolve_account_with_config(&records, "bob@example.com", &config.auth_config()).is_err()
    );
    let mut records = records;
    records[0].profile.label = Some(records[1].profile.id.to_string());
    assert_eq!(
        resolve_account_with_config(
            &records,
            records[1].profile.id.as_str(),
            &config.auth_config()
        )
        .map_err(anyhow::Error::msg)?,
        &records[1]
    );
    Ok(())
}

#[tokio::test]
async fn pending_or_unreadable_identity_keeps_label_and_id_selection_without_using_stale_email()
-> anyhow::Result<()> {
    let home = tempfile::TempDir::new()?;
    let store = AccountProfileStore::new(home.path().to_path_buf());
    store.allocate_profile(Some("Pending".into()), /*priority*/ 10)?;
    let profile = store.allocate_profile(Some("Unreadable".into()), /*priority*/ 20)?;
    store.complete_profile(&profile.id)?;
    let records = store.load_profile_records()?;
    for record in &records {
        if record.state == AccountProfileState::PendingLogin {
            save_email(record, "stale@example.com")?;
        } else {
            std::fs::write(
                record.profile.credential_home.join("auth.json"),
                "invalid JSON",
            )?;
        }
    }
    let config = codex_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    assert!(
        resolve_account_with_config(&records, "stale@example.com", &config.auth_config()).is_err()
    );
    for record in &records {
        assert_eq!(
            resolve_account_with_config(
                &records,
                record.profile.label.as_deref().unwrap(),
                &config.auth_config()
            )
            .map_err(anyhow::Error::msg)?,
            record
        );
    }
    Ok(())
}
