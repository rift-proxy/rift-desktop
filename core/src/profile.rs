use crate::text::t;
use crate::{
    failure::FailureCode,
    model::{display_name, Store, MAX_NAME_BYTES},
    outbound, persistence, subscription, Error, Result,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, path::Path};

pub const FORMAT: &str = "rift-profile";
pub const VERSION: u32 = 2;
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_SERVERS: usize = 50_000;
pub const MAX_SUBSCRIPTIONS: usize = 1_024;
/// Reported when even the compact form of a store does not fit `MAX_BYTES`.
pub fn too_large() -> &'static str {
    t("core.profile.too_large_to_export")
}
/// Subscription errors are short fixed texts; the bound keeps a hostile file from carrying a
/// multi-megabyte string into every state snapshot.
const MAX_ERROR_BYTES: usize = 1_024;
/// The application only generates UUIDs; the bound keeps a hostile file from carrying a
/// multi-megabyte id into every state snapshot.
const MAX_SUBSCRIPTION_ID_BYTES: usize = 128;

#[derive(Serialize, Deserialize)]
struct ProfileFile {
    format: String,
    version: u32,
    profile: Store,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Summary {
    pub version: u32,
    pub servers: usize,
    pub manual_servers: usize,
    pub subscriptions: usize,
    pub has_selected_server: bool,
}

pub fn summary(store: &Store) -> Summary {
    Summary {
        version: VERSION,
        servers: store.servers.len(),
        manual_servers: store
            .servers
            .iter()
            .filter(|server| server.source.is_none())
            .count(),
        subscriptions: store.subscriptions.len(),
        has_selected_server: store.selected.is_some(),
    }
}

/// Ingestion leaves `country` empty and the GeoIP lookup stores an upper-case ISO code.
fn country_valid(country: &str) -> bool {
    country.is_empty() || (country.len() == 2 && country.bytes().all(|b| b.is_ascii_uppercase()))
}

pub fn validate(store: &Store) -> Result<()> {
    persistence::validate(store)?;
    if store.servers.len() > MAX_SERVERS || store.subscriptions.len() > MAX_SUBSCRIPTIONS {
        return Err(Error::Invalid(t("core.profile.too_many_entries").into()));
    }

    let mut subscription_ids = HashSet::new();
    for source in &store.subscriptions {
        if source.id.is_empty()
            || source.id.len() > MAX_SUBSCRIPTION_ID_BYTES
            || !subscription_ids.insert(source.id.as_str())
            || source.name.trim().is_empty()
            || source.name.len() > 120
            || source
                .error
                .as_ref()
                .is_some_and(|error| error.len() > MAX_ERROR_BYTES)
        {
            return Err(Error::Invalid(
                t("core.profile.bad_subscription_data").into(),
            ));
        }
        subscription::validate_url(&source.url)?;
    }

    let mut server_rows = HashSet::new();
    for server in &store.servers {
        if server.id.is_empty()
            || server.name.trim().is_empty()
            || server.name.len() > MAX_NAME_BYTES
            || server.id != crate::model::Server::fingerprint(&server.outbound)
            || !server_rows.insert((server.source.as_deref(), server.id.as_str()))
            || server
                .source
                .as_ref()
                .is_some_and(|id| !subscription_ids.contains(id.as_str()))
            || !country_valid(&server.country)
        {
            return Err(Error::Invalid(t("core.common.bad_server_data").into()));
        }
        // The outbound is a file author's input: only what `parser` could build is accepted,
        // and the fields that drive probes, DNS and the UI must be the ones it implies.
        let endpoint = outbound::inspect(&server.outbound)?;
        if server.protocol != endpoint.protocol
            || server.host != endpoint.host
            || server.port != endpoint.port
        {
            return Err(Error::Invalid(t("core.common.bad_server_data").into()));
        }
    }

    if store.selected.as_ref().is_some_and(|selected| {
        !store
            .available_servers()
            .iter()
            .any(|server| &server.id == selected)
    }) {
        return Err(Error::Invalid(
            t("core.profile.selected_server_missing").into(),
        ));
    }
    Ok(())
}

/// Export must produce what `decode` accepts, for every store the application itself holds.
/// Display names are normalized exactly like ingestion does (blank or over-long remarks from
/// stores saved by earlier versions), and the file is written compact when the pretty form
/// would pass `MAX_BYTES`; only a store too large even then is refused, with its own message.
pub fn encode(store: &Store) -> Result<Vec<u8>> {
    let mut profile = store.clone();
    profile.version = VERSION;
    profile.prune_excluded();
    for server in &mut profile.servers {
        server.name = display_name(&server.name, &server.host);
    }
    validate(&profile)?;
    let file = ProfileFile {
        format: FORMAT.into(),
        version: VERSION,
        profile,
    };
    let pretty = serde_json::to_vec_pretty(&file)?;
    if pretty.len() <= MAX_BYTES {
        decode(&pretty)?;
        return Ok(pretty);
    }
    let compact = serde_json::to_vec(&file)?;
    if compact.len() <= MAX_BYTES {
        decode(&compact)?;
        return Ok(compact);
    }
    Err(Error::Invalid(too_large().into()))
}

pub fn decode(bytes: &[u8]) -> Result<Store> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::Invalid(t("core.profile.file_too_large").into()));
    }
    let file: ProfileFile = serde_json::from_slice(bytes)?;
    if file.format != FORMAT || ![1, VERSION].contains(&file.version) {
        return Err(Error::Invalid(t("core.profile.unsupported_version").into()));
    }
    let mut profile = file.profile;
    // A profile of an earlier version may hold settings this one has narrowed, as a state file may.
    profile.settings.bring_into_range();
    validate(&profile)?;
    profile.version = VERSION;
    profile.prune_excluded();
    Ok(profile)
}

