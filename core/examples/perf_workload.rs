//! Deterministic CPU/allocation workload for profilers (Samply, Heaptrack,
//! cargo-bloat). Each scenario is its own function so profiles attribute time to it:
//! - `refresh_path`: parse, merge, generate config, save state, run a selection cycle;
//! - `scrub_errors`: error texts cleaned for the local journal against 2000 servers' values;
//! - `coordinator`: `Application` with a stub sing-box: Refresh, Connect, Initial cycle,
//!   Disconnect.
//!
//! No network, no credentials. `RIFT_PERF_ROUNDS` (default 50) scales every scenario.
use async_trait::async_trait;
use rift_core::{
    application::{Action, Application, Snapshot},
    config,
    journal::{self, Secrets},
    model::Store,
    persistence,
    runtime::Runtime,
    selection,
    subscription::{merge, parse_subscription},
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[path = "../benches/common/mod.rs"]
mod fixtures;
use fixtures::{FixtureSource, InstantProbe, SERVERS};

type Error = Box<dyn std::error::Error>;
const SECRET: &str = "perf-secret-01234567890123456789012";

fn main() -> Result<(), Error> {
    let rounds: usize = std::env::var("RIFT_PERF_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let checksum = refresh_path(rounds, &runtime)?;
    println!("refresh: {rounds} rounds, {SERVERS} servers, checksum {checksum}");
    let hidden = scrub_errors(rounds);
    println!("scrub: {} texts, {hidden} values hidden", rounds * MESSAGES);
    runtime.block_on(coordinator(rounds))?;
    println!("coordinator: {rounds} Refresh + Connect + Initial cycle + Disconnect");
    Ok(())
}

#[inline(never)]
fn refresh_path(rounds: usize, runtime: &tokio::runtime::Runtime) -> Result<usize, Error> {
    let dir = tempfile::tempdir()?;
    let body = fixtures::subscription(SERVERS);
    let mut store = fixtures::store(0);
    let mut checksum = 0usize;
    for _ in 0..rounds {
        let parsed = parse_subscription(&body, "sub")?;
        store = merge(&store, "sub", parsed);
        let generated = config::generate(&store, SECRET)?;
        checksum += generated["outbounds"].as_array().map_or(0, Vec::len);
        persistence::save(&dir.path().join("state.json"), &store)?;
        let servers = store.available_servers();
        let outcome = runtime.block_on(selection::run(
            &InstantProbe,
            &servers,
            None,
            10,
            &CancellationToken::new(),
            |_| {},
        ))?;
        checksum += outcome.selected.len();
    }
    Ok(checksum)
}

const MESSAGES: usize = 3;

/// Short, typical and long (a log tail) error texts naming fixture servers.
fn error_texts(store: &Store) -> [String; MESSAGES] {
    let server = &store.servers[SERVERS / 2];
    let short = "Не удалось обновить подписку; предыдущие серверы сохранены".to_owned();
    let typical = format!(
        "Сервер {} ({}:{}) недоступен: connection refused by 203.0.113.7:{} via https://{}/sub?token=perf-secret-0123456789012",
        server.name, server.host, server.port, server.port, server.host
    );
    let long = (0..200)
        .map(|i| {
            let s = &store.servers[i * 7 % SERVERS];
            format!(
                "WARN outbound/{}[{}]: dial tcp {}:{}: i/o timeout; C:\\Users\\someone\\AppData\\Local\\Rift\\sing-box.log",
                s.protocol, s.name, s.host, s.port
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    [short, typical, long]
}

#[inline(never)]
fn scrub_errors(rounds: usize) -> usize {
    let store = fixtures::store(SERVERS);
    let known = Secrets::from_store(&store, std::path::Path::new("."));
    let texts = error_texts(&store);
    let mut hidden = 0;
    for _ in 0..rounds {
        for text in &texts {
            hidden += journal::clean(text, &known).matches("[redacted]").count();
        }
    }
    hidden
}

/// sing-box stand-in: always starts, never exits.
struct StubRuntime(Arc<AtomicBool>);
#[async_trait]
impl Runtime for StubRuntime {
    async fn validate(&mut self, _: &Store, _: &str) -> rift_core::Result<()> {
        Ok(())
    }
    async fn start(&mut self, _: &Store, _: &str) -> rift_core::Result<()> {
        self.0.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn stop(&mut self) -> rift_core::Result<()> {
        self.0.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn alive(&mut self) -> rift_core::Result<bool> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}

/// Connected, no cycle running or requested.
async fn idle(app: &Application) -> Result<(), Error> {
    let settled =
        |s: &Snapshot| s.runtime_active && !s.checking && !s.pending && s.status == "connected";
    let mut rx = app.snapshots.subscribe();
    tokio::time::timeout(Duration::from_secs(60), rx.wait_for(settled))
        .await
        .map_err(|_| "coordinator did not settle within 60 s")??;
    Ok(())
}

#[inline(never)]
async fn coordinator(rounds: usize) -> Result<(), Error> {
    let dir = tempfile::tempdir()?;
    let app = Application::with_services(
        fixtures::store(SERVERS),
        dir.path().into(),
        Box::new(StubRuntime(Arc::new(AtomicBool::new(false)))),
        Arc::new(InstantProbe),
        SECRET.into(),
        Arc::new(FixtureSource),
    );
    app.start();
    // The next cycle may not start before switch + interval, so every round reconnects:
    // Connect begins a fresh schedule with an Initial cycle over all servers.
    for _ in 0..rounds {
        app.execute(Action::Refresh {
            id: Some("sub".into()),
        })
        .await?;
        app.execute(Action::Connect).await?;
        idle(&app).await?;
        app.execute(Action::Disconnect).await?;
    }
    app.shutdown().await?;
    Ok(())
}
