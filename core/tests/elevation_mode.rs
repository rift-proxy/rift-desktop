//! Which runtime runs for which mode, and what a refusal leaves behind.
mod common;

use async_trait::async_trait;
use common::helper::{tun_store, FakeRuntime, Prompt, World, SECRET};
use rift_core::{
    dns::ResolverStats,
    elevation::{
        client::{HelperRuntime, HelperTimeouts},
        helper::Exit,
        mode::ModeRuntime,
    },
    failure::FailureCode,
    model::{Mode, Store},
    runtime::Runtime,
    sidecar_log::{SingBoxLog, Summary},
    Result,
};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};

fn quick() -> HelperTimeouts {
    HelperTimeouts {
        consent: Duration::from_millis(300),
        connect: Duration::from_millis(300),
        hello: Duration::from_millis(300),
        start: Duration::from_millis(300),
        stop: Duration::from_millis(300),
    }
}
fn modes(world: &Arc<World>, elevated: bool) -> ModeRuntime {
    ModeRuntime::new(
        Box::new(FakeRuntime(world.plain_calls.clone())),
        HelperRuntime::new(world.elevator(), world.transport()).with_timeouts(quick()),
        move || elevated,
    )
}
fn store(mode: Mode) -> Store {
    let mut store = tun_store();
    store.settings.mode = mode;
    store
}

#[tokio::test]
async fn proxy_runs_in_the_plain_sidecar_and_asks_for_nothing() {
    let world = World::new(Prompt::Accept);
    let mut rt = modes(&world, false);
    rt.start(&store(Mode::Proxy), SECRET).await.unwrap();
    assert_eq!(world.prompts(), 0);
    assert_eq!(world.log(), ["plain:start"]);
    assert!(rt.alive().unwrap());
}

#[tokio::test]
async fn tun_without_rights_runs_in_the_helper_after_one_prompt() {
    let world = World::new(Prompt::Accept);
    let mut rt = modes(&world, false);
    rt.start(&store(Mode::Tun), SECRET).await.unwrap();
    assert_eq!(world.log(), ["prompt", "helper:start"]);
    assert!(rt.alive().unwrap());
}

#[tokio::test]
async fn a_rift_with_administrator_rights_goes_round_the_helper() {
    for mode in [Mode::Tun, Mode::TunProxy] {
        let world = World::new(Prompt::Accept);
        let mut rt = modes(&world, true);
        rt.start(&store(mode.clone()), SECRET).await.unwrap();
        assert_eq!(world.prompts(), 0, "{mode:?}");
        assert_eq!(world.log(), ["plain:start"], "{mode:?}");
    }
}

#[tokio::test]
async fn a_refusal_leaves_a_running_proxy_exactly_as_it_was() {
    let world = World::new(Prompt::Decline);
    let mut rt = modes(&world, false);
    rt.start(&store(Mode::Proxy), SECRET).await.unwrap();
    let error = rt.start(&store(Mode::Tun), SECRET).await.unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::ElevationDeclined));
    // The prompt came before anything was touched: the proxy was neither stopped nor restarted.
    assert_eq!(world.log(), ["plain:start", "prompt"]);
    assert!(rt.alive().unwrap(), "the proxy still runs");
    assert!(
        !world.helper_calls.alive.load(Ordering::SeqCst),
        "no sing-box in TUN mode"
    );
}

#[tokio::test]
async fn consent_comes_before_the_proxy_is_stopped_for_the_tunnel() {
    let world = World::new(Prompt::Accept);
    let mut rt = modes(&world, false);
    rt.start(&store(Mode::Proxy), SECRET).await.unwrap();
    rt.start(&store(Mode::TunProxy), SECRET).await.unwrap();
    assert_eq!(
        world.log(),
        ["plain:start", "prompt", "plain:stop", "helper:start"]
    );
}

