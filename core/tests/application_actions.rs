//! Coordinator actions and worker scheduling as seen from outside: servers, subscriptions,
//! journal, measurements, cycle kinds and timers.
use async_trait::async_trait;
use rift_core::{
    application::{Action, Application, Category, Entry, Snapshot},
    config,
    failure::FailureCode,
    jitter::Jitter,
    model::{
        Availability, Filters, Mode, Server, Settings, SortColumn, Store, Subscription, Usage,
    },
    parser::parse_uri,
    persistence,
    routing::{ServerRoutes, NO_ROUTES_MODE},
    runtime::Runtime,
    selection::{Network, Probe},
    subscription::{FetchRequest, Fetched, SubscriptionSource},
    Error, Result,
};
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

/// Validates like `sing-box check`: the generated config must exist. Proxy port 10 stands
/// for a config sing-box rejects.
struct CheckingRuntime {
    alive: Arc<AtomicBool>,
    starts: Arc<AtomicUsize>,
}
#[async_trait]
impl Runtime for CheckingRuntime {
    async fn validate(&mut self, store: &Store, secret: &str) -> Result<()> {
        if store.settings.proxy_port == 10 {
            return Err(Error::Invalid("Rejected config".into()));
        }
        config::generate(store, secret).map(|_| ())
    }
    async fn start(&mut self, _: &Store, _: &str) -> Result<()> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.alive.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        self.alive.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(self.alive.load(Ordering::SeqCst))
    }
}

/// Latencies by port (`None` = fails), call logs, one optional gated proxy call.
#[derive(Default)]
struct Net {
    endpoint: Mutex<HashMap<u16, Option<u64>>>,
    endpoint_calls: Mutex<Vec<u16>>,
    proxy: Mutex<HashMap<u16, Option<u64>>>,
    proxies: Mutex<Vec<u16>>,
    selects: Mutex<Vec<String>>,
    gate: Mutex<Option<u16>>,
    entered: Notify,
    release: Notify,
    /// The machine has no network while this is set.
    offline: AtomicBool,
    /// The machine has a network but the sidecar cannot get out while this is set.
    deaf: AtomicBool,
    network_calls: AtomicUsize,
    /// How a proxy test of a port that does not answer fails (a plain network error if absent).
    proxy_failure: Mutex<HashMap<u16, FailureCode>>,
    /// The rules of a server cannot be set (the mode with no rules always can).
    routes_refused: AtomicBool,
}
impl Net {
    fn fail_proxy_with(&self, port: u16, code: FailureCode) {
        self.proxy_failure.lock().unwrap().insert(port, code);
    }
    fn set(&self, port: u16, endpoint: Option<u64>, proxy: Option<u64>) {
        self.endpoint.lock().unwrap().insert(port, endpoint);
        self.proxy.lock().unwrap().insert(port, proxy);
    }
    fn proxied(&self) -> Vec<u16> {
        self.proxies.lock().unwrap().clone()
    }
}
#[async_trait]
impl Probe for Net {
    async fn endpoint(&self, s: &Server) -> Result<u64> {
        self.endpoint_calls.lock().unwrap().push(s.port);
        self.endpoint
            .lock()
            .unwrap()
            .get(&s.port)
            .copied()
            .flatten()
            .ok_or(Error::Network)
    }
    async fn proxy(&self, s: &Server) -> Result<u64> {
        self.proxies.lock().unwrap().push(s.port);
        let gated = {
            let mut gate = self.gate.lock().unwrap();
            (*gate == Some(s.port)).then(|| gate.take()).is_some()
        };
        if gated {
            self.entered.notify_one();
            self.release.notified().await;
        }
        let answer = self.proxy.lock().unwrap().get(&s.port).copied().flatten();
        let code = self.proxy_failure.lock().unwrap().get(&s.port).copied();
        answer.ok_or_else(|| match code {
            Some(code) => Error::failure(code, "no answer through the server"),
            None => Error::Network,
        })
    }
    async fn select(&self, id: &str) -> Result<()> {
        self.selects.lock().unwrap().push(id.into());
        Ok(())
    }
    async fn routes(&self, mode: &str) -> Result<()> {
        if mode != NO_ROUTES_MODE && self.routes_refused.load(Ordering::SeqCst) {
            return Err(Error::failure(FailureCode::SelectorSwitch, "no"));
        }
        Ok(())
    }
    async fn network(&self) -> Network {
        self.network_calls.fetch_add(1, Ordering::SeqCst);
        if self.offline.load(Ordering::SeqCst) {
            Network::Down("no answer".into())
        } else if self.deaf.load(Ordering::SeqCst) {
            Network::SidecarDeaf("no answer from the sidecar".into())
        } else {
            Network::Up
        }
    }
}

/// Subscription bodies by URL (`None` = download fails), a fetch log, and one optional
/// gated URL (mirrors `Net`'s gate) for tests that need a fetch to park mid-flight.
/// The title and the traffic a provider reports in its headers.
type Headers = (Option<String>, Option<Usage>);
#[derive(Default)]
struct Feed {
    bodies: Mutex<HashMap<String, Option<String>>>,
    /// What the headers of a URL say: the provider's title and traffic.
    meta: Mutex<HashMap<String, Headers>>,
    /// The `User-Agent` each fetch asked for.
    agents: Mutex<Vec<(String, Option<String>)>>,
    fetches: Mutex<Vec<(String, Option<u16>)>>,
    gate: Mutex<Option<String>>,
    entered: Notify,
    release: Notify,
}
impl Feed {
    fn count(&self, url: &str) -> usize {
        self.fetches
            .lock()
            .unwrap()
            .iter()
            .filter(|(u, _)| u == url)
            .count()
    }
    fn total(&self) -> usize {
        self.fetches.lock().unwrap().len()
    }
}
#[async_trait]
impl SubscriptionSource for Feed {
    async fn fetch(&self, url: &str, proxy_port: Option<u16>) -> Result<String> {
        self.fetches.lock().unwrap().push((url.into(), proxy_port));
        let gated = {
            let mut gate = self.gate.lock().unwrap();
            (gate.as_deref() == Some(url))
                .then(|| gate.take())
                .is_some()
        };
        if gated {
            self.entered.notify_one();
            self.release.notified().await;
        }
        self.bodies
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .flatten()
            .ok_or(Error::Network)
    }
    async fn fetch_with(&self, request: FetchRequest<'_>) -> Result<Fetched> {
        self.agents
            .lock()
            .unwrap()
            .push((request.url.into(), request.user_agent.map(Into::into)));
        let body = self.fetch(request.url, request.proxy_port).await?;
        let (title, usage) = self
            .meta
            .lock()
            .unwrap()
            .get(request.url)
            .cloned()
            .unwrap_or_default();
        Ok(Fetched {
            body,
            title,
            usage,
            ..Fetched::default()
        })
    }
}

struct H {
    app: Arc<Application>,
    net: Arc<Net>,
    feed: Arc<Feed>,
    alive: Arc<AtomicBool>,
    dir: tempfile::TempDir,
}
fn server(port: u16, source: Option<&str>) -> Server {
    parse_uri(
        &format!("trojan://p@example.com:{port}"),
        source.map(Into::into),
    )
    .unwrap()
}
fn subscription(id: &str, url: &str) -> Subscription {
    Subscription {
        id: id.into(),
        name: id.into(),
        url: url.into(),
        updated_at: None,
        error: None,
        failure_code: None,
        ..Default::default()
    }
}
fn start(store: Store, net: Net, feed: Feed) -> H {
    let dir = tempfile::tempdir().unwrap();
    start_in(store, net, feed, dir)
}
fn start_in(store: Store, net: Net, feed: Feed, dir: tempfile::TempDir) -> H {
    start_with(store, net, feed, dir, Jitter::none())
}
/// Like `start`, with the spread of periodic intervals fixed to `offset_ms`.
fn start_jittered(store: Store, net: Net, feed: Feed, offset_ms: i64) -> H {
    let dir = tempfile::tempdir().unwrap();
    start_with(store, net, feed, dir, Jitter::fixed(offset_ms))
}
fn start_with(store: Store, net: Net, feed: Feed, dir: tempfile::TempDir, jitter: Jitter) -> H {
    let (net, feed) = (Arc::new(net), Arc::new(feed));
    let alive = Arc::new(AtomicBool::new(false));
    let app = Application::with_jitter(
        store,
        dir.path().into(),
        Box::new(CheckingRuntime {
            alive: alive.clone(),
            starts: Arc::new(AtomicUsize::new(0)),
        }),
        net.clone(),
        SECRET.into(),
        feed.clone(),
        jitter,
    );
    app.start();
    H {
        app,
        net,
        feed,
        alive,
        dir,
    }
}
fn manual(ports: &[u16], auto: bool) -> (Store, Net) {
    let mut store = Store::default();
    store.settings.auto_select = auto;
    let net = Net::default();
    for (i, &port) in ports.iter().enumerate() {
        store.servers.push(server(port, None));
        net.set(port, Some(10 + i as u64), Some(40 + i as u64));
    }
    (store, net)
}
async fn until(app: &Application, what: &str, predicate: impl Fn(&Snapshot) -> bool) {
    let mut rx = app.snapshots.subscribe();
    tokio::time::timeout(Duration::from_secs(5), rx.wait_for(|s| predicate(s)))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}: {:?}", app.snapshot()))
        .unwrap();
}
async fn idle(app: &Application) {
    until(app, "cycle to finish", |s| !s.checking && !s.pending).await;
}
fn id_of(s: &Snapshot, port_server: &Server) -> bool {
    s.selected.as_deref() == Some(port_server.id.as_str())
}

// ---- journal, snapshot shape ------------------------------------------------------------

#[tokio::test]
async fn journal_entries_carry_the_current_time() {
    let (store, net) = manual(&[1], false);
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let at = h.app.snapshot().journal.last().unwrap().at;
    assert!(now.abs_diff(at) <= 5, "{at} vs {now}");
}

#[test]
fn journal_without_failure_codes_remains_readable() {
    let entries: Vec<Entry> = serde_json::from_str(
        r#"[{"at":1,"level":"error","message":"Старая запись"},{"at":2,"level":"error","message":"Будущая запись","failure_code":"future_code"}]"#,
    )
    .unwrap();
    assert_eq!(entries[0].failure_code, None);
    assert_eq!(entries[1].failure_code, Some(FailureCode::Unknown));
}

/// A journal of `count` entries written `age` seconds before now and one second apart, each with
/// a message of `size` bytes.
fn aged_journal(count: u64, age: u64, size: usize) -> Vec<Entry> {
    let now = rift_core::application::timestamp();
    (0..count)
        .map(|i| Entry {
            at: now - age + i,
            level: "info".into(),
            message: format!("entry {i:04} {}", "x".repeat(size)),
            details: None,
            crash_id: None,
            failure_code: None,
            category: Category::default(),
        })
        .collect()
}
fn write_journal(dir: &tempfile::TempDir, entries: &[Entry]) {
    std::fs::write(
        dir.path().join("journal.json"),
        serde_json::to_vec(entries).unwrap(),
    )
    .unwrap();
}
fn saved_journal(dir: &tempfile::TempDir) -> Vec<u8> {
    std::fs::read(dir.path().join("journal.json")).unwrap()
}

#[tokio::test]
async fn journal_file_over_the_limits_is_cut_to_them_when_rift_starts() {
    let dir = tempfile::tempdir().unwrap();
    // 400 entries of 600 bytes (240 KB, over 100 KiB): the first 100 are two days old.
    let mut entries = aged_journal(100, 2 * 86_400, 600);
    entries.extend(aged_journal(300, 3_600, 600));
    write_journal(&dir, &entries);
    assert!(saved_journal(&dir).len() > 200_000);

    let (store, net) = manual(&[1], false);
    let h = start_in(store, net, Feed::default(), dir);
    let journal = h.app.snapshot().journal;
    let size = serde_json::to_vec(&journal).unwrap().len();
    assert!(size <= rift_core::journal::MAX_BYTES, "{size}");
    // The newest of the old file are what is left, none of the two-day-old ones.
    assert_eq!(journal.last().unwrap().message[..10], *"entry 0299");
    let now = rift_core::application::timestamp();
    assert!(journal.iter().all(|e| e.at + 86_400 >= now));
    assert!(journal.len() > 100, "{}", journal.len());

    // The file shrinks with the next entry, and the next entry is the newest of both.
    h.app
        .execute(Action::AddServer {
            uri: "trojan://p@example.com:1".into(),
        })
        .await
        .unwrap();
    let saved = saved_journal(&h.dir);
    assert!(
        saved.len() <= rift_core::journal::MAX_BYTES,
        "{}",
        saved.len()
    );
    let after = h.app.snapshot().journal;
    assert_eq!(saved, serde_json::to_vec(&after).unwrap());
    assert_eq!(after.last().unwrap().message, "Сервер добавлен");
}

#[tokio::test]
async fn journal_drops_a_day_old_entry_and_keeps_the_rest_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let now = rift_core::application::timestamp();
    let entry = |at: u64, message: &str| Entry {
        at,
        level: "info".into(),
        message: message.into(),
        details: None,
        crash_id: None,
        failure_code: None,
        category: Category::default(),
    };
    write_journal(
        &dir,
        &[
            entry(now - 86_400 - 60, "too old"),
            entry(now - 86_400 + 60, "just inside the day"),
        ],
    );
    let (store, net) = manual(&[1], false);
    let h = start_in(store, net, Feed::default(), dir);
    let messages = |h: &H| -> Vec<String> {
        h.app
            .snapshot()
            .journal
            .into_iter()
            .map(|e| e.message)
            .collect()
    };
    assert_eq!(messages(&h), ["just inside the day"]);
    h.app
        .execute(Action::AddServer {
            uri: "trojan://p@example.com:1".into(),
        })
        .await
        .unwrap();
    assert_eq!(messages(&h), ["just inside the day", "Сервер добавлен"]);

    // What is saved is what the next start reads.
    let H { app, dir, .. } = h;
    app.shutdown().await.unwrap();
    let (store, net) = manual(&[1], false);
    let again = start_in(store, net, Feed::default(), dir);
    assert_eq!(messages(&again), ["just inside the day", "Сервер добавлен"]);
}

#[tokio::test]
async fn journal_file_too_big_to_read_starts_an_empty_journal() {
    let dir = tempfile::tempdir().unwrap();
    // The reading limit (1 MiB) is older than the new limits and stays.
    write_journal(&dir, &aged_journal(2_000, 60, 600));
    assert!(saved_journal(&dir).len() > 1_048_576);
    let (store, net) = manual(&[1], false);
    let h = start_in(store, net, Feed::default(), dir);
    assert!(h.app.snapshot().journal.is_empty());
}

#[tokio::test]
async fn subscription_view_counts_only_its_own_servers() {
    let store = Store {
        subscriptions: vec![
            subscription("a", "https://a.example/s"),
            subscription("b", "https://b.example/s"),
        ],
        servers: vec![
            server(1, Some("a")),
            server(2, Some("a")),
            server(3, Some("b")),
            server(4, None),
        ],
        ..Store::default()
    };
    let h = start(store, Net::default(), Feed::default());
    let counts: Vec<usize> = h
        .app
        .snapshot()
        .subscriptions
        .iter()
        .map(|s| s.count)
        .collect();
    assert_eq!(counts, vec![2, 1]);
}

#[tokio::test]
async fn diagnostics_name_the_operating_system() {
    let (store, net) = manual(&[1], false);
    let h = start(store, net, Feed::default());
    h.app.execute(Action::ExportDiagnostics).await.unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(h.dir.path().join("diagnostics.json")).unwrap())
            .unwrap();
    let os = report["os"].as_str().unwrap();
    if cfg!(windows) {
        assert!(os.starts_with("Windows "), "{os}");
    } else {
        assert_eq!(os, std::env::consts::OS);
    }
}

#[tokio::test]
async fn frontend_error_details_are_cleaned_persisted_and_exported_only_on_request() {
    let mut store = Store::default();
    store
        .servers
        .push(parse_uri("trojan://frontend-password-canary@server.example:443", None).unwrap());
    store.subscriptions.push(subscription(
        "a",
        "https://provider.example/sub?token=frontend-token-canary",
    ));
    let h = start(store, Net::default(), Feed::default());
    let details = format!(
        "stage=parse; field=settings.vnext[0].users[0].id; line 4 column 12; frontend-password-canary frontend-token-canary {SECRET} https://unknown.example/private?token=other-token trojan://other-password@unknown.example:443 {{\"password\":\"body-only-canary\"}}\n{}",
        "Ж".repeat(rift_core::journal::MAX_DETAIL_BYTES),
    );
    h.app
        .log_external_error("Frontend diagnostic fixture", Some(&details));
    let snapshot = h.app.snapshot();
    let entry = snapshot
        .journal
        .iter()
        .find(|entry| entry.message == "Frontend diagnostic fixture")
        .unwrap();
    let cleaned = entry.details.as_deref().unwrap();
    assert!(cleaned.len() <= rift_core::journal::MAX_DETAIL_BYTES);
    for expected in ["parse", "settings.vnext[0].users[0].id", "line 4 column 12"] {
        assert!(cleaned.contains(expected), "{cleaned}");
    }
    let destination = h.dir.path().join("diagnostics.json");
    assert!(!destination.exists(), "errors never export themselves");
    h.app.execute(Action::ExportDiagnostics).await.unwrap();
    let journal = std::fs::read_to_string(h.dir.path().join("journal.json")).unwrap();
    let export = std::fs::read_to_string(destination).unwrap();
    assert!(journal.contains("line 4 column 12"));
    assert!(export.contains("line 4 column 12"));
    for secret in [
        "frontend-password-canary",
        "frontend-token-canary",
        SECRET,
        "other-token",
        "other-password",
        "body-only-canary",
        "https://",
        "trojan://",
    ] {
        assert!(!journal.contains(secret), "{secret} in journal");
        assert!(!export.contains(secret), "{secret} in export");
    }
}

