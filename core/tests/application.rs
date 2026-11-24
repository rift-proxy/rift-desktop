use async_trait::async_trait;
use rift_core::{
    application::{Action, Application},
    failure::FailureCode,
    model::{Server, Store},
    parser::parse_uri,
    runtime::Runtime,
    selection::Probe,
    Result,
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::Notify;
struct TestRuntime {
    alive: Arc<AtomicBool>,
}
#[async_trait]
impl Runtime for TestRuntime {
    async fn validate(&mut self, store: &Store, _: &str) -> Result<()> {
        if store.settings.proxy_port == 10 {
            return Err(rift_core::Error::Invalid("Rejected config".into()));
        }
        Ok(())
    }
    async fn start(&mut self, store: &Store, _: &str) -> Result<()> {
        if store.settings.proxy_port == 9 {
            self.alive.store(false, Ordering::SeqCst);
            return Err(rift_core::Error::Invalid("Occupied port".into()));
        }
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
struct TestProbe {
    entered: Notify,
    release: Notify,
    count: AtomicUsize,
    active: AtomicUsize,
    max: AtomicUsize,
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl Probe for TestProbe {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        Ok(20)
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        let n = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        let _active = Active(&self.active);
        self.max.fetch_max(n, Ordering::SeqCst);
        self.count.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        self.release.notified().await;
        Ok(40)
    }
    async fn select(&self, _: &str) -> Result<()> {
        Ok(())
    }
}
/// Bounded wait: a cycle that never starts must fail the test, not hang it
/// (cargo-mutants reports a hang as TIMEOUT instead of caught).
async fn entered(probe: &TestProbe) {
    tokio::time::timeout(std::time::Duration::from_secs(5), probe.entered.notified())
        .await
        .expect("selection cycle did not reach the proxy probe");
}
fn setup() -> (
    Arc<Application>,
    Arc<TestProbe>,
    tempfile::TempDir,
    Arc<AtomicBool>,
) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::default();
    store
        .servers
        .push(parse_uri("trojan://credential@example.com:443", None).unwrap());
    let p = Arc::new(TestProbe {
        entered: Notify::new(),
        release: Notify::new(),
        count: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
        max: AtomicUsize::new(0),
    });
    let alive = Arc::new(AtomicBool::new(false));
    let app = Application::with_probe(
        store,
        dir.path().into(),
        Box::new(TestRuntime {
            alive: alive.clone(),
        }),
        p.clone(),
        "12345678901234567890123456789012".into(),
    );
    app.start();
    (app, p, dir, alive)
}
#[tokio::test]
async fn concurrent_requests_coalesce_and_disconnect_cancels() {
    let (app, p, _dir, alive) = setup();
    app.execute(Action::Connect).await.unwrap();
    entered(&p).await;
    let mut tasks = vec![];
    for _ in 0..30 {
        let a = app.clone();
        tasks.push(tokio::spawn(async move {
            a.execute(Action::CheckNow).await.unwrap();
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(p.count.load(Ordering::SeqCst), 1);
    assert_eq!(p.max.load(Ordering::SeqCst), 1);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        app.execute(Action::Disconnect),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(p.active.load(Ordering::SeqCst), 0);
    assert!(!alive.load(Ordering::SeqCst));
    assert_eq!(app.snapshot().status, "disconnected");
    app.shutdown().await.unwrap();
}
#[tokio::test]
async fn manual_selection_cancels_check_and_persists() {
    let (app, p, dir, _) = setup();
    app.execute(Action::Connect).await.unwrap();
    entered(&p).await;
    let id = app.snapshot().servers[0].id.clone();
    // Bounded: Select must cancel the running check to get the coordinator.
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.execute(Action::Select { id: id.clone() }),
    )
    .await
    .expect("Select blocked by the running check")
    .unwrap();
    // The choice is a server in use like another: Auto Select stays on, kept and saved.
    assert!(app.snapshot().settings.auto_select);
    let saved = rift_core::persistence::load(&dir.path().join("state.json")).unwrap();
    assert_eq!(saved.selected, Some(id));
    assert!(saved.settings.auto_select);
    app.shutdown().await.unwrap();
}
#[tokio::test]
async fn diagnostics_excludes_credentials_and_user_names() {
    let (app, _, dir, _) = setup();
    app.execute(Action::ExportDiagnostics).await.unwrap();
    let data = std::fs::read_to_string(dir.path().join("diagnostics.json")).unwrap();
    assert!(!data.contains("credential"));
    assert!(!data.contains("example.com"));
    assert!(!data.contains("12345678901234567890123456789012"));
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_config_and_failed_restart_preserve_working_data() {
    let (app, p, dir, alive) = setup();
    app.execute(Action::Connect).await.unwrap();
    entered(&p).await;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        app.execute(Action::SetAuto { enabled: false }),
    )
    .await
    .expect("SetAuto blocked by the running check")
    .unwrap();
    let original = std::fs::read(dir.path().join("state.json")).unwrap();
    for port in [10, 9] {
        let mut settings = app.snapshot().settings;
        settings.proxy_port = port;
        let error = app
            .execute(Action::SaveSettings { settings })
            .await
            .unwrap_err();
        assert_eq!(
            error.failure_code(),
            Some(if port == 10 {
                FailureCode::SidecarConfigRejected
            } else {
                FailureCode::SidecarStartExit
            })
        );
        assert_eq!(app.snapshot().settings.proxy_port, 2080);
        assert!(alive.load(Ordering::SeqCst));
        assert!(app.snapshot().runtime_active);
        assert_eq!(
            std::fs::read(dir.path().join("state.json")).unwrap(),
            original
        );
    }
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn journal_survives_restart_and_clear_persists() {
    let (app, _, dir, alive) = setup();
    app.execute(Action::AddServer {
        uri: "trojan://new@127.0.0.1:443".into(),
    })
    .await
    .unwrap();
    app.shutdown().await.unwrap();
    let store = rift_core::persistence::load(&dir.path().join("state.json")).unwrap();
    let restored =
        Application::new(store, dir.path().into(), Box::new(TestRuntime { alive })).unwrap();
    assert!(!restored.snapshot().journal.is_empty());
    restored.execute(Action::ClearJournal).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("journal.json")).unwrap(),
        "[]"
    );
}

struct SlowSubscription(tokio::sync::Notify);
#[async_trait]
impl rift_core::subscription::SubscriptionSource for SlowSubscription {
    async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
        self.0.notify_one();
        std::future::pending().await
    }
}
#[tokio::test]
async fn disconnect_cancels_subscription_download_without_erasing_servers() {
    let (_, probe, dir, alive) = setup();
    let mut store = Store::default();
    store.subscriptions.push(rift_core::model::Subscription {
        id: "sub".into(),
        name: "Test".into(),
        url: "https://example.com/private".into(),
        updated_at: None,
        error: None,
        failure_code: None,
        ..Default::default()
    });
    store
        .servers
        .push(parse_uri("trojan://credential@example.com:443", Some("sub".into())).unwrap());
    let source = Arc::new(SlowSubscription(tokio::sync::Notify::new()));
    let app = Application::with_services(
        store,
        dir.path().into(),
        Box::new(TestRuntime { alive }),
        probe,
        "test-secret".into(),
        source.clone(),
    );
    app.start();
    tokio::time::timeout(std::time::Duration::from_secs(5), source.0.notified())
        .await
        .expect("startup refresh did not begin");
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        app.execute(Action::Disconnect),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(app.snapshot().servers.len(), 1);
    assert_eq!(app.snapshot().status, "disconnected");
    app.shutdown().await.unwrap();
}

struct ChangingSubscription(AtomicUsize);
#[async_trait]
impl rift_core::subscription::SubscriptionSource for ChangingSubscription {
    async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        Ok(if n == 0 {
            "trojan://credential@example.com:443"
        } else {
            "trojan://credential@example.com:444"
        }
        .into())
    }
}
#[tokio::test(start_paused = true)]
async fn refresh_check_now_and_tick_share_one_cycle_and_switch_cooldown() {
    let (old, probe, dir, alive) = setup();
    old.shutdown().await.unwrap();
    let mut store = Store::default();
    store.subscriptions.push(rift_core::model::Subscription {
        id: "sub".into(),
        name: "Test".into(),
        url: "https://example.com/private".into(),
        updated_at: None,
        error: None,
        failure_code: None,
        ..Default::default()
    });
    store
        .servers
        .push(parse_uri("trojan://credential@example.com:443", Some("sub".into())).unwrap());
    let app = Application::with_services(
        store,
        dir.path().into(),
        Box::new(TestRuntime { alive }),
        probe.clone(),
        "12345678901234567890123456789012".into(),
        Arc::new(ChangingSubscription(AtomicUsize::new(0))),
    );
    app.start();
    app.execute(Action::Connect).await.unwrap();
    entered(&probe).await;
    let refreshing = {
        let app = app.clone();
        tokio::spawn(async move {
            app.execute(Action::Refresh {
                id: Some("sub".into()),
            })
            .await
        })
    };
    app.execute(Action::CheckNow).await.unwrap();
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    assert_eq!(probe.count.load(Ordering::SeqCst), 1);
    probe.release.notify_one();
    // Bounded: a merge that keeps stale rows adds probes that never get released.
    tokio::time::timeout(std::time::Duration::from_secs(5), refreshing)
        .await
        .expect("refresh did not finish")
        .unwrap()
        .unwrap();
    // The refresh brought another address: it is looked at at once, without the minute of the
    // cooldown, and the Check now that waited is the same cycle, not one more after it.
    entered(&probe).await;
    assert_eq!(probe.count.load(Ordering::SeqCst), 2);
    assert_eq!(probe.max.load(Ordering::SeqCst), 1);
    probe.release.notify_one();
    for _ in 0..30 {
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
    }
    // The light check of the server in use asks it too now and then: count the cycles, not the
    // questions.
    let cycles = app
        .snapshot()
        .journal
        .iter()
        .filter(|e| e.message.starts_with("Проверка серверов запущена"))
        .count();
    assert_eq!(cycles, 2, "nothing is queued behind the second");
    // The last cycle is still inside the probe: shutdown has to cancel it.
    tokio::time::timeout(std::time::Duration::from_secs(5), app.shutdown())
        .await
        .expect("shutdown blocked by the running check")
        .unwrap();
}
