//! Routing and the databases as the application runs them: saving a profile (checked, applied
//! atomically, restarting what runs), downloading the databases, taking back one the check refuses.
mod common;

use async_trait::async_trait;
use common::{geoip_file, geosite_file};
use rift_core::{
    application::{Action, Application, Snapshot},
    config,
    failure::FailureCode,
    geodata::{Fetched, GeoCache, GeoSource, Kind},
    model::{Mode, Server, Store},
    parser::parse_uri,
    persistence,
    routing::{Route, Routing},
    runtime::Runtime,
    selection::Probe,
    Error, Result,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::Notify;

const SECRET: &str = "12345678901234567890123456789012";

#[derive(Default)]
struct Counts {
    validated: AtomicUsize,
    started: AtomicUsize,
    /// Refuse every config that has this word in it (a stand-in for `sing-box check`).
    refuse: Mutex<Option<String>>,
    alive: AtomicBool,
    /// The last config the runtime was started with.
    last: Mutex<Option<serde_json::Value>>,
}
/// Builds the config as the sidecar does: with the lists of the databases in the data folder.
struct FakeRuntime(Arc<Counts>, GeoCache);
impl FakeRuntime {
    fn config(&self, store: &Store) -> Result<serde_json::Value> {
        config::generate_with(store, SECRET, &self.1.resolve_store(store))
    }
}
#[async_trait]
impl Runtime for FakeRuntime {
    async fn validate(&mut self, store: &Store, _: &str) -> Result<()> {
        self.0.validated.fetch_add(1, Ordering::SeqCst);
        let config = self.config(store)?;
        if let Some(word) = self.0.refuse.lock().unwrap().as_deref() {
            if config.to_string().contains(word) {
                return Err(Error::Invalid("Rejected config".into()));
            }
        }
        Ok(())
    }
    async fn start(&mut self, store: &Store, _: &str) -> Result<()> {
        self.0.started.fetch_add(1, Ordering::SeqCst);
        self.0.alive.store(true, Ordering::SeqCst);
        *self.0.last.lock().unwrap() = Some(self.config(store)?);
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        self.0.alive.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(self.0.alive.load(Ordering::SeqCst))
    }
}
/// Answers every probe at once, or holds one proxy probe until released.
#[derive(Default)]
struct Net {
    hold: AtomicBool,
    entered: Notify,
}
#[async_trait]
impl Probe for Net {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        Ok(5)
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        if self.hold.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(20)
    }
    async fn select(&self, _: &str) -> Result<()> {
        Ok(())
    }
}
/// What was asked of the source, and what it answers per kind.
#[derive(Default)]
struct FakeSource {
    calls: Mutex<Vec<(Kind, Option<u16>)>>,
    answers: Mutex<HashMap<&'static str, Result<Vec<u8>>>>,
    /// Only the tunnel works (the direct way is blocked).
    tunnel_only: AtomicBool,
    delay: Mutex<Option<Duration>>,
}
impl FakeSource {
    fn with(sites: Option<Vec<u8>>, ips: Option<Vec<u8>>) -> Arc<Self> {
        let source = Arc::new(Self::default());
        for (kind, bytes) in [("geosite", sites), ("geoip", ips)] {
            source.answers.lock().unwrap().insert(
                kind,
                bytes.ok_or_else(|| Error::failure(FailureCode::GeodataDownload, "offline")),
            );
        }
        source
    }
    fn calls(&self) -> Vec<(Kind, Option<u16>)> {
        self.calls.lock().unwrap().clone()
    }
}
#[async_trait]
impl GeoSource for FakeSource {
    async fn fetch(
        &self,
        kind: Kind,
        proxy_port: Option<u16>,
        installed: Option<&str>,
    ) -> Result<Fetched> {
        self.calls.lock().unwrap().push((kind, proxy_port));
        let delay = *self.delay.lock().unwrap();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        if self.tunnel_only.load(Ordering::SeqCst) && proxy_port.is_none() {
            return Err(Error::failure(FailureCode::GeodataDownload, "blocked"));
        }
        let key = if kind == Kind::Geosite {
            "geosite"
        } else {
            "geoip"
        };
        match self.answers.lock().unwrap().get(key).unwrap() {
            // What a real source says when the checksum it publishes is the installed one.
            Ok(bytes) if installed == Some(format!("{:x}", Sha256::digest(bytes)).as_str()) => {
                Ok(Fetched::Same)
            }
            Ok(bytes) => Ok(Fetched::Latest(bytes.clone())),
            Err(_) => Err(Error::failure(FailureCode::GeodataDownload, "offline")),
        }
    }
}

