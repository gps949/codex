//! Read-only diagnostics for the fork's native multi-account pool.
//!
//! Checks in this module never refresh OAuth tokens, mutate scheduler state, or emit
//! account emails or secret material into details.

use std::fs;
use std::path::Path;

use chrono::Utc;
use codex_core::config::Config;
use codex_login::AccountPoolRuntime;
use codex_login::AccountProfileRecord;
use codex_login::AccountProfileState;
use codex_login::AccountProfileStore;
use codex_login::AccountRuntimeStateStore;
use codex_login::ApiAccountSelection;
use codex_login::ApiAccountStore;
use codex_login::load_auth_dot_json;
use codex_protocol::auth::AuthMode;

use super::CheckStatus;
use super::DoctorCheck;
use crate::account_config::format_rotation_strategy;

fn profile_has_usable_chatgpt_auth(config: &Config, record: &AccountProfileRecord) -> bool {
    if record.state != AccountProfileState::Ready {
        return false;
    }
    let Ok(Some(auth)) = load_auth_dot_json(
        &record.profile.credential_home,
        config.cli_auth_credentials_store_mode,
        config.auth_keyring_backend_kind(),
    ) else {
        return false;
    };
    let managed_mode = auth.auth_mode == Some(AuthMode::Chatgpt)
        || (auth.auth_mode.is_none()
            && auth.openai_api_key.is_none()
            && auth.personal_access_token.is_none()
            && auth.agent_identity.is_none()
            && auth.bedrock_api_key.is_none()
            && auth.bedrock_access_keys.is_none());
    managed_mode
        && auth.last_refresh.is_some()
        && auth.tokens.as_ref().is_some_and(|tokens| {
            !tokens.access_token.trim().is_empty() && !tokens.refresh_token.trim().is_empty()
        })
}

/// Evaluates fork-managed credential sources (`account-profiles.json` and `api-accounts.json`)
/// when root `auth.json` and auth environment variables are absent.
pub(super) fn pool_backed_auth_check(
    config: &Config,
    mut details: Vec<String>,
) -> Option<DoctorCheck> {
    let api_store = ApiAccountStore::new(
        config.codex_home.to_path_buf(),
        config.cli_auth_credentials_store_mode,
        config.auth_keyring_backend_kind(),
    );
    if let Ok(api_state) = api_store.load()
        && let ApiAccountSelection::Manual { profile_id } = api_state.selection
    {
        let has_key = api_store.has_key(&profile_id).unwrap_or(false);
        details.push(format!("manual API account selection: {profile_id}"));
        details.push(format!("manual API key present: {has_key}"));
        return Some(if has_key {
            DoctorCheck::new(
                "auth.credentials",
                "auth",
                CheckStatus::Ok,
                "auth is provided by a managed API account",
            )
            .details(details)
        } else {
            DoctorCheck::new(
                "auth.credentials",
                "auth",
                CheckStatus::Fail,
                "selected managed API account is missing its API key",
            )
            .details(details)
            .remediation(
                "Replace the API key in codex account manage or run codex account auto to return to subscriptions.",
            )
        });
    }

    let manifest_path = config.codex_home.join("account-profiles.json");
    if !manifest_path.exists() {
        return None;
    }

    let store = AccountProfileStore::new(config.codex_home.to_path_buf());
    let records = match store.load_profile_records() {
        Ok(records) => records,
        Err(error) => {
            details.push(format!("account pool manifest error: {error}"));
            return Some(
                DoctorCheck::new(
                    "auth.credentials",
                    "auth",
                    CheckStatus::Fail,
                    "account pool manifest could not be read",
                )
                .details(details)
                .remediation("Inspect account-profiles.json or run codex account add."),
            );
        }
    };
    if records.is_empty() {
        return None;
    }

    let suspended = AccountPoolRuntime::is_home_suspended(&config.codex_home);
    let usable_enabled = records
        .iter()
        .filter(|record| {
            !record.profile.disabled && profile_has_usable_chatgpt_auth(config, record)
        })
        .count();
    details.push(format!("account pool profiles: {}", records.len()));
    details.push(format!(
        "account pool ready enabled profiles: {usable_enabled}"
    ));
    details.push(format!("account pool suspended: {suspended}"));

    if suspended {
        return Some(
            DoctorCheck::new(
                "auth.credentials",
                "auth",
                CheckStatus::Fail,
                "account pool credentials exist, but the account pool is suspended",
            )
            .details(details)
            .remediation(
                "Run codex account auto or codex account use <profile> to resume the account pool, or run codex login.",
            ),
        );
    }

    if usable_enabled > 0 {
        Some(
            DoctorCheck::new(
                "auth.credentials",
                "auth",
                CheckStatus::Ok,
                "auth is provided by the multi-account pool",
            )
            .details(details),
        )
    } else {
        Some(
            DoctorCheck::new(
                "auth.credentials",
                "auth",
                CheckStatus::Fail,
                "account pool has no enabled profile with valid credentials",
            )
            .details(details)
            .remediation(
                "Enable or re-authenticate a profile with codex account enable / codex account login, or run codex account add.",
            ),
        )
    }
}

