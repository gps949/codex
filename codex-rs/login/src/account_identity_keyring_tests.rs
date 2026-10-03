use std::any::Any;
use std::sync::Arc;

use codex_keyring_store::tests::MockKeyringStore;
use keyring::credential::Credential;
use keyring::credential::CredentialApi;
use keyring::credential::CredentialBuilderApi;
use keyring::mock::MockCredential;
use pretty_assertions::assert_eq;
use sha2::Digest;
use sha2::Sha256;
use tempfile::TempDir;

use super::*;
use crate::AccountProfileRecord;
use crate::auth::save_auth;

struct PersistentMockBuilder(MockKeyringStore);

impl CredentialBuilderApi for PersistentMockBuilder {
    fn build(
        &self,
        _target: Option<&str>,
        _service: &str,
        user: &str,
    ) -> keyring::Result<Box<Credential>> {
        Ok(Box::new(PersistentMockCredential(self.0.credential(user))))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct PersistentMockCredential(Arc<MockCredential>);

impl CredentialApi for PersistentMockCredential {
    fn set_secret(&self, secret: &[u8]) -> keyring::Result<()> {
        self.0.set_secret(secret)
    }

    fn get_secret(&self) -> keyring::Result<Vec<u8>> {
        self.0.get_secret()
    }

    fn delete_credential(&self) -> keyring::Result<()> {
        self.0.delete_credential()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct ResetKeyringBuilder;

impl Drop for ResetKeyringBuilder {
    fn drop(&mut self) {
        keyring::set_default_credential_builder(keyring::default::default_credential_builder());
    }
}

fn store_key(home: &Path) -> String {
    let canonical = home.canonicalize().unwrap();
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    let hex = format!("{digest:x}");
    format!("cli|{}", &hex[..16])
}

#[test]
#[serial_test::serial(account_login_keyring)]
fn duplicate_login_removes_direct_keyring_and_fallback_credentials_without_revoking() {
    let mock = MockKeyringStore::default();
    keyring::set_default_credential_builder(Box::new(PersistentMockBuilder(mock.clone())));
    let _reset = ResetKeyringBuilder;
    let backend = AuthKeyringBackendKind::Direct;
    for mode in [
        AuthCredentialsStoreMode::Keyring,
        AuthCredentialsStoreMode::Auto,
    ] {
        let home = TempDir::new().unwrap();
        let store = AccountProfileStore::new(home.path().to_path_buf());
        let canonical = store
            .ensure_legacy_root_profile(/*label*/ None, /*priority*/ 0)
            .unwrap();
        let pending = store
            .allocate_profile(/*label*/ None, /*priority*/ 10)
            .unwrap();
        let auth =
            super::tests::chatgpt_auth_with_ids("same-user", "workspace", "fixture@example.com");
        save_auth(&canonical.credential_home, &auth, mode, backend).unwrap();
        save_auth(&pending.credential_home, &auth, mode, backend).unwrap();
        // A stale file and ephemeral overlay must disappear along with the keyring entry.
        save_auth(
            &pending.credential_home,
            &auth,
            AuthCredentialsStoreMode::File,
            backend,
        )
        .unwrap();
        save_auth(
            &pending.credential_home,
            &auth,
            AuthCredentialsStoreMode::Ephemeral,
            backend,
        )
        .unwrap();
        let pending_key = store_key(&pending.credential_home);
        let canonical_key = store_key(&canonical.credential_home);
        assert!(mock.saved_value(&pending_key).is_some());

        assert_eq!(
            reconcile_duplicate_new_login(&store, &pending, mode, backend).unwrap(),
            Some(canonical.clone())
        );
        assert_eq!(mock.saved_value(&pending_key), None);
        assert_eq!(
            mock.saved_value(&canonical_key)
                .map(|value| serde_json::from_str::<AuthDotJson>(&value).unwrap()),
            Some(auth)
        );
        assert!(!pending.credential_home.exists());
        assert_eq!(
            store.load_profile_records().unwrap(),
            vec![AccountProfileRecord {
                profile: canonical,
                state: AccountProfileState::Ready
            }]
        );
    }
}

#[test]
#[serial_test::serial(account_login_keyring)]
fn failed_keyring_cleanup_keeps_pending_profile_for_retry() {
    let mock = MockKeyringStore::default();
    keyring::set_default_credential_builder(Box::new(PersistentMockBuilder(mock.clone())));
    let _reset = ResetKeyringBuilder;
    let home = TempDir::new().unwrap();
    let store = AccountProfileStore::new(home.path().to_path_buf());
    let pending = store
        .allocate_profile(/*label*/ None, /*priority*/ 10)
        .unwrap();
    let auth = super::tests::chatgpt_auth_with_ids("same-user", "workspace", "fixture@example.com");
    let mode = AuthCredentialsStoreMode::Keyring;
    let backend = AuthKeyringBackendKind::Direct;
    save_auth(&pending.credential_home, &auth, mode, backend).unwrap();
    let key = store_key(&pending.credential_home);
    mock.set_error(
        &key,
        keyring::Error::Invalid("fixture".into(), "delete failed".into()),
    );

    assert!(abandon_pending_login(&store, &pending.id, mode, backend).is_err());
    assert_eq!(
        store.load_profile_records().unwrap(),
        vec![AccountProfileRecord {
            profile: pending.clone(),
            state: AccountProfileState::PendingLogin
        }]
    );
    assert!(pending.credential_home.exists());
    assert!(mock.saved_value(&key).is_some());

    assert!(abandon_pending_login(&store, &pending.id, mode, backend).unwrap());
    assert_eq!(mock.saved_value(&key), None);
    assert_eq!(store.load_profile_records().unwrap(), Vec::new());
    assert!(!pending.credential_home.exists());
}