struct H {
    app: Arc<Application>,
    counts: Arc<Counts>,
    net: Arc<Net>,
    dir: tempfile::TempDir,
}
fn server(port: u16) -> Server {
    parse_uri(&format!("trojan://p@example.com:{port}"), None).unwrap()
}
fn store(mode: Mode) -> Store {
    let mut store = Store {
        servers: vec![server(1)],
        ..Store::default()
    };
    store.settings.mode = mode;
    store
}
fn start_with(store: Store, dir: tempfile::TempDir) -> H {
    let counts = Arc::new(Counts::default());
    let net = Arc::new(Net::default());
    let app = Application::with_probe(
        store,
        dir.path().into(),
        Box::new(FakeRuntime(
            counts.clone(),
            GeoCache::open(dir.path().join("geodata")),
        )),
        net.clone(),
        SECRET.into(),
    );
    H {
        app,
        counts,
        net,
        dir,
    }
}
fn start(store: Store) -> H {
    start_with(store, tempfile::tempdir().unwrap())
}
async fn wait(app: &Application, what: &str, ready: impl Fn(&Snapshot) -> bool) {
    let mut rx = app.snapshots.subscribe();
    tokio::time::timeout(Duration::from_secs(8), rx.wait_for(|s| ready(s)))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}: {:?}", app.snapshot().geodata))
        .unwrap();
}
fn save(order: [Route; 3], block: &str, direct: &str, proxy: &str) -> Action {
    Action::SaveRouting {
        order,
        block: block.into(),
        direct: direct.into(),
        proxy: proxy.into(),
    }
}
const DEFAULT_ORDER: [Route; 3] = [Route::Block, Route::Direct, Route::Proxy];

// ---- saving a profile ------------------------------------------------------------------------

#[tokio::test]
async fn a_saved_profile_is_stored_shown_and_applied() {
    let h = start(store(Mode::Proxy));
    assert_eq!(h.app.snapshot().routing, Routing::default());
    h.app
        .execute(save(
            [Route::Direct, Route::Block, Route::Proxy],
            "domain:ads.example\nkeyword:tracker",
            "domain:.ru, 10.0.0.0/8",
            "process:chrome.exe",
        ))
        .await
        .unwrap();
    let shown = h.app.snapshot().routing;
    assert_eq!(shown.order, [Route::Direct, Route::Block, Route::Proxy]);
    assert_eq!(shown.block, ["domain:ads.example", "keyword:tracker"]);
    assert_eq!(shown.direct, ["domain:.ru", "10.0.0.0/8"]);
    assert_eq!(
        shown.proxy,
        ["process:chrome.exe"],
        "kept though it is the rest and not applied"
    );
    assert_eq!(
        persistence::load(&h.dir.path().join("state.json"))
            .unwrap()
            .routing,
        shown
    );
    assert!(h
        .app
        .snapshot()
        .journal
        .iter()
        .any(|entry| entry.message == "Маршрутизация сохранена"));
}