#[tokio::test]
async fn packaged_frontend_positions_survive_journal_and_export_without_foreign_urls() {
    let h = start(Store::default(), Net::default(), Feed::default());
    // The first detail is Error.stack; the second is ErrorEvent's filename fallback.
    for detail in [
        "TypeError: boom\n    at refresh (http://tauri.localhost/assets/index-ab12.js:12:34)\nhttps://foreign.example/private.js:44:55\nC:\\Users\\runneradmin\\private.js:1:2",
        "Script error\ntauri://localhost/assets/index-cd34.js:56:78",
        "Error: boom\nhttps://tauri.localhost/assets/index-ef56.js:90:12\nhttps://tauri.localhost/assets/index-ab12.js?token=query-canary:1:2\nhttps://credentials-canary@tauri.localhost/assets/index-ab12.js:1:2",
    ] {
        h.app.log_external_error("Frontend position fixture", Some(detail));
    }
    let snapshot = serde_json::to_string(&h.app.snapshot().journal).unwrap();
    h.app.execute(Action::ExportDiagnostics).await.unwrap();
    let journal = std::fs::read_to_string(h.dir.path().join("journal.json")).unwrap();
    let export = std::fs::read_to_string(h.dir.path().join("diagnostics.json")).unwrap();
    for text in [&snapshot, &journal, &export] {
        for location in [
            "assets/index-ab12.js:12:34",
            "assets/index-cd34.js:56:78",
            "assets/index-ef56.js:90:12",
        ] {
            assert!(text.contains(location), "{location} missing: {text}");
        }
        for private in [
            "http://",
            "https://",
            "tauri://",
            "foreign.example",
            "runneradmin",
            "query-canary",
            "credentials-canary",
        ] {
            assert!(!text.contains(private), "{private} survived: {text}");
        }
    }
}

#[tokio::test]
async fn subscription_failure_code_reaches_state_journal_and_diagnostics() {
    let store = Store {
        subscriptions: vec![subscription("a", "https://a.example/s")],
        ..Store::default()
    };
    let h = start(store, Net::default(), Feed::default());
    until(&h.app, "failed subscription", |snapshot| {
        snapshot.subscriptions[0].failure_code.is_some()
    })
    .await;
    let snapshot = h.app.snapshot();
    assert_eq!(
        snapshot.subscriptions[0].failure_code,
        Some(FailureCode::SubscriptionTransport)
    );
    assert!(snapshot.journal.iter().any(|entry| {
        entry.failure_code == Some(FailureCode::SubscriptionTransport)
            && !entry.message.contains("https://")
    }));

    h.app.execute(Action::ExportDiagnostics).await.unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(h.dir.path().join("diagnostics.json")).unwrap())
            .unwrap();
    assert_eq!(
        report["failures"]["subscriptions"]["subscription_transport"],
        1
    );
    assert!(report.to_string().contains("subscription_transport"));
    assert!(!report.to_string().contains("https://a.example/s"));
}

#[tokio::test]
async fn subscription_without_usable_servers_is_recorded_as_an_invalid_body() {
    let store = Store {
        subscriptions: vec![subscription("a", "https://a.example/s")],
        ..Store::default()
    };
    let feed = Feed::default();
    feed.bodies
        .lock()
        .unwrap()
        .insert("https://a.example/s".into(), Some("vless://broken".into()));
    let h = start(store, Net::default(), feed);
    until(&h.app, "failed subscription", |snapshot| {
        snapshot.subscriptions[0].failure_code.is_some()
    })
    .await;
    let snapshot = h.app.snapshot();
    assert_eq!(
        snapshot.subscriptions[0].failure_code,
        Some(FailureCode::SubscriptionInvalidBody)
    );
    assert!(snapshot
        .journal
        .iter()
        .any(|entry| entry.failure_code == Some(FailureCode::SubscriptionInvalidBody)));
}

/// A source that already delivered two servers, a manual server, and a selection on one of
/// the source's rows: everything a failed refresh must leave alone.
fn store_with_source_servers() -> (Store, Vec<String>) {
    let mut store = Store {
        subscriptions: vec![subscription("a", "https://a.example/s")],
        ..Store::default()
    };
    store.subscriptions[0].updated_at = Some(1_789_742_400);
    store.servers = vec![
        server(1001, Some("a")),
        server(1002, Some("a")),
        server(2001, None),
    ];
    store.selected = Some(store.servers[0].id.clone());
    let ids = store.servers.iter().map(|s| s.id.clone()).collect();
    (store, ids)
}
/// What a failed refresh must not touch: the rows (ids, sources, credentials), the selection
/// and the subscription's last success.
fn kept_by_a_failed_refresh(store: &Store) -> serde_json::Value {
    serde_json::json!({
        "servers": store.servers.iter().map(|s| serde_json::to_value(s).unwrap()).collect::<Vec<_>>(),
        "selected": store.selected,
        "updated_at": store.subscriptions[0].updated_at,
    })
}
fn assert_rows_survived(h: &H, before: &Store, ids: &[String]) {
    let snapshot = h.app.snapshot();
    assert_eq!(
        snapshot.servers.iter().map(|s| &s.id).collect::<Vec<_>>(),
        ids.iter().collect::<Vec<_>>()
    );
    assert_eq!(
        snapshot
            .servers
            .iter()
            .map(|s| s.source.as_deref())
            .collect::<Vec<_>>(),
        [Some("a"), Some("a"), None]
    );
    assert_eq!(snapshot.selected, before.selected);
    assert_eq!(snapshot.subscriptions[0].count, 2);
    assert_eq!(snapshot.subscriptions[0].updated_at, Some(1_789_742_400));
}

#[tokio::test]
async fn failed_download_keeps_the_source_servers_the_manual_server_and_the_selection() {
    let (store, ids) = store_with_source_servers();
    let dir = tempfile::tempdir().unwrap();
    persistence::save(&dir.path().join("state.json"), &store).unwrap();
    // The feed has no body for the URL: the transport fails.
    let h = start_in(store.clone(), Net::default(), Feed::default(), dir);
    until(&h.app, "failed subscription", |snapshot| {
        snapshot.subscriptions[0].failure_code.is_some()
    })
    .await;

    assert_rows_survived(&h, &store, &ids);
    let snapshot = h.app.snapshot();
    assert_eq!(
        snapshot.subscriptions[0].failure_code,
        Some(FailureCode::SubscriptionTransport)
    );
    assert!(snapshot.subscriptions[0].error.is_some());
    // The failure is the only thing written: the saved rows and selection are the old ones.
    let saved = persistence::load(&h.dir.path().join("state.json")).unwrap();
    assert_eq!(
        kept_by_a_failed_refresh(&saved),
        kept_by_a_failed_refresh(&store)
    );
    assert_eq!(
        saved.subscriptions[0].failure_code,
        Some(FailureCode::SubscriptionTransport)
    );
}

#[tokio::test]
async fn subscription_without_usable_servers_keeps_the_previous_servers() {
    for body in ["", "# only a comment\n", "vless://broken"] {
        let (store, ids) = store_with_source_servers();
        let dir = tempfile::tempdir().unwrap();
        persistence::save(&dir.path().join("state.json"), &store).unwrap();
        let feed = Feed::default();
        feed.bodies
            .lock()
            .unwrap()
            .insert("https://a.example/s".into(), Some(body.into()));
        let h = start_in(store.clone(), Net::default(), feed, dir);
        until(&h.app, "failed subscription", |snapshot| {
            snapshot.subscriptions[0].failure_code.is_some()
        })
        .await;

        assert_rows_survived(&h, &store, &ids);
        assert_eq!(
            h.app.snapshot().subscriptions[0].failure_code,
            Some(FailureCode::SubscriptionInvalidBody),
            "body {body:?}"
        );
        let saved = persistence::load(&h.dir.path().join("state.json")).unwrap();
        assert_eq!(
            kept_by_a_failed_refresh(&saved),
            kept_by_a_failed_refresh(&store),
            "body {body:?}"
        );
    }
}

#[tokio::test]
async fn download_that_cannot_be_saved_keeps_the_previous_servers_and_marks_nothing_refreshed() {
    let (store, ids) = store_with_source_servers();
    let dir = tempfile::tempdir().unwrap();
    // A directory in place of state.json: the downloaded servers cannot be saved.
    std::fs::create_dir(dir.path().join("state.json")).unwrap();
    let feed = Feed::default();
    feed.bodies.lock().unwrap().insert(
        "https://a.example/s".into(),
        Some("trojan://p@example.com:3001\ntrojan://p@example.com:3002".into()),
    );
    let h = start_in(store.clone(), Net::default(), feed, dir);
    until(&h.app, "the startup refresh to fail", |snapshot| {
        snapshot
            .journal
            .iter()
            .any(|entry| entry.failure_code == Some(FailureCode::Persistence))
    })
    .await;
    let error = h
        .app
        .execute(Action::Refresh {
            id: Some("a".into()),
        })
        .await
        .unwrap_err();

    assert_eq!(error.failure_code(), Some(FailureCode::Persistence));
    assert!(
        h.feed.count("https://a.example/s") >= 2,
        "both were downloaded"
    );
    assert_rows_survived(&h, &store, &ids);
    let snapshot = h.app.snapshot();
    assert_eq!(
        snapshot.subscriptions[0].error, None,
        "an unsavable result is not a subscription failure"
    );
    assert_eq!(snapshot.subscriptions[0].failure_code, None);
    assert!(h.dir.path().join("state.json").is_dir());
    assert!(
        std::fs::read_dir(h.dir.path()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp")),
        "no temporary file is left behind"
    );
}

#[tokio::test]
async fn connecting_without_servers_reports_no_available_server() {
    let h = start(Store::default(), Net::default(), Feed::default());
    let error = h.app.execute(Action::Connect).await.unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::NoAvailableServer));
    let snapshot = h.app.snapshot();
    assert!(snapshot.error.is_some());
    assert!(snapshot
        .journal
        .iter()
        .any(|entry| entry.failure_code == Some(FailureCode::NoAvailableServer)));
}

// ---- manual servers and subscriptions ---------------------------------------------------

#[tokio::test]
async fn manual_server_is_added_once_and_removed_without_its_subscription_twin() {
    let store = Store {
        subscriptions: vec![subscription("a", "https://a.example/s")],
        servers: vec![server(1, Some("a"))],
        ..Store::default()
    };
    let h = start(store, Net::default(), Feed::default());
    let add = || Action::AddServer {
        uri: "trojan://p@example.com:1".into(),
    };
    h.app.execute(add()).await.unwrap();
    h.app.execute(add()).await.unwrap();
    let servers = h.app.snapshot().servers;
    assert_eq!(servers.len(), 2, "one subscription row + one manual copy");
    let id = servers[0].id.clone();
    h.app.execute(Action::RemoveServer { id }).await.unwrap();
    let left = h.app.snapshot().servers;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].source.as_deref(), Some("a"));
}

#[tokio::test]
async fn removing_the_only_server_while_disconnected_works() {
    let (store, net) = manual(&[1], false);
    let h = start(store, net, Feed::default());
    let id = h.app.snapshot().servers[0].id.clone();
    h.app.execute(Action::RemoveServer { id }).await.unwrap();
    assert!(h.app.snapshot().servers.is_empty());
}

#[tokio::test]
async fn a_new_subscription_is_named_by_its_host_and_an_edit_keeps_the_name() {
    let h = start(Store::default(), Net::default(), Feed::default());
    let save = |id: Option<String>, url: &str| Action::SaveSubscription {
        id,
        url: url.into(),
        interval_mins: None,
        user_agent: None,
        filters: Default::default(),
        provider_routes: true,
    };
    h.app
        .execute(save(None, "https://panel.example/sub/abc"))
        .await
        .unwrap();
    let first = h.app.snapshot().subscriptions[0].clone();
    assert_eq!(
        first.name, "panel.example",
        "no title yet: the host stands in"
    );
    h.app
        .execute(save(Some(first.id.clone()), "https://other.example/new"))
        .await
        .unwrap();
    assert_eq!(
        h.app.snapshot().subscriptions[0].name,
        "panel.example",
        "an edit does not rename: only the provider names a subscription"
    );
    assert!(h
        .app
        .execute(save(None, "http://insecure.example/s"))
        .await
        .is_err());
}

#[tokio::test]
async fn subscription_actions_touch_only_the_named_subscription() {
    let store = Store {
        subscriptions: vec![
            subscription("a", "https://a.example/s"),
            subscription("b", "https://b.example/s"),
        ],
        servers: vec![server(1, Some("a")), server(2, Some("b"))],
        ..Store::default()
    };
    let h = start(store, Net::default(), Feed::default());
    h.app
        .execute(Action::SaveSubscription {
            id: Some("b".into()),
            url: "https://b.example/new".into(),
            interval_mins: None,
            user_agent: None,
            filters: Default::default(),
            provider_routes: true,
        })
        .await
        .unwrap();
    let subs = h.app.snapshot().subscriptions;
    assert_eq!(subs[0].name, "a");
    assert_eq!(subs[1].name, "b");
    assert_eq!(
        h.app.subscription_url("a").await.unwrap(),
        "https://a.example/s"
    );
    assert_eq!(
        h.app.subscription_url("b").await.unwrap(),
        "https://b.example/new"
    );
    h.app
        .execute(Action::RemoveSubscription { id: "a".into() })
        .await
        .unwrap();
    let s = h.app.snapshot();
    assert_eq!(s.subscriptions.len(), 1);
    assert_eq!(s.subscriptions[0].id, "b");
    assert_eq!(s.servers.len(), 1);
    assert_eq!(s.servers[0].source.as_deref(), Some("b"));
}

// ---- selection bookkeeping --------------------------------------------------------------

#[tokio::test]
async fn removing_the_selected_server_clears_or_replaces_the_selection() {
    let (store, net) = manual(&[1, 2], false);
    let (first, second) = (store.servers[0].clone(), store.servers[1].clone());
    let h = start(store, net, Feed::default());
    h.app
        .execute(Action::Select {
            id: second.id.clone(),
        })
        .await
        .unwrap();
    h.app
        .execute(Action::RemoveServer {
            id: second.id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        h.app.snapshot().selected,
        None,
        "disconnected: nothing to pick"
    );
    h.app
        .execute(Action::AddServer {
            uri: "trojan://p@example.com:2".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        h.app.snapshot().selected,
        None,
        "adding while disconnected picks nothing"
    );
    h.app.execute(Action::Connect).await.unwrap();
    h.app
        .execute(Action::Select {
            id: second.id.clone(),
        })
        .await
        .unwrap();
    h.app
        .execute(Action::RemoveServer { id: second.id })
        .await
        .unwrap();
    assert!(
        id_of(&h.app.snapshot(), &first),
        "connected manual mode keeps a live server"
    );
}

#[tokio::test]
async fn stale_saved_selection_falls_back_to_the_first_server() {
    let (mut store, net) = manual(&[1, 2], false);
    store.selected = Some("gone".into());
    let first = store.servers[0].id.clone();
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    assert_eq!(h.net.selects.lock().unwrap().as_slice(), [first]);
}

#[tokio::test]
async fn measurements_survive_publishing_the_store() {
    let (store, net) = manual(&[1, 2], true);
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "both measured", |s| {
        s.servers.iter().all(|v| v.proxy_ms.is_some())
    })
    .await;
    idle(&h.app).await;
    h.app
        .execute(Action::SetAuto { enabled: false })
        .await
        .unwrap();
    let proxy: Vec<_> = h
        .app
        .snapshot()
        .servers
        .iter()
        .map(|v| v.proxy_ms)
        .collect();
    assert_eq!(proxy, vec![Some(40), Some(41)]);
}

#[tokio::test]
async fn retired_manual_selection_is_typed_and_keeps_the_working_server_and_saved_data() {
    let (mut store, net) = manual(&[1], false);
    store.selected = Some(store.servers[0].id.clone());
    let selected = store.selected.clone();
    let retired = parse_uri("ss://aes-128-cfb:synthetic@legacy.example:443", None).unwrap();
    let id = retired.id.clone();
    store.servers.push(retired);
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    let saved = std::fs::read(h.dir.path().join("state.json")).unwrap();
    let switches = h.net.selects.lock().unwrap().len();
    assert_eq!(
        h.app
            .execute(Action::Select { id: id.clone() })
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::UnsupportedLegacyParameters)
    );
    assert_eq!(h.app.snapshot().selected, selected);
    assert!(h.alive.load(Ordering::SeqCst));
    assert_eq!(h.net.selects.lock().unwrap().len(), switches);
    assert_eq!(
        std::fs::read(h.dir.path().join("state.json")).unwrap(),
        saved
    );
    h.app
        .execute(Action::SetAuto { enabled: false })
        .await
        .unwrap();
    let snapshot = h.app.snapshot();
    let view = snapshot.servers.iter().find(|s| s.id == id).unwrap();
    assert_eq!(
        view.failure_code,
        Some(FailureCode::UnsupportedLegacyParameters)
    );
    assert_eq!(view.status, "failed");
}

