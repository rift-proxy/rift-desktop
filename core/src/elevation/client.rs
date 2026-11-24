//! Rift's side of the elevated helper: asks Windows for consent through an
//! [`Elevator`], listens on a one-instance channel it made first, greets the helper by token and
//! then drives sing-box through it. Everything that touches Windows sits behind [`Elevator`] and
//! [`Transport`], so the sequence is the same, and tested, on any system.
use super::protocol::{read_frame, write_frame, FromHelper, ToHelper, VERSION};
use crate::text::t;
use crate::{failure::FailureCode, model::Store, runtime::Runtime, Error, Result};
use async_trait::async_trait;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite, WriteHalf},
    sync::mpsc,
    task::JoinHandle,
    time::timeout,
};

/// What came of asking for the helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launched {
    /// The helper process was started (it connects next).
    Started,
    /// The user refused the Windows prompt.
    Declined,
    /// It could not be started, for any other reason.
    Failed,
}
/// Starts `rift.exe --tun-helper <pipe> <token>` with administrator rights and so asks Windows for
/// consent. It resolves when the process is started or the request is over; it does not wait for
/// the helper to end.
#[async_trait]
pub trait Elevator: Send + Sync {
    async fn launch(&self, pipe: &str, token: &str) -> Launched;
}
/// A duplex channel to the helper.
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
pub type BoxStream = Box<dyn Stream>;
/// The one instance of a channel Rift made, with a name nobody can guess.
#[async_trait]
pub trait Listener: Send {
    fn name(&self) -> String;
    /// The next client; the first one is taken for the helper.
    async fn accept(&mut self) -> std::io::Result<BoxStream>;
}
pub trait Transport: Send + Sync {
    /// Makes the channel before the helper is started, so that its name cannot be taken first.
    fn listen(&self) -> Result<Box<dyn Listener>>;
}

/// How long each step may take before it is a failure.
#[derive(Clone, Copy, Debug)]
pub struct HelperTimeouts {
    /// The prompt of Windows may wait for the user this long.
    pub consent: Duration,
    /// After the helper started, it must connect within this time.
    pub connect: Duration,
    /// And greet within this time.
    pub hello: Duration,
    /// Answer to `Start`: sing-box checked, started and answering.
    pub start: Duration,
    /// Answer to `Stop`.
    pub stop: Duration,
}
impl Default for HelperTimeouts {
    fn default() -> Self {
        Self {
            consent: Duration::from_secs(180),
            connect: Duration::from_secs(20),
            hello: Duration::from_secs(10),
            start: Duration::from_secs(60),
            stop: Duration::from_secs(10),
        }
    }
}

enum Event {
    Message(FromHelper),
    Closed,
    Invalid,
}
struct Session {
    writer: WriteHalf<BoxStream>,
    events: mpsc::Receiver<Event>,
    reading: JoinHandle<()>,
    running: bool,
}
impl Drop for Session {
    fn drop(&mut self) {
        // Dropping the writer closes the channel: the helper sees it and ends.
        self.reading.abort();
    }
}