#[tokio::test]
async fn a_new_profile_restarts_what_runs_and_an_unchanged_one_does_not() {
    let h = start(store(Mode::Proxy));
    h.app.execute(Action::Connect).await.unwrap();
    assert_eq!(h.counts.started.load(Ordering::SeqCst), 1);

    h.app
        .execute(save(DEFAULT_ORDER, "", "domain:.ru", ""))
        .await
        .unwrap();
    assert_eq!(
        h.counts.started.load(Ordering::SeqCst),
        2,
        "the config changed: a controlled restart"
    );
    assert_eq!(
        h.counts.validated.load(Ordering::SeqCst),
        1,
        "checked before it was applied"
    );
    let running = h.counts.last.lock().unwrap().clone().unwrap();
    assert_eq!(running["route"]["rules"][2]["domain_suffix"][0], "ru");

    // The same profile again: nothing to check or restart.
    h.app
        .execute(save(DEFAULT_ORDER, "", "domain:.ru", ""))
        .await
        .unwrap();
    assert_eq!(h.counts.started.load(Ordering::SeqCst), 2);
    assert_eq!(h.counts.validated.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn changing_only_the_list_that_is_not_applied_changes_no_config() {
    let h = start(store(Mode::Proxy));
    h.app.execute(Action::Connect).await.unwrap();
    // Proxy is the last list of the default order: its entries are stored, not applied.
    h.app
        .execute(save(
            DEFAULT_ORDER,
            "",
            &Routing::default().direct.join("\n"),
            "domain:later.example",
        ))
        .await
        .unwrap();
    assert_eq!(h.app.snapshot().routing.proxy, ["domain:later.example"]);
    assert_eq!(
        h.counts.started.load(Ordering::SeqCst),
        1,
        "no restart for a list that is not applied"
    );
    assert_eq!(h.counts.validated.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_entry_that_cannot_be_read_changes_nothing() {
    let h = start(store(Mode::Proxy));
    h.app.execute(Action::Connect).await.unwrap();
    let before = h.app.snapshot().routing;
    let error = h
        .app
        .execute(save(DEFAULT_ORDER, "domain:ok.example\nnot a rule", "", ""))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not a rule"), "{error}");
    assert_eq!(h.app.snapshot().routing, before);
    assert_eq!(h.counts.started.load(Ordering::SeqCst), 1);
    assert_eq!(h.counts.validated.load(Ordering::SeqCst), 0);
    let stored = persistence::load(&h.dir.path().join("state.json"));
    assert!(stored.map_or(true, |store| store.routing == before));
}

#[tokio::test]
async fn a_config_the_runtime_refuses_leaves_the_running_one_and_the_stored_profile() {
    let h = start(store(Mode::Proxy));
    h.app.execute(Action::Connect).await.unwrap();
    *h.counts.refuse.lock().unwrap() = Some("refused.example".into());
    let before = h.app.snapshot().routing;
    let error = h
        .app
        .execute(save(DEFAULT_ORDER, "domain:refused.example", "", ""))
        .await
        .unwrap_err();
    assert_eq!(
        error.failure_code(),
        Some(FailureCode::SidecarConfigRejected)
    );
    assert_eq!(h.app.snapshot().routing, before);
    assert_eq!(
        h.counts.started.load(Ordering::SeqCst),
        1,
        "the running sing-box was not touched"
    );
    assert!(h.app.snapshot().runtime_active);
}

#[tokio::test]
async fn resetting_brings_back_the_profile_of_a_new_installation() {
    let h = start(store(Mode::Proxy));
    h.app.execute(Action::Connect).await.unwrap();
    h.app
        .execute(save(
            [Route::Proxy, Route::Block, Route::Direct],
            "domain:b.example",
            "",
            "domain:p.example",
        ))
        .await
        .unwrap();
    let started = h.counts.started.load(Ordering::SeqCst);
    h.app.execute(Action::ResetRouting).await.unwrap();
    assert_eq!(h.app.snapshot().routing, Routing::default());
    assert_eq!(h.counts.started.load(Ordering::SeqCst), started + 1);
    // Resetting what is already the default is not a change.
    h.app.execute(Action::ResetRouting).await.unwrap();
    assert_eq!(h.counts.started.load(Ordering::SeqCst), started + 1);
}

#[tokio::test]
async fn every_mode_takes_the_profile_the_same_way() {
    for mode in [Mode::Proxy, Mode::Tun, Mode::TunProxy] {
        let h = start(store(mode));
        h.app
            .execute(save(
                DEFAULT_ORDER,
                "domain:b.example",
                "domain:d.example",
                "",
            ))
            .await
            .unwrap();
        assert_eq!(h.app.snapshot().routing.block, ["domain:b.example"]);
    }
}

#[tokio::test]
async fn saving_a_profile_interrupts_a_running_check_instead_of_waiting_for_it() {
    let h = start(store(Mode::Proxy));
    h.app.start();
    h.net.hold.store(true, Ordering::SeqCst);
    h.app.execute(Action::Connect).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), h.net.entered.notified())
        .await
        .expect("the check reached its probe");
    tokio::time::timeout(
        Duration::from_secs(5),
        h.app
            .execute(save(DEFAULT_ORDER, "domain:b.example", "", "")),
    )
    .await
    .expect("the save waited for the running check")
    .unwrap();
    assert_eq!(h.app.snapshot().routing.block, ["domain:b.example"]);
}

#[tokio::test]
async fn a_list_the_database_does_not_have_is_refused_only_when_there_is_a_database() {
    let h = start(store(Mode::Proxy));
    // No database yet: the name cannot be judged, the profile is accepted.
    h.app
        .execute(save(
            DEFAULT_ORDER,
            "",
            "geosite:category-ru, geosite:made-up",
            "",
        ))
        .await
        .unwrap();
    // With one, a made-up list is a typing mistake.
    std::fs::create_dir_all(h.dir.path().join("geodata")).unwrap();
    std::fs::write(
        h.dir.path().join("geodata").join("geosite.dat"),
        geosite_file(),
    )
    .unwrap();
    let error = h
        .app
        .execute(save(
            DEFAULT_ORDER,
            "",
            "geosite:category-ru, geosite:made-up",
            "domain:x.example",
        ))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("geosite:made-up"), "{error}");
    h.app
        .execute(save(
            DEFAULT_ORDER,
            "",
            "geosite:category-ru, geosite:google",
            "",
        ))
        .await
        .unwrap();
}

// ---- the databases ---------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_database_is_downloaded_in_the_background_and_shown() {
    let h = start(store(Mode::Proxy));
    let source = FakeSource::with(Some(geosite_file()), Some(geoip_file()));
    h.app.set_geo_source(source.clone());
    assert!(h.app.snapshot().geodata.available);
    assert!(
        h.app.snapshot().geodata.wanted,
        "the default profile has a geosite list"
    );
    assert!(h.app.snapshot().geodata.geosite.is_none());
    h.app.start();
    wait(&h.app, "the database", |s| {
        s.geodata.geosite.is_some() && !s.geodata.updating
    })
    .await;
    // Only the database the profile refers to was fetched.
    assert_eq!(source.calls(), [(Kind::Geosite, None)]);
    assert!(h.app.snapshot().geodata.geoip.is_none());
    assert_eq!(h.app.snapshot().geodata.failure_code, None);
    assert!(h.dir.path().join("geodata").join("geosite.dat").exists());
}