pub fn read(path: &Path) -> Result<Store> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > MAX_BYTES as u64 {
        return Err(Error::Invalid(t("core.profile.file_too_large").into()));
    }
    decode(&std::fs::read(path)?)
}

/// The data directory `sing-box check` works from cannot be used: the machine, not the profile
/// file, is why the check did not run. Typed `Persistence`, like a failed read, so the text is
/// the only thing that tells the two stages apart.
pub fn check_unavailable() -> &'static str {
    t("core.profile.data_folder_unavailable")
}

/// Shown when applying a checked profile fails; the failure's typed code goes to the journal.
pub fn import_failed_text() -> &'static str {
    t("core.profile.apply_failed")
}

/// Localized, safe text for a failed `export_profile`/`write_profile`. Core messages never
/// carry paths, file content or credentials, so a limit message may be shown as is.
pub fn export_error_text(error: &Error) -> String {
    match error {
        Error::Invalid(message) => message.clone(),
        Error::Failure {
            code: FailureCode::Persistence,
            ..
        } => t("core.profile.save_failed").into(),
        _ => t("core.profile.prepare_failed").into(),
    }
}

/// Localized, safe text for a failed `inspect_profile`. A profile whose content is wrong and
/// an environment that cannot check it (a core's timeout or start failure) are told apart,
/// per core, so the file is not blamed for a slow machine.
pub fn inspect_error_text(error: &Error) -> String {
    match error {
        Error::Invalid(message) => message.clone(),
        Error::Json(_) => t("core.profile.bad_format").into(),
        Error::Failure {
            code: FailureCode::Persistence,
            message,
            ..
        } if message == check_unavailable() => check_unavailable().into(),
        Error::Io(_)
        | Error::Failure {
            code: FailureCode::Persistence,
            ..
        } => t("core.profile.read_failed").into(),
        Error::Failure {
            code: FailureCode::XrayConfigRejected,
            ..
        } => t("core.profile.unsupported_xray_config").into(),
        Error::Failure {
            code: FailureCode::XrayConfigTimeout,
            ..
        } => t("core.profile.xray_check_timeout").into(),
        Error::Failure {
            code: FailureCode::XrayStartExit,
            ..
        } => t("core.profile.xray_check_start_failed").into(),
        Error::Failure {
            code: FailureCode::SidecarConfigRejected,
            ..
        } => t("core.profile.unsupported_config").into(),
        Error::Failure {
            code: FailureCode::SidecarConfigTimeout,
            ..
        } => t("core.profile.check_timeout").into(),
        Error::Failure {
            code: FailureCode::SidecarStartExit,
            ..
        } => t("core.profile.singbox_check_start_failed").into(),
        _ => t("core.profile.check_failed").into(),
    }
}
