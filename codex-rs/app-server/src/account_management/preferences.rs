//! Nonsecret manager preferences shared by browser and terminal clients.

use super::AccountManager;
use serde::Deserialize;
use serde::Serialize;
use std::io::Read;
use std::io::Write;
use std::path::Path;

const FILE_NAME: &str = ".account-manager-ui.json";
const MAX_BYTES: u64 = 4096;

/// Explicit interface language; English is the first-run fallback.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ManagerLanguage {
    #[default]
    #[serde(rename = "en")]
    English,
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
}

/// Nonsecret choices shared by managers using the same Codex home.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagerPreferences {
    pub language: ManagerLanguage,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SavedPreferences {
    schema_version: u32,
    language: ManagerLanguage,
}

impl AccountManager {
    /// Unreadable preferences use English without preventing account administration.
    pub fn preferences(&self) -> ManagerPreferences {
        read(&self.config.codex_home)
    }

    /// Stores only UI choices; no authentication or execution configuration is changed.
    pub fn save_preferences(&self, preferences: ManagerPreferences) -> anyhow::Result<()> {
        write(&self.config.codex_home, preferences)
    }
}

fn read(home: &Path) -> ManagerPreferences {
    let saved = (|| {
        let file = std::fs::File::open(home.join(FILE_NAME)).ok()?;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 > MAX_BYTES {
            return None;
        }
        let saved: SavedPreferences = serde_json::from_slice(&bytes).ok()?;
        (saved.schema_version == 1).then_some(ManagerPreferences {
            language: saved.language,
        })
    })();
    saved.unwrap_or_default()
}

fn write(home: &Path, preferences: ManagerPreferences) -> anyhow::Result<()> {
    std::fs::create_dir_all(home)?;
    let mut file = tempfile::NamedTempFile::new_in(home)?;
    serde_json::to_writer(
        &mut file,
        &SavedPreferences {
            schema_version: 1,
            language: preferences.language,
        },
    )?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(home.join(FILE_NAME))?;
    Ok(())
}

#[cfg(test)]
#[path = "preferences_tests.rs"]
mod tests;