#[tokio::test]
async fn nothing_is_downloaded_without_a_source_or_without_a_list_that_needs_it() {
    let h = start(store(Mode::Proxy));
    assert!(!h.app.snapshot().geodata.available);
    h.app.start();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        h.app.snapshot().geodata.geosite.is_none(),
        "no source: nothing to fetch from"
    );
    assert!(
        h.app.execute(Action::UpdateGeodata).await.is_err(),
        "and a manual update says so"
    );

    let source = FakeSource::with(Some(geosite_file()), Some(geoip_file()));
    let quiet = start(store(Mode::Proxy));
    quiet.app.set_geo_source(source.clone());
    quiet
        .app
        .execute(save(DEFAULT_ORDER, "", "domain:.ru", ""))
        .await
        .unwrap();
    assert!(!quiet.app.snapshot().geodata.wanted);
    quiet.app.start();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(source.calls().is_empty(), "no list refers to a database");
}

#[tokio::test]
async fn a_new_database_restarts_a_running_connection_with_its_lists() {
    let h = start(store(Mode::Proxy));
    h.app.execute(Action::Connect).await.unwrap();
    let started = h.counts.started.load(Ordering::SeqCst);
    let source = FakeSource::with(Some(geosite_file()), Some(geoip_file()));
    h.app.set_geo_source(source);
    h.app.execute(Action::UpdateGeodata).await.unwrap();
    assert!(h.app.snapshot().geodata.geosite.is_some());
    assert_eq!(
        h.counts.started.load(Ordering::SeqCst),
        started + 1,
        "restarted to take the lists in"
    );
    assert!(h
        .app
        .snapshot()
        .journal
        .iter()
        .any(|entry| entry.message == "Базы маршрутизации обновлены"));
}

/// Cycles of the check of the servers begun so far, by their journal line.
fn checks_begun(app: &Application) -> usize {
    app.snapshot()
        .journal
        .iter()
        .filter(|e| e.message.starts_with("Проверка серверов запущена"))
        .count()
}

#[tokio::test(start_paused = true)]
async fn a_new_database_leaves_what_the_servers_were_measured_at_and_asks_nobody_again() {
    // The databases restart sing-box so that it takes the lists in; the servers are the same, the
    // one in use is the same, and nothing about them has to be found out again.
    // More servers than the pool holds: a list no longer than that is looked at in full every minute.
    let mut many = store(Mode::Proxy);
    many.servers = (1..=5u16)
        .map(|port| parse_uri(&format!("trojan://p@h{port}.example.com:{port}"), None).unwrap())
        .collect();
    let h = start(many);
    h.app.start();
    h.app.execute(Action::Connect).await.unwrap();
    wait(&h.app, "the first check", |s| {
        !s.checking && s.servers.iter().all(|v| v.proxy_ms.is_some())
    })
    .await;
    let before = h.app.snapshot();
    let checks = checks_begun(&h.app);
    let started = h.counts.started.load(Ordering::SeqCst);
    h.app
        .set_geo_source(FakeSource::with(Some(geosite_file()), Some(geoip_file())));
    h.app.execute(Action::UpdateGeodata).await.unwrap();
    let immediate = h.app.snapshot();
    for (old, new) in before.servers.iter().zip(&immediate.servers) {
        assert_eq!(
            (&old.status, old.proxy_ms, old.endpoint_ms, old.checked_at),
            (&new.status, new.proxy_ms, new.endpoint_ms, new.checked_at),
            "updating geodata itself preserves every measured field"
        );
    }
    tokio::time::sleep(Duration::from_secs(70)).await;
    assert_eq!(
        h.counts.started.load(Ordering::SeqCst),
        started + 1,
        "restarted once"
    );
    let after = h.app.snapshot();
    assert_eq!(
        checks_begun(&h.app),
        checks,
        "no check of the servers began"
    );
    for (old, new) in before.servers.iter().zip(&after.servers) {
        assert_eq!(
            (&old.status, old.proxy_ms, old.endpoint_ms),
            (&new.status, new.proxy_ms, new.endpoint_ms)
        );
        assert!(new.checked_at.unwrap() >= old.checked_at.unwrap());
    }
    assert!(
        after
            .journal
            .iter()
            .any(|entry| entry.message.starts_with("Быстрая проверка:")),
        "the normal periodic fast check remains scheduled"
    );
    assert_eq!(after.selected, before.selected);
    assert_eq!(after.status, "connected");
}