#[tokio::test]
async fn a_saved_retired_selection_is_refused_without_starting_or_choosing_a_neighbour() {
    let (mut store, net) = manual(&[1], false);
    let retired = parse_uri("ss://aes-128-cfb:synthetic@legacy.example:443", None).unwrap();
    let id = retired.id.clone();
    store.servers.push(retired);
    store.selected = Some(id.clone());
    let h = start(store, net, Feed::default());
    let path = h.dir.path().join("state.json");
    let before = std::fs::read(&path).ok();
    assert_eq!(
        h.app
            .execute(Action::Connect)
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::UnsupportedLegacyParameters)
    );
    assert!(!h.alive.load(Ordering::SeqCst));
    assert_eq!(h.app.snapshot().selected, Some(id));
    assert_eq!(h.app.snapshot().servers.len(), 2);
    assert!(h.net.selects.lock().unwrap().is_empty());
    assert_eq!(std::fs::read(&path).ok(), before);
}

// ---- cycles: kinds, triggers, timers -----------------------------------------------------

#[tokio::test]
async fn manual_mode_checks_only_on_request_and_only_the_selected_server() {
    let (store, net) = manual(&[1, 2], false);
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        h.net.proxied().is_empty(),
        "no automatic cycles in manual mode"
    );
    h.app.execute(Action::CheckNow).await.unwrap();
    until(&h.app, "manual check", |s| s.servers[0].proxy_ms.is_some()).await;
    idle(&h.app).await;
    assert_eq!(h.net.proxied(), vec![1], "only the selected server");
}

#[tokio::test]
async fn enabling_auto_select_while_disconnected_queues_nothing() {
    let (store, net) = manual(&[1], false);
    let h = start(store, net, Feed::default());
    h.app
        .execute(Action::SetAuto { enabled: true })
        .await
        .unwrap();
    assert!(!h.app.snapshot().pending);
    h.app
        .execute(Action::AddServer {
            uri: "trojan://p@example.com:2".into(),
        })
        .await
        .unwrap();
    assert!(!h.app.snapshot().pending, "no restart, no Initial");
}

#[tokio::test(start_paused = true)]
async fn check_now_looks_at_every_server_but_keeps_a_working_one_that_is_not_clearly_slower() {
    // B (port 1) answers first, C (port 2) is faster through the proxy.
    let store = Store {
        servers: vec![server(1, None), server(2, None)],
        ..Store::default()
    };
    let net = Net::default();
    net.set(1, Some(10), Some(80));
    net.set(2, Some(20), Some(40));
    let c = store.servers[1].clone();
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "Initial to reach C", |s| {
        id_of(s, &c) && !s.checking
    })
    .await;
    let switches = h.net.selects.lock().unwrap().len();
    h.net.proxies.lock().unwrap().clear();
    tokio::time::advance(Duration::from_secs(61)).await;
    idle(&h.app).await;
    h.app.execute(Action::CheckNow).await.unwrap();
    idle(&h.app).await;
    // An Initial cycle: B is asked as well, C is still the faster, so nothing moves.
    assert!(
        h.net.proxied().contains(&1),
        "B was asked in the check the owner started"
    );
    assert!(id_of(&h.app.snapshot(), &c));
    assert_eq!(h.net.selects.lock().unwrap().len(), switches);
}

#[tokio::test(start_paused = true)]
async fn check_now_moves_to_a_server_that_became_clearly_faster() {
    let store = Store {
        servers: vec![server(1, None), server(2, None)],
        ..Store::default()
    };
    let net = Net::default();
    net.set(1, Some(10), Some(80));
    net.set(2, Some(20), Some(40));
    let b = store.servers[0].clone();
    let c = store.servers[1].clone();
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "Initial to reach C", |s| {
        id_of(s, &c) && !s.checking
    })
    .await;
    // B gets 190 ms and 95 % better than C: past both thresholds.
    h.net.set(1, Some(10), Some(10));
    h.net.set(2, Some(20), Some(200));
    tokio::time::advance(Duration::from_secs(61)).await;
    idle(&h.app).await;
    h.app.execute(Action::CheckNow).await.unwrap();
    until(&h.app, "Check Now to reach B", |s| {
        id_of(s, &b) && !s.checking
    })
    .await;
}

/// How often the journal said something that contains `text`.
fn said(h: &H, text: &str) -> usize {
    h.app
        .snapshot()
        .journal
        .iter()
        .filter(|entry| entry.message.contains(text))
        .count()
}
/// Lets the paused clock run until the network has been asked `calls` times.
async fn network_asked(h: &H, calls: usize) {
    for _ in 0..400 {
        if h.net.network_calls.load(Ordering::SeqCst) >= calls {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "the network was asked {} times",
        h.net.network_calls.load(Ordering::SeqCst)
    );
}

#[tokio::test(start_paused = true)]
async fn a_sidecar_that_cannot_get_out_is_said_once_and_the_cycle_still_runs() {
    let (store, net) = manual(&[1, 2], true);
    net.deaf.store(true, Ordering::SeqCst);
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "the first cycle", |s| {
        !s.checking && !s.pending && s.selected.is_some()
    })
    .await;
    assert!(
        !h.net.endpoint_calls.lock().unwrap().is_empty(),
        "servers were asked"
    );
    assert_eq!(said(&h, "Нет сети"), 0);
    assert_eq!(said(&h, "sing-box не выходит в сеть сам"), 1);
    assert_eq!(
        said(&h, "(no answer from the sidecar)"),
        1,
        "with what it saw"
    );
    // Not at every cycle.
    tokio::time::advance(Duration::from_secs(61)).await;
    network_asked(&h, 2).await;
    assert_eq!(said(&h, "sing-box не выходит в сеть сам"), 1);
}

#[tokio::test(start_paused = true)]
async fn without_a_network_no_server_is_asked_and_the_owed_initial_waits_for_it() {
    let (store, net) = manual(&[1, 2], true);
    net.offline.store(true, Ordering::SeqCst);
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    network_asked(&h, 1).await;
    until(&h.app, "the first cycle to be skipped", |s| {
        !s.checking && s.pending
    })
    .await;
    assert_eq!(h.net.network_calls.load(Ordering::SeqCst), 1);
    assert!(h.net.endpoint_calls.lock().unwrap().is_empty());
    assert_eq!(said(&h, "Нет сети"), 1);
    assert_eq!(said(&h, "(no answer)"), 1, "the journal says what was seen");
    // Not on every tick: the network is asked again when the interval has passed.
    tokio::time::advance(Duration::from_secs(30)).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(h.net.network_calls.load(Ordering::SeqCst), 1);
    tokio::time::advance(Duration::from_secs(31)).await;
    network_asked(&h, 2).await;
    assert!(h.net.endpoint_calls.lock().unwrap().is_empty());
    assert_eq!(said(&h, "Нет сети"), 1, "said once, not at every cycle");
    // The network is back: the Initial that was owed runs, and the journal says so.
    h.net.offline.store(false, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(61)).await;
    until(&h.app, "the owed Initial", |s| {
        !s.checking && !s.pending && s.selected.is_some()
    })
    .await;
    assert!(!h.net.endpoint_calls.lock().unwrap().is_empty());
    assert_eq!(said(&h, "Сеть появилась"), 1);
}

#[tokio::test(start_paused = true)]
async fn check_now_without_a_network_leaves_the_servers_and_the_choice_alone() {
    let (store, net) = manual(&[1, 2], true);
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    idle(&h.app).await;
    let chosen = h.app.snapshot().selected.clone();
    assert!(chosen.is_some());
    let switches = h.net.selects.lock().unwrap().len();
    h.net.endpoint_calls.lock().unwrap().clear();
    h.net.proxies.lock().unwrap().clear();
    h.net.offline.store(true, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(61)).await;
    let before = h.net.network_calls.load(Ordering::SeqCst);
    h.app.execute(Action::CheckNow).await.unwrap();
    network_asked(&h, before + 1).await;
    until(&h.app, "the check to be skipped", |s| !s.checking).await;
    // Not even the server in use was measured for the table: no cycle ran. (The light check of
    // the selected server between cycles is another thing and goes on.)
    assert!(h.net.endpoint_calls.lock().unwrap().is_empty());
    assert_eq!(h.net.selects.lock().unwrap().len(), switches);
    assert_eq!(h.app.snapshot().selected, chosen);
    assert_eq!(said(&h, "Нет сети"), 1);
}

/// The server on `port` as the screen shows it.
fn row(app: &Application, port_server: &Server) -> rift_core::model::ServerView {
    app.snapshot()
        .servers
        .into_iter()
        .find(|v| v.id == port_server.id)
        .expect("the server is shown")
}
fn asked(h: &H, port: u16) -> usize {
    h.net
        .endpoint_calls
        .lock()
        .unwrap()
        .iter()
        .filter(|p| **p == port)
        .count()
}
/// Lets the paused clock run until `done` holds.
async fn wait_until(what: &str, done: impl Fn() -> bool) {
    for _ in 0..600 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}
/// Seconds from now until `retry_at`, as the screen would show them.
fn retry_in(app: &Application, port_server: &Server) -> Option<u64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    row(app, port_server)
        .retry_at
        .map(|at| at.saturating_sub(now))
}
/// Connects, and asks every server once more by hand: a server that does not answer has missed
/// twice by then, which is what puts it in the list.
async fn connect_and_ask_twice(h: &H) {
    h.app.execute(Action::Connect).await.unwrap();
    idle(&h.app).await;
    tokio::time::advance(Duration::from_secs(61)).await;
    idle(&h.app).await;
    h.app.execute(Action::CheckNow).await.unwrap();
    idle(&h.app).await;
}
/// The shape of a real subscription behind a blocking network: servers whose port opens but
/// through which nothing comes back in time (the test times out), servers that fail at once, and
/// the one that works. Every kind of miss puts a server in the list at its second miss in a row.
#[tokio::test(start_paused = true)]
async fn servers_that_time_out_are_listed_like_those_that_fail_at_once() {
    let store = Store {
        servers: (1..=6).map(|port| server(port, None)).collect(),
        ..Store::default()
    };
    let net = Net::default();
    net.set(1, Some(10), Some(40));
    for port in 2..=4 {
        net.set(port, Some(90), None);
        net.fail_proxy_with(port, FailureCode::ProxyTimeout);
    }
    for port in 5..=6 {
        net.set(port, Some(20), None);
        net.fail_proxy_with(port, FailureCode::ProxyTestFailed);
    }
    let h = start(store.clone(), net, Feed::default());
    connect_and_ask_twice(&h).await;
    for port in 2u16..=6 {
        let server = &store.servers[usize::from(port) - 1];
        assert!(
            row(&h.app, server).retry_at.is_some(),
            "the server of port {port} missed twice and is in the list: {:?}",
            row(&h.app, server)
        );
    }
    assert_eq!(row(&h.app, &store.servers[0]).retry_at, None);
}

/// A subscription of servers 1 (answers), 2 (does not) and 3 (answers, slower).
fn one_two_three() -> (Store, Net, [Server; 3]) {
    let store = Store {
        servers: vec![server(1, None), server(2, None), server(3, None)],
        // A list no longer than the pool is looked at whole every time, the dead included: the
        // pool is two here so that three servers are more than it holds.
        settings: Settings {
            pool_size: 2,
            ..Settings::default()
        },
        ..Store::default()
    };
    let net = Net::default();
    net.set(1, Some(10), Some(40));
    net.set(2, None, None);
    net.set(3, Some(30), Some(60));
    let servers = [
        store.servers[0].clone(),
        store.servers[1].clone(),
        store.servers[2].clone(),
    ];
    (store, net, servers)
}

#[tokio::test(start_paused = true)]
async fn a_server_that_missed_twice_is_left_out_and_asked_again_alone_at_times_of_its_own() {
    let (store, net, [one, two, three]) = one_two_three();
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    idle(&h.app).await;
    assert!(id_of(&h.app.snapshot(), &one));
    // One miss is noise: it is measured with the others in the next cycle.
    assert_eq!(asked(&h, 2), 1);
    assert_eq!(row(&h.app, &two).retry_at, None);
    // The connection is lost, and the next cycle asks S2 again: the second miss puts it in the
    // list, to be asked again in five minutes.
    h.net.set(1, Some(10), None);
    tokio::time::advance(Duration::from_secs(61)).await;
    until(&h.app, "the replacement", |s| {
        s.selected.as_deref() == Some(three.id.as_str()) && !s.checking
    })
    .await;
    assert_eq!(asked(&h, 2), 2);
    let first = retry_in(&h.app, &two).expect("in the list");
    assert!((295..=305).contains(&first), "{first}");
    assert_eq!(row(&h.app, &three).retry_at, None);
    // A later cycle leaves it alone.
    h.net.set(3, Some(30), None);
    h.net.set(1, Some(10), Some(40));
    tokio::time::advance(Duration::from_secs(61)).await;
    until(&h.app, "the cycle that leaves it alone", |s| {
        s.selected.as_deref() == Some(one.id.as_str()) && !s.checking
    })
    .await;
    assert_eq!(asked(&h, 2), 2, "left out of the cycle");
    // At its time it is asked again, alone, and misses again: the wait is longer now.
    tokio::time::advance(Duration::from_secs(301)).await;
    wait_until("the question at its time", || asked(&h, 2) == 3).await;
    wait_until("the longer wait", || {
        retry_in(&h.app, &two).is_some_and(|s| (595..=605).contains(&s))
    })
    .await;
    // It comes back: one answer takes it out of the list, and it is measured with the others.
    h.net.set(2, Some(20), Some(50));
    tokio::time::advance(Duration::from_secs(601)).await;
    wait_until("the answer", || row(&h.app, &two).retry_at.is_none()).await;
    assert_eq!(row(&h.app, &two).status, "online");
    assert_eq!(asked(&h, 2), 4);
    // Nobody switched on its account: the server in use stays.
    assert!(id_of(&h.app.snapshot(), &one));
}

#[tokio::test(start_paused = true)]
async fn check_now_asks_the_dead_servers_too_and_an_answer_takes_them_out() {
    let (store, net, [one, two, _]) = one_two_three();
    let h = start(store, net, Feed::default());
    connect_and_ask_twice(&h).await;
    assert!(id_of(&h.app.snapshot(), &one));
    assert!(row(&h.app, &two).retry_at.is_some(), "missed twice");
    let before = asked(&h, 2);
    // It is repaired; the owner does not wait for its time.
    h.net.set(2, Some(20), Some(45));
    tokio::time::advance(Duration::from_secs(61)).await;
    idle(&h.app).await;
    h.app.execute(Action::CheckNow).await.unwrap();
    idle(&h.app).await;
    // Asked by the check the owner started: once in the sweep, and again for the median.
    assert!(asked(&h, 2) > before, "{}", asked(&h, 2));
    assert_eq!(row(&h.app, &two).retry_at, None);
    assert_eq!(row(&h.app, &two).status, "online");
}

#[tokio::test(start_paused = true)]
async fn a_new_connection_starts_with_no_dead_servers_and_no_misses() {
    let (store, net, [_, two, _]) = one_two_three();
    let h = start(store, net, Feed::default());
    connect_and_ask_twice(&h).await;
    assert!(row(&h.app, &two).retry_at.is_some());
    h.app.execute(Action::Disconnect).await.unwrap();
    assert_eq!(
        row(&h.app, &two).retry_at,
        None,
        "the list does not outlive the connection"
    );
    let before = asked(&h, 2);
    h.app.execute(Action::Connect).await.unwrap();
    idle(&h.app).await;
    assert_eq!(asked(&h, 2), before + 1, "measured again with the others");
    assert_eq!(row(&h.app, &two).retry_at, None, "one miss again, not two");
}

#[tokio::test(start_paused = true)]
async fn the_server_in_use_is_never_left_out_even_when_it_is_in_the_list() {
    // One server, and it does not answer: it is in the list, and still the only one measured.
    let store = Store {
        servers: vec![server(1, None)],
        ..Store::default()
    };
    let net = Net::default();
    net.set(1, None, None);
    let one = store.servers[0].clone();
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    idle(&h.app).await;
    assert_eq!(row(&h.app, &one).retry_at, None, "one miss");
    tokio::time::advance(Duration::from_secs(61)).await;
    wait_until("the second miss", || row(&h.app, &one).retry_at.is_some()).await;
    let before = asked(&h, 1);
    tokio::time::advance(Duration::from_secs(61)).await;
    wait_until("the next cycle to look at it", || asked(&h, 1) > before).await;
}

#[tokio::test(start_paused = true)]
async fn when_the_server_in_use_fails_and_nobody_else_is_left_the_dead_are_tried_in_the_same_cycle()
{
    let store = Store {
        servers: vec![server(1, None), server(2, None)],
        ..Store::default()
    };
    let net = Net::default();
    net.set(1, Some(10), Some(40));
    net.set(2, None, None);
    let (one, two) = (store.servers[0].clone(), store.servers[1].clone());
    let h = start(store, net, Feed::default());
    connect_and_ask_twice(&h).await;
    assert!(id_of(&h.app.snapshot(), &one));
    assert!(row(&h.app, &two).retry_at.is_some());
    // S2 is repaired, but its time has not come; then S1 fails.
    h.net.set(2, Some(20), Some(50));
    h.net.set(1, Some(10), None);
    tokio::time::advance(Duration::from_secs(61)).await;
    until(&h.app, "the dead server taking over", |s| {
        s.selected.as_deref() == Some(two.id.as_str()) && !s.checking
    })
    .await;
    assert_eq!(row(&h.app, &two).retry_at, None);
}

