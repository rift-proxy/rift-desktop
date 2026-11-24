//! The elevated process (`rift.exe --tun-helper`). It speaks over one channel to the
//! Rift that started it, runs sing-box for the stores that pass the same checks as an imported
//! profile, and ends when the channel closes, when told to or when sing-box ends. It writes
//! nothing to disk, takes no raw config and runs nothing but sing-box.
use super::protocol::{read_frame, write_frame, FromHelper, ToHelper, VERSION};
use crate::text::t;
use crate::{
    failure::FailureCode, model::Mode, model::Store, profile, runtime::Runtime, Error, Result,
};
use std::time::Duration;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
    time::interval,
};

/// How the helper ended; the process exit code follows from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// Rift asked for it.
    Stopped,
    /// Rift is gone or closed the channel.
    ChannelClosed,
    /// sing-box ended by itself.
    SingBoxExited,
    /// A frame that is not the protocol.
    Protocol,
}
impl Exit {
    pub const fn code(self) -> i32 {
        match self {
            Self::Stopped => 0,
            Self::ChannelClosed => 2,
            Self::SingBoxExited => 3,
            Self::Protocol => 4,
        }
    }
}
/// Whether the helper runs sing-box for this store: the checks of an imported profile (limits,
/// cross references, the `outbound` allowlist), a TUN mode (a Proxy run needs no rights) and a
/// secret of the shape Rift makes.
pub fn vet(store: &Store, secret: &str) -> Result<()> {
    profile::validate(store)?;
    if store.settings.mode == Mode::Proxy {
        return Err(Error::Invalid(t("core.elevation.helper_needs_tun").into()));
    }
    if secret.len() != 32
        || !secret
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid(t("core.elevation.bad_api_secret").into()));
    }
    Ok(())
}
/// Ends the reading task with whatever is serving, even when the serving is cancelled.
struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
enum Incoming {
    Message(ToHelper),
    Closed,
    Bad,
}
/// Serves one channel until it ends. `poll` is how often a run of sing-box is looked at.
pub async fn serve<S, R>(stream: S, token: &str, runtime: &mut R, poll: Duration) -> Exit
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    R: Runtime + ?Sized,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let hello = FromHelper::Hello {
        version: VERSION,
        token: token.to_owned(),
    };
    if write_frame(&mut writer, &hello).await.is_err() {
        return Exit::ChannelClosed;
    }
    // A task reads, so that a frame is never half read when the timer below fires.
    let (sender, mut incoming) = mpsc::channel(4);
    let _reading = AbortOnDrop(tokio::spawn(async move {
        loop {
            let next = match read_frame::<_, ToHelper>(&mut reader).await {
                Ok(Some(message)) => Incoming::Message(message),
                Ok(None) => Incoming::Closed,
                Err(_) => Incoming::Bad,
            };
            let last = !matches!(next, Incoming::Message(_));
            if sender.send(next).await.is_err() || last {
                break;
            }
        }
    }));
    let mut running = false;
    let mut timer = interval(poll);
    let mut last_diagnostics = None;
    let exit = loop {
        tokio::select! {
            next = incoming.recv() => match next {
                Some(Incoming::Message(ToHelper::Start { store, secret })) => {
                    let started = match vet(&store, &secret) {
                        Ok(()) => runtime.start(&store, &secret).await,
                        Err(error) => Err(error.with_code(FailureCode::SidecarConfigRejected)),
                    };
                    // Validation can reject a replacement without stopping the old child.
                    // Keep observing whichever child actually remains after the attempt.
                    running = started.is_ok() || runtime.alive().unwrap_or(running);
                    let reply = match started {
                        Ok(()) => {
                            let handoff=runtime.proxy_channel().and_then(|channel|
                                channel.read().ok().and_then(|value|value.clone()));
                            match handoff {
                                Some(handoff)=>FromHelper::StartedWithHandoff{port:handoff.port(),secret:handoff.secret().to_owned()},
                                None=>FromHelper::Started,
                            }
                        },
                        Err(error) => FromHelper::Failed {
                            code: error.failure_code().unwrap_or(FailureCode::SidecarStartExit),
                            startup: super::protocol::StartupFailure::from_error(&error),
                        },
                    };
                    let diagnostics = runtime.diagnostics();
                    last_diagnostics = Some(diagnostics.clone());
                    if diagnostics.generation != 0 && write_frame(&mut writer, &FromHelper::Diagnostics { diagnostics }).await.is_err() {
                        break Exit::ChannelClosed;
                    }
                    if write_frame(&mut writer, &reply).await.is_err() {
                        break Exit::ChannelClosed;
                    }
                }
                Some(Incoming::Message(ToHelper::Stop)) => {
                    let _ = runtime.stop().await;
                    let diagnostics = runtime.diagnostics();
                    if diagnostics.generation != 0 {
                        let _ = write_frame(&mut writer, &FromHelper::Diagnostics { diagnostics }).await;
                    }
                    let _ = write_frame(&mut writer, &FromHelper::Stopped).await;
                    break Exit::Stopped;
                }
                Some(Incoming::Bad) => break Exit::Protocol,
                Some(Incoming::Closed) | None => break Exit::ChannelClosed,
            },
            _ = timer.tick(), if running => {
                let ended = match runtime.alive() {
                    Ok(true) => None,
                    Ok(false) => Some(runtime.crash_code()),
                    Err(_) => Some(FailureCode::Unknown),
                };
                if let Some(code) = ended {
                    // Reap both owned children before sending the final facts; intentional
                    // sibling termination cannot replace the already selected crash code.
                    let _ = runtime.stop().await;
                    let diagnostics = runtime.diagnostics();
                    if diagnostics.generation != 0 {
                        let _ = write_frame(&mut writer, &FromHelper::Diagnostics { diagnostics }).await;
                    }
                    let message=if code==FailureCode::SidecarCrashed {FromHelper::Exited}
                        else {FromHelper::ExitedWithCode{code}};
                    let _ = write_frame(&mut writer, &message).await;
                    break Exit::SingBoxExited;
                }
                let diagnostics = runtime.diagnostics();
                if diagnostics.generation != 0 && last_diagnostics.as_ref() != Some(&diagnostics) {
                    last_diagnostics = Some(diagnostics.clone());
                    if write_frame(&mut writer, &FromHelper::Diagnostics { diagnostics }).await.is_err() {
                        break Exit::ChannelClosed;
                    }
                }
            }
        }
    };
    // Whatever ended the helper, no sing-box is left running by it.
    let _ = runtime.stop().await;
    exit
}