#[tokio::test]
async fn a_failed_download_is_shown_with_its_cause_and_not_repeated_every_tick() {
    let h = start(store(Mode::Proxy));
    let source = FakeSource::with(None, None);
    h.app.set_geo_source(source.clone());
    h.app.start();
    wait(&h.app, "the failure", |s| {
        s.geodata.failure_code.is_some() && !s.geodata.updating
    })
    .await;
    assert_eq!(
        h.app.snapshot().geodata.failure_code,
        Some(FailureCode::GeodataDownload)
    );
    assert!(h
        .app
        .snapshot()
        .journal
        .iter()
        .any(|e| e.failure_code == Some(FailureCode::GeodataDownload)));
    let attempts = source.calls().len();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(
        source.calls().len(),
        attempts,
        "a base that cannot be reached is not asked every second"
    );
}

#[tokio::test]
async fn through_the_tunnel_when_the_direct_way_is_blocked_but_not_in_tun_mode() {
    let h = start(store(Mode::Proxy));
    h.app.execute(Action::Connect).await.unwrap();
    let source = FakeSource::with(Some(geosite_file()), None);
    source.tunnel_only.store(true, Ordering::SeqCst);
    h.app.set_geo_source(source.clone());
    h.app.execute(Action::UpdateGeodata).await.ok();
    assert!(
        h.app.snapshot().geodata.geosite.is_some(),
        "the proxy retry installed geosite"
    );
    let port = h.app.snapshot().settings.proxy_port;
    assert!(source.calls().contains(&(Kind::Geosite, None)));
    assert!(
        source.calls().contains(&(Kind::Geosite, Some(port))),
        "retried through the local proxy"
    );

    // Pure TUN has no local proxy to go through: the direct attempt is the only one.
    let tun = start(store(Mode::Tun));
    tun.app.execute(Action::Connect).await.unwrap();
    let blocked = FakeSource::with(Some(geosite_file()), None);
    blocked.tunnel_only.store(true, Ordering::SeqCst);
    tun.app.set_geo_source(blocked.clone());
    assert!(tun.app.execute(Action::UpdateGeodata).await.is_err());
    assert!(blocked.calls().iter().all(|(_, via)| via.is_none()));
}

#[tokio::test]
async fn a_database_the_runtime_check_refuses_is_taken_back() {
    let h = start(store(Mode::Proxy));
    // The previous database is on disk and works.
    std::fs::create_dir_all(h.dir.path().join("geodata")).unwrap();
    let path = h.dir.path().join("geodata").join("geosite.dat");
    let old = geosite_file();
    std::fs::write(&path, &old).unwrap();
    h.app.execute(Action::Connect).await.unwrap();
    let started = h.counts.started.load(Ordering::SeqCst);
    // The new database carries a domain the runtime refuses to run with.
    *h.counts.refuse.lock().unwrap() = Some("poison.example".into());
    let poisoned = common::file(&[common::site("CATEGORY-RU", &[(2, "poison.example")])]);
    h.app
        .set_geo_source(FakeSource::with(Some(poisoned), Some(geoip_file())));
    let error = h.app.execute(Action::UpdateGeodata).await.unwrap_err();
    assert_eq!(
        error.failure_code(),
        Some(FailureCode::SidecarConfigRejected)
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        old,
        "the database that was there is back"
    );
    assert_eq!(
        h.counts.started.load(Ordering::SeqCst),
        started,
        "the running sing-box was not touched"
    );
    assert!(h.app.snapshot().runtime_active);
}

#[tokio::test]
async fn a_database_that_was_not_there_is_removed_when_the_check_refuses_it() {
    let mut with_poison = store(Mode::Proxy);
    with_poison.routing.direct = vec!["geosite:category-ru".into(), "domain:poison.example".into()];
    let h = start(with_poison);
    *h.counts.refuse.lock().unwrap() = Some("poison.example".into());
    h.app
        .set_geo_source(FakeSource::with(Some(geosite_file()), None));
    assert!(h.app.execute(Action::UpdateGeodata).await.is_err());
    assert!(!h.dir.path().join("geodata").join("geosite.dat").exists());
    assert!(h.app.snapshot().geodata.geosite.is_none());
}

