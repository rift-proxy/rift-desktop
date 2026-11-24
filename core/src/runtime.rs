use crate::elevation::protocol::{StartupFailure, StartupStage};
use crate::text::t;
use crate::{
    config::{self, Resolver},
    dns::{Chain, DnsBridge, ResolverStats},
    failure::FailureCode,
    geodata::GeoCache,
    launch::LaunchPlan,
    model::Store,
    probe::NetworkProbe,
    selection::Probe,
    sidecar_log::{self, SingBoxLog, Summary},
    Error, Result,
};
use async_trait::async_trait;
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::AsyncWriteExt,
    process::{Child, Command},
    time::{sleep, timeout},
};

/// Configs earlier versions wrote next to state.json; they held credentials and the API
/// secret in plain text. The config now reaches sing-box only through its stdin.
const LEGACY_CONFIGS: [&str; 2] = ["candidate.json", "sing-box.json"];

#[derive(Clone, Copy, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    SingBox,
    Xray,
}
#[derive(Clone, Copy, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitStage {
    Startup,
    Running,
}
#[derive(Clone, Copy, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Termination {
    Unexpected,
    Requested,
    SiblingFailure,
}
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChildExit {
    pub engine: Engine,
    pub stage: ExitStage,
    pub termination: Termination,
    pub code: Option<i32>,
    pub signal: Option<i32>,
}
impl ChildExit {
    pub(crate) fn observed(
        engine: Engine,
        stage: ExitStage,
        termination: Termination,
        status: std::process::ExitStatus,
    ) -> Self {
        #[cfg(unix)]
        let signal = {
            use std::os::unix::process::ExitStatusExt;
            status.signal()
        };
        #[cfg(not(unix))]
        let signal = None;
        Self {
            engine,
            stage,
            termination,
            code: status.code(),
            signal,
        }
    }
}

/// Safe facts of the latest actual launch attempt, retained after cleanup. At most two
/// exits and two warning categories; no strings, addresses or raw child output.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDiagnostics {
    pub generation: u64,
    pub primary_exit: Option<Engine>,
    pub exits: Vec<ChildExit>,
    pub listener: Option<sidecar_log::ListenerFailure>,
    pub warnings: Vec<crate::xray_log::WarningCount>,
}
impl RuntimeDiagnostics {
    /// Validate bounds and uniqueness before accepting a helper observation.
    pub(crate) fn valid(&self) -> bool {
        self.exits.len() <= 2
            && self.warnings.len() <= 2
            && (self.exits.len() < 2 || self.exits[0].engine != self.exits[1].engine)
            && (self.warnings.len() < 2 || self.warnings[0].category != self.warnings[1].category)
            && self.exits.iter().all(|exit| {
                exit.signal
                    .is_none_or(|signal| (1..=127).contains(&signal) && exit.code.is_none())
            })
            && self.warnings.iter().all(|warning| warning.count != 0)
            && (self.generation != 0
                || (self.exits.is_empty() && self.listener.is_none() && self.warnings.is_empty()))
            && self.listener.is_none_or(|listener| listener.port != 0)
            && self.primary_exit.is_none_or(|engine| {
                self.exits.iter().any(|exit| {
                    exit.engine == engine && exit.termination == Termination::Unexpected
                })
            })
    }
}

impl<'de> serde::Deserialize<'de> for RuntimeDiagnostics {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            generation: u64,
            primary_exit: Option<Engine>,
            #[serde(deserialize_with = "deserialize_pair")]
            exits: Vec<ChildExit>,
            listener: Option<sidecar_log::ListenerFailure>,
            #[serde(deserialize_with = "deserialize_pair")]
            warnings: Vec<crate::xray_log::WarningCount>,
        }
        let wire = <Wire as serde::Deserialize>::deserialize(deserializer)?;
        let diagnostics = Self {
            generation: wire.generation,
            primary_exit: wire.primary_exit,
            exits: wire.exits,
            listener: wire.listener,
            warnings: wire.warnings,
        };
        if !diagnostics.valid() {
            return Err(serde::de::Error::custom("invalid runtime observations"));
        }
        Ok(diagnostics)
    }
}

fn deserialize_pair<'de, D, T>(deserializer: D) -> std::result::Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    struct Pair<T>(std::marker::PhantomData<T>);
    impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for Pair<T> {
        type Value = Vec<T>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("at most two closed observations")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut values = Vec::with_capacity(2);
            while let Some(value) = seq.next_element()? {
                if values.len() == 2 {
                    return Err(serde::de::Error::custom("too many runtime observations"));
                }
                values.push(value);
            }
            Ok(values)
        }
    }
    deserializer.deserialize_seq(Pair(std::marker::PhantomData))
}