#[tokio::test(start_paused = true)]
async fn the_dead_are_asked_again_beside_a_running_cycle_with_connections_of_their_own() {
    let (store, net, [_, two, _]) = one_two_three();
    let h = start(store, net, Feed::default());
    connect_and_ask_twice(&h).await;
    // Repaired, but its answer through the server is parked when it is asked again.
    h.net.set(2, Some(20), Some(50));
    *h.net.gate.lock().unwrap() = Some(2);
    tokio::time::advance(Duration::from_secs(301)).await;
    tokio::time::timeout(Duration::from_secs(5), h.net.entered.notified())
        .await
        .expect("the question reached the server");
    assert!(
        row(&h.app, &two).retry_at.is_some(),
        "still waiting for the answer"
    );
    // A cycle runs meanwhile and does not wait for it.
    let before = asked(&h, 1);
    h.app.execute(Action::CheckNow).await.unwrap();
    wait_until("the cycle to finish beside the parked question", || {
        let s = h.app.snapshot();
        !s.checking && !s.pending && asked(&h, 1) > before
    })
    .await;
    h.net.release.notify_one();
    wait_until("the parked question to end", || {
        row(&h.app, &two).retry_at.is_none()
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_dead_are_asked_again_at_their_time_even_while_a_long_cycle_is_running() {
    let (store, net, [one, two, three]) = one_two_three();
    let h = start(store, net, Feed::default());
    connect_and_ask_twice(&h).await;
    assert!(id_of(&h.app.snapshot(), &one));
    assert!(row(&h.app, &two).retry_at.is_some());
    // The comparison that Auto Select starts parks at the answer of S3 and goes on for a long time.
    h.net.set(2, Some(20), Some(50));
    *h.net.gate.lock().unwrap() = Some(3);
    h.app
        .execute(Action::SetAuto { enabled: false })
        .await
        .unwrap();
    tokio::time::advance(Duration::from_secs(61)).await;
    h.app
        .execute(Action::SetAuto { enabled: true })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), h.net.entered.notified())
        .await
        .expect("the cycle reached S3");
    assert!(h.app.snapshot().checking, "the cycle is running");
    let before = asked(&h, 2);
    // The time of S2 comes during the cycle: it is asked, without waiting for the cycle to end.
    tokio::time::advance(Duration::from_secs(301)).await;
    wait_until("the question during the cycle", || asked(&h, 2) > before).await;
    assert!(h.app.snapshot().checking, "the cycle is still parked");
    wait_until("the answer during the cycle", || {
        row(&h.app, &two).retry_at.is_none()
    })
    .await;
    h.net.release.notify_one();
    idle(&h.app).await;
    assert!(id_of(&h.app.snapshot(), &one) || id_of(&h.app.snapshot(), &three));
}

#[tokio::test(start_paused = true)]
async fn without_a_network_the_dead_are_not_asked_and_wait_as_long_as_before() {
    let (store, net, [_, two, _]) = one_two_three();
    let h = start(store, net, Feed::default());
    connect_and_ask_twice(&h).await;
    let before = asked(&h, 2);
    h.net.offline.store(true, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(301)).await;
    let calls = h.net.network_calls.load(Ordering::SeqCst);
    network_asked(&h, calls + 1).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(asked(&h, 2), before, "not asked without a network");
    // No new failure: the wait is the first one again, five minutes.
    let again = retry_in(&h.app, &two).expect("still in the list");
    assert!((290..=305).contains(&again), "{again}");
}

#[tokio::test(start_paused = true)]
async fn auto_mode_checks_again_after_the_interval_and_reports_when() {
    let (store, net) = manual(&[1], true);
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "first cycle", |s| {
        !s.checking && s.next_check.is_some()
    })
    .await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let next = h.app.snapshot().next_check.unwrap();
    assert!(
        (now + 55..=now + 65).contains(&next),
        "next check {next} vs now {now}"
    );
    let before = h.net.proxied().len();
    tokio::time::advance(Duration::from_secs(61)).await;
    until(&h.app, "periodic cycle", |_| h.net.proxied().len() > before).await;
}

#[tokio::test(start_paused = true)]
async fn the_next_automatic_check_is_spread_around_the_interval() {
    for (offset, earliest, latest) in [(10_000, 65, 75), (-10_000, 45, 55)] {
        let (store, net) = manual(&[1], true);
        let h = start_jittered(store, net, Feed::default(), offset);
        h.app.execute(Action::Connect).await.unwrap();
        until(&h.app, "first cycle", |s| {
            !s.checking && s.next_check.is_some()
        })
        .await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let next = h.app.snapshot().next_check.unwrap();
        assert!(
            (now + earliest..=now + latest).contains(&next),
            "offset {offset}: next check {next} vs now {now}"
        );
        // The light checks of the selected server also reach the probe; a cycle is told by its
        // journal line.
        let cycles = || {
            h.app
                .snapshot()
                .journal
                .iter()
                .filter(|e| e.message.starts_with("Проверка серверов запущена"))
                .count()
        };
        let before = cycles();
        tokio::time::advance(Duration::from_secs(earliest - 8)).await;
        tokio::task::yield_now().await;
        assert_eq!(cycles(), before, "offset {offset}: not yet due");
        tokio::time::advance(Duration::from_secs(latest - earliest + 8)).await;
        until(&h.app, "periodic cycle", |_| cycles() > before).await;
    }
}

#[tokio::test(start_paused = true)]
async fn the_retry_of_a_failed_background_refresh_is_spread_around_a_minute() {
    for (offset, retried_at) in [(10_000i64, 70u64), (-10_000, 50)] {
        let mut store = Store {
            subscriptions: vec![subscription("a", "https://a.example/s")],
            ..Store::default()
        };
        store.settings.proxy_port = 10;
        let feed = Feed::default();
        feed.bodies.lock().unwrap().insert(
            "https://a.example/s".into(),
            Some("trojan://p@example.com:1".into()),
        );
        let h = start_jittered(store, Net::default(), feed, offset);
        tokio::time::timeout(Duration::from_secs(5), async {
            while h.feed.total() == 0 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("startup refresh");
        tokio::time::sleep(Duration::from_secs(retried_at - 5)).await;
        assert_eq!(
            h.feed.total(),
            1,
            "offset {offset}: no retry before the spread minute"
        );
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(
            h.feed.total(),
            2,
            "offset {offset}: retried at {retried_at} s"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_subscription_is_refreshed_after_its_interval_spread_by_the_jitter() {
    for (offset, refreshed_at) in [(10_000i64, 70u64), (-10_000, 50)] {
        let mut store = Store {
            subscriptions: vec![subscription("a", "https://a.example/s")],
            ..Store::default()
        };
        // The shortest interval: one minute.
        store.settings.subscription_interval_mins = 1;
        let feed = Feed::default();
        feed.bodies.lock().unwrap().insert(
            "https://a.example/s".into(),
            Some("trojan://p@example.com:1".into()),
        );
        let h = start_jittered(store, Net::default(), feed, offset);
        tokio::time::timeout(Duration::from_secs(5), async {
            while h.feed.total() == 0 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("startup refresh");
        tokio::time::sleep(Duration::from_secs(refreshed_at - 5)).await;
        assert_eq!(h.feed.total(), 1, "offset {offset}: not yet due");
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(
            h.feed.total(),
            2,
            "offset {offset}: refreshed at {refreshed_at} s"
        );
    }
}

#[tokio::test]
async fn saving_settings_with_auto_already_on_does_not_rerun_initial() {
    let (store, net) = manual(&[1], true);
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "first cycle", |s| {
        !s.checking && s.next_check.is_some()
    })
    .await;
    let before = h.net.proxied().len();
    let mut settings = h.app.snapshot().settings;
    settings.theme = "dark".into();
    tokio::time::timeout(
        Duration::from_secs(5),
        h.app.execute(Action::SaveSettings { settings }),
    )
    .await
    .expect("SaveSettings blocked by the running check")
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(h.net.proxied().len(), before);
    assert!(!h.app.snapshot().pending);
}

#[tokio::test]
async fn cancelled_periodic_cycle_is_not_requeued() {
    let (mut store, net) = manual(&[1], true);
    // Already on the only server: Initial makes no switch, so no cooldown blocks CheckNow.
    store.selected = Some(store.servers[0].id.clone());
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "first cycle", |s| {
        !s.checking && s.next_check.is_some()
    })
    .await;
    *h.net.gate.lock().unwrap() = Some(1);
    h.app.execute(Action::CheckNow).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), h.net.entered.notified())
        .await
        .expect("periodic check started");
    let probed = h.net.proxied().len();
    // A change of the generated config interrupts the check; a theme alone does not.
    let mut settings = h.app.snapshot().settings;
    settings.proxy_port = 2090;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        h.app.execute(Action::SaveSettings { settings }),
    )
    .await
    .expect("SaveSettings blocked by the running check")
    .unwrap();
    idle(&h.app).await;
    assert!(
        !h.app.snapshot().pending,
        "only Initial resumes after interruption"
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        h.net.proxied().len(),
        probed + 1,
        "the restart runs one Initial; the interrupted periodic check is not requeued"
    );
}

#[tokio::test]
async fn a_selected_server_recovering_marks_the_connection_connected() {
    let (mut store, net) = manual(&[1], true);
    store.selected = Some(store.servers[0].id.clone());
    net.set(1, Some(10), None);
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "failed first cycle", |s| {
        s.status == "error" && !s.checking
    })
    .await;
    assert_eq!(
        h.app.snapshot().servers[0].failure_code,
        Some(FailureCode::ProxyTransport)
    );
    h.net.set(1, Some(10), Some(40));
    h.app.execute(Action::CheckNow).await.unwrap();
    until(&h.app, "recovered", |s| s.status == "connected").await;
    assert_eq!(
        h.net.selects.lock().unwrap().len(),
        0,
        "no switch was needed"
    );
}

#[tokio::test]
async fn shutdown_stops_the_owned_runtime() {
    let (store, net) = manual(&[1], false);
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    assert!(h.alive.load(Ordering::SeqCst));
    h.app.shutdown().await.unwrap();
    assert!(!h.alive.load(Ordering::SeqCst));
}

#[tokio::test]
async fn check_now_probes_manual_and_subscription_copies_once_and_updates_both_rows() {
    let mut manual = server(1, None);
    manual.name = "manual".into();
    let mut subscribed = manual.clone();
    subscribed.name = "subscription".into();
    subscribed.source = Some("sub".into());
    let id = manual.id.clone();
    let mut store = Store {
        servers: vec![manual, subscribed],
        subscriptions: vec![subscription("sub", "https://sub.example/s")],
        selected: Some(id.clone()),
        ..Store::default()
    };
    store.settings.auto_select = false;
    let net = Net::default();
    net.set(1, Some(10), Some(20));
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    h.net.endpoint_calls.lock().unwrap().clear();
    h.net.proxies.lock().unwrap().clear();

    h.app.execute(Action::CheckNow).await.unwrap();
    until(&h.app, "manual duplicate check", |snapshot| {
        !snapshot.checking
            && snapshot
                .servers
                .iter()
                .filter(|server| server.id == id)
                .all(|server| server.proxy_ms == Some(20))
    })
    .await;

    assert_eq!(h.net.endpoint_calls.lock().unwrap().as_slice(), [1]);
    assert_eq!(h.net.proxied(), [1]);
    let snapshot = h.app.snapshot();
    let copies: Vec<_> = snapshot
        .servers
        .iter()
        .filter(|server| server.id == id)
        .collect();
    assert_eq!(copies.len(), 2, "Store/UI rows stay source-owned");
    assert!(copies
        .iter()
        .all(|server| server.endpoint_ms == Some(10) && server.proxy_ms == Some(20)));
    assert_eq!(snapshot.selected.as_deref(), Some(id.as_str()));
}

// ---- subscription refresh ----------------------------------------------------------------

fn two_feeds(include_b: bool) -> (Store, Feed) {
    let mut store = Store {
        subscriptions: vec![subscription("a", "https://a.example/s")],
        ..Store::default()
    };
    if include_b {
        store
            .subscriptions
            .push(subscription("b", "https://b.example/s"));
    }
    let feed = Feed::default();
    feed.bodies.lock().unwrap().insert(
        "https://a.example/s".into(),
        Some("trojan://p@example.com:1".into()),
    );
    feed.bodies
        .lock()
        .unwrap()
        .insert("https://b.example/s".into(), None);
    (store, feed)
}

#[tokio::test(start_paused = true)]
async fn legacy_disabled_source_refreshes_and_participates_in_auto_selection() {
    let (mut store, net) = manual(&[], true);
    store.subscriptions = vec![subscription("a", "https://a.example/s")];
    store.servers = vec![server(1, Some("a"))];
    let selected = store.servers[0].id.clone();
    store.selected = Some(selected.clone());
    let mut legacy = serde_json::to_value(&store).unwrap();
    legacy["subscriptions"][0]["enabled"] = serde_json::json!(false);
    let restored = persistence::decode(&serde_json::to_vec(&legacy).unwrap()).unwrap();
    net.set(1, Some(10), Some(40));
    let feed = Feed::default();
    feed.bodies.lock().unwrap().insert(
        "https://a.example/s".into(),
        Some("trojan://p@example.com:1".into()),
    );
    let h = start(restored, net, feed);
    until(&h.app, "legacy source refreshed", |s| {
        s.subscriptions[0].updated_at.is_some()
    })
    .await;
    assert_eq!(h.feed.count("https://a.example/s"), 1);
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "legacy source selected", |s| {
        !s.checking && s.selected.as_ref() == Some(&selected) && s.servers[0].proxy_ms.is_some()
    })
    .await;
    assert!(!h.net.proxied().is_empty());
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(h.dir.path().join("state.json")).unwrap()).unwrap();
    assert!(saved["subscriptions"][0].get("enabled").is_none());
    let snapshot = serde_json::to_value(h.app.snapshot()).unwrap();
    assert!(snapshot["subscriptions"][0].get("enabled").is_none());
    h.app.shutdown().await.unwrap();
}

#[test]
fn obsolete_subscription_toggle_ipc_is_rejected() {
    for enabled in [false, true] {
        assert!(serde_json::from_value::<Action>(serde_json::json!({
            "type": "enable_subscription", "id": "a", "enabled": enabled,
        }))
        .is_err());
    }
}

#[tokio::test(start_paused = true)]
async fn refresh_fetches_all_subscriptions_and_marks_each_one() {
    let (store, feed) = two_feeds(true);
    let h = start(store, Net::default(), feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[0].updated_at.is_some()
    })
    .await;
    assert_eq!(
        h.feed.count("https://b.example/s"),
        1,
        "every stored subscription is refreshed"
    );
    h.app
        .execute(Action::Refresh {
            id: Some("b".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        h.feed.count("https://a.example/s"),
        1,
        "only the named subscription"
    );
    let subs = h.app.snapshot().subscriptions;
    assert!(subs[0].error.is_none());
    assert!(subs[1].error.is_some());
    assert!(subs[1].updated_at.is_none());
}

#[tokio::test(start_paused = true)]
async fn refresh_timers_use_minutes_not_seconds_when_everything_succeeds() {
    let (store, feed) = two_feeds(false);
    let h = start(store, Net::default(), feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[0].updated_at.is_some()
    })
    .await;
    let first = h.feed.total();
    tokio::time::advance(Duration::from_secs(30 * 60)).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.feed.total(), first, "60-minute interval, not 60 seconds");
}

#[tokio::test(start_paused = true)]
async fn failing_subscription_retries_sooner_and_then_backs_off() {
    // "b" never has a body configured: it fails every attempt (see two_feeds).
    let (store, feed) = two_feeds(true);
    let h = start(store, Net::default(), feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[0].updated_at.is_some()
    })
    .await;
    assert_eq!(h.feed.total(), 2, "first cycle: a and b");
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        h.feed.total(),
        3,
        "retried ~60s after the first failure, but only b: a's own schedule is unaffected"
    );
    tokio::time::advance(Duration::from_secs(121)).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(h.feed.total(), 4, "backoff doubled to ~120s, still only b");
    tokio::time::advance(Duration::from_secs(200)).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        h.feed.total(),
        4,
        "next backoff step is ~240s, not reached yet — not retrying every 60s forever"
    );
    assert_eq!(
        h.feed.count("https://a.example/s"),
        1,
        "a never left its own 60-minute cadence just because b kept failing"
    );
    assert_eq!(h.feed.count("https://b.example/s"), 3);
}

#[tokio::test(start_paused = true)]
async fn failing_subscription_recovering_resets_the_backoff() {
    let (store, feed) = two_feeds(true);
    let h = start(store, Net::default(), feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[0].updated_at.is_some()
    })
    .await;
    assert_eq!(h.feed.total(), 2);
    // "b" starts succeeding before its ~60s backoff retry fires.
    h.feed.bodies.lock().unwrap().insert(
        "https://b.example/s".into(),
        Some("trojan://p@b.example:1".into()),
    );
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        h.feed.total(),
        3,
        "the backed-off retry ran for b only, and it now succeeds"
    );
    assert_eq!(
        h.feed.count("https://a.example/s"),
        1,
        "a was not due yet and was not re-fetched just because b was retried"
    );
    assert!(h.app.snapshot().subscriptions[1].error.is_none());
    assert_eq!(h.app.snapshot().subscriptions[1].failure_code, None);
    let after_recovery = h.feed.total();
    tokio::time::advance(Duration::from_secs(30 * 60)).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        h.feed.total(),
        after_recovery,
        "back to the full 60-minute interval, not still backing off"
    );
}