#[tokio::test]
async fn two_requests_at_once_download_once_and_shutdown_cancels_a_running_download() {
    let h = start(store(Mode::Proxy));
    let source = FakeSource::with(Some(geosite_file()), Some(geoip_file()));
    *source.delay.lock().unwrap() = Some(Duration::from_millis(300));
    h.app.set_geo_source(source.clone());
    let (a, b) = tokio::join!(
        h.app.execute(Action::UpdateGeodata),
        h.app.execute(Action::UpdateGeodata)
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(
        source.calls().len(),
        2,
        "one download of each database, not two"
    );

    let slow = start(store(Mode::Proxy));
    let hanging = FakeSource::with(Some(geosite_file()), Some(geoip_file()));
    *hanging.delay.lock().unwrap() = Some(Duration::from_secs(30));
    slow.app.set_geo_source(hanging);
    let app = slow.app.clone();
    let task = tokio::spawn(async move { app.execute(Action::UpdateGeodata).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    slow.app.shutdown().await.unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancelled at once")
        .unwrap();
    assert!(outcome.is_err());
    assert!(!slow.app.snapshot().geodata.updating);
}

fn said(h: &H, text: &str) -> usize {
    h.app
        .snapshot()
        .journal
        .iter()
        .filter(|entry| entry.message == text)
        .count()
}

#[tokio::test]
async fn an_update_by_hand_does_not_download_a_version_that_is_already_installed() {
    let h = start(store(Mode::Proxy));
    h.app.execute(Action::Connect).await.unwrap();
    let source = FakeSource::with(Some(geosite_file()), Some(geoip_file()));
    h.app.set_geo_source(source.clone());
    h.app.execute(Action::UpdateGeodata).await.unwrap();
    let geodata = h.app.snapshot().geodata;
    assert!(geodata.geosite.is_some() && geodata.geoip.is_some());
    assert_eq!(said(&h, "geosite: загружена новая версия"), 1);
    assert_eq!(said(&h, "geoip: загружена новая версия"), 1);
    let started = h.counts.started.load(Ordering::SeqCst);
    let written = std::fs::metadata(h.dir.path().join("geodata").join("geoip.dat"))
        .unwrap()
        .modified()
        .unwrap();

    h.app.execute(Action::UpdateGeodata).await.unwrap();
    assert_eq!(said(&h, "geosite: уже последняя версия"), 1);
    assert_eq!(said(&h, "geoip: уже последняя версия"), 1);
    assert_eq!(
        said(&h, "Базы маршрутизации обновлены"),
        1,
        "nothing new, nothing applied"
    );
    assert_eq!(
        h.counts.started.load(Ordering::SeqCst),
        started,
        "the connection was not restarted"
    );
    assert_eq!(
        std::fs::metadata(h.dir.path().join("geodata").join("geoip.dat"))
            .unwrap()
            .modified()
            .unwrap(),
        written,
        "the file was not written again"
    );
    let geodata = h.app.snapshot().geodata;
    assert!(geodata.geosite.unwrap().checked_at.is_some());
    assert!(geodata.geoip.unwrap().checked_at.is_some());
    assert_eq!(geodata.failure_code, None);
}

#[tokio::test]
async fn the_time_a_database_was_last_confirmed_is_still_shown_after_a_restart() {
    let h = start(store(Mode::Proxy));
    h.app.execute(Action::Connect).await.unwrap();
    h.app
        .set_geo_source(FakeSource::with(Some(geosite_file()), Some(geoip_file())));
    h.app.execute(Action::UpdateGeodata).await.unwrap();
    // The second look finds the installed versions to be the latest: that is the confirmation.
    h.app.execute(Action::UpdateGeodata).await.unwrap();
    let before = h.app.snapshot().geodata;
    let confirmed = (
        before.geosite.unwrap().checked_at.unwrap(),
        before.geoip.unwrap().checked_at.unwrap(),
    );
    h.app.shutdown().await.unwrap();
    // Another Application over the same data folder: the time is not "this run only".
    let again = start_with(store(Mode::Proxy), h.dir);
    let after = again.app.snapshot().geodata;
    assert_eq!(
        (
            after.geosite.unwrap().checked_at.unwrap(),
            after.geoip.unwrap().checked_at.unwrap()
        ),
        confirmed
    );
}

#[tokio::test]
async fn the_database_being_fetched_is_shown_and_a_failure_names_its_database() {
    let h = start(store(Mode::Proxy));
    let source = FakeSource::with(Some(geosite_file()), None);
    *source.delay.lock().unwrap() = Some(Duration::from_millis(400));
    h.app.set_geo_source(source);
    let app = h.app.clone();
    let task = tokio::spawn(async move { app.execute(Action::UpdateGeodata).await });
    wait(&h.app, "geosite to be fetched", |s| {
        s.geodata.current.as_deref() == Some("geosite")
    })
    .await;
    wait(&h.app, "geoip to be fetched", |s| {
        s.geodata.current.as_deref() == Some("geoip")
    })
    .await;
    assert!(task.await.unwrap().is_err(), "geoip could not be fetched");
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.geodata.current, None);
    assert!(snapshot.geodata.geosite.is_some(), "geosite stays");
    assert_eq!(said(&h, "geoip: не загружена (geodata_download)"), 1);
    assert_eq!(said(&h, "geosite: загружена новая версия"), 1);
}

#[tokio::test]
async fn diagnostics_show_the_lists_in_use_and_the_state_of_the_databases() {
    let h = start(store(Mode::Proxy));
    let source = FakeSource::with(Some(geosite_file()), Some(geoip_file()));
    h.app.set_geo_source(source);
    h.app.execute(Action::UpdateGeodata).await.unwrap();
    h.app.execute(Action::ExportDiagnostics).await.unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(h.dir.path().join("diagnostics.json")).unwrap())
            .unwrap();
    let sets = report["config"]["route"]["rule_set"].as_array().unwrap();
    assert_eq!(sets.len(), 1, "{sets:?}");
    assert_eq!(sets[0], serde_json::json!({"type": "inline"}));
    assert!(!report.to_string().contains("geosite-category-ru"));
    let geodata = &report["geodata"];
    assert_eq!(geodata["uses_geosite"], true);
    assert_eq!(geodata["uses_geoip"], false);
    assert!(geodata["geosite"]["bytes"].as_u64().unwrap() > 0);
    assert!(geodata["geoip"]["checked_at"].as_u64().is_some());
    assert_eq!(geodata["failure_code"], serde_json::Value::Null);
    let text = report.to_string();
    assert!(!text.contains("yandex.ru"), "no lists of domains");
}

#[tokio::test]
async fn the_view_says_which_databases_the_profile_uses() {
    let h = start(store(Mode::Proxy));
    let geodata = h.app.snapshot().geodata;
    assert!(geodata.uses_geosite, "the default profile has geosite:");
    assert!(!geodata.uses_geoip, "and no geoip:");
    h.app
        .execute(save(DEFAULT_ORDER, "", "geoip:ru", ""))
        .await
        .unwrap();
    let geodata = h.app.snapshot().geodata;
    assert!(!geodata.uses_geosite);
    assert!(geodata.uses_geoip);
}

#[tokio::test]
async fn a_database_in_the_data_folder_is_used_from_the_start() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("geodata")).unwrap();
    std::fs::write(
        dir.path().join("geodata").join("geosite.dat"),
        geosite_file(),
    )
    .unwrap();
    std::fs::write(dir.path().join("geodata").join("geoip.dat"), geoip_file()).unwrap();
    let h = start_with(store(Mode::Proxy), dir);
    let view = h.app.snapshot().geodata;
    assert!(view.geosite.is_some() && view.geoip.is_some());
    assert!(view.geosite.unwrap().bytes > 0);
}

