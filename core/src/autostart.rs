//! Starting Rift with Windows and connecting at launch.
//!
//! The registration lives behind [`Autostart`] so the coordinator and its tests do not touch the
//! registry; the Windows implementation writes one value under the current user's `Run` key
//! (no administrator rights). Whether to connect at launch is a pure decision on the store:
//! [`auto_connect_plan`].
use crate::text::t;
use crate::{model::Store, Result};

/// Argument of the command that Windows runs at logon: the window stays in the tray.
pub const ARG: &str = "--autostart";

/// Registration of Rift in the list of programs that start with Windows.
pub trait Autostart: Send + Sync {
    /// An entry exists and starts this very `Rift.exe`.
    fn is_enabled(&self) -> Result<bool>;
    /// Creates (or rewrites) the entry, or removes it; removing a missing entry succeeds.
    fn set(&self, enabled: bool) -> Result<()>;
}

/// Where no registration is possible (tests, other platforms): always off.
pub struct NoAutostart;
impl Autostart for NoAutostart {
    fn is_enabled(&self) -> Result<bool> {
        Ok(false)
    }
    fn set(&self, _: bool) -> Result<()> {
        Ok(())
    }
}

/// What to do when Rift starts.
#[derive(Debug, PartialEq, Eq)]
pub enum AutoConnect {
    /// The setting is off.
    Off,
    /// Connect with the saved mode and the last selected server.
    Connect,
    /// Wanted, but impossible; the text is the journal entry.
    Skip(&'static str),
}

/// Connect at launch only when there is something to connect to: an enabled server and, with
/// Auto Select off, a selected one (Auto Select picks the server itself).
pub fn auto_connect_plan(store: &Store) -> AutoConnect {
    if !store.settings.auto_connect {
        return AutoConnect::Off;
    }
    let servers = store.available_servers();
    if servers.is_empty() {
        return AutoConnect::Skip(t("core.autostart.skipped_no_servers"));
    }
    let selected = store
        .selected
        .as_ref()
        .is_some_and(|id| servers.iter().any(|server| &server.id == id));
    if !store.settings.auto_select && !selected {
        return AutoConnect::Skip(t("core.autostart.skipped_no_selection"));
    }
    AutoConnect::Connect
}

#[cfg(windows)]
pub use registry::RegistryAutostart;

#[cfg(windows)]
mod registry {
    use super::{Autostart, ARG};
    use crate::text::t;
    use crate::{failure::FailureCode, Error, Result};
    use std::{io::ErrorKind, path::Path};
    use winreg::{enums::HKEY_CURRENT_USER, enums::KEY_READ, enums::KEY_WRITE, RegKey};

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE: &str = "Rift";

    /// The `Run` value of the current user: `"<path to Rift.exe>" --autostart`.
    pub struct RegistryAutostart {
        command: String,
    }
    impl RegistryAutostart {
        pub fn new(executable: &Path) -> Result<Self> {
            let path = executable.to_string_lossy();
            // A quote in the path would end the quoting of the command line.
            if path.contains('"') {
                return Err(failed());
            }
            Ok(Self {
                command: format!("\"{path}\" {ARG}"),
            })
        }
    }
    fn failed() -> Error {
        Error::failure(FailureCode::Autostart, t("core.autostart.registry_failed"))
    }
    impl Autostart for RegistryAutostart {
        fn is_enabled(&self) -> Result<bool> {
            let Ok(run) =
                RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_READ)
            else {
                return Ok(false);
            };
            match run.get_value::<String, _>(VALUE) {
                Ok(current) => Ok(current == self.command),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
                Err(_) => Err(failed()),
            }
        }
        fn set(&self, enabled: bool) -> Result<()> {
            let run = RegKey::predef(HKEY_CURRENT_USER)
                .open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_WRITE)
                .map_err(|_| failed())?;
            if enabled {
                return run.set_value(VALUE, &self.command).map_err(|_| failed());
            }
            match run.delete_value(VALUE) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                Err(_) => Err(failed()),
            }
        }
    }
}
