//! The runtime Rift really uses: the plain sidecar for Proxy, and the elevated helper
//! for a TUN mode when Rift itself has no administrator rights. Which one runs follows the mode of
//! the store it is given, so a mode change is a change of runtime and nothing above it knows.
use super::client::HelperRuntime;
use crate::{
    dns::ResolverStats, failure::FailureCode, model::Mode, model::Store, runtime::Runtime,
    sidecar_log::Summary, Result,
};
use async_trait::async_trait;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Active {
    None,
    Plain,
    Helper,
}
pub struct ModeRuntime {
    plain: Box<dyn Runtime>,
    helper: HelperRuntime,
    /// Whether this Rift has administrator rights itself: then no helper is needed.
    elevated: Box<dyn Fn() -> bool + Send + Sync>,
    active: Active,
    facts_from_plain: bool,
    channel: Option<crate::proxy_channel::ProxyChannel>,
}
impl ModeRuntime {
    pub fn new(
        plain: Box<dyn Runtime>,
        helper: HelperRuntime,
        elevated: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        let channel = plain
            .proxy_channel()
            .map(|_| std::sync::Arc::new(std::sync::RwLock::new(None)));
        Self {
            plain,
            helper,
            elevated: Box::new(elevated),
            active: Active::None,
            facts_from_plain: true,
            channel,
        }
    }
    /// A TUN mode and no rights: the helper runs sing-box.
    fn wants_helper(&self, mode: &Mode) -> bool {
        *mode != Mode::Proxy && !(self.elevated)()
    }
}
#[async_trait]
impl Runtime for ModeRuntime {
    /// sing-box checks a config without rights; the plain sidecar does it for both ways.
    async fn validate(&mut self, store: &Store, secret: &str) -> Result<()> {
        self.plain.validate(store, secret).await
    }
    async fn start(&mut self, store: &Store, secret: &str) -> Result<()> {
        if self.wants_helper(&store.settings.mode) {
            // Consent first: a refusal, or a helper that cannot start, leaves what runs as it is.
            self.helper.prepare().await?;
            if let Some(channel) = &self.channel {
                crate::proxy_channel::set(channel, None);
            }
            if self.active == Active::Plain {
                self.plain.stop().await?;
                self.active = Active::None;
            }
            // The helper restarts sing-box inside itself when it already runs one: no new prompt.
            self.active = Active::Helper;
            self.facts_from_plain = false;
            let started = self.helper.start(store, secret).await;
            if started.is_err() && !self.helper.alive().unwrap_or(false) {
                self.active = Active::None;
            }
            if let Some(channel) = &self.channel {
                let value = if self.active == Active::Helper {
                    self.helper
                        .proxy_channel()
                        .and_then(|c| c.read().ok().and_then(|v| v.clone()))
                } else {
                    None
                };
                crate::proxy_channel::set(channel, value);
            }
            started
        } else {
            // Revoke the wrapper copy too: delegated stop only clears its own channel.
            if let Some(channel) = &self.channel {
                crate::proxy_channel::set(channel, None);
            }
            if self.active == Active::Helper || self.helper.has_session() {
                self.helper.stop().await?;
            }
            self.active = Active::Plain;
            self.facts_from_plain = true;
            let started = self.plain.start(store, secret).await;
            if started.is_err() && !self.plain.alive().unwrap_or(false) {
                self.active = Active::None;
            }
            if let Some(channel) = &self.channel {
                let value = if self.active == Active::Plain {
                    self.plain
                        .proxy_channel()
                        .and_then(|c| c.read().ok().and_then(|v| v.clone()))
                } else {
                    None
                };
                crate::proxy_channel::set(channel, value);
            }
            started
        }
    }
    async fn stop(&mut self) -> Result<()> {
        if let Some(channel) = &self.channel {
            crate::proxy_channel::set(channel, None);
        }
        let helper = self.helper.stop().await;
        let plain = self.plain.stop().await;
        self.active = Active::None;
        helper.and(plain)
    }
    fn alive(&mut self) -> Result<bool> {
        let alive = match self.active {
            Active::None => Ok(false),
            Active::Plain => self.plain.alive(),
            Active::Helper => self.helper.alive(),
        }?;
        if !alive {
            if let Some(channel) = &self.channel {
                crate::proxy_channel::set(channel, None);
            }
        }
        Ok(alive)
    }
    fn proxy_channel(&self) -> Option<crate::proxy_channel::ProxyChannel> {
        self.channel.clone()
    }
    fn crash_code(&mut self) -> FailureCode {
        if self.facts_from_plain {
            self.plain.crash_code()
        } else {
            self.helper.crash_code()
        }
    }
    // What the plain sidecar knows about its DNS bridge and its log. The helper keeps both to
    // itself, so a TUN run through it has nothing to report here.
    fn dns_stats(&self) -> Vec<ResolverStats> {
        if !self.facts_from_plain {
            return Vec::new();
        }
        self.plain.dns_stats()
    }
    fn diagnostics(&self) -> crate::runtime::RuntimeDiagnostics {
        if self.facts_from_plain {
            self.plain.diagnostics()
        } else {
            self.helper.diagnostics()
        }
    }
    fn informational_notes(&self, now: u64) -> Vec<String> {
        if self.facts_from_plain {
            self.plain.informational_notes(now)
        } else {
            self.helper.informational_notes(now)
        }
    }
    fn log_notes(&self, now: u64) -> Vec<String> {
        if !self.facts_from_plain {
            return Vec::new();
        }
        self.plain.log_notes(now)
    }
    fn log_summary(&self) -> Option<Summary> {
        if !self.facts_from_plain {
            return None;
        }
        self.plain.log_summary()
    }
    fn xray_log_summary(&self) -> Option<Summary> {
        if !self.facts_from_plain {
            return None;
        }
        self.plain.xray_log_summary()
    }
    fn last_direct_failure(&self) -> Option<String> {
        if !self.facts_from_plain {
            return None;
        }
        self.plain.last_direct_failure()
    }
}