pub(super) fn account_pool_check(config: &Config) -> DoctorCheck {
    let exe_parent = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    account_pool_check_with_install_dir(config, exe_parent.as_deref())
}

fn account_pool_check_with_install_dir(config: &Config, install_dir: Option<&Path>) -> DoctorCheck {
    let mut details = Vec::new();
    let mut warnings = Vec::new();
    let manifest_path = config.codex_home.join("account-profiles.json");
    details.push(format!("profiles manifest: {}", manifest_path.display()));
    details.push(format!(
        "rotation strategy: {}",
        format_rotation_strategy(config.account_pool.effective_rotation_strategy())
    ));
    details.push(format!(
        "return to preferred: {}",
        config.account_pool.effective_return_to_preferred()
    ));

    if let Some(install_dir) = install_dir {
        let packages_dir = install_dir.join(".codex-packages");
        if let Ok(entries) = fs::read_dir(&packages_dir) {
            let release_count = entries
                .flatten()
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with("release."))
                })
                .count();
            details.push(format!(
                "installed release generations ({}): {release_count}",
                packages_dir.display()
            ));
            if release_count > 3 {
                warnings.push(format!(
                    "{release_count} release generations exist under {}; rerun ./install.sh to prune superseded releases",
                    packages_dir.display()
                ));
            }
        }
    }

    let unresolved_journals = count_unresolved_reset_journals(&config.codex_home);
    if unresolved_journals > 0 {
        details.push(format!(
            "unresolved reset-credit journals: {unresolved_journals}"
        ));
        warnings.push(format!(
            "{unresolved_journals} unconfirmed reset-credit journal(s) need review in codex account manage"
        ));
    }

    if !manifest_path.exists() {
        details.push("account pool state: not configured".to_string());
        if let Some(first_warning) = warnings.first() {
            return DoctorCheck::new(
                "account_pool.health",
                "account-pool",
                CheckStatus::Warning,
                first_warning.clone(),
            )
            .details(details);
        }
        return DoctorCheck::new(
            "account_pool.health",
            "account-pool",
            CheckStatus::Ok,
            "multi-account pool is not configured",
        )
        .details(details);
    }

    let store = AccountProfileStore::new(config.codex_home.to_path_buf());
    let records = match store.load_profile_records() {
        Ok(records) => records,
        Err(error) => {
            details.push(format!("manifest read error: {error}"));
            return DoctorCheck::new(
                "account_pool.health",
                "account-pool",
                CheckStatus::Fail,
                "account-profiles.json could not be read",
            )
            .details(details)
            .remediation(
                "Fix or restore account-profiles.json, or re-add profiles with codex account add.",
            );
        }
    };

    let suspended = AccountPoolRuntime::is_home_suspended(&config.codex_home);
    details.push(format!("suspended: {suspended}"));

    let mut ready_usable = 0usize;
    let mut disabled = 0usize;
    let mut pending = 0usize;
    let mut broken_auth = 0usize;
    for record in &records {
        if record.profile.disabled {
            disabled += 1;
        }
        match record.state {
            AccountProfileState::PendingLogin => pending += 1,
            AccountProfileState::Ready => {
                if profile_has_usable_chatgpt_auth(config, record) {
                    if !record.profile.disabled {
                        ready_usable += 1;
                    }
                } else if !record.profile.disabled {
                    broken_auth += 1;
                }
            }
        }
    }
    details.push(format!("enrolled profiles: {}", records.len()));
    details.push(format!("ready enabled profiles: {ready_usable}"));
    details.push(format!("disabled profiles: {disabled}"));
    details.push(format!("pending login profiles: {pending}"));
    details.push(format!("enabled profiles needing re-login: {broken_auth}"));

    let now = Utc::now();
    match AccountRuntimeStateStore::new(config.codex_home.to_path_buf()).load() {
        Ok(runtime) => {
            let cooling_down = runtime
                .profiles
                .iter()
                .filter(|profile| profile.exhausted_until.is_some_and(|until| until > now))
                .count();
            details.push(format!(
                "active profile: {}",
                runtime
                    .active_profile_id
                    .as_ref()
                    .map(codex_login::AccountProfileId::as_str)
                    .unwrap_or("none")
            ));
            details.push(format!("manual selection: {}", runtime.manual_selection));
            details.push(format!("profiles in quota cooldown: {cooling_down}"));
        }
        Err(error) => {
            details.push(format!("runtime state error: {error}"));
            warnings.push(
                "account-runtime-state.json could not be parsed; run codex account auto to rebuild selection state"
                    .to_string(),
            );
        }
    }

    if suspended && !records.is_empty() {
        warnings.push(
            "account pool is suspended (.account-pool-suspended); run codex account auto or codex account use <profile> to resume"
                .to_string(),
        );
    }
    if broken_auth > 0 {
        warnings.push(format!(
            "{broken_auth} enabled profile(s) have missing or incomplete credentials; run codex account list and codex account login <profile>"
        ));
    }
    if pending > 0 {
        warnings.push(format!(
            "{pending} profile(s) have unfinished login attempts"
        ));
    }

    if !records.is_empty() && ready_usable == 0 && !suspended {
        return DoctorCheck::new(
            "account_pool.health",
            "account-pool",
            CheckStatus::Warning,
            "account pool has no ready enabled profiles",
        )
        .details(details)
        .remediation(
            "Run codex account list to inspect profiles, or codex account add to enroll one.",
        );
    }

    if let Some(first_warning) = warnings.first() {
        for extra in warnings.iter().skip(1) {
            details.push(format!("warning: {extra}"));
        }
        return DoctorCheck::new(
            "account_pool.health",
            "account-pool",
            CheckStatus::Warning,
            first_warning.clone(),
        )
        .details(details);
    }

    DoctorCheck::new(
        "account_pool.health",
        "account-pool",
        CheckStatus::Ok,
        format!(
            "multi-account pool has {ready_usable} ready enabled profile(s) ({} total)",
            records.len()
        ),
    )
    .details(details)
}