#[tokio::test(start_paused = true)]
async fn manual_refresh_of_one_subscription_does_not_disturb_another_s_backoff() {
    // "b" fails every attempt; a manual refresh of the healthy "a" alone must not reset or
    // pull forward b's own backoff schedule (CTL-002/GB-1: this was a shared Inner-wide
    // counter before, so any single-subscription refresh clobbered every subscription's
    // schedule).
    let (store, feed) = two_feeds(true);
    let h = start(store, Net::default(), feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[0].updated_at.is_some()
    })
    .await;
    assert_eq!(h.feed.total(), 2, "first cycle: a and b");
    h.app
        .execute(Action::Refresh {
            id: Some("a".into()),
        })
        .await
        .unwrap();
    assert_eq!(h.feed.total(), 3, "manual refresh fetched only a");
    // If b's backoff had been reset/pulled forward by the manual refresh of a, it would not
    // be due yet at 61s; it is still due on its own original ~60s schedule.
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        h.feed.total(),
        4,
        "b's own backoff retry still fired on schedule"
    );
    assert_eq!(h.feed.count("https://a.example/s"), 2);
    assert_eq!(h.feed.count("https://b.example/s"), 2);
}

#[tokio::test(start_paused = true)]
async fn editing_a_subscription_clears_its_backoff_so_it_refetches_promptly() {
    // "b" fails every attempt and backs off; editing its URL to one that works must not
    // wait out that backoff. (A dead `next_refresh` field left SaveSubscription's
    // "refetch now" signal going nowhere once per-source backoff replaced the single
    // shared timer — caught in differential review of that refactor.)
    let (store, feed) = two_feeds(true);
    let h = start(store, Net::default(), feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[0].updated_at.is_some()
    })
    .await;
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        h.feed.total(),
        3,
        "b's own ~60s backoff retry, still failing"
    );
    // b is now mid-backoff (next due in ~120s). Point it at a URL that succeeds.
    h.feed.bodies.lock().unwrap().insert(
        "https://b.example/fresh".into(),
        Some("trojan://p@b.example:1".into()),
    );
    h.app
        .execute(Action::SaveSubscription {
            id: Some("b".into()),
            url: "https://b.example/fresh".into(),
            interval_mins: None,
            user_agent: None,
            filters: Default::default(),
            provider_routes: true,
        })
        .await
        .unwrap();
    until(
        &h.app,
        "edited subscription refetched despite being mid-backoff",
        |s| s.subscriptions[1].error.is_none(),
    )
    .await;
    assert_eq!(h.feed.count("https://b.example/fresh"), 1);
}

#[tokio::test(start_paused = true)]
async fn failed_background_refresh_is_retried_after_a_minute() {
    // The refreshed servers produce a config the runtime rejects: the refresh fails.
    let mut store = Store {
        subscriptions: vec![subscription("a", "https://a.example/s")],
        ..Store::default()
    };
    store.settings.proxy_port = 10;
    let feed = Feed::default();
    feed.bodies.lock().unwrap().insert(
        "https://a.example/s".into(),
        Some("trojan://p@example.com:1".into()),
    );
    let h = start(store, Net::default(), feed);
    tokio::time::timeout(Duration::from_secs(5), async {
        while h.feed.total() == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("startup refresh");
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(h.feed.total(), 1, "no retry every tick");
    tokio::time::sleep(Duration::from_secs(40)).await;
    assert_eq!(h.feed.total(), 2, "retried a minute later");
}

#[tokio::test]
async fn refresh_after_shutdown_is_cancelled_without_downloading() {
    let (store, feed) = two_feeds(true);
    let h = start(store, Net::default(), feed);
    h.app.shutdown().await.unwrap();
    let before = h.feed.total();
    let error = h
        .app
        .execute(Action::Refresh { id: None })
        .await
        .unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::Cancelled));
    assert_eq!(h.feed.total(), before);
    assert!(h
        .app
        .snapshot()
        .journal
        .iter()
        .any(|entry| entry.failure_code == Some(FailureCode::Cancelled)));
}

#[tokio::test(start_paused = true)]
async fn connected_refresh_falls_back_to_the_active_proxy_after_a_direct_failure() {
    let (mut store, net) = manual(&[1], true);
    store.subscriptions = vec![subscription("a", "https://a.example/s")];
    store.settings.proxy_port = 4321;
    // "a" has no body configured: every fetch attempt fails, direct or via proxy.
    let feed = Feed::default();
    let h = start(store, net, feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[0].error.is_some()
    })
    .await;
    assert_eq!(
        *h.feed.fetches.lock().unwrap(),
        vec![("https://a.example/s".into(), None)],
        "disconnected at startup: direct only, no proxy retry"
    );
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "connected", |s| s.status == "connected").await;
    h.feed.fetches.lock().unwrap().clear();
    h.app.execute(Action::Refresh { id: None }).await.unwrap();
    assert_eq!(
        *h.feed.fetches.lock().unwrap(),
        vec![
            ("https://a.example/s".into(), None),
            ("https://a.example/s".into(), Some(4321)),
        ],
        "connected: direct first, then retried through the active server's tunnel"
    );
}

#[tokio::test(start_paused = true)]
async fn connected_tun_mode_refresh_has_no_proxy_to_fall_back_to() {
    // Pure Tun mode has no local mixed inbound (config.rs) to retry a download through.
    let (mut store, net) = manual(&[1], true);
    store.settings.mode = Mode::Tun;
    store.subscriptions = vec![subscription("a", "https://a.example/s")];
    let feed = Feed::default();
    let h = start(store, net, feed);
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "connected", |s| s.status == "connected").await;
    h.feed.fetches.lock().unwrap().clear();
    h.app.execute(Action::Refresh { id: None }).await.unwrap();
    assert_eq!(
        *h.feed.fetches.lock().unwrap(),
        vec![("https://a.example/s".into(), None)],
        "no second, proxied attempt when there is no local inbound to route through"
    );
}

#[tokio::test]
async fn refresh_queued_before_disconnect_does_not_download() {
    let (mut store, net) = manual(&[1], true);
    store.subscriptions = vec![subscription("a", "https://a.example/s")];
    let feed = Feed::default();
    feed.bodies.lock().unwrap().insert(
        "https://a.example/s".into(),
        Some("trojan://p@example.com:1".into()),
    );
    *net.gate.lock().unwrap() = Some(1);
    let h = start(store, net, feed);
    h.app.execute(Action::Connect).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), h.net.entered.notified())
        .await
        .expect("cycle holds the coordinator");
    let before = h.feed.total();
    let refresh = {
        let app = h.app.clone();
        tokio::spawn(async move { app.execute(Action::Refresh { id: None }).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    tokio::time::timeout(Duration::from_secs(5), h.app.execute(Action::Disconnect))
        .await
        .expect("Disconnect blocked by the running check")
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), refresh)
        .await
        .unwrap();
    assert_eq!(
        h.feed.total(),
        before,
        "disconnect cancels the queued refresh"
    );
}

#[tokio::test]
async fn cancelling_while_two_refreshes_overlap_stops_the_one_actually_running() {
    // Dropping the coordinator lock during subscription network I/O (so other actions stay
    // responsive) made overlapping refresh() calls possible for the first time. Each used
    // to reset the single shared Control::refresh token unconditionally, so a later call
    // could silently orphan an earlier one's cancellation path — refresh_lock now serializes
    // their network phases so there is only ever one active token to cancel.
    let (store, net) = manual(&[1], true);
    let feed = Feed::default();
    feed.bodies.lock().unwrap().insert(
        "https://a.example/s".into(),
        Some("trojan://p@a.example:1".into()),
    );
    feed.bodies.lock().unwrap().insert(
        "https://b.example/s".into(),
        Some("trojan://p@b.example:1".into()),
    );
    let mut store = store;
    store.subscriptions = vec![
        subscription("a", "https://a.example/s"),
        subscription("b", "https://b.example/s"),
    ];
    let h = start(store, net, feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[1].updated_at.is_some()
    })
    .await;
    let before_b = h.feed.count("https://b.example/s");
    *h.feed.gate.lock().unwrap() = Some("https://a.example/s".into());

    let refresh_a = {
        let app = h.app.clone();
        tokio::spawn(async move {
            app.execute(Action::Refresh {
                id: Some("a".into()),
            })
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), h.feed.entered.notified())
        .await
        .expect("a's refresh holds refresh_lock, gated mid-fetch");

    let refresh_b = {
        let app = h.app.clone();
        tokio::spawn(async move {
            app.execute(Action::Refresh {
                id: Some("b".into()),
            })
            .await
        })
    };
    // Best-effort: give b's call a moment to actually reach (and queue behind)
    // refresh_lock. The real proof is the assertions below, not this timing.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        h.feed.count("https://b.example/s"),
        before_b,
        "b is queued behind a, not fetched concurrently"
    );

    // Select's id doesn't need to resolve — execute() calls cancel() unconditionally
    // before apply() ever runs, cancelling whatever token Control::refresh currently
    // holds. If that were b's (clobbering a's), a would keep running past this point.
    let _ = h
        .app
        .execute(Action::Select {
            id: "no-such-server".into(),
        })
        .await;

    let result_a = tokio::time::timeout(Duration::from_secs(5), refresh_a)
        .await
        .expect("a's refresh task finished")
        .unwrap();
    assert!(
        result_a.is_err(),
        "a's in-flight refresh was actually cancelled, not orphaned"
    );

    h.feed.release.notify_one();
    let result_b = tokio::time::timeout(Duration::from_secs(5), refresh_b)
        .await
        .expect("b's refresh task finished")
        .unwrap();
    assert!(result_b.is_ok(), "b proceeds normally once a's slot frees");
    assert_eq!(h.feed.count("https://b.example/s"), before_b + 1);
}

