use crate::text::t;
use crate::{model::Store, Error, Result};
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path};
pub fn load(path: &Path) -> Result<Store> {
    // Security boundary: the desktop passes app_data_dir/state.json; no IPC command or
    // subscription field supplies this path. Semgrep Pro's generic Actix path-traversal
    // rule treats every public Path parameter as tainted, but its trace has no web source.
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Store::default()),
        Err(e) => return Err(e.into()),
    };
    decode(&bytes)
}
/// Parses and validates the persisted state; shared by `load` and the `persisted_state` fuzz target.
pub fn decode(bytes: &[u8]) -> Result<Store> {
    let mut store: Store = serde_json::from_slice(bytes)?;
    // Earlier versions allowed settings this one does not: a file is read, not refused for them.
    store.settings.bring_into_range();
    validate(&store)?;
    store.version = 2;
    // A key that no server stands behind is a server that left while the file was not looking.
    store.prune_excluded();
    Ok(store)
}
pub fn validate(store: &Store) -> Result<()> {
    if ![1, 2].contains(&store.version) {
        return Err(Error::Invalid(
            t("core.persistence.unsupported_data_version").into(),
        ));
    }
    store.settings.validate()?;
    if store.excluded.len() > crate::model::MAX_EXCLUDED
        || store
            .excluded
            .iter()
            .any(|key| key.len() > crate::model::MAX_EXCLUDED_KEY_BYTES)
    {
        return Err(Error::Invalid(
            t("core.persistence.bad_excluded_list").into(),
        ));
    }
    // The outbound is a free-form Value, but every consumer (config::generate) treats it as a
    // sing-box outbound object: a string, number, bool, array or null here is damage, not data.
    if store.servers.iter().any(|s| !s.outbound.is_object()) {
        return Err(Error::Invalid(t("core.common.bad_server_data").into()));
    }
    for source in &store.subscriptions {
        source.validate_options()?;
    }
    store.routing.validate()?;
    store.validate_routes()?;
    Ok(())
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid(t("core.persistence.bad_path").into()))?;
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| Error::Io(e.error))?;
    Ok(())
}
pub fn save(path: &Path, store: &Store) -> Result<()> {
    validate(store)?;
    let mut migrated = store.clone();
    migrated.version = 2;
    let bytes = serde_json::to_vec_pretty(&migrated)?;
    // Never replace working state with bytes our reader cannot accept.
    decode(&bytes)?;
    backup_v1(path)?;
    atomic_write(path, &bytes)
}

/// Keep the exact original bytes before the first replacement of a v1 file. A failed backup
/// aborts saving. The generated name never replaces another backup or leaves the data directory.
fn backup_v1(path: &Path) -> Result<()> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| v["version"].as_u64())
        != Some(1)
    {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid(t("core.persistence.bad_path").into()))?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::Invalid(t("core.persistence.bad_path").into()))?;
    let mut backup_name = name.to_os_string();
    backup_name.push(format!(".v1-{:x}.bak", Sha256::digest(&bytes)));
    let backup = parent.join(backup_name);
    if backup.exists() {
        return check_backup(&backup, &bytes);
    }
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(&bytes)?;
    temp.as_file().sync_all()?;
    if let Err(error) = temp.persist_noclobber(&backup) {
        if error.error.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(Error::Io(error.error));
        }
        check_backup(&backup, &bytes)?;
    }
    Ok(())
}

fn check_backup(path: &Path, bytes: &[u8]) -> Result<()> {
    if std::fs::read(path)? != bytes {
        return Err(Error::Invalid(
            t("core.persistence.backup_corrupted").into(),
        ));
    }
    Ok(())
}