fn count_unresolved_reset_journals(codex_home: &Path) -> usize {
    let dir = codex_home.join("reset-credit-journal");
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry.path().extension().and_then(|ext| ext.to_str()) == Some("json")
        })
        .count()
}

#[cfg(test)]
mod tests {
    use codex_core::config::ConfigBuilder;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    async fn test_config(home: &Path) -> Config {
        fs::write(home.join("config.toml"), "").expect("write empty config");
        ConfigBuilder::default()
            .codex_home(home.to_path_buf())
            .build()
            .await
            .expect("build test config")
    }

    fn write_ready_pool_profile(home: &Path, id: &str, email: &str) {
        let profiles = json!({
            "version": 1,
            "profiles": [{
                "id": id,
                "label": "Work Pro",
                "priority": 0,
                "credential_location": "managed_profile",
                "state": "ready",
                "disabled": false
            }]
        });
        fs::write(home.join("account-profiles.json"), profiles.to_string())
            .expect("write manifest");
        let cred_dir = home.join("auth-profiles").join(id);
        fs::create_dir_all(&cred_dir).expect("create credential dir");
        let auth = json!({
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": "eyJhbGciOiJub25lIn0.e30.c2ln",
                "access_token": "secret-access-token",
                "refresh_token": "secret-refresh-token",
                "account_id": format!("acct-{email}")
            },
            "last_refresh": "2026-01-01T00:00:00Z"
        });
        fs::write(cred_dir.join("auth.json"), auth.to_string()).expect("write profile auth");
    }

    #[tokio::test]
    async fn pool_only_home_satisfies_auth_check_and_redacts_secrets() {
        let home = TempDir::new().expect("tempdir");
        write_ready_pool_profile(home.path(), "acct-1", "alice@example.com");
        let config = test_config(home.path()).await;

        let auth = pool_backed_auth_check(&config, Vec::new()).expect("pool auth check");
        assert_eq!(auth.status, CheckStatus::Ok);
        assert_eq!(auth.summary, "auth is provided by the multi-account pool");

        let pool_check = account_pool_check_with_install_dir(&config, None);
        assert_eq!(pool_check.status, CheckStatus::Ok);
        let serialized = serde_json::to_string(&pool_check).expect("serialize check");
        assert!(!serialized.contains("alice@example.com"));
        assert!(!serialized.contains("secret-access-token"));
    }

    #[tokio::test]
    async fn suspended_pool_without_root_auth_fails_with_resume_remediation() {
        let home = TempDir::new().expect("tempdir");
        write_ready_pool_profile(home.path(), "acct-1", "alice@example.com");
        AccountPoolRuntime::suspend_home(home.path()).expect("suspend pool");
        let config = test_config(home.path()).await;

        let auth = pool_backed_auth_check(&config, Vec::new()).expect("pool auth check");
        assert_eq!(auth.status, CheckStatus::Fail);
        assert!(
            auth.remediation
                .as_deref()
                .is_some_and(|rem| rem.contains("codex account auto"))
        );

        let pool_check = account_pool_check_with_install_dir(&config, None);
        assert_eq!(pool_check.status, CheckStatus::Warning);
    }

    #[tokio::test]
    async fn account_pool_check_warns_on_excess_release_generations() {
        let home = TempDir::new().expect("tempdir");
        let install_dir = TempDir::new().expect("install tempdir");
        let packages = install_dir.path().join(".codex-packages");
        for idx in 0..4 {
            fs::create_dir_all(packages.join(format!("release.old-{idx}")))
                .expect("create release dir");
        }
        let config = test_config(home.path()).await;

        let check = account_pool_check_with_install_dir(&config, Some(install_dir.path()));
        assert_eq!(check.status, CheckStatus::Warning);
        assert!(check.summary.contains("4 release generations"));
    }
}