#[tokio::test]
async fn identical_refreshes_share_one_fetch_and_a_later_request_starts_a_new_one() {
    let store = Store {
        subscriptions: vec![subscription("a", "https://a.example/s")],
        ..Store::default()
    };
    let feed = Feed::default();
    feed.bodies.lock().unwrap().insert(
        "https://a.example/s".into(),
        Some("trojan://p@a.example:1".into()),
    );
    let h = start(store, Net::default(), feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[0].updated_at.is_some()
    })
    .await;
    let before = h.feed.count("https://a.example/s");
    *h.feed.gate.lock().unwrap() = Some("https://a.example/s".into());

    let first = {
        let app = h.app.clone();
        tokio::spawn(async move {
            app.execute(Action::Refresh {
                id: Some("a".into()),
            })
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), h.feed.entered.notified())
        .await
        .expect("first refresh entered the fetch");
    assert!(h.app.snapshot().refreshing);
    let second = {
        let app = h.app.clone();
        tokio::spawn(async move {
            app.execute(Action::Refresh {
                id: Some("a".into()),
            })
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(h.feed.count("https://a.example/s"), before + 1);

    h.feed.release.notify_one();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert!(!h.app.snapshot().refreshing);
    assert_eq!(h.feed.count("https://a.example/s"), before + 1);

    h.app
        .execute(Action::Refresh {
            id: Some("a".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        h.feed.count("https://a.example/s"),
        before + 2,
        "a request after the flight completed starts a new fetch"
    );
}

#[tokio::test]
async fn refresh_all_absorbs_queued_sources_without_refetching_the_active_source() {
    let (store, feed) = two_feeds(true);
    feed.bodies.lock().unwrap().insert(
        "https://b.example/s".into(),
        Some("trojan://p@b.example:1".into()),
    );
    let h = start(store, Net::default(), feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[1].updated_at.is_some()
    })
    .await;
    let before_a = h.feed.count("https://a.example/s");
    let before_b = h.feed.count("https://b.example/s");
    *h.feed.gate.lock().unwrap() = Some("https://a.example/s".into());

    let refresh_a = {
        let app = h.app.clone();
        tokio::spawn(async move {
            app.execute(Action::Refresh {
                id: Some("a".into()),
            })
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), h.feed.entered.notified())
        .await
        .expect("a refresh entered the fetch");
    let refresh_b = {
        let app = h.app.clone();
        tokio::spawn(async move {
            app.execute(Action::Refresh {
                id: Some("b".into()),
            })
            .await
        })
    };
    let refresh_all = {
        let app = h.app.clone();
        tokio::spawn(async move { app.execute(Action::Refresh { id: None }).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(h.feed.count("https://b.example/s"), before_b);

    h.feed.release.notify_one();
    refresh_a.await.unwrap().unwrap();
    refresh_b.await.unwrap().unwrap();
    refresh_all.await.unwrap().unwrap();
    assert_eq!(h.feed.count("https://a.example/s"), before_a + 1);
    assert_eq!(h.feed.count("https://b.example/s"), before_b + 1);
    assert!(!h.app.snapshot().refreshing);
}

fn spawn_refresh(
    app: &Arc<Application>,
    id: Option<&str>,
) -> tokio::task::JoinHandle<Result<Snapshot>> {
    let app = app.clone();
    let id = id.map(str::to_owned);
    tokio::spawn(async move { app.execute(Action::Refresh { id }).await })
}
async fn finished_with_error<T>(
    task: tokio::task::JoinHandle<T>,
    what: &str,
) -> tokio::task::JoinError {
    match tokio::time::timeout(Duration::from_secs(5), task).await {
        Err(_) => panic!("{what} did not finish"),
        Ok(Ok(_)) => panic!("{what} was expected to fail"),
        Ok(Err(error)) => error,
    }
}
async fn finished<T>(task: tokio::task::JoinHandle<T>, what: &str) -> T {
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish"))
        .unwrap()
}

#[tokio::test]
async fn cancelled_refresh_does_not_hide_its_source_from_the_queued_refresh_all() {
    let (store, feed) = two_feeds(true);
    feed.bodies.lock().unwrap().insert(
        "https://b.example/s".into(),
        Some("trojan://p@b.example:1".into()),
    );
    let h = start(store, Net::default(), feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[1].updated_at.is_some()
    })
    .await;
    let before_a = h.feed.count("https://a.example/s");
    let before_b = h.feed.count("https://b.example/s");
    *h.feed.gate.lock().unwrap() = Some("https://a.example/s".into());

    let refresh_a = spawn_refresh(&h.app, Some("a"));
    tokio::time::timeout(Duration::from_secs(5), h.feed.entered.notified())
        .await
        .expect("a refresh entered the fetch");
    let refresh_all = spawn_refresh(&h.app, None);
    tokio::time::sleep(Duration::from_millis(100)).await;

    // A manual choice cancels the download in flight but not the refresh queued behind it.
    let _ = h
        .app
        .execute(Action::Select {
            id: "no-such-server".into(),
        })
        .await;
    assert!(finished(refresh_a, "cancelled refresh").await.is_err());
    finished(refresh_all, "queued refresh-all")
        .await
        .expect("the queued refresh-all runs");
    assert_eq!(
        h.feed.count("https://a.example/s"),
        before_a + 2,
        "the cancelled attempt did not count as done: refresh-all downloads a again"
    );
    assert_eq!(h.feed.count("https://b.example/s"), before_b + 1);
    assert!(!h.app.snapshot().refreshing);
}

#[tokio::test]
async fn refresh_whose_result_could_not_be_stored_is_repeated_by_the_queued_refresh_all() {
    let (store, feed) = two_feeds(false);
    let h = start(store, Net::default(), feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[0].updated_at.is_some()
    })
    .await;
    // A directory in place of state.json: the downloaded servers cannot be saved.
    let state = h.dir.path().join("state.json");
    let _ = std::fs::remove_file(&state);
    std::fs::create_dir(&state).unwrap();
    let before = h.feed.count("https://a.example/s");
    *h.feed.gate.lock().unwrap() = Some("https://a.example/s".into());

    let refresh_a = spawn_refresh(&h.app, Some("a"));
    tokio::time::timeout(Duration::from_secs(5), h.feed.entered.notified())
        .await
        .expect("a refresh entered the fetch");
    let refresh_all = spawn_refresh(&h.app, None);
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.feed.release.notify_one();

    let first = finished(refresh_a, "refresh of a").await.unwrap_err();
    assert_eq!(first.failure_code(), Some(FailureCode::Persistence));
    assert!(finished(refresh_all, "queued refresh-all").await.is_err());
    assert_eq!(
        h.feed.count("https://a.example/s"),
        before + 2,
        "a result that was not stored does not count as done"
    );
    assert!(!h.app.snapshot().refreshing);
}

#[tokio::test(start_paused = true)]
async fn unstored_refresh_leaves_the_source_due_for_the_automatic_follow_up() {
    let store = Store {
        subscriptions: vec![subscription("a", "https://a.example/s")],
        ..Store::default()
    };
    let feed = Feed::default();
    feed.bodies
        .lock()
        .unwrap()
        .insert("https://a.example/s".into(), None);
    let h = start(store, Net::default(), feed);
    until(&h.app, "failed startup refresh", |s| {
        s.subscriptions[0].error.is_some()
    })
    .await;
    // The panel recovers, but a directory in place of state.json keeps the servers from
    // being saved.
    h.feed.bodies.lock().unwrap().insert(
        "https://a.example/s".into(),
        Some("trojan://p@example.com:1".into()),
    );
    let state = h.dir.path().join("state.json");
    let _ = std::fs::remove_file(&state);
    std::fs::create_dir(&state).unwrap();
    let before = h.feed.count("https://a.example/s");
    *h.feed.gate.lock().unwrap() = Some("https://a.example/s".into());

    let refresh_a = spawn_refresh(&h.app, Some("a"));
    tokio::time::timeout(Duration::from_secs(5), h.feed.entered.notified())
        .await
        .expect("a refresh entered the fetch");
    // The one-minute retry of the failed source falls due: the worker's unforced
    // refresh-all queues behind the manual refresh.
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    h.feed.release.notify_one();

    let first = finished(refresh_a, "refresh of a").await.unwrap_err();
    assert_eq!(first.failure_code(), Some(FailureCode::Persistence));
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        h.feed.count("https://a.example/s"),
        before + 2,
        "the unstored result did not postpone a by a full interval, and the worker does not \
         repeat the download every tick"
    );
    assert!(!h.app.snapshot().refreshing);

    // Once the file can be written again, the retry a minute later stores the servers.
    std::fs::remove_dir(&state).unwrap();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    let subscription = &h.app.snapshot().subscriptions[0];
    assert!(subscription.updated_at.is_some() && subscription.error.is_none());
    assert_eq!(h.feed.count("https://a.example/s"), before + 3);
}

/// Source `a` answers with one server; `b` and `c` are available to add later. A
/// directory in place of `state.json` keeps the worker's startup refresh from storing `a`: it
/// fails with `Persistence` and pauses the worker's due-check for a minute. The directory is
/// gone when this returns, so later actions can save.
async fn after_a_failed_automatic_refresh() -> H {
    let store = Store {
        subscriptions: vec![subscription("a", "https://a.example/s")],
        ..Store::default()
    };
    let feed = Feed::default();
    for host in ["a", "b", "c"] {
        feed.bodies.lock().unwrap().insert(
            format!("https://{host}.example/s"),
            Some(format!("trojan://p@{host}.example:1")),
        );
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("state.json")).unwrap();
    let h = start_in(store, Net::default(), feed, dir);
    until(&h.app, "the failed automatic refresh", |snapshot| {
        snapshot
            .journal
            .iter()
            .any(|entry| entry.failure_code == Some(FailureCode::Persistence))
    })
    .await;
    std::fs::remove_dir(h.dir.path().join("state.json")).unwrap();
    h
}
fn no_error_entries(snapshot: &Snapshot) -> bool {
    snapshot.journal.iter().all(|entry| entry.level != "error")
}

#[tokio::test(start_paused = true)]
async fn settings_from_the_interface_that_are_out_of_range_are_refused_not_brought_into_it() {
    // A file of an earlier version is read with its interval brought into 50-300 s; the same
    // number sent by the interface is an error, and nothing is saved.
    let (store, net) = manual(&[1], false);
    let h = start(store, net, Feed::default());
    for (field, value) in [
        ("interval_secs", 1),
        ("interval_secs", 49),
        ("interval_secs", 301),
        ("interval_secs", 86400),
        ("concurrency", 0),
        ("concurrency", 4),
        ("concurrency", 64),
    ] {
        let wire = format!(r#"{{"type":"save_settings","settings":{{"{field}":{value}}}}}"#);
        let action: Action = serde_json::from_str(&wire).unwrap();
        let error = h.app.execute(action).await.unwrap_err();
        assert!(
            matches!(error, Error::Invalid(_)),
            "{field} {value}: {error:?}"
        );
    }
    let settings = h.app.snapshot().settings;
    assert_eq!((settings.interval_secs, settings.concurrency), (60, 3));
    // Inside the range the same wire form is saved.
    let action: Action =
        serde_json::from_str(r#"{"type":"save_settings","settings":{"interval_secs":120}}"#)
            .unwrap();
    h.app.execute(action).await.unwrap();
    assert_eq!(h.app.snapshot().settings.interval_secs, 120);
}

#[tokio::test(start_paused = true)]
async fn settings_save_cancelling_the_automatic_refresh_does_not_postpone_its_retry() {
    let url = "https://a.example/s";
    let store = Store {
        servers: vec![server(1, None)],
        subscriptions: vec![subscription("a", url)],
        ..Store::default()
    };
    let feed = Feed::default();
    feed.bodies
        .lock()
        .unwrap()
        .insert(url.into(), Some("trojan://p@example.com:1".into()));
    *feed.gate.lock().unwrap() = Some(url.into());
    let h = start(store, Net::default(), feed);
    tokio::time::timeout(Duration::from_secs(5), h.feed.entered.notified())
        .await
        .expect("the startup refresh entered the fetch");
    // Only a change of the generated config interrupts a download: a theme alone does not.
    let mut settings = h.app.snapshot().settings;
    settings.proxy_port = 2090;

    h.app
        .execute(Action::SaveSettings { settings })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    assert_eq!(
        h.feed.count(url),
        2,
        "the interrupted download is repeated on the next tick, not a minute later"
    );
    let snapshot = h.app.snapshot();
    assert!(snapshot.subscriptions[0].updated_at.is_some());
    assert!(
        no_error_entries(&snapshot),
        "an interruption is not an error: {:?}",
        snapshot.journal
    );
}

#[tokio::test(start_paused = true)]
async fn disconnect_cancelling_the_automatic_refresh_does_not_postpone_its_retry() {
    let url = "https://a.example/s";
    let (mut store, net) = manual(&[1], false);
    store.subscriptions.push(subscription("a", url));
    let feed = Feed::default();
    feed.bodies
        .lock()
        .unwrap()
        .insert(url.into(), Some("trojan://p@example.com:2".into()));
    *feed.gate.lock().unwrap() = Some(url.into());
    let h = start(store, net, feed);
    tokio::time::timeout(Duration::from_secs(5), h.feed.entered.notified())
        .await
        .expect("the startup refresh entered the fetch");
    h.app.execute(Action::Connect).await.unwrap();

    h.app.execute(Action::Disconnect).await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    assert_eq!(
        h.feed.count(url),
        2,
        "repeated once the client is disconnected"
    );
    let snapshot = h.app.snapshot();
    assert!(snapshot.subscriptions[0].updated_at.is_some());
    assert!(
        no_error_entries(&snapshot),
        "Disconnect is not an error: {:?}",
        snapshot.journal
    );
}

#[tokio::test(start_paused = true)]
async fn saving_a_subscription_after_a_failed_automatic_refresh_downloads_it_at_once() {
    let h = after_a_failed_automatic_refresh().await;
    assert_eq!(
        h.feed.count("https://c.example/s"),
        0,
        "premise: c is not known yet"
    );

    h.app
        .execute(Action::SaveSubscription {
            id: Some("a".into()),
            url: "https://c.example/s".into(),
            interval_mins: None,
            user_agent: None,
            filters: Default::default(),
            provider_routes: true,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    assert_eq!(
        h.feed.count("https://c.example/s"),
        1,
        "the edited URL is not held back by the pause after the failure"
    );
    assert!(h.app.snapshot().subscriptions[0].updated_at.is_some());
}

#[tokio::test(start_paused = true)]
async fn importing_a_profile_after_a_failed_automatic_refresh_downloads_at_once() {
    let h = after_a_failed_automatic_refresh().await;
    let before = h.feed.count("https://a.example/s");
    let imported = Store {
        subscriptions: vec![subscription("a", "https://a.example/s")],
        ..Store::default()
    };

    h.app.import_profile(imported).await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    assert_eq!(
        h.feed.count("https://a.example/s"),
        before + 1,
        "the import is not held back by the pause after the failure"
    );
    assert!(h.app.snapshot().subscriptions[0].updated_at.is_some());
}

#[tokio::test(start_paused = true)]
async fn imported_source_retries_when_its_first_refresh_is_cancelled() {
    let url = "https://a.example/s";
    let h = after_a_failed_automatic_refresh().await;
    let before = h.feed.count(url);
    let errors_before = h
        .app
        .snapshot()
        .journal
        .iter()
        .filter(|entry| entry.level == "error")
        .count();
    *h.feed.gate.lock().unwrap() = Some(url.into());
    let imported = Store {
        servers: vec![server(1, None)],
        subscriptions: vec![subscription("a", url)],
        ..Store::default()
    };

    h.app.import_profile(imported).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), h.feed.entered.notified())
        .await
        .expect("the import refresh entered the fetch");
    // Only a change of the generated config interrupts a download: a theme alone does not.
    let mut settings = h.app.snapshot().settings;
    settings.proxy_port = 2090;
    h.app
        .execute(Action::SaveSettings { settings })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    assert_eq!(
        h.feed.count(url),
        before + 2,
        "the cancelled first import refresh is repeated on the next tick"
    );
    let snapshot = h.app.snapshot();
    assert!(snapshot.subscriptions[0].updated_at.is_some());
    assert_eq!(
        snapshot
            .journal
            .iter()
            .filter(|entry| entry.level == "error")
            .count(),
        errors_before,
        "the cancellation adds no error entry"
    );
}

#[tokio::test(start_paused = true)]
async fn adding_a_source_after_a_failed_automatic_refresh_downloads_it_at_once() {
    let h = after_a_failed_automatic_refresh().await;
    assert_eq!(
        h.feed.count("https://b.example/s"),
        0,
        "premise: b has not been added"
    );

    h.app
        .execute(Action::SaveSubscription {
            id: None,
            url: "https://b.example/s".into(),
            interval_mins: None,
            user_agent: None,
            filters: Default::default(),
            provider_routes: true,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;

    assert_eq!(
        h.feed.count("https://b.example/s"),
        1,
        "the pause after the failure does not hold back a newly added source"
    );
}

#[tokio::test(start_paused = true)]
async fn saving_a_source_clears_its_own_backoff() {
    let url = "https://a.example/s";
    let store = Store {
        subscriptions: vec![subscription("a", url)],
        ..Store::default()
    };
    let feed = Feed::default();
    feed.bodies.lock().unwrap().insert(url.into(), None);
    let h = start(store, Net::default(), feed);
    until(&h.app, "the source failure", |snapshot| {
        snapshot.subscriptions[0].error.is_some()
    })
    .await;
    assert_eq!(h.feed.count(url), 1, "premise: one failed fetch");
    h.feed
        .bodies
        .lock()
        .unwrap()
        .insert(url.into(), Some("trojan://p@example.com:1".into()));

    h.app
        .execute(Action::SaveSubscription {
            id: Some("a".into()),
            url: url.into(),
            interval_mins: None,
            user_agent: None,
            filters: Default::default(),
            provider_routes: true,
        })
        .await
        .unwrap();
    // Let the notified worker run before the paused clock can jump to the timeout deadline.
    tokio::task::yield_now().await;
    until(&h.app, "the saved source to refresh", |snapshot| {
        snapshot.subscriptions[0].updated_at.is_some()
    })
    .await;

    assert_eq!(
        h.feed.count(url),
        2,
        "saving explicitly retries instead of preserving the failed fetch backoff"
    );
    let source = &h.app.snapshot().subscriptions[0];
    assert!(source.updated_at.is_some() && source.error.is_none());
}

/// `a` is refreshed by hand and parked mid-fetch, `b` is healthy and not due for an hour, `c`
/// failed at startup and is due again after its one-minute backoff. While `a` is in flight the
/// background worker asks for an automatic refresh of everything due and the user asks for a
/// manual refresh of `c`, in either order. Only `c` may be downloaded by the merged batch.
async fn merged_refresh_forces_only_the_requested_source(manual_first: bool) {
    let store = Store {
        subscriptions: vec![
            subscription("a", "https://a.example/s"),
            subscription("b", "https://b.example/s"),
            subscription("c", "https://c.example/s"),
        ],
        ..Store::default()
    };
    let feed = Feed::default();
    for (url, host) in [("https://a.example/s", "a"), ("https://b.example/s", "b")] {
        feed.bodies
            .lock()
            .unwrap()
            .insert(url.into(), Some(format!("trojan://p@{host}.example:1")));
    }
    feed.bodies
        .lock()
        .unwrap()
        .insert("https://c.example/s".into(), None);
    let h = start(store, Net::default(), feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[0].updated_at.is_some()
            && s.subscriptions[1].updated_at.is_some()
            && s.subscriptions[2].error.is_some()
    })
    .await;
    let before = |url: &str| h.feed.count(url);
    let (before_a, before_b, before_c) = (
        before("https://a.example/s"),
        before("https://b.example/s"),
        before("https://c.example/s"),
    );
    *h.feed.gate.lock().unwrap() = Some("https://a.example/s".into());
    let refresh_a = spawn_refresh(&h.app, Some("a"));
    tokio::time::timeout(Duration::from_secs(5), h.feed.entered.notified())
        .await
        .expect("a refresh entered the fetch");

    let settle = || tokio::time::sleep(Duration::from_secs(3));
    let manual_c;
    if manual_first {
        manual_c = spawn_refresh(&h.app, Some("c"));
        settle().await;
        tokio::time::advance(Duration::from_secs(120)).await;
        settle().await;
    } else {
        tokio::time::advance(Duration::from_secs(120)).await;
        settle().await;
        manual_c = spawn_refresh(&h.app, Some("c"));
        settle().await;
    }
    h.feed.release.notify_one();
    finished(refresh_a, "refresh of a").await.unwrap();
    finished(manual_c, "manual refresh of c").await.unwrap();

    assert_eq!(h.feed.count("https://a.example/s"), before_a + 1);
    assert_eq!(
        h.feed.count("https://b.example/s"),
        before_b,
        "a healthy source that is not due keeps its schedule"
    );
    assert_eq!(
        h.feed.count("https://c.example/s"),
        before_c + 1,
        "the manual and the automatic request share one download of c"
    );
    assert!(!h.app.snapshot().refreshing);
}

#[tokio::test(start_paused = true)]
async fn manual_refresh_queued_behind_an_automatic_refresh_all_does_not_force_other_sources() {
    merged_refresh_forces_only_the_requested_source(false).await;
}

#[tokio::test(start_paused = true)]
async fn automatic_refresh_all_queued_behind_a_manual_refresh_does_not_force_other_sources() {
    merged_refresh_forces_only_the_requested_source(true).await;
}

#[tokio::test]
async fn dropped_refresh_leader_fails_its_waiters_and_reopens_the_gate() {
    // The leader owns the single-flight gate. If its task is aborted mid-download, the
    // requests joined to it must not wait for ever, and the next refresh must be able to lead.
    let (store, feed) = two_feeds(false);
    let h = start(store, Net::default(), feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[0].updated_at.is_some()
    })
    .await;
    let before = h.feed.count("https://a.example/s");
    *h.feed.gate.lock().unwrap() = Some("https://a.example/s".into());

    let leader = spawn_refresh(&h.app, Some("a"));
    tokio::time::timeout(Duration::from_secs(5), h.feed.entered.notified())
        .await
        .expect("the leader entered the fetch");
    let follower = spawn_refresh(&h.app, Some("a"));
    let queued = spawn_refresh(&h.app, None);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(h.app.snapshot().refreshing);

    leader.abort();
    assert!(leader.await.unwrap_err().is_cancelled());
    let follower = finished(follower, "the joined request").await;
    assert!(matches!(follower, Err(Error::Cancelled)), "{follower:?}");
    let queued = finished(queued, "the queued request").await;
    assert!(matches!(queued, Err(Error::Cancelled)), "{queued:?}");
    assert!(!h.app.snapshot().refreshing);

    let again = tokio::time::timeout(
        Duration::from_secs(5),
        h.app.execute(Action::Refresh {
            id: Some("a".into()),
        }),
    )
    .await
    .expect("the gate is open again");
    again.unwrap();
    assert_eq!(h.feed.count("https://a.example/s"), before + 2);
    assert!(!h.app.snapshot().refreshing);
}

/// Panics on its first download, then serves a valid list.
struct PanickingOnce(AtomicBool);
#[async_trait]
impl SubscriptionSource for PanickingOnce {
    async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
        assert!(
            self.0.swap(true, Ordering::SeqCst),
            "injected download panic"
        );
        Ok("trojan://p@a.example:1".into())
    }
}

#[tokio::test]
async fn panicking_refresh_leader_reopens_the_gate() {
    // No worker here: the panic must come from the explicit refresh, and the next explicit
    // refresh must not wait behind it.
    let store = Store {
        subscriptions: vec![subscription("a", "https://a.example/s")],
        ..Store::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let app = Application::with_services(
        store,
        dir.path().into(),
        Box::new(CheckingRuntime {
            alive: Arc::new(AtomicBool::new(false)),
            starts: Arc::new(AtomicUsize::new(0)),
        }),
        Arc::new(Net::default()),
        SECRET.into(),
        Arc::new(PanickingOnce(AtomicBool::new(false))),
    );
    let leader = spawn_refresh(&app, None);
    assert!(finished_with_error(leader, "the panicking leader")
        .await
        .is_panic());
    assert!(!app.snapshot().refreshing);

    let again = tokio::time::timeout(
        Duration::from_secs(5),
        app.execute(Action::Refresh { id: None }),
    )
    .await
    .expect("the gate is open again");
    again.unwrap();
    assert!(app.snapshot().subscriptions[0].updated_at.is_some());
    assert!(!app.snapshot().refreshing);
}

#[tokio::test]
async fn a_plain_cancel_after_a_disconnect_still_lets_the_queued_refresh_run() {
    // Disconnect leaves `disconnecting` set until the next Connect, and refreshing while
    // disconnected is allowed. Only a Disconnect that happens while a request waits cancels
    // that request; a later settings change or manual choice must not.
    let (mut store, net) = manual(&[1], false);
    store.subscriptions = vec![
        subscription("a", "https://a.example/s"),
        subscription("b", "https://b.example/s"),
    ];
    let feed = Feed::default();
    for (url, host) in [("https://a.example/s", "a"), ("https://b.example/s", "b")] {
        feed.bodies
            .lock()
            .unwrap()
            .insert(url.into(), Some(format!("trojan://p@{host}.example:1")));
    }
    let h = start(store, net, feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions.iter().all(|sub| sub.updated_at.is_some())
    })
    .await;
    h.app.execute(Action::Connect).await.unwrap();
    h.app.execute(Action::Disconnect).await.unwrap();
    let before_b = h.feed.count("https://b.example/s");
    *h.feed.gate.lock().unwrap() = Some("https://a.example/s".into());

    let refresh_a = spawn_refresh(&h.app, Some("a"));
    tokio::time::timeout(Duration::from_secs(5), h.feed.entered.notified())
        .await
        .expect("a refresh entered the fetch");
    let refresh_b = spawn_refresh(&h.app, Some("b"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = h
        .app
        .execute(Action::Select {
            id: "no-such-server".into(),
        })
        .await;

    assert!(finished(refresh_a, "cancelled refresh").await.is_err());
    finished(refresh_b, "queued refresh")
        .await
        .expect("the queued refresh is not cancelled by a plain cancel");
    assert_eq!(h.feed.count("https://b.example/s"), before_b + 1);
    assert!(!h.app.snapshot().refreshing);
}

#[tokio::test]
async fn disconnect_cancels_the_active_and_coalesced_refreshes() {
    let (store, feed) = two_feeds(true);
    feed.bodies.lock().unwrap().insert(
        "https://b.example/s".into(),
        Some("trojan://p@b.example:1".into()),
    );
    let h = start(store, Net::default(), feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[1].updated_at.is_some()
    })
    .await;
    let before_b = h.feed.count("https://b.example/s");
    *h.feed.gate.lock().unwrap() = Some("https://a.example/s".into());

    let refresh_a = {
        let app = h.app.clone();
        tokio::spawn(async move {
            app.execute(Action::Refresh {
                id: Some("a".into()),
            })
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), h.feed.entered.notified())
        .await
        .expect("a refresh entered the fetch");
    let refresh_b = {
        let app = h.app.clone();
        tokio::spawn(async move {
            app.execute(Action::Refresh {
                id: Some("b".into()),
            })
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;

    h.app.execute(Action::Disconnect).await.unwrap();
    assert!(refresh_a.await.unwrap().is_err());
    assert!(refresh_b.await.unwrap().is_err());
    assert_eq!(h.feed.count("https://b.example/s"), before_b);
    assert!(!h.app.snapshot().refreshing);
}

#[tokio::test]
async fn removing_a_subscription_during_its_in_flight_refresh_does_not_resurrect_servers() {
    // The coordinator lock is released during the fetch; a concurrent RemoveSubscription
    // can now commit before the fetch's result is merged back in. Without an existence
    // check at merge time, that merge would resurrect servers under the removed id —
    // permanently orphaned (RemoveServer only matches manually-added servers).
    let store = Store {
        subscriptions: vec![subscription("a", "https://a.example/s")],
        ..Store::default()
    };
    let feed = Feed::default();
    feed.bodies.lock().unwrap().insert(
        "https://a.example/s".into(),
        Some("trojan://p@a.example:1".into()),
    );
    let h = start(store, Net::default(), feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[0].updated_at.is_some()
    })
    .await;
    *h.feed.gate.lock().unwrap() = Some("https://a.example/s".into());

    let refresh_a = {
        let app = h.app.clone();
        tokio::spawn(async move {
            app.execute(Action::Refresh {
                id: Some("a".into()),
            })
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), h.feed.entered.notified())
        .await
        .expect("a's refresh is gated mid-fetch");

    h.app
        .execute(Action::RemoveSubscription { id: "a".into() })
        .await
        .unwrap();
    assert!(h.app.snapshot().subscriptions.is_empty());

    h.feed.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), refresh_a)
        .await
        .expect("a's refresh task finished")
        .unwrap()
        .unwrap();

    let snapshot = h.app.snapshot();
    assert!(
        snapshot.subscriptions.is_empty(),
        "subscription stays removed"
    );
    assert!(
        snapshot.servers.is_empty(),
        "the in-flight fetch's servers were not resurrected under the removed id"
    );
}

#[tokio::test]
async fn editing_a_subscription_url_during_its_in_flight_refresh_discards_the_old_url_result() {
    // Same two-phase hazard as removal: the lock is released during the fetch, and
    // SaveSubscription may change the URL of an existing id in place. The old URL's
    // content is then stale and must not be merged into the subscription under the new one.
    let store = Store {
        subscriptions: vec![subscription("a", "https://a.example/old")],
        ..Store::default()
    };
    let feed = Feed::default();
    feed.bodies.lock().unwrap().insert(
        "https://a.example/old".into(),
        Some("trojan://p@a.example:1#original".into()),
    );
    let h = start(store, Net::default(), feed);
    until(&h.app, "startup refresh", |s| {
        s.subscriptions[0].updated_at.is_some()
    })
    .await;
    let names = |h: &H| -> Vec<String> {
        h.app
            .snapshot()
            .servers
            .iter()
            .map(|s| s.name.clone())
            .collect()
    };
    assert_eq!(names(&h), ["original"]);
    *h.feed.gate.lock().unwrap() = Some("https://a.example/old".into());

    let refresh_a = {
        let app = h.app.clone();
        tokio::spawn(async move {
            app.execute(Action::Refresh {
                id: Some("a".into()),
            })
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), h.feed.entered.notified())
        .await
        .expect("a's refresh of the old URL is gated mid-fetch");

    // The replacement URL is unreachable, so the only way servers can change is by
    // merging the old URL's body, which is exactly what must not happen.
    h.app
        .execute(Action::SaveSubscription {
            id: Some("a".into()),
            url: "https://a.example/new".into(),
            interval_mins: None,
            user_agent: None,
            filters: Default::default(),
            provider_routes: true,
        })
        .await
        .unwrap();
    h.feed.bodies.lock().unwrap().insert(
        "https://a.example/old".into(),
        Some("trojan://p@a.example:2#stale".into()),
    );
    h.feed.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), refresh_a)
        .await
        .expect("a's refresh task finished")
        .unwrap()
        .unwrap();

    until(&h.app, "the new URL was fetched (and failed)", |s| {
        s.subscriptions[0].error.is_some()
    })
    .await;
    assert!(h.feed.count("https://a.example/new") >= 1);
    assert_eq!(
        names(&h),
        ["original"],
        "content fetched from the replaced URL was not merged"
    );
}

#[tokio::test(start_paused = true)]
async fn a_manual_refresh_does_not_cancel_a_pending_automatic_retry() {
    // subscription_retry_at is the worker's own fallback timer for a structural (non-fetch)
    // failure; an unrelated, successful manual refresh must not reset it, or it would
    // silently cancel a retry the automatic cycle still owes from an earlier failure.
    let mut store = Store {
        subscriptions: vec![
            subscription("a", "https://a.example/s"),
            subscription("b", "https://b.example/s"),
        ],
        ..Store::default()
    };
    store.settings.proxy_port = 10; // CheckingRuntime rejects any config at this port.
    let feed = Feed::default();
    feed.bodies.lock().unwrap().insert(
        "https://a.example/s".into(),
        Some("trojan://p@a.example:1".into()),
    );
    feed.bodies.lock().unwrap().insert(
        "https://b.example/s".into(),
        Some("trojan://p@b.example:1".into()),
    );
    let h = start(store, Net::default(), feed);
    tokio::time::timeout(Duration::from_secs(5), async {
        while h.feed.total() < 2 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("startup refresh attempted both subscriptions");
    let after_startup = h.feed.total();
    // Startup's own refresh already failed structurally (runtime rejects the config),
    // scheduling subscription_retry_at ~60s out. A manual refresh of a's own
    // subscription succeeds at the fetch level (replace() still fails the same way).
    h.app
        .execute(Action::Refresh {
            id: Some("a".into()),
        })
        .await
        .unwrap_err();
    tokio::time::advance(Duration::from_secs(30)).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        h.feed.total(),
        after_startup + 1,
        "only the manual refresh of a ran; no automatic retry yet at 30s"
    );
    tokio::time::advance(Duration::from_secs(40)).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(
        h.feed.total() > after_startup + 1,
        "the automatic retry still fired ~60s after startup, not cancelled by the \
         unrelated manual refresh at 30s"
    );
}

#[tokio::test]
async fn a_failing_selected_server_does_not_look_connected() {
    // Selected A answers first but fails through the proxy; B is checked next.
    let (mut store, net) = manual(&[1, 2], true);
    store.selected = Some(store.servers[0].id.clone());
    net.set(1, Some(10), None);
    net.set(2, Some(20), None);
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "failed first cycle", |s| {
        s.status == "error" && !s.checking
    })
    .await;
    h.net.set(2, Some(20), Some(40));
    *h.net.gate.lock().unwrap() = Some(2);
    h.app.execute(Action::CheckNow).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), h.net.entered.notified())
        .await
        .expect("cycle reached B");
    assert_eq!(
        h.app.snapshot().status,
        "error",
        "A failed; nothing is connected yet"
    );
    h.net.release.notify_one();
    until(&h.app, "switch to B", |s| s.status == "connected").await;
    assert_eq!(h.app.snapshot().servers[1].failure_code, None);
}

#[tokio::test(start_paused = true)]
async fn nothing_is_checked_while_disconnected() {
    let (store, net) = manual(&[1, 2], true);
    let h = start(store, net, Feed::default());
    tokio::time::sleep(Duration::from_secs(180)).await;
    assert!(h.net.proxied().is_empty(), "no probes without a connection");
    assert_eq!(h.app.snapshot().status, "disconnected");
}

// ---- what the provider says: name, traffic, term; per-subscription options ------------------

const TROJAN: &str = "trojan://p@example.com:1";

fn usage(used: u64, total: u64, expire: Option<u64>) -> Usage {
    Usage {
        used,
        total,
        expire,
    }
}
fn provider(feed: &Feed, url: &str, body: &str, title: Option<&str>, usage: Option<Usage>) {
    feed.bodies
        .lock()
        .unwrap()
        .insert(url.into(), Some(body.into()));
    feed.meta
        .lock()
        .unwrap()
        .insert(url.into(), (title.map(Into::into), usage));
}
async fn refreshed(app: &Application, id: &str) {
    until(app, "the subscription to refresh", |s| {
        s.subscriptions
            .iter()
            .any(|source| source.id == id && source.updated_at.is_some())
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn the_provider_names_the_subscription_and_reports_its_usage() {
    let url = "https://a.example/s";
    let store = Store {
        subscriptions: vec![subscription("a", url)],
        ..Store::default()
    };
    let feed = Feed::default();
    provider(
        &feed,
        url,
        TROJAN,
        Some("My VPN\n"),
        Some(usage(40, 100, None)),
    );
    let h = start(store, Net::default(), feed);
    refreshed(&h.app, "a").await;
    let view = h.app.snapshot().subscriptions[0].clone();
    assert_eq!(
        view.name, "My VPN",
        "the title replaces the placeholder, cleaned"
    );
    assert_eq!(view.usage, Some(usage(40, 100, None)));
    assert_eq!(view.availability, Availability::Ok);

    // A later answer without a title keeps the name; without traffic it clears the figures.
    provider(&h.feed, url, TROJAN, None, None);
    h.app
        .execute(Action::Refresh {
            id: Some("a".into()),
        })
        .await
        .unwrap();
    let view = h.app.snapshot().subscriptions[0].clone();
    assert_eq!(view.name, "My VPN");
    assert_eq!(view.usage, None);

    // A new title renames it again, and used-up traffic shows as such.
    provider(
        &h.feed,
        url,
        TROJAN,
        Some("Новое имя"),
        Some(usage(100, 100, None)),
    );
    h.app
        .execute(Action::Refresh {
            id: Some("a".into()),
        })
        .await
        .unwrap();
    let view = h.app.snapshot().subscriptions[0].clone();
    assert_eq!(view.name, "Новое имя");
    assert_eq!(view.availability, Availability::Exhausted);
}

#[tokio::test(start_paused = true)]
async fn every_subscription_sends_its_own_user_agent() {
    let (a, b) = ("https://a.example/s", "https://b.example/s");
    let mut with_agent = subscription("a", a);
    with_agent.user_agent = Some("Happ/1.0".into());
    let store = Store {
        subscriptions: vec![with_agent, subscription("b", b)],
        ..Store::default()
    };
    let feed = Feed::default();
    provider(&feed, a, TROJAN, None, None);
    provider(&feed, b, "trojan://p@example.com:2", None, None);
    let h = start(store, Net::default(), feed);
    refreshed(&h.app, "a").await;
    refreshed(&h.app, "b").await;
    let agents = h.feed.agents.lock().unwrap().clone();
    assert!(agents.contains(&(a.into(), Some("Happ/1.0".into()))));
    assert!(
        agents.contains(&(b.into(), None)),
        "none means Rift's own User-Agent"
    );
}

#[tokio::test(start_paused = true)]
async fn filters_drop_servers_at_import_and_never_leave_an_empty_source() {
    let url = "https://a.example/s";
    let mut filtered = subscription("a", url);
    filtered.filters = Filters {
        include: vec!["keep".into()],
        ..Filters::default()
    };
    let store = Store {
        subscriptions: vec![filtered],
        ..Store::default()
    };
    let feed = Feed::default();
    provider(
        &feed,
        url,
        "trojan://p@example.com:1#keep-1\ntrojan://p@example.com:2#drop\ntrojan://p@example.com:3#KEEP-2",
        None,
        None,
    );
    let h = start(store, Net::default(), feed);
    refreshed(&h.app, "a").await;
    let snapshot = h.app.snapshot();
    assert_eq!(
        snapshot.servers.len(),
        2,
        "the filtered server is not stored"
    );
    assert!(snapshot
        .journal
        .iter()
        .any(|entry| entry.message.contains("отфильтровано: 1")));

    // New filters that match nothing: an error of the subscription, the servers stay.
    h.app
        .execute(Action::SaveSubscription {
            id: Some("a".into()),
            url: url.into(),
            interval_mins: None,
            user_agent: None,
            filters: Filters {
                include: vec!["nothing".into()],
                ..Filters::default()
            },
            provider_routes: true,
        })
        .await
        .unwrap();
    until(&h.app, "the refresh with the new filters", |s| {
        s.subscriptions[0].failure_code == Some(FailureCode::SubscriptionFiltered)
    })
    .await;
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.servers.len(), 2, "the previous servers are kept");
    assert!(snapshot.subscriptions[0].error.is_some());
}

#[tokio::test(start_paused = true)]
async fn a_subscription_with_its_own_interval_is_refreshed_on_that_schedule() {
    let (fast, slow) = ("https://fast.example/s", "https://slow.example/s");
    let mut quick = subscription("fast", fast);
    quick.interval_mins = Some(2);
    let store = Store {
        subscriptions: vec![quick, subscription("slow", slow)],
        ..Store::default()
    };
    let feed = Feed::default();
    provider(&feed, fast, TROJAN, None, None);
    provider(&feed, slow, "trojan://p@example.com:2", None, None);
    let h = start(store, Net::default(), feed);
    refreshed(&h.app, "fast").await;
    refreshed(&h.app, "slow").await;
    assert_eq!((h.feed.count(fast), h.feed.count(slow)), (1, 1));
    tokio::time::sleep(Duration::from_secs(115)).await;
    assert_eq!(h.feed.count(fast), 1, "not due before its two minutes");
    tokio::time::sleep(Duration::from_secs(15)).await;
    assert_eq!(h.feed.count(fast), 2, "refreshed after its own interval");
    assert_eq!(h.feed.count(slow), 1, "the global hour has not passed");
}

fn priced(port: u16, source: &str, used_up: bool) -> (Server, Subscription) {
    let mut sub = subscription(source, &format!("https://{source}.example/s"));
    sub.usage = Some(usage(if used_up { 10 } else { 1 }, 10, None));
    (server(port, Some(source)), sub)
}

#[tokio::test]
async fn auto_select_never_probes_or_picks_a_used_up_subscription() {
    let (used_server, used) = priced(1, "used", true);
    let (fine_server, fine) = priced(3, "fine", false);
    let store = Store {
        servers: vec![used_server, server(2, None), fine_server],
        subscriptions: vec![used, fine],
        ..Store::default()
    };
    let net = Net::default();
    net.set(1, Some(1), Some(1)); // the fastest, but not allowed
    net.set(2, Some(20), Some(60));
    net.set(3, Some(30), Some(40));
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "the first cycle", |s| {
        !s.checking && s.next_check.is_some()
    })
    .await;
    assert!(
        !h.net.endpoint_calls.lock().unwrap().contains(&1),
        "a used-up source is not even probed"
    );
    let chosen = h.app.snapshot().selected.unwrap();
    assert_eq!(chosen, server(3, None).id, "the best of the allowed ones");
}

#[tokio::test]
async fn a_selected_server_of_an_exhausted_subscription_is_left_for_an_allowed_one() {
    let (used_server, used) = priced(1, "used", true);
    let selected = used_server.id.clone();
    let store = Store {
        servers: vec![used_server, server(2, None)],
        subscriptions: vec![used],
        selected: Some(selected.clone()),
        ..Store::default()
    };
    let net = Net::default();
    net.set(1, Some(1), Some(1));
    net.set(2, Some(20), Some(60));
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "the first cycle", |s| {
        !s.checking && s.next_check.is_some()
    })
    .await;
    assert_eq!(h.app.snapshot().selected, Some(server(2, None).id));
}

#[tokio::test]
async fn the_servers_of_a_used_up_subscription_stay_available_by_hand() {
    let (used_server, used) = priced(1, "used", true);
    let id = used_server.id.clone();
    let store = Store {
        servers: vec![used_server, server(2, None)],
        subscriptions: vec![used],
        ..Store::default()
    };
    let net = Net::default();
    net.set(1, Some(1), Some(1));
    net.set(2, Some(20), Some(60));
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    h.app
        .execute(Action::Select { id: id.clone() })
        .await
        .unwrap();
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.selected, Some(id));
    assert!(
        snapshot.settings.auto_select,
        "a manual choice leaves Auto Select on"
    );
    assert_eq!(snapshot.servers.len(), 2, "still listed");
}

#[tokio::test]
async fn with_every_source_used_up_auto_select_reports_no_available_server() {
    let (used_server, used) = priced(1, "used", true);
    let store = Store {
        servers: vec![used_server],
        subscriptions: vec![used],
        ..Store::default()
    };
    let net = Net::default();
    net.set(1, Some(1), Some(1));
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "the cycle to end", |s| {
        !s.checking && s.status == "error"
    })
    .await;
    let snapshot = h.app.snapshot();
    assert!(snapshot
        .journal
        .iter()
        .any(|entry| entry.failure_code == Some(FailureCode::NoAvailableServer)));
    assert!(h.net.endpoint_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_term_that_ends_while_rift_runs_is_shown_without_a_refresh() {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut source = subscription("a", "https://a.example/s");
    source.usage = Some(usage(0, 0, Some(now + 2)));
    let store = Store {
        subscriptions: vec![source],
        ..Store::default()
    };
    let h = start(store, Net::default(), Feed::default());
    assert_eq!(
        h.app.snapshot().subscriptions[0].availability,
        Availability::Ok
    );
    until(&h.app, "the term to end", |s| {
        s.subscriptions[0].availability == Availability::Expired
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_cycle_whose_switch_cannot_set_the_rules_puts_the_selector_back() {
    // B (port 1) is in use and slow, C (port 2) is much faster and brings rules that cannot be set.
    let mut store = Store {
        servers: vec![server(1, None), server(2, None)],
        ..Store::default()
    };
    store.servers[1].routes = Some(ServerRoutes {
        block: vec!["udp:443".into()],
        direct: vec![],
    });
    let (b, c) = (store.servers[0].id.clone(), store.servers[1].id.clone());
    store.selected = Some(b.clone());
    let net = Net::default();
    net.set(1, Some(10), Some(300));
    net.set(2, Some(10), Some(40));
    net.routes_refused.store(true, Ordering::SeqCst);
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "the cycle to try the faster server", |_| {
        h.net.selects.lock().unwrap().contains(&c)
    })
    .await;
    idle(&h.app).await;
    let selects = h.net.selects.lock().unwrap().clone();
    assert_eq!(
        selects.last(),
        Some(&b),
        "after the failed switch the selector is on the kept choice again: {selects:?}"
    );
    assert_eq!(h.app.snapshot().selected, Some(b));
}

#[tokio::test]
async fn a_new_order_of_the_servers_shows_while_a_check_still_holds_the_coordinator() {
    let (store, net) = manual(&[1, 2], true);
    // The check of the first server parks: the cycle holds the coordinator until it is let go.
    *net.gate.lock().unwrap() = Some(1);
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    h.net.entered.notified().await;
    let order = {
        let app = h.app.clone();
        tokio::spawn(async move {
            app.execute(Action::SetServersSort {
                column: SortColumn::ProxyMs,
                descending: true,
            })
            .await
        })
    };
    until(&h.app, "the new order to show", |s| {
        s.servers_sort.column == SortColumn::ProxyMs && s.servers_sort.descending
    })
    .await;
    // It is kept once the check lets go.
    h.net.release.notify_one();
    order.await.unwrap().unwrap();
    idle(&h.app).await;
    let kept = persistence::load(&h.dir.path().join("state.json")).unwrap();
    assert_eq!(kept.servers_sort.column, SortColumn::ProxyMs);
    assert!(kept.servers_sort.descending);
}

#[derive(Default)]
struct LaunchCheckingRuntime {
    alive: bool,
    starts: Arc<AtomicUsize>,
    stops: Arc<AtomicUsize>,
}
#[async_trait]
impl Runtime for LaunchCheckingRuntime {
    async fn validate(&mut self, store: &Store, secret: &str) -> Result<()> {
        rift_core::launch::LaunchPlan::build(
            store,
            secret,
            &rift_core::geodata::GeoSet::none(),
            rift_core::config::Resolver::Bridge(1),
            65533,
            SECRET,
        )
        .map(|_| ())
    }
    async fn start(&mut self, store: &Store, secret: &str) -> Result<()> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.validate(store, secret).await?;
        if self.alive {
            self.stops.fetch_add(1, Ordering::SeqCst);
        }
        self.alive = true;
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        self.stops
            .fetch_add(usize::from(self.alive), Ordering::SeqCst);
        self.alive = false;
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(self.alive)
    }
}

#[tokio::test]
async fn retired_selection_does_not_lock_disconnected_store_edits() {
    for all_retired in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::default();
        store.settings.auto_select = false;
        let retired = parse_uri("ss://aes-128-cfb:synthetic@legacy.example:443", None).unwrap();
        store.selected = Some(retired.id.clone());
        store.servers.push(retired);
        if !all_retired {
            store.servers.push(server(1, None));
        }
        store
            .subscriptions
            .push(subscription("a", "https://a.example/s"));
        let feed = Arc::new(Feed::default());
        feed.bodies.lock().unwrap().insert(
            "https://a.example/s".into(),
            Some("trojan://p@new.example:4".into()),
        );
        let app = Application::with_services(
            store,
            dir.path().into(),
            Box::new(LaunchCheckingRuntime::default()),
            Arc::new(Net::default()),
            SECRET.into(),
            feed.clone(),
        );
        let mut settings = app.snapshot().settings;
        settings.proxy_port = 38111;
        app.execute(Action::SaveSettings { settings })
            .await
            .unwrap();
        app.execute(Action::AddServer {
            uri: "trojan://p@new.example:2".into(),
        })
        .await
        .unwrap();
        app.execute(Action::AddServers {
            uris: vec!["trojan://p@new.example:3".into()],
        })
        .await
        .unwrap();
        app.execute(Action::Refresh { id: None }).await.unwrap();
        assert_eq!(feed.total(), 1);
        assert_eq!(
            app.snapshot().servers.len(),
            if all_retired { 4 } else { 5 }
        );
        assert_eq!(
            app.execute(Action::Connect)
                .await
                .unwrap_err()
                .failure_code(),
            Some(FailureCode::UnsupportedLegacyParameters)
        );
        assert!(!app.snapshot().runtime_active);
        let saved = persistence::load(&dir.path().join("state.json")).unwrap();
        assert_eq!(saved.settings.proxy_port, 38111);
        app.execute(Action::Select {
            id: saved
                .servers
                .iter()
                .find(|s| !s.is_retired())
                .unwrap()
                .id
                .clone(),
        })
        .await
        .unwrap();
        app.execute(Action::Connect).await.unwrap();
        app.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn connected_retired_selection_replacement_is_refused_without_touching_the_runtime() {
    for refresh in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::default();
        store.settings.auto_select = false;
        store.servers = vec![server(1, Some("a")), server(2, None)];
        store.selected = Some(store.servers[0].id.clone());
        store
            .subscriptions
            .push(subscription("a", "https://a.example/s"));
        let mut imported = store.clone();
        imported.servers[0] =
            parse_uri("ss://aes-128-cfb:synthetic@example.com:1", Some("a".into())).unwrap();
        imported.selected = Some(imported.servers[0].id.clone());
        let runtime = LaunchCheckingRuntime::default();
        let starts = runtime.starts.clone();
        let stops = runtime.stops.clone();
        let net = Arc::new(Net::default());
        let feed = Arc::new(Feed::default());
        feed.bodies.lock().unwrap().insert(
            "https://a.example/s".into(),
            Some("ss://aes-128-cfb:synthetic@example.com:1".into()),
        );
        let app = Application::with_services(
            store.clone(),
            dir.path().into(),
            Box::new(runtime),
            net.clone(),
            SECRET.into(),
            feed,
        );
        app.execute(Action::Connect).await.unwrap();
        let before = app.snapshot();
        let saved = std::fs::read(dir.path().join("state.json")).unwrap();
        let selects = net.selects.lock().unwrap().clone();
        let error = if refresh {
            app.execute(Action::Refresh {
                id: Some("a".into()),
            })
            .await
            .unwrap_err()
        } else {
            app.import_profile(imported).await.unwrap_err()
        };
        assert_eq!(
            error.failure_code(),
            Some(FailureCode::UnsupportedLegacyParameters)
        );
        assert_eq!(starts.load(Ordering::SeqCst), 1, "no start or restore");
        assert_eq!(stops.load(Ordering::SeqCst), 0, "no teardown");
        assert!(app.snapshot().runtime_active);
        assert_eq!(app.snapshot().status, before.status);
        assert_eq!(app.snapshot().selected, store.selected);
        assert_eq!(*net.selects.lock().unwrap(), selects);
        assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), saved);
        app.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn implicit_replacement_skips_retired_and_stops_when_no_supported_node_remains() {
    for replacement in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::default();
        store.settings.auto_select = false;
        store.servers = vec![
            parse_uri("ss://aes-128-cfb:synthetic@legacy.example:443", None).unwrap(),
            server(1, None),
        ];
        store.selected = Some(store.servers[1].id.clone());
        let removed = store.selected.clone().unwrap();
        let expected = replacement.then(|| server(2, None).id);
        if replacement {
            store.servers.push(server(2, None));
        }
        let app = Application::with_services(
            store,
            dir.path().into(),
            Box::new(LaunchCheckingRuntime::default()),
            Arc::new(Net::default()),
            SECRET.into(),
            Arc::new(Feed::default()),
        );
        app.execute(Action::Connect).await.unwrap();
        app.execute(Action::RemoveServer { id: removed })
            .await
            .unwrap();
        assert_eq!(app.snapshot().selected, expected);
        assert_eq!(app.snapshot().runtime_active, replacement);
        let saved = persistence::load(&dir.path().join("state.json")).unwrap();
        assert_eq!(saved.selected, expected);
        assert!(saved.servers[0].is_retired());
        app.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn recheck_infrastructure_failure_does_not_cancel_future_rechecks() {
    let (mut store, net) = manual(&[], true);
    store.servers = vec![
        parse_uri("trojan://p@one.example:1", None).unwrap(),
        parse_uri("trojan://p@two.example:2", None).unwrap(),
    ];
    let two = store.servers[1].clone();
    net.set(1, Some(10), Some(40));
    net.set(2, None, None);
    store.settings.pool_size = 1;
    store.settings.interval_secs = 300;
    store.settings.full_interval_mins = 30;
    let h = start(store, net, Feed::default());
    h.app.execute(Action::Connect).await.unwrap();
    wait_until("initial measured", || {
        row(&h.app, &two).failure_code.is_some() && !h.app.snapshot().checking
    })
    .await;
    tokio::time::advance(Duration::from_secs(301)).await;
    h.app.execute(Action::CheckNow).await.unwrap();
    wait_until("quarantined", || {
        row(&h.app, &two).retry_at.is_some() && !h.app.snapshot().checking
    })
    .await;
    assert!(row(&h.app, &two).retry_at.is_some());
    h.net.set(2, Some(10), None);
    h.net.fail_proxy_with(2, FailureCode::XrayCrashed);
    let before = h.net.proxied().iter().filter(|&&p| p == 2).count();
    for _ in 0..320 {
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }
    assert_eq!(
        h.net.proxied().iter().filter(|&&p| p == 2).count(),
        before + 1,
        "an infrastructure failure postpones the pass, not a probe every worker tick"
    );
    let retry = retry_in(&h.app, &two).unwrap();
    assert!(
        (270..=305).contains(&retry),
        "same five-minute wait: {retry}"
    );
    assert_ne!(
        row(&h.app, &two).failure_code,
        Some(FailureCode::XrayCrashed),
        "infrastructure failure is not attributed to the remote"
    );
    h.net.set(2, Some(10), Some(50));
    for _ in 0..200 {
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }
    assert_eq!(
        h.net.proxied().iter().filter(|&&p| p == 2).count(),
        before + 1,
        "repair does not bypass the postponed deadline"
    );
    assert!(row(&h.app, &two).retry_at.is_some());
    for _ in 0..120 {
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }
    assert_eq!(
        h.net.proxied().iter().filter(|&&p| p == 2).count(),
        before + 2,
        "the next due pass recovers, without incrementing the remote failure backoff"
    );
    assert!(row(&h.app, &two).retry_at.is_none());
    assert_eq!(row(&h.app, &two).status, "online");
    h.alive.store(false, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(2)).await;
    until(&h.app, "crash detected", |s| !s.runtime_active).await;
    h.net.set(2, None, None);
    h.app.execute(Action::Connect).await.unwrap();
    wait_until("reconnect measured", || {
        row(&h.app, &two).failure_code.is_some() && !h.app.snapshot().checking
    })
    .await;
    tokio::time::advance(Duration::from_secs(301)).await;
    h.app.execute(Action::CheckNow).await.unwrap();
    wait_until("requarantined", || {
        row(&h.app, &two).retry_at.is_some() && !h.app.snapshot().checking
    })
    .await;
    assert!(row(&h.app, &two).retry_at.is_some());
    h.net.set(2, Some(10), Some(50));
    for _ in 0..320 {
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }
    assert!(row(&h.app, &two).retry_at.is_none());
    assert_eq!(row(&h.app, &two).status, "online");
    h.app.shutdown().await.unwrap();
}

#[tokio::test]
async fn subscription_replacement_of_selected_node_skips_retired_rows() {
    for remove in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::default();
        store.settings.auto_select = false;
        store.servers = vec![
            parse_uri("ss://aes-128-cfb:synthetic@legacy.example:443", None).unwrap(),
            server(1, Some("a")),
        ];
        store.selected = Some(store.servers[1].id.clone());
        store
            .subscriptions
            .push(subscription("a", "https://a.example/s"));
        if remove {
            store.servers.push(server(2, None));
        }
        let feed = Arc::new(Feed::default());
        feed.bodies.lock().unwrap().insert(
            "https://a.example/s".into(),
            Some("trojan://p@example.com:2".into()),
        );
        let app = Application::with_services(
            store,
            dir.path().into(),
            Box::new(LaunchCheckingRuntime::default()),
            Arc::new(Net::default()),
            SECRET.into(),
            feed,
        );
        app.execute(Action::Connect).await.unwrap();
        app.execute(if remove {
            Action::RemoveSubscription { id: "a".into() }
        } else {
            Action::Refresh {
                id: Some("a".into()),
            }
        })
        .await
        .unwrap();
        assert_eq!(app.snapshot().selected, Some(server(2, None).id));
        assert!(app.snapshot().runtime_active);
        let saved = persistence::load(&dir.path().join("state.json")).unwrap();
        assert!(saved.servers[0].is_retired());
        app.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn subscription_restart_cancels_old_recheck_and_due_recovery_still_runs() {
    let (mut state, net) = manual(&[], true);
    state.settings.pool_size = 1;
    state.settings.interval_secs = 300;
    state.settings.full_interval_mins = 30;
    state.servers = vec![
        parse_uri("trojan://p@one.example:1", None).unwrap(),
        parse_uri("trojan://p@two.example:2", None).unwrap(),
        parse_uri("trojan://p@three.example:3", Some("a".into())).unwrap(),
    ];
    state
        .subscriptions
        .push(subscription("a", "https://a.example/s"));
    let two = state.servers[1].clone();
    net.set(1, Some(10), Some(40));
    net.set(2, None, None);
    net.set(3, Some(30), Some(60));
    let feed = Feed::default();
    feed.bodies.lock().unwrap().insert(
        "https://a.example/s".into(),
        Some("trojan://p@three.example:3".into()),
    );
    let h = start(state, net, feed);
    h.app.execute(Action::Connect).await.unwrap();
    wait_until("initial miss", || {
        row(&h.app, &two).failure_code.is_some() && !h.app.snapshot().checking
    })
    .await;
    tokio::time::advance(Duration::from_secs(301)).await;
    h.app.execute(Action::CheckNow).await.unwrap();
    wait_until("quarantine", || {
        row(&h.app, &two).retry_at.is_some() && !h.app.snapshot().checking
    })
    .await;
    h.net.set(2, Some(10), Some(50));
    *h.net.gate.lock().unwrap() = Some(2);
    for _ in 0..310 {
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }
    tokio::time::timeout(Duration::from_secs(5), h.net.entered.notified())
        .await
        .expect("the due recheck reached the recovered server before the runtime restart");
    let before = asked(&h, 2);
    h.net.set(2, None, None);
    h.feed.bodies.lock().unwrap().insert(
        "https://a.example/s".into(),
        Some("trojan://rotated@three.example:3".into()),
    );
    h.app
        .execute(Action::Refresh {
            id: Some("a".into()),
        })
        .await
        .unwrap();
    // The old blocked proxy must be dropped, not allowed to hold the recheck-running flag.
    wait_until("new runtime recheck", || asked(&h, 2) > before).await;
    assert!(row(&h.app, &two).retry_at.is_some());
    h.net.set(2, Some(10), Some(50));
    for _ in 0..620 {
        tokio::time::advance(Duration::from_secs(1)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }
    assert!(row(&h.app, &two).retry_at.is_none());
    assert_eq!(row(&h.app, &two).status, "online");
    h.app.shutdown().await.unwrap();
}