// ---- looking for new databases ---------------------------------------------------------------

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
/// Both databases on disk, confirmed as the latest that many seconds ago (`None`: never).
fn with_databases(
    store: Store,
    geosite_checked_ago: Option<u64>,
    geoip_checked_ago: Option<u64>,
) -> H {
    let dir = tempfile::tempdir().unwrap();
    let cache = GeoCache::open(dir.path().join("geodata"));
    cache.install(Kind::Geosite, &geosite_file()).unwrap();
    cache.install(Kind::Geoip, &geoip_file()).unwrap();
    for (kind, ago) in [
        (Kind::Geosite, geosite_checked_ago),
        (Kind::Geoip, geoip_checked_ago),
    ] {
        if let Some(ago) = ago {
            cache.set_checked_at(kind, now() - ago).unwrap();
        }
    }
    start_with(store, dir)
}
const HOUR: u64 = 3600;

#[tokio::test(start_paused = true)]
async fn a_database_confirmed_twelve_hours_ago_is_asked_about_and_a_recent_one_is_not() {
    let h = with_databases(store(Mode::Proxy), Some(13 * HOUR), Some(HOUR));
    let source = FakeSource::with(Some(geosite_file()), Some(geoip_file()));
    h.app.set_geo_source(source.clone());
    h.app.start();
    tokio::time::sleep(Duration::from_secs(5)).await;
    let asked: Vec<Kind> = source.calls().iter().map(|(kind, _)| *kind).collect();
    assert_eq!(asked, [Kind::Geosite], "only the one that is due");
    // Nothing new: the answer is "the same", and that is now written down.
    let cache = GeoCache::open(h.dir.path().join("geodata"));
    assert!(now() - cache.checked_at(Kind::Geosite).unwrap() < 60);
    assert!(h
        .app
        .snapshot()
        .geodata
        .geosite
        .unwrap()
        .checked_at
        .is_some());
}

