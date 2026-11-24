//! The Windows end of the helper: the named pipe, whose server is Rift and whose client
//! is the helper, the check for administrator rights and the process of the helper itself. None
//! of it has been run on Windows by the tests of this repository, which replace the prompt and
//! the channel with fakes.
use super::{
    client::{BoxStream, Listener, Transport},
    helper::serve,
    uac::{parse_helper_args, pipe_name_valid},
};
use crate::text::t;
use crate::{failure::FailureCode, runtime::Sidecar, Error, Result};
use async_trait::async_trait;
use std::time::Duration;
use tokio::{
    net::windows::named_pipe::{ClientOptions, PipeMode, ServerOptions},
    time::{sleep, Instant},
};

/// Whether this Rift has administrator rights itself (then no helper is needed).
pub fn is_elevated() -> bool {
    crate::windows::require_elevation().is_ok()
}

/// Exit codes of the helper process besides those of `Exit`.
const EXIT_ARGUMENTS: i32 = 5;
const EXIT_NO_CHANNEL: i32 = 6;

fn full_name(name: &str) -> String {
    format!(r"\\.\pipe\{name}")
}
/// The channel: one instance, first of its name, local clients only, a name nobody can guess.
pub struct PipeTransport;
impl Transport for PipeTransport {
    fn listen(&self) -> Result<Box<dyn Listener>> {
        let name = format!("rift-helper-{}", uuid::Uuid::new_v4().simple());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .max_instances(1)
            .pipe_mode(PipeMode::Byte)
            .create(full_name(&name))
            .map_err(|_| {
                Error::failure(
                    FailureCode::ElevationFailed,
                    t("core.elevation.channel_failed"),
                )
            })?;
        Ok(Box::new(PipeListener {
            name,
            server: Some(server),
        }))
    }
}
struct PipeListener {
    name: String,
    server: Option<tokio::net::windows::named_pipe::NamedPipeServer>,
}
#[async_trait]
impl Listener for PipeListener {
    fn name(&self) -> String {
        self.name.clone()
    }
    async fn accept(&mut self) -> std::io::Result<BoxStream> {
        let server = self
            .server
            .take()
            .ok_or_else(|| std::io::Error::other("the channel has taken its client"))?;
        server.connect().await?;
        Ok(Box::new(server))
    }
}

/// The helper side: connects to the channel Rift made (it exists before the helper starts, but a
/// busy instance or a moment's delay is waited out) and serves it.
async fn connect(name: &str) -> Option<BoxStream> {
    const ERROR_PIPE_BUSY: i32 = 231;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match ClientOptions::new().open(full_name(name)) {
            Ok(client) => return Some(Box::new(client)),
            Err(error)
                if (error.raw_os_error() == Some(ERROR_PIPE_BUSY)
                    || error.kind() == std::io::ErrorKind::NotFound)
                    && Instant::now() < deadline =>
            {
                sleep(Duration::from_millis(50)).await;
            }
            Err(_) => return None,
        }
    }
}
/// `rift.exe --tun-helper <channel> <token>`: the whole life of the elevated process. Returns its
/// exit code.
pub fn run_helper(args: &[String]) -> i32 {
    let Some(parsed) = parse_helper_args(args).filter(|parsed| pipe_name_valid(&parsed.pipe))
    else {
        return EXIT_ARGUMENTS;
    };
    // Only locates the bundled sing-box and the data folder next to this very executable.
    let executable = std::env::current_exe(); // nosemgrep: rust.lang.security.current-exe.current-exe
    let Ok(executable) = executable else {
        return EXIT_ARGUMENTS;
    };
    let Some(folder) = executable.parent() else {
        return EXIT_ARGUMENTS;
    };
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return EXIT_ARGUMENTS;
    };
    runtime.block_on(async {
        let Some(stream) = connect(&parsed.pipe).await else {
            return EXIT_NO_CHANNEL;
        };
        let mut sidecar = Sidecar::new(folder.join("sing-box.exe"), folder.join("data"))
            .with_xray_binary(folder.join("xray.exe"));
        serve(
            stream,
            &parsed.token,
            &mut sidecar,
            Duration::from_millis(500),
        )
        .await
        .code()
    })
}
