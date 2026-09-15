use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;

const ACCOUNT_POOL_LOCK: &str = ".account-pool.lock";
const WINDOW_WARMUP_LOCK: &str = ".window-warmup.lock";
const AUTH_REFRESH_LOCK: &str = ".auth-refresh.lock";

/// Serializes account metadata transactions across processes; the handle releases on drop.
pub(crate) fn lock(home: &Path) -> io::Result<File> {
    lock_named(home, ACCOUNT_POOL_LOCK)
}

/// Serializes identity-preserving 5h-window warmup so two Codex processes do not
/// send the same standby `1+1?` request at once.
pub(crate) fn warmup_lock(home: &Path) -> io::Result<File> {
    lock_named(home, WINDOW_WARMUP_LOCK)
}

/// Serializes OAuth refresh + `auth.json` writes for one credential home.
pub(crate) fn refresh_lock(home: &Path) -> io::Result<File> {
    lock_named(home, AUTH_REFRESH_LOCK)
}

fn lock_named(home: &Path, name: &str) -> io::Result<File> {
    let file = open_lock_file(home, name)?;
    file.lock()?;
    Ok(file)
}

fn open_lock_file(home: &Path, name: &str) -> io::Result<File> {
    std::fs::create_dir_all(home)?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.join(name))
}