#[tokio::test(start_paused = true)]
async fn an_unchanged_database_is_neither_written_nor_the_connection_restarted() {
    let h = with_databases(store(Mode::Proxy), Some(13 * HOUR), Some(13 * HOUR));
    h.app.execute(Action::Connect).await.unwrap();
    let started = h.counts.started.load(Ordering::SeqCst);
    let path = h.dir.path().join("geodata").join("geosite.dat");
    let before = (
        std::fs::read(&path).unwrap(),
        std::fs::metadata(&path).unwrap().modified().unwrap(),
    );
    let source = FakeSource::with(Some(geosite_file()), Some(geoip_file()));
    h.app.set_geo_source(source.clone());
    h.app.start();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(source.calls().len(), 2, "both were due");
    let after = (
        std::fs::read(&path).unwrap(),
        std::fs::metadata(&path).unwrap().modified().unwrap(),
    );
    assert_eq!(before, after, "the file was not written");
    assert_eq!(
        h.counts.started.load(Ordering::SeqCst),
        started,
        "no restart"
    );
}

#[tokio::test(start_paused = true)]
async fn the_next_look_is_twelve_hours_after_the_last_and_a_restart_does_not_forget_it() {
    let h = with_databases(store(Mode::Proxy), Some(13 * HOUR), Some(13 * HOUR));
    let source = FakeSource::with(Some(geosite_file()), Some(geoip_file()));
    h.app.set_geo_source(source.clone());
    h.app.start();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(source.calls().len(), 2);
    // Eleven hours later nothing is due, however often the worker looks.
    tokio::time::sleep(Duration::from_secs(11 * HOUR)).await;
    assert_eq!(source.calls().len(), 2, "not yet");
    // A restart an hour after the look: the folder remembers it, nothing is asked at once.
    h.app.shutdown().await.unwrap();
    let H { dir, .. } = h;
    let again = start_with(store(Mode::Proxy), dir);
    let source = FakeSource::with(Some(geosite_file()), Some(geoip_file()));
    again.app.set_geo_source(source.clone());
    again.app.start();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(source.calls().is_empty(), "asked again right after a start");
    // Twelve hours after the look, written back for a wall clock that a paused test does not move.
    let cache = GeoCache::open(again.dir.path().join("geodata"));
    for kind in [Kind::Geosite, Kind::Geoip] {
        cache.set_checked_at(kind, now() - 13 * HOUR).unwrap();
    }
    again.app.shutdown().await.unwrap();
    let H { dir, .. } = again;
    let later = start_with(store(Mode::Proxy), dir);
    let source = FakeSource::with(Some(geosite_file()), Some(geoip_file()));
    later.app.set_geo_source(source.clone());
    later.app.start();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(
        source.calls().len(),
        2,
        "both asked again at the start that follows"
    );
}

#[tokio::test(start_paused = true)]
async fn with_the_updates_turned_off_nothing_is_asked_but_a_missing_database_is_fetched_once() {
    let mut off = store(Mode::Proxy);
    off.settings.geodata_auto_update = false;
    // The geosite is there and old; the geoip is not there but a rule needs it.
    let dir = tempfile::tempdir().unwrap();
    let cache = GeoCache::open(dir.path().join("geodata"));
    cache.install(Kind::Geosite, &geosite_file()).unwrap();
    cache
        .set_checked_at(Kind::Geosite, now() - 40 * HOUR)
        .unwrap();
    off.routing.direct.push("geoip:ru".into());
    let h = start_with(off, dir);
    let source = FakeSource::with(Some(geosite_file()), Some(geoip_file()));
    h.app.set_geo_source(source.clone());
    h.app.start();
    tokio::time::sleep(Duration::from_secs(5)).await;
    let asked: Vec<Kind> = source.calls().iter().map(|(kind, _)| *kind).collect();
    assert_eq!(asked, [Kind::Geoip], "the missing one, and only that");
    assert!(GeoCache::open(h.dir.path().join("geodata")).has(Kind::Geoip));
    // Days later: still nothing, the geosite is old and left alone.
    tokio::time::sleep(Duration::from_secs(48 * HOUR)).await;
    assert_eq!(source.calls().len(), 1);
    // The button still works.
    h.app.execute(Action::UpdateGeodata).await.unwrap();
    assert!(source.calls().len() >= 2);
}