pub(crate) async fn drain_readers(readers: &mut Vec<tokio::task::JoinHandle<()>>) {
    for mut reader in readers.drain(..) {
        if timeout(Duration::from_millis(100), &mut reader)
            .await
            .is_err()
        {
            reader.abort();
            let _ = reader.await;
        }
    }
}

#[async_trait]
pub trait Runtime: Send {
    async fn validate(&mut self, store: &Store, secret: &str) -> Result<()>;
    async fn start(&mut self, store: &Store, secret: &str) -> Result<()>;
    async fn stop(&mut self) -> Result<()>;
    fn alive(&mut self) -> Result<bool>;
    /// Why the runtime is gone when `alive` says it is: the child crashed, or, for a runtime
    /// that works through a helper process, the helper itself did.
    fn crash_code(&mut self) -> FailureCode {
        FailureCode::SidecarCrashed
    }
    fn diagnostics(&self) -> RuntimeDiagnostics {
        RuntimeDiagnostics::default()
    }
    /// Informational notices, never connection failures or warning-level journal entries.
    fn informational_notes(&self, _now: u64) -> Vec<String> {
        Vec::new()
    }
    fn proxy_channel(&self) -> Option<crate::proxy_channel::ProxyChannel> {
        None
    }
    /// How the resolvers behind the names sing-box dials have answered, for the diagnostics.
    fn dns_stats(&self) -> Vec<ResolverStats> {
        Vec::new()
    }
    /// What sing-box said about its own failures since the last call, worth a line of the
    /// journal each (see `sidecar_log`: never its words).
    fn log_notes(&self, _now: u64) -> Vec<String> {
        Vec::new()
    }
    /// The reduced log of the run, for the diagnostics.
    fn log_summary(&self) -> Option<Summary> {
        None
    }
    /// Xray warnings reduced to fixed classes, kept separately from sing-box observations.
    fn xray_log_summary(&self) -> Option<Summary> {
        None
    }
    /// The last failure of the direct outbound, as a phrase.
    fn last_direct_failure(&self) -> Option<String> {
        None
    }
}
/// How long the sidecar waits for sing-box before it reports a typed timeout.
#[derive(Clone, Copy, Debug)]
pub struct SidecarTimeouts {
    /// The verdict of `sing-box check` (`SidecarConfigTimeout`).
    pub check: Duration,
    /// Handing the config to a new child's stdin (`SidecarStartExit`).
    pub feed: Duration,
    /// The new child's API answering after it took the config (`SidecarReadinessTimeout`).
    pub ready: Duration,
}
impl Default for SidecarTimeouts {
    fn default() -> Self {
        Self {
            check: Duration::from_secs(15),
            feed: Duration::from_secs(10),
            ready: Duration::from_secs(10),
        }
    }
}
pub struct Sidecar {
    binary: PathBuf,
    directory: PathBuf,
    /// The geosite and geoip databases in `<directory>/geodata`: the routing lists that need
    /// them are written into the config from here.
    geo: GeoCache,
    timeouts: SidecarTimeouts,
    /// The resolvers behind the names sing-box dials; one chain for every start.
    chain: fn() -> Chain,
    /// The DNS port of the running child: it lives as long as the child does.
    bridge: Option<DnsBridge>,
    /// What the child said about its failures, reduced; shared with the tasks that read it.
    log: Arc<SingBoxLog>,
    child: Option<Child>,
    xray: Option<crate::xray_process::XrayProcess>,
    crashed: Option<FailureCode>,
    channel: crate::proxy_channel::ProxyChannel,
    generation: u64,
    stage: ExitStage,
    termination: Termination,
    exit: Option<ChildExit>,
    primary_exit: Option<Engine>,
    readers: Vec<tokio::task::JoinHandle<()>>,
    #[cfg(windows)]
    job: Option<crate::windows::ChildJob>,
}
impl Sidecar {
    pub fn new(binary: PathBuf, directory: PathBuf) -> Self {
        Self {
            geo: GeoCache::open(directory.join("geodata")),
            binary,
            directory,
            timeouts: SidecarTimeouts::default(),
            chain: Chain::public,
            bridge: None,
            log: Arc::new(SingBoxLog::default()),
            child: None,
            xray: None,
            crashed: None,
            channel: std::sync::Arc::new(std::sync::RwLock::new(None)),
            generation: 0,
            stage: ExitStage::Startup,
            termination: Termination::Unexpected,
            exit: None,
            primary_exit: None,
            readers: Vec::new(),
            #[cfg(windows)]
            job: None,
        }
    }
    /// Other resolvers than the public ones, for tests that must not reach the network.
    #[must_use]
    pub fn with_chain(mut self, chain: fn() -> Chain) -> Self {
        self.chain = chain;
        self
    }
    /// Production enables the second owned core; the base constructor also serves isolated
    /// sing-box lifecycle/failure tests. This is not a runtime fallback when Xray fails.
    #[must_use]
    pub fn with_xray_binary(mut self, binary: PathBuf) -> Self {
        self.xray = Some(crate::xray_process::XrayProcess::new(
            binary,
            self.directory.clone(),
        ));
        self
    }
    /// Inject a synthetic CA into the owned Xray child, never OS trust. Integration fixtures
    /// only: this capability has no Store/profile/helper/IPC input and never disables TLS checks.
    #[doc(hidden)]
    pub fn with_test_xray_ca(mut self, pem: String) -> Result<Self> {
        self.xray
            .as_mut()
            .ok_or_else(|| Error::Invalid(t("core.runtime.xray_not_configured").into()))?
            .set_test_ca(pem)?;
        Ok(self)
    }
    pub fn xray_pid(&self) -> Option<u32> {
        self.xray
            .as_ref()
            .and_then(crate::xray_process::XrayProcess::pid)
    }
    /// Shorter limits for tests that inject stalled/failed children.
    #[must_use]
    pub fn with_timeouts(mut self, timeouts: SidecarTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }
    async fn start_children(&mut self, store: &Store, secret: &str) -> Result<()> {
        let bridge = DnsBridge::start((self.chain)()).await.map_err(|error| {
            StartupFailure::io(StartupStage::DnsBridgeStart, &error).attach(Error::failure(
                FailureCode::SidecarStartExit,
                t("core.runtime.dns_port_failed"),
            ))
        })?;
        let bridge_port = bridge.port();
        self.bridge = Some(bridge);
        self.crashed = None;
        let geo = self.geo.resolve_store(store);
        let config = if let Some(xray) = self.xray.as_mut() {
            let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
            let port = listener.local_addr()?.port();
            let password = uuid::Uuid::new_v4().simple().to_string();
            let plan = LaunchPlan::build(
                store,
                secret,
                &geo,
                Resolver::Bridge(bridge_port),
                port,
                &password,
            )?;
            let plan = if plan.requires_interface() {
                plan.with_interface(&crate::network_binding::physical_interface()?)?
            } else {
                plan
            };
            let user = plan.xray()["inbounds"][0]["settings"]["accounts"][0]["user"]
                .as_str()
                .ok_or_else(|| Error::Invalid(t("core.runtime.no_verified_channel").into()))?;
            drop(listener);
            xray.start(plan.xray(), port, user, &password, self.timeouts)
                .await?;
            crate::proxy_channel::set(
                &self.channel,
                Some(crate::proxy_channel::ProxyHandoff::new(
                    port, password, store,
                )?),
            );
            plan.sing_box().clone()
        } else {
            config::generate_resolving(store, secret, &geo, Resolver::Bridge(bridge_port))?
        };
        let config = serde_json::to_vec(&config)?;
        let mut child = self
            .run_command()
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                StartupFailure::io(StartupStage::SingBoxSpawn, &error).attach(Error::failure(
                    FailureCode::SidecarStartExit,
                    t("core.common.singbox_exited_unknown"),
                ))
            })?;
        // Attach, then resume: an error leaves the child suspended and kill_on_drop ends it.
        #[cfg(windows)]
        {
            let job = crate::windows::ChildJob::attach(&child)?;
            crate::windows::resume(&child)?;
            self.job = Some(job);
        }
        // A new run, a clean log; both pipes are read, and only the reduced facts are kept.
        // Readers of the previous child's pipes may still drain buffered output. Their Arc
        // belongs to that run; replacing it prevents late lines entering this run's summary.
        // This collector is set at the generation boundary, before any child is spawned.
        if let Some(p) = child.stdout.take() {
            self.readers
                .push(tokio::spawn(sidecar_log::read(p, self.log.clone())));
        }
        if let Some(p) = child.stderr.take() {
            self.readers
                .push(tokio::spawn(sidecar_log::read(p, self.log.clone())));
        }
        // After resume: a suspended child would never drain a config larger than the pipe.
        let fed = timeout(self.timeouts.feed, feed(&mut child, &config)).await;
        self.child = Some(child);
        match fed {
            Ok(Ok(())) => {}
            // feed() itself failed: almost always a closed pipe because the child already
            // exited (e.g. a port conflict), same as an early exit below.
            Ok(Err(error)) => {
                self.stop().await?;
                return Err(
                    StartupFailure::io(StartupStage::SingBoxConfigFeed, &error).attach(
                        Error::failure(
                            FailureCode::SidecarStartExit,
                            t("core.common.singbox_exited_unknown"),
                        ),
                    ),
                );
            }
            // The wrapper timed out, not feed() itself: the child may still be alive and only
            // slow to read its stdin (antivirus scanning the fresh binary, disk contention).
            // Ports/TUN wording would misdirect the user toward an unrelated cause.
            Err(_) => {
                self.stop().await?;
                return Err(
                    StartupFailure::timeout(StartupStage::SingBoxConfigFeed).attach(
                        Error::failure(
                            FailureCode::SidecarStartExit,
                            t("core.runtime.singbox_config_timeout"),
                        ),
                    ),
                );
            }
        }
        let probe = NetworkProbe::new(store.settings.clone(), secret.into())?;
        let ready = timeout(self.timeouts.ready, async {
            loop {
                if !self.alive()? {
                    if self.crashed == Some(FailureCode::XrayCrashed) {
                        return Err(Error::failure(
                            FailureCode::XrayCrashed,
                            t("core.runtime.xray_exited_at_start"),
                        ));
                    }
                    return Err(Error::failure(
                        FailureCode::SidecarStartExit,
                        t("core.common.singbox_exited_unknown"),
                    ));
                }
                if probe.ready().await.is_ok() {
                    return Ok(());
                }
                sleep(Duration::from_millis(100)).await;
            }
        })
        .await;
        match ready {
            Ok(Ok(())) => {
                // Started in the mode with no rules: the server it works through has its own.
                if let Err(error) = probe.routes(&store.live_routes_mode()).await {
                    self.stop().await?;
                    return Err(error);
                }
                self.stage = ExitStage::Running;
                if let Some(xray) = self.xray.as_mut() {
                    xray.mark_running();
                }
                Ok(())
            }
            other => {
                self.stop().await?;
                match other {
                    Ok(Err(e)) => Err(e),
                    _ => Err(Error::failure(
                        FailureCode::SidecarReadinessTimeout,
                        t("core.common.singbox_api_silent"),
                    )),
                }
            }
        }
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.current_dir(&self.directory).kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW, never a shell.
        cmd
    }
    /// The long-running child: on Windows created suspended, so it cannot run (or outlive a
    /// crashed Rift) before it is in the Job Object. The config comes on stdin.
    fn run_command(&self) -> Command {
        let mut cmd = self.command();
        cmd.args(["run", "-c", "stdin"]).stdin(Stdio::piped());
        #[cfg(windows)]
        cmd.creation_flags(crate::windows::SUSPENDED_NO_WINDOW);
        cmd
    }
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }
}
/// Writes the whole config to the child's stdin and closes it (EOF ends sing-box's read).
async fn feed(child: &mut Child, config: &[u8]) -> std::io::Result<()> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other(t("core.runtime.no_config_channel")))?;
    stdin.write_all(config).await?;
    stdin.shutdown().await?;
    Ok(())
}
/// Removes configs with credentials left by earlier versions; only these two own files.
fn remove_legacy_configs(directory: &std::path::Path) -> Result<()> {
    for name in LEGACY_CONFIGS {
        match std::fs::remove_file(directory.join(name)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    Ok(())
}
#[async_trait]
impl Runtime for Sidecar {
    async fn validate(&mut self, store: &Store, secret: &str) -> Result<()> {
        // Environment failures keep their own codes: only sing-box's verdict on the config
        // is a rejection, and callers must not blame the config for a machine problem.
        std::fs::create_dir_all(&self.directory)
            .map_err(|error| Error::from(error).with_code(FailureCode::Persistence))?;
        remove_legacy_configs(&self.directory)
            .map_err(|error| error.with_code(FailureCode::Persistence))?;
        // `check` reads the config and connects to nothing: any port will do for the bridge.
        let geo = self.geo.resolve_store(store);
        let config = if let Some(xray) = &self.xray {
            let port = [65533, 65534, 65535]
                .into_iter()
                .find(|p| *p != store.settings.api_port && *p != store.settings.proxy_port)
                .expect("three ports, two exclusions");
            let plan = LaunchPlan::build(
                store,
                secret,
                &geo,
                Resolver::Bridge(1),
                port,
                "synthetic-validation-secret-0123456789012345",
            )?;
            xray.validate(plan.xray(), self.timeouts).await?;
            plan.sing_box().clone()
        } else {
            config::generate_resolving(store, secret, &geo, Resolver::Bridge(1))?
        };
        let config = serde_json::to_vec(&config)?;
        let mut child = self
            .command()
            .args(["check", "-c", "stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                StartupFailure::io(StartupStage::SingBoxConfigCheckSpawn, &error).attach(
                    Error::failure(
                        FailureCode::SidecarStartExit,
                        t("core.runtime.check_start_failed"),
                    ),
                )
            })?;
        let checked = timeout(self.timeouts.check, async {
            // A child that exits before reading everything closes the pipe: that is a
            // rejected config, reported by its exit status below.
            let _ = feed(&mut child, &config).await;
            child.wait().await
        })
        .await
        .map_err(|_| {
            StartupFailure::timeout(StartupStage::SingBoxConfigCheck).attach(Error::failure(
                FailureCode::SidecarConfigTimeout,
                t("core.common.config_check_timeout"),
            ))
        })?;
        let checked = checked.map_err(|error| {
            StartupFailure::io(StartupStage::SingBoxConfigCheck, &error).attach(Error::failure(
                FailureCode::SidecarStartExit,
                t("core.runtime.check_start_failed"),
            ))
        })?;
        if !checked.success() {
            return Err(
                StartupFailure::exit(StartupStage::SingBoxConfigCheck, checked).attach(
                    Error::failure(
                        FailureCode::SidecarConfigRejected,
                        t("core.common.singbox_rejected_config"),
                    ),
                ),
            );
        }
        Ok(())
    }
    async fn start(&mut self, store: &Store, secret: &str) -> Result<()> {
        #[cfg(windows)]
        if store.settings.mode != crate::model::Mode::Proxy {
            crate::windows::require_elevation()?;
        }
        self.validate(store, secret).await?;
        self.stop().await?;
        self.generation = self.generation.saturating_add(1);
        self.stage = ExitStage::Startup;
        self.termination = Termination::Unexpected;
        self.exit = None;
        self.primary_exit = None;
        self.crashed = None;
        self.log = Arc::new(SingBoxLog::with_listeners(
            (store.settings.mode != crate::model::Mode::Tun).then_some(store.settings.proxy_port),
            store.settings.api_port,
        ));
        if let Some(xray) = self.xray.as_mut() {
            xray.reset_observations();
        }
        let outcome = self.start_children(store, secret).await;
        if outcome.is_err() {
            let _ = self.stop().await;
            if matches!(
                outcome.as_ref().err().and_then(Error::failure_code),
                Some(FailureCode::SidecarStartExit | FailureCode::SidecarReadinessTimeout)
            ) {
                if let Some(listener) = self.log.listener_failure() {
                    return Err(listener.error());
                }
            }
        }
        outcome
    }
    fn proxy_channel(&self) -> Option<crate::proxy_channel::ProxyChannel> {
        self.xray.as_ref().map(|_| self.channel.clone())
    }
    fn dns_stats(&self) -> Vec<ResolverStats> {
        self.bridge
            .as_ref()
            .map(DnsBridge::stats)
            .unwrap_or_default()
    }
    fn diagnostics(&self) -> RuntimeDiagnostics {
        let mut exits: Vec<_> = self.exit.iter().cloned().collect();
        if let Some(exit) = self.xray.as_ref().and_then(|xray| xray.exit()) {
            exits.push(exit.clone());
        }
        RuntimeDiagnostics {
            generation: self.generation,
            primary_exit: self.primary_exit,
            exits,
            listener: self.log.listener_failure(),
            warnings: self
                .xray
                .as_ref()
                .map(|xray| xray.warning_summary())
                .unwrap_or_default(),
        }
    }
    fn informational_notes(&self, now: u64) -> Vec<String> {
        self.xray
            .as_ref()
            .map(|xray| xray.informational_notes(now))
            .unwrap_or_default()
    }
    fn log_notes(&self, now: u64) -> Vec<String> {
        let mut notes = self.log.notes(now);
        if let Some(xray) = &self.xray {
            notes.extend(xray.log_notes(now));
        }
        notes
    }
    fn log_summary(&self) -> Option<Summary> {
        Some(self.log.summary())
    }
    fn xray_log_summary(&self) -> Option<Summary> {
        self.xray
            .as_ref()
            .map(crate::xray_process::XrayProcess::log_summary)
    }
    fn last_direct_failure(&self) -> Option<String> {
        self.log.last_direct()
    }
    async fn stop(&mut self) -> Result<()> {
        crate::proxy_channel::set(&self.channel, None);
        self.bridge = None;
        let mut failure = None;
        if let Some(mut child) = self.child.take() {
            let stopped: std::io::Result<()> = async {
                let status = if let Some(status) = child.try_wait()? {
                    status
                } else {
                    if self.termination == Termination::Unexpected {
                        self.termination = Termination::Requested;
                    }
                    child.kill().await?;
                    child.wait().await?
                };
                self.exit.get_or_insert_with(|| {
                    ChildExit::observed(Engine::SingBox, self.stage, self.termination, status)
                });
                if self.termination == Termination::Unexpected {
                    self.primary_exit.get_or_insert(Engine::SingBox);
                }
                Ok(())
            }
            .await;
            if let Err(error) = stopped {
                failure = Some(Error::from(error));
            }
        }
        if let Some(xray) = self.xray.as_mut() {
            if self.primary_exit == Some(Engine::SingBox) {
                xray.start_kill();
            }
            if let Err(error) = xray.stop().await {
                failure.get_or_insert(error);
            }
            if self.primary_exit.is_none()
                && xray
                    .exit()
                    .is_some_and(|exit| exit.termination == Termination::Unexpected)
            {
                self.primary_exit = Some(Engine::Xray);
            }
        }
        #[cfg(windows)]
        {
            self.job = None;
        }
        drain_readers(&mut self.readers).await;
        failure.map_or(Ok(()), Err)
    }
    fn alive(&mut self) -> Result<bool> {
        // Preserve the first observed failure; killing its sibling must not relabel the cause.
        if self.crashed.is_some() {
            return Ok(false);
        }
        let sing_box_alive = match &mut self.child {
            Some(child) => match child.try_wait()? {
                Some(status) => {
                    self.exit.get_or_insert_with(|| {
                        ChildExit::observed(Engine::SingBox, self.stage, self.termination, status)
                    });
                    false
                }
                None => true,
            },
            None => false,
        };
        if !sing_box_alive {
            if self.child.is_some() {
                self.crashed = Some(FailureCode::SidecarCrashed);
                self.primary_exit.get_or_insert(Engine::SingBox);
            }
            crate::proxy_channel::set(&self.channel, None);
            self.bridge = None;
            if let Some(xray) = self.xray.as_mut() {
                xray.start_kill();
            }
            return Ok(false);
        }
        let xray_alive = match self.xray.as_mut() {
            Some(xray) => xray.alive()?,
            None => true,
        };
        if !xray_alive {
            self.crashed = Some(FailureCode::XrayCrashed);
            self.primary_exit.get_or_insert(Engine::Xray);
            crate::proxy_channel::set(&self.channel, None);
            if let Some(child) = self.child.as_mut() {
                self.termination = Termination::SiblingFailure;
                let _ = child.start_kill();
            }
            self.bridge = None;
            return Ok(false);
        }
        Ok(true)
    }
    fn crash_code(&mut self) -> FailureCode {
        self.crashed.unwrap_or(FailureCode::SidecarCrashed)
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
    }
}

#[cfg(all(test, unix))]
mod lifecycle_tests {
    use super::*;

    #[tokio::test]
    async fn running_exit_retains_status_and_requested_stop_is_not_a_crash() {
        for intentional in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut sidecar = Sidecar::new("/bin/sh".into(), dir.path().into());
            sidecar.generation = 1;
            sidecar.stage = ExitStage::Running;
            let mut child = Command::new("/bin/sh")
                .args([
                    "-c",
                    if intentional {
                        "exec sleep 30"
                    } else {
                        "exit 37"
                    },
                ])
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            if !intentional {
                child.wait().await.unwrap();
            }
            sidecar.child = Some(child);
            if !intentional {
                assert!(!sidecar.alive().unwrap());
            }
            sidecar.stop().await.unwrap();
            let facts = sidecar.diagnostics();
            assert_eq!(facts.exits.len(), 1);
            let exit = &facts.exits[0];
            assert_eq!(exit.stage, ExitStage::Running);
            assert_eq!(exit.engine, Engine::SingBox);
            if intentional {
                assert_eq!(facts.primary_exit, None);
                assert_eq!(exit.termination, Termination::Requested);
                assert_eq!(exit.signal, Some(9));
            } else {
                assert_eq!(facts.primary_exit, Some(Engine::SingBox));
                assert_eq!(exit.termination, Termination::Unexpected);
                assert_eq!(exit.code, Some(37));
            }
            sidecar.stop().await.unwrap();
            assert_eq!(sidecar.diagnostics(), facts);
        }
    }

    #[tokio::test]
    async fn xray_status_query_error_does_not_fabricate_a_crash_or_poison_cleanup() {
        for retry in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut sidecar = Sidecar::new("/bin/sh".into(), dir.path().into());
            sidecar.generation = 1;
            sidecar.stage = ExitStage::Running;
            sidecar.child = Some(
                Command::new("/bin/sh")
                    .args(["-c", "exec sleep 30"])
                    .kill_on_drop(true)
                    .spawn()
                    .unwrap(),
            );
            let xray_child = Command::new("/bin/sh")
                .args(["-c", "exec sleep 30"])
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            sidecar.xray = Some(
                crate::xray_process::XrayProcess::new("/bin/sh".into(), dir.path().into())
                    .with_test_child(xray_child, Some(std::io::ErrorKind::PermissionDenied)),
            );

            let error = sidecar.alive().unwrap_err();
            assert!(
                matches!(error, Error::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied)
            );
            assert_eq!(sidecar.crashed, None);
            assert_eq!(sidecar.termination, Termination::Unexpected);
            assert!(sidecar
                .child
                .as_mut()
                .unwrap()
                .try_wait()
                .unwrap()
                .is_none());
            let facts = sidecar.diagnostics();
            assert_eq!(facts.primary_exit, None);
            assert!(facts.exits.is_empty());
            assert_eq!(
                serde_json::from_value::<RuntimeDiagnostics>(serde_json::to_value(&facts).unwrap())
                    .unwrap(),
                facts
            );
            if retry {
                assert!(sidecar.alive().unwrap());
            }

            sidecar.stop().await.unwrap();
            let facts = sidecar.diagnostics();
            assert_eq!(facts.primary_exit, None);
            assert_eq!(facts.exits.len(), 2);
            for exit in &facts.exits {
                assert_eq!(exit.stage, ExitStage::Running);
                assert_eq!(exit.termination, Termination::Requested);
                assert_eq!(exit.signal, Some(9));
            }
            assert_eq!(
                serde_json::from_value::<RuntimeDiagnostics>(serde_json::to_value(&facts).unwrap())
                    .unwrap(),
                facts
            );
            sidecar.stop().await.unwrap();
            assert_eq!(sidecar.diagnostics(), facts);
        }
    }

    #[tokio::test]
    async fn observed_xray_crash_stops_sibling_and_retains_valid_exit_facts() {
        let dir = tempfile::tempdir().unwrap();
        let mut sidecar = Sidecar::new("/bin/sh".into(), dir.path().into());
        sidecar.generation = 1;
        sidecar.stage = ExitStage::Running;
        sidecar.child = Some(
            Command::new("/bin/sh")
                .args(["-c", "exec sleep 30"])
                .kill_on_drop(true)
                .spawn()
                .unwrap(),
        );
        let mut xray_child = Command::new("/bin/sh")
            .args(["-c", "exit 41"])
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        xray_child.wait().await.unwrap();
        sidecar.xray = Some(
            crate::xray_process::XrayProcess::new("/bin/sh".into(), dir.path().into())
                .with_test_child(xray_child, None),
        );

        assert!(!sidecar.alive().unwrap());
        assert_eq!(sidecar.crash_code(), FailureCode::XrayCrashed);
        assert_eq!(sidecar.termination, Termination::SiblingFailure);
        assert!(!sidecar.alive().unwrap());
        timeout(
            Duration::from_secs(5),
            sidecar.child.as_mut().unwrap().wait(),
        )
        .await
        .expect("observing Xray death must terminate the sibling before cleanup")
        .unwrap();
        sidecar.stop().await.unwrap();
        let facts = sidecar.diagnostics();
        assert_eq!(facts.primary_exit, Some(Engine::Xray));
        assert_eq!(facts.exits.len(), 2);
        let xray = facts
            .exits
            .iter()
            .find(|exit| exit.engine == Engine::Xray)
            .unwrap();
        assert_eq!(xray.stage, ExitStage::Running);
        assert_eq!(xray.termination, Termination::Unexpected);
        assert_eq!(xray.code, Some(41));
        let sibling = facts
            .exits
            .iter()
            .find(|exit| exit.engine == Engine::SingBox)
            .unwrap();
        assert_eq!(sibling.termination, Termination::SiblingFailure);
        assert_eq!(sibling.signal, Some(9));
        assert_eq!(
            serde_json::from_value::<RuntimeDiagnostics>(serde_json::to_value(&facts).unwrap())
                .unwrap(),
            facts
        );
        sidecar.stop().await.unwrap();
        assert_eq!(sidecar.diagnostics(), facts);
    }

    #[test]
    fn helper_diagnostics_refuse_unbounded_or_inconsistent_observations_on_decode() {
        let facts = RuntimeDiagnostics {
            generation: 1,
            primary_exit: Some(Engine::SingBox),
            exits: vec![ChildExit {
                engine: Engine::SingBox,
                stage: ExitStage::Startup,
                termination: Termination::Unexpected,
                code: Some(42),
                signal: None,
            }],
            listener: Some(sidecar_log::ListenerFailure {
                purpose: sidecar_log::ListenerPurpose::Proxy,
                port: 2080,
                cause: sidecar_log::ListenerCause::AccessDenied,
            }),
            warnings: vec![crate::xray_log::WarningCount {
                category: crate::xray_log::WarningCategory::GrpcDeprecated,
                count: 1,
            }],
        };
        let value = serde_json::to_value(&facts).unwrap();
        assert_eq!(
            serde_json::from_value::<RuntimeDiagnostics>(value.clone()).unwrap(),
            facts
        );
        for mutate in [
            ("port", serde_json::json!(0)),
            ("purpose", serde_json::json!("remote")),
        ] {
            let mut bad = value.clone();
            bad["listener"][mutate.0] = mutate.1;
            assert!(serde_json::from_value::<RuntimeDiagnostics>(bad).is_err());
        }
        for (path, replacement) in [
            ("signal", serde_json::json!(9)),
            ("signal", serde_json::json!(-1)),
            ("termination", serde_json::json!("requested")),
            ("engine", serde_json::json!("unknown")),
            ("raw_output", serde_json::json!("secret-canary")),
        ] {
            let mut bad = value.clone();
            bad["exits"][0][path] = replacement;
            assert!(serde_json::from_value::<RuntimeDiagnostics>(bad).is_err());
        }
        for count in [2, 3, 100] {
            let mut bad = value.clone();
            bad["exits"] = serde_json::json!(vec![value["exits"][0].clone(); count]);
            assert!(serde_json::from_value::<RuntimeDiagnostics>(bad).is_err());
            let mut bad = value.clone();
            bad["warnings"] = serde_json::json!(vec![value["warnings"][0].clone(); count]);
            assert!(serde_json::from_value::<RuntimeDiagnostics>(bad).is_err());
        }
    }

    #[tokio::test]
    async fn a_dead_child_closes_its_dns_bridge() {
        let dir = tempfile::tempdir().unwrap();
        let mut sidecar = Sidecar::new("/bin/sh".into(), dir.path().into());
        sidecar.bridge = Some(DnsBridge::start(Chain::new(Vec::new())).await.unwrap());
        let port = sidecar.bridge.as_ref().unwrap().port();
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .unwrap();
        child.wait().await.unwrap();
        sidecar.child = Some(child);
        assert!(!sidecar.alive().unwrap());
        assert!(sidecar.bridge.is_none());
        timeout(Duration::from_secs(1), async {
            while tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .is_ok()
            {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("aborted bridge releases its listening socket");
    }

    #[tokio::test]
    async fn a_failed_spawn_drops_the_bridge() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("sidecar");
        std::fs::copy("/bin/sh", &binary).unwrap();
        // Validation succeeds but removes the executable before run can spawn it.
        std::fs::write(dir.path().join("check"), "cat >/dev/null\nrm ./sidecar\n").unwrap();
        let mut sidecar = Sidecar::new(binary, dir.path().into());
        let mut store = Store::default();
        store
            .servers
            .push(crate::parser::parse_uri("trojan://p@127.0.0.1:9", None).unwrap());
        assert!(sidecar
            .start(&store, "12345678901234567890123456789012")
            .await
            .is_err());
        assert!(sidecar.bridge.is_none());
        assert!(!sidecar.alive().unwrap());
    }
}

#[cfg(all(test, unix))]
mod log_generation_tests {
    use super::*;
    #[tokio::test]
    async fn late_output_of_the_old_run_is_isolated_from_the_new_run() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("check"), "cat >/dev/null\n").unwrap();
        std::fs::write(dir.path().join("run"), "cat >/dev/null\nexec sleep 30\n").unwrap();
        let mut sidecar =
            Sidecar::new("/bin/sh".into(), dir.path().into()).with_timeouts(SidecarTimeouts {
                ready: Duration::from_millis(20),
                ..SidecarTimeouts::default()
            });
        let old = sidecar.log.clone();
        let mut store = Store::default();
        store
            .servers
            .push(crate::parser::parse_uri("trojan://p@127.0.0.1:9", None).unwrap());
        let _ = sidecar
            .start(&store, "12345678901234567890123456789012")
            .await;
        old.record("ERROR connection: dial tcp 127.0.0.1:9: i/o timeout", 1);
        assert_eq!(old.summary().lines, 1);
        assert_eq!(
            sidecar.log_summary().unwrap().lines,
            0,
            "old pipe readers keep their own log"
        );
    }
}
