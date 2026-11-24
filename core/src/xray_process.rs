//! Owned Xray child. Only trusted launch plans reach stdin; output becomes bounded enums.
use crate::elevation::protocol::{StartupFailure, StartupStage};
use crate::text::t;
use crate::{
    failure::FailureCode,
    runtime::{ChildExit, Engine, ExitStage, SidecarTimeouts, Termination},
    Error, Result,
};
use serde_json::Value;
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    process::{Child, Command},
    time::{sleep, timeout},
};

pub(crate) struct XrayProcess {
    binary: PathBuf,
    directory: PathBuf,
    child: Option<Child>,
    test_ca: Option<String>,
    log: Arc<crate::sidecar_log::SingBoxLog>,
    readers: Vec<tokio::task::JoinHandle<()>>,
    stage: ExitStage,
    termination: Termination,
    exit: Option<ChildExit>,
    #[cfg(all(test, unix))]
    alive_error: Option<std::io::ErrorKind>,
    #[cfg(windows)]
    job: Option<crate::windows::ChildJob>,
}
impl XrayProcess {
    pub fn new(binary: PathBuf, directory: PathBuf) -> Self {
        Self {
            binary,
            directory,
            child: None,
            test_ca: None,
            log: Arc::default(),
            readers: Vec::new(),
            stage: ExitStage::Startup,
            termination: Termination::Unexpected,
            exit: None,
            #[cfg(all(test, unix))]
            alive_error: None,
            #[cfg(windows)]
            job: None,
        }
    }
    #[cfg(all(test, unix))]
    pub(crate) fn with_test_child(
        mut self,
        child: Child,
        alive_error: Option<std::io::ErrorKind>,
    ) -> Self {
        self.child = Some(child);
        self.mark_running();
        self.alive_error = alive_error;
        self
    }
    pub fn reset_observations(&mut self) {
        self.stage = ExitStage::Startup;
        self.termination = Termination::Unexpected;
        self.exit = None;
        self.log = Arc::default();
    }
    pub fn mark_running(&mut self) {
        self.stage = ExitStage::Running;
    }
    pub fn exit(&self) -> Option<&ChildExit> {
        self.exit.as_ref()
    }
    pub fn warning_summary(&self) -> Vec<crate::xray_log::WarningCount> {
        self.log.warning_summary()
    }
    pub fn informational_notes(&self, now: u64) -> Vec<String> {
        self.log.informational_notes(now)
    }
    pub fn set_test_ca(&mut self, pem: String) -> Result<()> {
        if pem.len() > 65536 || reqwest::Certificate::from_pem(pem.as_bytes()).is_err() {
            return Err(Error::Invalid(
                t("core.xray_process.bad_test_certificate").into(),
            ));
        }
        self.test_ca = Some(pem);
        Ok(())
    }
    fn config_bytes(&self, config: &Value) -> Result<Vec<u8>> {
        let Some(root) = &self.test_ca else {
            return Ok(serde_json::to_vec(config)?);
        };
        let mut config = config.clone();
        for outbound in config["outbounds"]
            .as_array_mut()
            .ok_or_else(|| Error::Invalid(t("core.xray_process.bad_plan").into()))?
        {
            if outbound["streamSettings"]["security"] == "tls" {
                let tls = &mut outbound["streamSettings"]["tlsSettings"];
                tls["certificates"] = serde_json::json!([{"usage":"verify","certificate":root.lines().collect::<Vec<_>>()}]);
                tls["disableSystemRoot"] = serde_json::json!(true);
            }
        }
        Ok(serde_json::to_vec(&config)?)
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.current_dir(&self.directory)
            .kill_on_drop(true)
            .args(["run", "-c", "stdin:", "-format", "json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        cmd.creation_flags(0x08000000);
        cmd
    }
    pub async fn validate(&self, config: &Value, limits: SidecarTimeouts) -> Result<()> {
        let bytes = self.config_bytes(config)?;
        let mut child = self.command().arg("-test").spawn().map_err(|error| {
            StartupFailure::io(StartupStage::XrayConfigCheckSpawn, &error).attach(Error::failure(
                FailureCode::XrayStartExit,
                t("core.xray_process.check_start_failed"),
            ))
        })?;
        let verdict = timeout(limits.check, async {
            let _ = feed(&mut child, &bytes).await;
            child.wait().await
        })
        .await
        .map_err(|_| {
            StartupFailure::timeout(StartupStage::XrayConfigCheck).attach(Error::failure(
                FailureCode::XrayConfigTimeout,
                t("core.common.xray_config_check_timeout"),
            ))
        })?
        .map_err(|error| {
            StartupFailure::io(StartupStage::XrayConfigCheck, &error).attach(Error::failure(
                FailureCode::XrayStartExit,
                t("core.xray_process.check_start_failed"),
            ))
        })?;
        if !verdict.success() {
            return Err(
                StartupFailure::exit(StartupStage::XrayConfigCheck, verdict).attach(
                    Error::failure(
                        FailureCode::XrayConfigRejected,
                        t("core.xray_process.config_rejected_check"),
                    ),
                ),
            );
        }
        Ok(())
    }
    pub async fn start(
        &mut self,
        config: &Value,
        port: u16,
        user: &str,
        password: &str,
        limits: SidecarTimeouts,
    ) -> Result<()> {
        self.stop().await?;
        self.reset_observations();
        let mut command = self.command();
        #[cfg(windows)]
        command.creation_flags(crate::windows::SUSPENDED_NO_WINDOW);
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                StartupFailure::io(StartupStage::XraySpawn, &error).attach(Error::failure(
                    FailureCode::XrayStartExit,
                    t("core.xray_process.start_failed"),
                ))
            })?;
        #[cfg(windows)]
        {
            let job = crate::windows::ChildJob::attach(&child)?;
            crate::windows::resume(&child)?;
            self.job = Some(job);
        }
        // A fresh collector prevents a late reader from a previous run changing this run.
        self.log = Arc::default();
        if let Some(pipe) = child.stdout.take() {
            self.readers
                .push(tokio::spawn(crate::sidecar_log::read_observed(
                    pipe,
                    self.log.clone(),
                    crate::xray_log::observe,
                )));
        }
        if let Some(pipe) = child.stderr.take() {
            self.readers
                .push(tokio::spawn(crate::sidecar_log::read_observed(
                    pipe,
                    self.log.clone(),
                    crate::xray_log::observe,
                )));
        }
        let bytes = self.config_bytes(config)?;
        let fed = timeout(limits.feed, feed(&mut child, &bytes)).await;
        self.child = Some(child);
        if !matches!(fed, Ok(Ok(()))) {
            self.stop().await?;
            let observation = match fed {
                Err(_) => StartupFailure::timeout(StartupStage::XrayConfigFeed),
                Ok(Err(error)) => StartupFailure::io(StartupStage::XrayConfigFeed, &error),
                Ok(Ok(())) => unreachable!("successful feed does not enter failure branch"),
            };
            return Err(observation.attach(Error::failure(
                FailureCode::XrayStartExit,
                t("core.xray_process.config_not_accepted"),
            )));
        }
        let ready = timeout(limits.ready, async {
            loop {
                if !self.alive()? {
                    return Err(Error::failure(
                        FailureCode::XrayStartExit,
                        t("core.common.xray_exited_at_start"),
                    ));
                }
                if timeout(
                    Duration::from_millis(500),
                    authenticate(port, user, password),
                )
                .await
                .is_ok_and(|r| r.is_ok())
                {
                    return Ok(());
                }
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        match ready {
            Ok(Ok(())) => Ok(()),
            other => {
                self.stop().await?;
                match other {
                    Ok(Err(error)) => Err(error),
                    _ => Err(Error::failure(
                        FailureCode::XrayReadinessTimeout,
                        t("core.xray_process.channel_silent"),
                    )),
                }
            }
        }
    }
    pub fn alive(&mut self) -> Result<bool> {
        #[cfg(all(test, unix))]
        if let Some(error) = self.alive_error.take() {
            return Err(std::io::Error::from(error).into());
        }
        match self.child.as_mut() {
            Some(child) => match child.try_wait()? {
                Some(status) => {
                    self.exit.get_or_insert_with(|| {
                        ChildExit::observed(Engine::Xray, self.stage, self.termination, status)
                    });
                    Ok(false)
                }
                None => Ok(true),
            },
            None => Ok(false),
        }
    }
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }
    pub fn log_summary(&self) -> crate::sidecar_log::Summary {
        self.log.summary()
    }
    pub fn log_notes(&self, now: u64) -> Vec<String> {
        self.log.notes_for(now, "Xray")
    }
    pub fn start_kill(&mut self) {
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.exit.get_or_insert_with(|| {
                        ChildExit::observed(Engine::Xray, self.stage, self.termination, status)
                    });
                }
                Ok(None) => {
                    self.termination = Termination::SiblingFailure;
                    let _ = child.start_kill();
                }
                Err(_) => {}
            }
        }
    }
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
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
                ChildExit::observed(Engine::Xray, self.stage, self.termination, status)
            });
        }
        crate::runtime::drain_readers(&mut self.readers).await;
        #[cfg(windows)]
        {
            self.job = None;
        }
        Ok(())
    }
}
impl Drop for XrayProcess {
    fn drop(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
    }
}
async fn feed(child: &mut Child, config: &[u8]) -> std::io::Result<()> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("missing config pipe"))?;
    stdin.write_all(config).await?;
    stdin.shutdown().await
}
async fn authenticate(port: u16, user: &str, password: &str) -> std::io::Result<()> {
    let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    stream.write_all(&[5, 1, 2]).await?;
    let mut response = [0; 2];
    stream.read_exact(&mut response).await?;
    if response != [5, 2] {
        return Err(std::io::Error::other("wrong socks auth method"));
    }
    let mut credentials = vec![
        1,
        u8::try_from(user.len()).map_err(|_| std::io::Error::other("user length"))?,
    ];
    credentials.extend_from_slice(user.as_bytes());
    credentials
        .push(u8::try_from(password.len()).map_err(|_| std::io::Error::other("password length"))?);
    credentials.extend_from_slice(password.as_bytes());
    stream.write_all(&credentials).await?;
    stream.read_exact(&mut response).await?;
    if response != [1, 0] {
        return Err(std::io::Error::other("socks auth rejected"));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn xray_status_survives_cleanup_and_intentional_sibling_stop() {
        for termination in [
            Termination::Unexpected,
            Termination::Requested,
            Termination::SiblingFailure,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut xray = XrayProcess::new("/bin/sh".into(), dir.path().into());
            xray.mark_running();
            let mut child = Command::new("/bin/sh")
                .args([
                    "-c",
                    if termination == Termination::Unexpected {
                        "exit 41"
                    } else {
                        "exec sleep 30"
                    },
                ])
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            if termination == Termination::Unexpected {
                child.wait().await.unwrap();
            }
            xray.child = Some(child);
            if termination == Termination::Unexpected {
                assert!(!xray.alive().unwrap());
            } else if termination == Termination::SiblingFailure {
                xray.start_kill();
            }
            xray.stop().await.unwrap();
            let exit = xray.exit().unwrap().clone();
            assert_eq!(exit.engine, Engine::Xray);
            assert_eq!(exit.stage, ExitStage::Running);
            assert_eq!(exit.termination, termination);
            if termination == Termination::Unexpected {
                assert_eq!(exit.code, Some(41));
            } else {
                assert_eq!(exit.signal, Some(9));
            }
            xray.stop().await.unwrap();
            assert_eq!(xray.exit(), Some(&exit));
            xray.reset_observations();
            assert!(xray.exit().is_none());
            assert_eq!(xray.stage, ExitStage::Startup);
        }
    }
}
