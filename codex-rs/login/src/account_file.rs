use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;

/// Serializes account metadata transactions across processes; the handle releases on drop.
pub(crate) fn lock(home: &Path) -> io::Result<File> {
    std::fs::create_dir_all(home)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.join(".account-pool.lock"))?;
    file.lock()?;
    Ok(file)
}