pub struct HelperRuntime {
    elevator: Arc<dyn Elevator>,
    transport: Arc<dyn Transport>,
    timeouts: HelperTimeouts,
    session: Option<Session>,
    /// Why the last session ended on its own.
    lost: Option<FailureCode>,
    channel: crate::proxy_channel::ProxyChannel,
    diagnostics: crate::runtime::RuntimeDiagnostics,
    /// Last emitted count and time per warning category, not the latest observed growth.
    warning_notes:
        std::sync::Mutex<std::collections::HashMap<crate::xray_log::WarningCategory, (u64, u64)>>,
}
fn failure(code: FailureCode, message: &str) -> Error {
    Error::failure(code, message)
}
/// The safe words for a code the helper reports.
fn failed_with(code: FailureCode) -> Error {
    let message = match code {
        FailureCode::SidecarConfigRejected => t("core.common.singbox_rejected_config"),
        FailureCode::SidecarConfigTimeout => t("core.common.config_check_timeout"),
        FailureCode::SidecarReadinessTimeout => t("core.common.singbox_api_silent"),
        FailureCode::XrayConfigRejected => t("core.elevation.xray_rejected_config"),
        FailureCode::XrayConfigTimeout => t("core.common.xray_config_check_timeout"),
        FailureCode::XrayStartExit => t("core.common.xray_exited_at_start"),
        FailureCode::XrayReadinessTimeout => t("core.elevation.xray_channel_late"),
        FailureCode::XrayCrashed => t("core.common.xray_exited_unexpectedly"),
        FailureCode::Unknown => t("core.common.cores_state_check_failed"),
        FailureCode::ElevationChannel => t("core.elevation.helper_channel_unavailable"),
        FailureCode::LocalPortInUse => t("core.elevation.port_busy"),
        FailureCode::LocalPortDenied => t("core.elevation.port_forbidden"),
        _ => t("core.common.singbox_exited_unknown"),
    };
    Error::failure(code, message)
}
fn exit_code(code: FailureCode) -> FailureCode {
    if matches!(
        code,
        FailureCode::XrayCrashed | FailureCode::SidecarCrashed | FailureCode::Unknown
    ) {
        code
    } else {
        FailureCode::ElevationChannel
    }
}
impl HelperRuntime {
    pub fn new(elevator: Arc<dyn Elevator>, transport: Arc<dyn Transport>) -> Self {
        Self {
            elevator,
            transport,
            timeouts: HelperTimeouts::default(),
            session: None,
            lost: None,
            channel: Arc::new(std::sync::RwLock::new(None)),
            diagnostics: crate::runtime::RuntimeDiagnostics::default(),
            warning_notes: std::sync::Mutex::default(),
        }
    }
    #[must_use]
    pub fn with_timeouts(mut self, timeouts: HelperTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }
    fn observe(&mut self, diagnostics: crate::runtime::RuntimeDiagnostics) -> bool {
        if !diagnostics.valid() {
            return false;
        }
        if diagnostics.generation != self.diagnostics.generation {
            self.warning_notes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
        self.diagnostics = diagnostics;
        true
    }
    /// A helper is connected (and has not ended).
    pub fn has_session(&mut self) -> bool {
        self.drain();
        self.session.is_some()
    }
    /// Takes in what the helper has said since the last look.
    fn drain(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let mut ended = None;
        loop {
            match session.events.try_recv() {
                Ok(Event::Message(FromHelper::Exited)) => {
                    session.running = false;
                    ended.get_or_insert(FailureCode::SidecarCrashed);
                }
                Ok(Event::Message(FromHelper::ExitedWithCode { code })) => {
                    session.running = false;
                    ended.get_or_insert_with(|| exit_code(code));
                }
                Ok(Event::Message(FromHelper::Diagnostics { diagnostics })) => {
                    if !diagnostics.valid() {
                        ended.get_or_insert(FailureCode::ElevationChannel);
                        break;
                    }
                    if diagnostics.generation != self.diagnostics.generation {
                        self.warning_notes
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clear();
                    }
                    self.diagnostics = diagnostics;
                }
                Ok(Event::Message(_)) => {}
                Ok(Event::Invalid) => {
                    ended.get_or_insert(FailureCode::ElevationChannel);
                    break;
                }
                Ok(Event::Closed) | Err(mpsc::error::TryRecvError::Disconnected) => {
                    ended = ended.or(Some(FailureCode::ElevationHelperCrashed));
                    break;
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
            }
        }
        // A helper that said `Exited` ends right after; one whose channel closed without a word
        // crashed. Either way the session is over.
        if let Some(code) = ended {
            crate::proxy_channel::set(&self.channel, None);
            self.lost = Some(code);
            self.session = None;
        }
    }
    /// Gets a connected, greeted helper: the one already there, or a new one that the user has
    /// agreed to. Nothing else is touched, so a refusal leaves a running proxy as it was.
    pub async fn prepare(&mut self) -> Result<()> {
        if self.has_session() {
            return Ok(());
        }
        self.lost = None;
        let mut listener = self
            .transport
            .listen()
            .map_err(|error| error.with_code(FailureCode::ElevationFailed))?;
        let name = listener.name();
        let token = uuid::Uuid::new_v4().simple().to_string();
        // The helper may connect before the prompt's answer is back to us; both are awaited.
        let launch = self.elevator.launch(&name, &token);
        tokio::pin!(launch);
        let accept = listener.accept();
        tokio::pin!(accept);
        let mut stream = None;
        let consent = tokio::time::sleep(self.timeouts.consent);
        tokio::pin!(consent);
        let launched = loop {
            tokio::select! {
                result = &mut launch => break result,
                accepted = &mut accept, if stream.is_none() => stream = Some(accepted),
                _ = &mut consent => break Launched::Failed,
            }
        };
        match launched {
            Launched::Started => {}
            Launched::Declined => {
                return Err(failure(
                    FailureCode::ElevationDeclined,
                    t("core.elevation.uac_declined"),
                ))
            }
            Launched::Failed => {
                return Err(failure(
                    FailureCode::ElevationFailed,
                    t("core.elevation.helper_start_failed"),
                ))
            }
        }
        let stream = match stream {
            Some(accepted) => accepted,
            None => timeout(self.timeouts.connect, &mut accept)
                .await
                .map_err(|_| {
                    failure(
                        FailureCode::ElevationFailed,
                        t("core.elevation.helper_not_connected"),
                    )
                })?,
        }
        .map_err(|_| {
            failure(
                FailureCode::ElevationChannel,
                t("core.elevation.helper_link_lost"),
            )
        })?;
        let (mut reader, writer) = tokio::io::split(stream);
        let hello = timeout(
            self.timeouts.hello,
            read_frame::<_, FromHelper>(&mut reader),
        )
        .await
        .map_err(|_| {
            failure(
                FailureCode::ElevationChannel,
                t("core.elevation.helper_silent_hello"),
            )
        })??;
        match hello {
            Some(FromHelper::Hello {
                version,
                token: heard,
            }) if version == VERSION && heard == token => {}
            _ => {
                return Err(failure(
                    FailureCode::ElevationChannel,
                    t("core.elevation.helper_unexpected_answer"),
                ))
            }
        }
        let (sender, events) = mpsc::channel(16);
        let reading = tokio::spawn(async move {
            loop {
                let event = match read_frame::<_, FromHelper>(&mut reader).await {
                    Ok(Some(message)) => Event::Message(message),
                    Ok(None) => Event::Closed,
                    Err(_) => Event::Invalid,
                };
                let last = !matches!(event, Event::Message(_));
                if sender.send(event).await.is_err() || last {
                    break;
                }
            }
        });
        self.diagnostics = crate::runtime::RuntimeDiagnostics::default();
        self.warning_notes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.session = Some(Session {
            writer,
            events,
            reading,
            running: false,
        });
        Ok(())
    }
    async fn send(&mut self, message: &ToHelper) -> Result<()> {
        let session = self.session.as_mut().ok_or_else(|| {
            failure(
                FailureCode::ElevationChannel,
                t("core.elevation.helper_not_attached"),
            )
        })?;
        write_frame(&mut session.writer, message)
            .await
            .map_err(|error| error.with_code(FailureCode::ElevationChannel))
    }
    /// The next thing the helper says, within `limit`; `None` once it is gone.
    async fn hear(&mut self, limit: Duration) -> Option<FromHelper> {
        let session = self.session.as_mut()?;
        match timeout(limit, session.events.recv()).await {
            Ok(Some(Event::Message(message))) => Some(message),
            Ok(Some(Event::Invalid)) => {
                self.lost.get_or_insert(FailureCode::ElevationChannel);
                self.session = None;
                Some(FromHelper::Failed {
                    code: FailureCode::ElevationChannel,
                    startup: None,
                })
            }
            Ok(Some(Event::Closed) | None) => {
                self.lost.get_or_insert(FailureCode::ElevationHelperCrashed);
                self.session = None;
                None
            }
            Err(_) => Some(FromHelper::Failed {
                code: FailureCode::ElevationChannel,
                startup: None,
            }),
        }
    }
}
#[async_trait]
impl Runtime for HelperRuntime {
    /// The helper builds and checks its own config: Rift's check is done by the plain sidecar.
    async fn validate(&mut self, _: &Store, _: &str) -> Result<()> {
        Ok(())
    }
    async fn start(&mut self, store: &Store, secret: &str) -> Result<()> {
        self.prepare().await?;
        let previous = self.channel.read().ok().and_then(|value| value.clone());
        crate::proxy_channel::set(&self.channel, None);
        self.send(&ToHelper::Start {
            store: Box::new(store.clone()),
            secret: secret.to_owned(),
        })
        .await?;
        loop {
            match self.hear(self.timeouts.start).await {
                Some(FromHelper::Diagnostics { diagnostics }) => {
                    if !self.observe(diagnostics) {
                        self.session = None;
                        return Err(failed_with(FailureCode::ElevationChannel));
                    }
                }
                Some(FromHelper::Started) => {
                    crate::proxy_channel::set(&self.channel, None);
                    if let Some(session) = self.session.as_mut() {
                        session.running = true;
                    }
                    return Ok(());
                }
                Some(FromHelper::StartedWithHandoff { port, secret }) => {
                    let handoff = match crate::proxy_channel::ProxyHandoff::new(port, secret, store)
                    {
                        Ok(handoff) => handoff,
                        Err(error) => {
                            self.session = None;
                            crate::proxy_channel::set(&self.channel, None);
                            return Err(error);
                        }
                    };
                    crate::proxy_channel::set(&self.channel, Some(handoff));
                    if let Some(session) = self.session.as_mut() {
                        session.running = true;
                    }
                    return Ok(());
                }
                Some(FromHelper::Failed {
                    code: FailureCode::ElevationChannel,
                    ..
                }) => {
                    self.session = None;
                    return Err(failure(
                        FailureCode::ElevationChannel,
                        t("core.elevation.helper_late"),
                    ));
                }
                Some(FromHelper::Failed { code, startup }) => {
                    if startup.is_some_and(|startup| !startup.matches_code(code)) {
                        self.session = None;
                        return Err(failed_with(FailureCode::ElevationChannel));
                    }
                    // These failures precede stopping the old runtime in Sidecar::start/vet.
                    if matches!(
                        code,
                        FailureCode::SidecarConfigRejected
                            | FailureCode::SidecarConfigTimeout
                            | FailureCode::XrayConfigRejected
                            | FailureCode::XrayConfigTimeout
                    ) {
                        crate::proxy_channel::set(&self.channel, previous);
                    }
                    if matches!(
                        code,
                        FailureCode::LocalPortInUse | FailureCode::LocalPortDenied
                    ) {
                        if let Some(listener) = self.diagnostics.listener {
                            let error = listener.error();
                            if error.failure_code() == Some(code) {
                                return Err(error);
                            }
                        }
                    }
                    let error = failed_with(code);
                    return Err(match startup {
                        Some(startup) => startup.attach(error),
                        None => error,
                    });
                }
                Some(FromHelper::ExitedWithCode { code }) => {
                    let code = exit_code(code);
                    crate::proxy_channel::set(&self.channel, None);
                    self.lost = Some(code);
                    self.session = None;
                    return Err(failed_with(code));
                }
                Some(FromHelper::Exited) => {
                    crate::proxy_channel::set(&self.channel, None);
                    self.lost = Some(FailureCode::SidecarCrashed);
                    self.session = None;
                    return Err(failed_with(FailureCode::SidecarStartExit));
                }
                Some(_) => {}
                None => {
                    return Err(failure(
                        FailureCode::ElevationHelperCrashed,
                        t("core.elevation.helper_exited"),
                    ))
                }
            }
        }
    }
    async fn stop(&mut self) -> Result<()> {
        crate::proxy_channel::set(&self.channel, None);
        let previous_lost = self.lost;
        if self.session.is_some() {
            let _ = self.send(&ToHelper::Stop).await;
            while let Some(message) = self.hear(self.timeouts.stop).await {
                match message {
                    FromHelper::Diagnostics { diagnostics } => {
                        if !self.observe(diagnostics) {
                            break;
                        }
                    }
                    FromHelper::ExitedWithCode { code } => {
                        self.lost.get_or_insert_with(|| exit_code(code));
                    }
                    FromHelper::Exited => {
                        self.lost.get_or_insert(FailureCode::SidecarCrashed);
                    }
                    FromHelper::Stopped | FromHelper::Failed { .. } => break,
                    _ => {}
                }
            }
        }
        // Dropped either way: the channel closes and a helper that did not hear Stop ends too.
        self.session = None;
        if self.lost == Some(FailureCode::ElevationHelperCrashed) {
            // EOF during an intentional stop is not proof of a helper crash.
            self.lost = previous_lost;
        }
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        self.drain();
        Ok(self.session.as_ref().is_some_and(|session| session.running))
    }
    fn crash_code(&mut self) -> FailureCode {
        self.drain();
        self.lost.unwrap_or(FailureCode::SidecarCrashed)
    }
    fn diagnostics(&self) -> crate::runtime::RuntimeDiagnostics {
        self.diagnostics.clone()
    }
    fn informational_notes(&self, now: u64) -> Vec<String> {
        let mut said = self.warning_notes.lock().unwrap_or_else(|e| e.into_inner());
        self.diagnostics
            .warnings
            .iter()
            .filter_map(|warning| {
                if said.get(&warning.category).is_some_and(|&(count, at)| {
                    warning.count <= count
                        || now.saturating_sub(at) < crate::sidecar_log::QUIET_SECS
                }) {
                    return None;
                }
                said.insert(warning.category, (warning.count, now));
                Some(warning.category.note().to_owned())
            })
            .collect()
    }
    fn proxy_channel(&self) -> Option<crate::proxy_channel::ProxyChannel> {
        Some(self.channel.clone())
    }
}
