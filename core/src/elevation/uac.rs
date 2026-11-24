//! How Rift asks Windows for administrator rights, as much of it as needs no Windows: the command
//! line of the helper, what the answer of the shell means and what the helper accepts on its
//! command line. The call itself is in `windows.rs`.
use super::client::{Elevator, Launched};
use async_trait::async_trait;
use std::path::PathBuf;

/// `rift.exe` started with this as its first argument is the helper, not the application.
pub const HELPER_ARG: &str = "--tun-helper";
/// Win32 `ERROR_CANCELLED`: what `ShellExecuteExW` reports when the user answered the Windows
/// prompt with "No". It does not depend on the language of Windows.
const ERROR_CANCELLED: i32 = 1223;

/// What the shell made of the request: the OS error number of a failed call, if it had one.
pub fn launched_from_shell(result: Result<(), Option<i32>>) -> Launched {
    match result {
        Ok(()) => Launched::Started,
        Err(Some(ERROR_CANCELLED)) => Launched::Declined,
        Err(_) => Launched::Failed,
    }
}
/// The arguments the shell passes to the helper: the mark, the channel and the token, separated by
/// single spaces. Both values are checked here, so nothing that needs quoting (or is not Rift's
/// own) ever reaches the command line of a process with administrator rights.
pub fn helper_arguments(pipe: &str, token: &str) -> Option<String> {
    (pipe_name_valid(pipe) && token_valid(token)).then(|| format!("{HELPER_ARG} {pipe} {token}"))
}
/// The name of the channel the helper may connect to: made by Rift, local, 128 random bits. A
/// name with a separator, a dot or another host would point somewhere else.
pub fn pipe_name_valid(name: &str) -> bool {
    name.strip_prefix("rift-helper-").is_some_and(lower_hex_32)
}
pub fn token_valid(token: &str) -> bool {
    lower_hex_32(token)
}
fn lower_hex_32(text: &str) -> bool {
    text.len() == 32
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
/// What follows `--tun-helper` on the command line: the channel and the token, nothing else.
#[derive(Debug, PartialEq, Eq)]
pub struct HelperArgs {
    pub pipe: String,
    pub token: String,
}
pub fn parse_helper_args(args: &[String]) -> Option<HelperArgs> {
    match args {
        [pipe, token] if pipe_name_valid(pipe) && token_valid(token) => Some(HelperArgs {
            pipe: pipe.clone(),
            token: token.clone(),
        }),
        _ => None,
    }
}

/// Asks Windows (the shell, verb `runas`) to start `rift.exe --tun-helper` with administrator
/// rights: the same prompt as "Run as administrator" in Explorer.
pub struct ShellElevator {
    /// This very `rift.exe`: the helper is the same program.
    executable: PathBuf,
}
impl ShellElevator {
    pub fn new(executable: PathBuf) -> Self {
        Self { executable }
    }
}
#[async_trait]
impl Elevator for ShellElevator {
    async fn launch(&self, pipe: &str, token: &str) -> Launched {
        #[cfg(windows)]
        {
            let Some(arguments) = helper_arguments(pipe, token) else {
                return Launched::Failed;
            };
            let executable = self.executable.clone();
            let (sender, receiver) = tokio::sync::oneshot::channel();
            // The call waits for the user. A thread of its own, not one of the pool: it may stay
            // there after the wait for the answer was given up (the helper then finds no
            // channel and ends), and it must never hold up the end of the runtime.
            let spawned = std::thread::Builder::new()
                .name("rift-uac".into())
                .spawn(move || {
                    let result = crate::windows::shell_execute(
                        "runas",
                        &executable,
                        &arguments,
                        executable.parent(),
                    )
                    .map_err(|error| error.raw_os_error());
                    let _ = sender.send(launched_from_shell(result));
                });
            if spawned.is_err() {
                return Launched::Failed;
            }
            receiver.await.unwrap_or(Launched::Failed)
        }
        #[cfg(not(windows))]
        {
            let _ = (&self.executable, pipe, token);
            Launched::Failed
        }
    }
}
