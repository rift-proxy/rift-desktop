//! Install-local persistence. Legacy directories are read only, never deleted.
use crate::{model::Store, persistence, Result};
use std::{io::Write, path::Path};

/// Fail closed before using any application storage (including updater temporaries).
pub fn prepare(directory: &Path) -> Result<()> {
    let tmp = directory.join("tmp");
    std::fs::create_dir_all(&tmp)?;
    let mut probe = tempfile::NamedTempFile::new_in(directory)?;
    probe.write_all(b"Rift storage check")?;
    probe.as_file().sync_all()?;
    // Also validate tmp writability since updater staging uses it.
    let mut tmp_probe = tempfile::NamedTempFile::new_in(&tmp)?;
    tmp_probe.write_all(b"Rift tmp check")?;
    tmp_probe.as_file().sync_all()?;
    Ok(())
}

/// The destination is authoritative, including when damaged. Publish state last so
/// interrupted migration can be retried; never move/delete the user's originals.
pub fn load_or_migrate(directory: &Path, legacy: &Path) -> Result<Store> {
    let destination = directory.join("state.json");
    if destination.try_exists()? {
        return persistence::load(&destination);
    }
    let source = legacy.join("state.json");
    let store = if source.try_exists()? {
        persistence::load(&source)?
    } else {
        Store::default()
    };
    // Migrate companions even when legacy state.json doesn't exist.
    for name in ["journal.json", "diagnostics.json"] {
        let target = directory.join(name);
        if target.try_exists()? {
            // A partial previous migration may have published this file already.
            // Require it to be readable, not a directory masquerading as a file.
            std::fs::read(&target)?;
            continue;
        }
        match std::fs::read(legacy.join(name)) {
            Ok(bytes) => persistence::atomic_write(&target, &bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    persistence::save(&destination, &store)?;
    Ok(store)
}
