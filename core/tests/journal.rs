//! The journal by source: what lands in which category, how old entries read, and that one
//! check cycle is one line, not one line per server.
use async_trait::async_trait;
use rift_core::{
    application::{Action, Application, Category, Entry},
    failure::FailureCode,
    model::{Server, Store, Subscription},
    parser::parse_uri,
    runtime::Runtime,
    selection::Probe,
    subscription::SubscriptionSource,
    Error, Result,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

struct FakeRuntime(Arc<AtomicBool>);
#[async_trait]
impl Runtime for FakeRuntime {
    async fn validate(&mut self, _: &Store, _: &str) -> Result<()> {
        Ok(())
    }
    async fn start(&mut self, _: &Store, _: &str) -> Result<()> {
        self.0.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        self.0.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}
/// Every second server answers, the others do not.
struct Alternating;
#[async_trait]
impl Probe for Alternating {
    async fn endpoint(&self, server: &Server) -> Result<u64> {
        if server.port.is_multiple_of(2) {
            Ok(5)
        } else {
            Err(Error::Network)
        }
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        Ok(20)
    }
    async fn select(&self, _: &str) -> Result<()> {
        Ok(())
    }
}
struct BrokenFeed;
#[async_trait]
impl SubscriptionSource for BrokenFeed {
    async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
        Err(Error::Network)
    }
}

fn server(port: u16) -> Server {
    parse_uri(&format!("trojan://p@example.com:{port}"), None).unwrap()
}
fn start(
    store: Store,
    probe: Arc<dyn Probe>,
) -> (Arc<Application>, Arc<AtomicBool>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let alive = Arc::new(AtomicBool::new(false));
    let app = Application::with_services(
        store,
        dir.path().into(),
        Box::new(FakeRuntime(alive.clone())),
        probe,
        "12345678901234567890123456789012".into(),
        Arc::new(BrokenFeed),
    );
    app.start();
    (app, alive, dir)
}
fn in_category(app: &Application, category: Category) -> Vec<Entry> {
    app.snapshot()
        .journal
        .into_iter()
        .filter(|entry| entry.category == category)
        .collect()
}
async fn wait(
    app: &Application,
    what: &str,
    ready: impl Fn(&rift_core::application::Snapshot) -> bool,
) {
    let mut rx = app.snapshots.subscribe();
    tokio::time::timeout(Duration::from_secs(5), rx.wait_for(|s| ready(s)))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .unwrap();
}

#[test]
fn entries_written_before_the_journal_had_sources_read_as_the_application() {
    let old = r#"[{"at":1,"level":"info","message":"Сервер добавлен"},
        {"at":2,"level":"error","message":"x","failure_code":"proxy_timeout"}]"#;
    let entries: Vec<Entry> = serde_json::from_str(old).unwrap();
    assert!(entries.iter().all(|entry| entry.category == Category::App));
    // A value a later version adds is not an error either.
    let future: Entry =
        serde_json::from_str(r#"{"at":3,"level":"info","message":"m","category":"traffic"}"#)
            .unwrap();
    assert_eq!(future.category, Category::App);
}

#[test]
fn the_category_goes_to_the_ui_and_back_under_its_wire_name() {
    for (category, wire) in [
        (Category::App, "app"),
        (Category::Subscriptions, "subscriptions"),
        (Category::Checks, "checks"),
        (Category::Core, "core"),
    ] {
        let entry = Entry {
            at: 1,
            level: "info".into(),
            message: "m".into(),
            details: None,
            crash_id: None,
            failure_code: None,
            category,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["category"], wire);
        assert_eq!(
            serde_json::from_value::<Entry>(json).unwrap().category,
            category
        );
    }
}

#[test]
fn an_error_belongs_to_the_stage_its_code_names() {
    use FailureCode::*;
    for code in [
        EndpointTimeout,
        EndpointConnect,
        ProxyTimeout,
        ProxyTransport,
        ProxyHttpStatus,
        ProxyTestFailed,
        ProxyMissing,
        ConnectionLost,
        ProxyInvalidResponse,
        SelectorSwitch,
        NoAvailableServer,
    ] {
        assert_eq!(
            Category::of_failure(Some(code)),
            Category::Checks,
            "{code:?}"
        );
    }
    for code in [
        SubscriptionTimeout,
        SubscriptionTransport,
        SubscriptionHttpStatus,
        SubscriptionInvalidBody,
        SubscriptionFiltered,
    ] {
        assert_eq!(
            Category::of_failure(Some(code)),
            Category::Subscriptions,
            "{code:?}"
        );
    }
    for code in [
        SidecarConfigRejected,
        SidecarConfigTimeout,
        SidecarStartExit,
        SidecarReadinessTimeout,
        SidecarCrashed,
        SidecarStop,
    ] {
        assert_eq!(Category::of_failure(Some(code)), Category::Core, "{code:?}");
    }
    for code in [Persistence, Cancelled, Autostart, Unknown] {
        assert_eq!(Category::of_failure(Some(code)), Category::App, "{code:?}");
    }
    assert_eq!(Category::of_failure(None), Category::App);
}

#[tokio::test]
async fn the_process_server_choice_and_the_application_each_write_to_their_own_source() {
    let store = Store {
        servers: vec![server(2), server(3), server(4)],
        ..Store::default()
    };
    let (app, _alive, _dir) = start(store, Arc::new(Alternating));
    app.execute(Action::Connect).await.unwrap();
    wait(&app, "the first cycle", |s| {
        !s.checking && s.next_check.is_some()
    })
    .await;
    app.execute(Action::AddServers {
        uris: vec!["trojan://p@added.example:443".into()],
    })
    .await
    .unwrap();
    app.execute(Action::Disconnect).await.unwrap();

    let core: Vec<_> = in_category(&app, Category::Core)
        .into_iter()
        .map(|e| e.message)
        .collect();
    assert_eq!(core, ["sing-box запущен", "Отключено"]);
    let checks: Vec<_> = in_category(&app, Category::Checks)
        .into_iter()
        .map(|e| e.message)
        .collect();
    assert_eq!(checks[0], "Проверка серверов запущена: подключение");
    // The check ends with one line about what it decided, in numbers and without a name.
    let decisions: Vec<&String> = checks
        .iter()
        .filter(|m| m.starts_with("Выбор сервера:"))
        .collect();
    assert_eq!(decisions.len(), 1, "{checks:?}");
    assert!(
        decisions[0].starts_with("Выбор сервера: выбран лучший из ответивших, "),
        "{}",
        decisions[0]
    );
    assert!(!decisions[0].contains("example"), "no name or address");
    let application: Vec<_> = in_category(&app, Category::App)
        .into_iter()
        .map(|e| e.message)
        .collect();
    assert_eq!(application, ["Серверов добавлено: 1"]);
}

#[tokio::test]
async fn a_check_cycle_leaves_one_summary_not_a_line_per_server() {
    // 3 servers answer the TCP check (even ports), 3 do not.
    let store = Store {
        servers: (1..=6).map(server).collect(),
        ..Store::default()
    };
    let (app, _alive, _dir) = start(store, Arc::new(Alternating));
    app.execute(Action::Connect).await.unwrap();
    wait(&app, "the first cycle", |s| {
        !s.checking && s.next_check.is_some()
    })
    .await;
    let checks = in_category(&app, Category::Checks);
    let summaries: Vec<_> = checks
        .iter()
        .filter(|entry| entry.message.starts_with("Проверка завершена"))
        .collect();
    assert_eq!(summaries.len(), 1);
    assert_eq!(
        summaries[0].message,
        "Проверка завершена: проверено 6, достижимо 3, рабочих 3"
    );
    assert!(
        checks.len() <= 4,
        "a cycle writes a few lines, not one per server: {}",
        checks.len()
    );
    for entry in &checks {
        assert!(!entry.message.contains("example.com"), "{}", entry.message);
    }
}

#[tokio::test]
async fn a_failed_subscription_download_is_a_subscription_entry() {
    let store = Store {
        servers: vec![server(1)],
        subscriptions: vec![Subscription {
            id: "a".into(),
            name: "a".into(),
            url: "https://a.example/s".into(),
            ..Default::default()
        }],
        ..Store::default()
    };
    let (app, _alive, _dir) = start(store, Arc::new(Alternating));
    wait(&app, "the failed refresh", |s| {
        s.journal
            .iter()
            .any(|e| e.category == Category::Subscriptions)
    })
    .await;
    let failed = in_category(&app, Category::Subscriptions);
    assert_eq!(failed[0].level, "error");
    assert_eq!(
        failed[0].failure_code,
        Some(FailureCode::SubscriptionTransport)
    );
    assert!(in_category(&app, Category::Checks).is_empty());
}

#[tokio::test]
async fn a_connection_without_servers_is_reported_under_checks() {
    let (app, _alive, _dir) = start(Store::default(), Arc::new(Alternating));
    assert!(app.execute(Action::Connect).await.is_err());
    let entry = &in_category(&app, Category::Checks)[0];
    assert_eq!(entry.failure_code, Some(FailureCode::NoAvailableServer));
}

#[tokio::test]
async fn the_category_survives_the_journal_file() {
    let store = Store {
        servers: vec![server(2)],
        ..Store::default()
    };
    let (app, _alive, dir) = start(store, Arc::new(Alternating));
    app.execute(Action::Connect).await.unwrap();
    wait(&app, "the first cycle", |s| {
        !s.checking && s.next_check.is_some()
    })
    .await;
    app.execute(Action::Disconnect).await.unwrap();
    let saved: Vec<Entry> =
        serde_json::from_slice(&std::fs::read(dir.path().join("journal.json")).unwrap()).unwrap();
    assert!(saved.iter().any(|entry| entry.category == Category::Core));
    assert!(saved.iter().any(|entry| entry.category == Category::Checks));
    // The diagnostics report carries the category of every entry too.
    app.execute(Action::ExportDiagnostics).await.unwrap();
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("diagnostics.json")).unwrap())
            .unwrap();
    assert!(report["journal"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["category"].is_string()));
}