#[tokio::test]
async fn changes_within_tun_modes_do_not_ask_again() {
    let world = World::new(Prompt::Accept);
    let mut rt = modes(&world, false);
    rt.start(&store(Mode::Tun), SECRET).await.unwrap();
    rt.start(&store(Mode::TunProxy), SECRET).await.unwrap();
    rt.start(&store(Mode::TunProxy), SECRET).await.unwrap();
    assert_eq!(world.prompts(), 1);
    assert_eq!(world.helper_calls.starts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn leaving_tun_ends_the_helper_and_starts_the_plain_sidecar() {
    let world = World::new(Prompt::Accept);
    let mut rt = modes(&world, false);
    rt.start(&store(Mode::Tun), SECRET).await.unwrap();
    rt.start(&store(Mode::Proxy), SECRET).await.unwrap();
    assert_eq!(world.last_exit().await, Exit::Stopped);
    assert_eq!(world.prompts(), 1);
    // The helper stops sing-box when told and once more as it ends: stopping twice is harmless.
    let mut log = world.log();
    log.dedup();
    assert_eq!(
        log,
        ["prompt", "helper:start", "helper:stop", "plain:start"]
    );
    assert!(rt.alive().unwrap());
}

#[tokio::test]
async fn stop_ends_everything_and_alive_follows() {
    let world = World::new(Prompt::Accept);
    let mut rt = modes(&world, false);
    rt.start(&store(Mode::Tun), SECRET).await.unwrap();
    rt.stop().await.unwrap();
    assert!(!rt.alive().unwrap());
    assert_eq!(world.last_exit().await, Exit::Stopped);
    rt.start(&store(Mode::Proxy), SECRET).await.unwrap();
    rt.stop().await.unwrap();
    assert!(!rt.alive().unwrap());
}

#[tokio::test]
async fn a_dead_helper_is_a_dead_runtime_with_its_own_reason() {
    let world = World::new(Prompt::Accept);
    let mut rt = modes(&world, false);
    rt.start(&store(Mode::Tun), SECRET).await.unwrap();
    world.crash_last_helper();
    for _ in 0..200 {
        if !rt.alive().unwrap() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!rt.alive().unwrap());
    assert_eq!(rt.crash_code(), FailureCode::ElevationHelperCrashed);
}

#[tokio::test]
async fn a_plain_sidecar_that_dies_keeps_the_old_reason() {
    let world = World::new(Prompt::Accept);
    let mut rt = modes(&world, false);
    rt.start(&store(Mode::Proxy), SECRET).await.unwrap();
    world.plain_calls.alive.store(false, Ordering::SeqCst);
    assert!(!rt.alive().unwrap());
    assert_eq!(rt.crash_code(), FailureCode::SidecarCrashed);
}

#[tokio::test]
async fn checking_a_config_never_asks_for_rights() {
    let world = World::new(Prompt::Decline);
    let mut rt = modes(&world, false);
    rt.validate(&store(Mode::Tun), SECRET).await.unwrap();
    assert_eq!(world.prompts(), 0);
}

/// A plain sidecar that has something to say about its DNS bridge and its log.
struct Talking(Arc<SingBoxLog>);
#[async_trait]
impl Runtime for Talking {
    async fn validate(&mut self, _: &Store, _: &str) -> Result<()> {
        Ok(())
    }
    async fn start(&mut self, _: &Store, _: &str) -> Result<()> {
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(true)
    }
    fn dns_stats(&self) -> Vec<ResolverStats> {
        vec![ResolverStats {
            name: "google",
            answered: 2,
            missed: 0,
            last_miss: None,
            last_answer_ms: Some(40),
            left_out: false,
        }]
    }
    fn log_notes(&self, now: u64) -> Vec<String> {
        self.0.notes(now)
    }
    fn log_summary(&self) -> Option<Summary> {
        Some(self.0.summary())
    }
    fn last_direct_failure(&self) -> Option<String> {
        self.0.last_direct()
    }
}

#[tokio::test]
async fn what_the_plain_sidecar_knows_about_dns_and_its_log_is_not_lost_on_the_way_up() {
    let world = World::new(Prompt::Accept);
    let log = Arc::new(SingBoxLog::default());
    let line = "+0300 2026-10-19 17:09:42 ERROR [1 10.1s] connection: open connection to private-site.example:443 using outbound/direct[direct]: lookup private-site.example: (dial udp 8.8.4.4:53: i/o timeout)";
    log.record(line, 1_000);
    log.record(line, 1_000);
    let rt = ModeRuntime::new(
        Box::new(Talking(log)),
        HelperRuntime::new(world.elevator(), world.transport()).with_timeouts(quick()),
        || false,
    );
    assert_eq!(rt.dns_stats().len(), 1);
    assert_eq!(rt.log_summary().map(|summary| summary.lines), Some(2));
    assert!(rt.last_direct_failure().is_some());
    assert!(!rt.log_notes(1_000).is_empty());
}

#[tokio::test]
async fn helper_tun_never_reports_facts_of_the_previous_plain_run() {
    let world = World::new(Prompt::Accept);
    let log = Arc::new(SingBoxLog::default());
    log.record("ERROR connection: open connection to site.example:443 using outbound/direct[direct]: dial tcp: i/o timeout", 1000);
    let mut rt = ModeRuntime::new(
        Box::new(Talking(log)),
        HelperRuntime::new(world.elevator(), world.transport()).with_timeouts(quick()),
        || false,
    );
    rt.start(&store(Mode::Proxy), SECRET).await.unwrap();
    assert_eq!(rt.log_summary().unwrap().lines, 1);
    rt.start(&store(Mode::Tun), SECRET).await.unwrap();
    assert!(rt.dns_stats().is_empty());
    assert!(rt.log_summary().is_none());
    assert!(rt.last_direct_failure().is_none());
    assert!(rt.log_notes(1000).is_empty());
    rt.stop().await.unwrap();
    assert!(rt.log_summary().is_none());
    assert!(rt.log_notes(1000).is_empty());
    assert!(rt.last_direct_failure().is_none());
}

struct GatedGateway {
    channel: rift_core::proxy_channel::ProxyChannel,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    starts: usize,
}
#[async_trait]
impl Runtime for GatedGateway {
    async fn validate(&mut self, store: &Store, _: &str) -> Result<()> {
        if store.settings.proxy_port == 10 {
            return Err(rift_core::Error::failure(
                FailureCode::XrayConfigRejected,
                "synthetic",
            ));
        }
        Ok(())
    }
    async fn start(&mut self, store: &Store, secret: &str) -> Result<()> {
        self.validate(store, secret).await?;
        self.starts += 1;
        *self.channel.write().unwrap() = None;
        if self.starts == 2 {
            self.entered.notify_one();
            self.release.notified().await;
        }
        *self.channel.write().unwrap() = Some(rift_core::proxy_channel::ProxyHandoff::new(
            12340 + self.starts as u16,
            SECRET.into(),
            store,
        )?);
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        *self.channel.write().unwrap() = None;
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(self.channel.read().unwrap().is_some())
    }
    fn proxy_channel(&self) -> Option<rift_core::proxy_channel::ProxyChannel> {
        Some(self.channel.clone())
    }
}

#[tokio::test]
async fn wrapper_revokes_gateway_during_replacement_but_preserves_rejected_runtime() {
    let world = World::new(Prompt::Decline);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut rt = ModeRuntime::new(
        Box::new(GatedGateway {
            channel: Arc::new(std::sync::RwLock::new(None)),
            entered: entered.clone(),
            release: release.clone(),
            starts: 0,
        }),
        HelperRuntime::new(world.elevator(), world.transport()).with_timeouts(quick()),
        || false,
    );
    rt.start(&store(Mode::Proxy), SECRET).await.unwrap();
    let channel = rt.proxy_channel().unwrap();
    assert_eq!(channel.read().unwrap().as_ref().unwrap().port(), 12341);
    let task = tokio::spawn(async move {
        rt.start(&store(Mode::Proxy), SECRET).await.unwrap();
        rt
    });
    entered.notified().await;
    assert!(
        channel.read().unwrap().is_none(),
        "old stopped gateway must not be published"
    );
    release.notify_one();
    let mut rt = task.await.unwrap();
    assert_eq!(channel.read().unwrap().as_ref().unwrap().port(), 12342);
    let mut bad = store(Mode::Proxy);
    bad.settings.proxy_port = 10;
    assert!(rt.start(&bad, SECRET).await.is_err());
    assert_eq!(channel.read().unwrap().as_ref().unwrap().port(), 12342);
    assert!(rt.start(&store(Mode::Tun), SECRET).await.is_err());
    assert_eq!(channel.read().unwrap().as_ref().unwrap().port(), 12342);
    rt.stop().await.unwrap();
    assert!(channel.read().unwrap().is_none());
}
