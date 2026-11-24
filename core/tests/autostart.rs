//! Starting with Windows and connecting at launch: the registration port, its reconciliation,
//! the plan of the launch connection.
use async_trait::async_trait;
use rift_core::{
    application::{Action, Application},
    autostart::{auto_connect_plan, AutoConnect, Autostart},
    failure::FailureCode,
    model::{Server, Store, Subscription},
    parser::parse_uri,
    runtime::Runtime,
    selection::Probe,
    Error, Result,
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};

const SECRET: &str = "12345678901234567890123456789012";

fn server(host: &str) -> Server {
    parse_uri(&format!("trojan://xy@{host}:443"), None).unwrap()
}
fn store(auto_connect: bool, auto_select: bool, selected: bool) -> Store {
    let mut store = Store::default();
    store.settings.auto_connect = auto_connect;
    store.settings.auto_select = auto_select;
    store.servers.push(server("203.0.113.1"));
    store.servers.push(server("203.0.113.2"));
    if selected {
        store.selected = Some(store.servers[1].id.clone());
    }
    store
}

#[test]
fn the_launch_connection_needs_the_setting_and_something_to_connect_to() {
    assert_eq!(
        auto_connect_plan(&store(false, true, true)),
        AutoConnect::Off
    );
    assert_eq!(
        auto_connect_plan(&store(true, true, true)),
        AutoConnect::Connect
    );
    // Auto Select picks the server itself, with or without a saved choice.
    assert_eq!(
        auto_connect_plan(&store(true, true, false)),
        AutoConnect::Connect
    );
    assert_eq!(
        auto_connect_plan(&store(true, false, true)),
        AutoConnect::Connect
    );
    assert!(matches!(
        auto_connect_plan(&store(true, false, false)),
        AutoConnect::Skip(reason) if reason.contains("сервер не выбран")
    ));
    let mut empty = store(true, true, false);
    empty.servers.clear();
    assert!(matches!(
        auto_connect_plan(&empty),
        AutoConnect::Skip(reason) if reason.contains("нет серверов")
    ));
}

#[test]
fn a_saved_server_of_a_present_subscription_is_a_choice() {
    let mut store = store(true, false, false);
    store.subscriptions.push(Subscription {
        id: "off".into(),
        name: "Off".into(),
        url: "https://a.example/s".into(),
        updated_at: None,
        error: None,
        failure_code: None,
        ..Default::default()
    });
    let mut from_subscription = server("203.0.113.9");
    from_subscription.source = Some("off".into());
    store.selected = Some(from_subscription.id.clone());
    store.servers.push(from_subscription);
    assert_eq!(auto_connect_plan(&store), AutoConnect::Connect);
}

/// Remembers every call; `present` is what the registry would hold.
#[derive(Default)]
struct Registry {
    present: AtomicBool,
    fail: AtomicBool,
    calls: Mutex<Vec<String>>,
}
impl Autostart for Registry {
    fn is_enabled(&self) -> Result<bool> {
        self.calls.lock().unwrap().push("is_enabled".into());
        Ok(self.present.load(Ordering::SeqCst))
    }
    fn set(&self, enabled: bool) -> Result<()> {
        self.calls.lock().unwrap().push(format!("set({enabled})"));
        if self.fail.load(Ordering::SeqCst) {
            return Err(Error::failure(
                FailureCode::Autostart,
                "Не удалось изменить автозапуск Windows",
            ));
        }
        self.present.store(enabled, Ordering::SeqCst);
        Ok(())
    }
}
impl Registry {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[derive(Default)]
struct Counts {
    validated: AtomicUsize,
    started: AtomicUsize,
}
struct FakeRuntime {
    counts: Arc<Counts>,
    reject_config: bool,
    fail_start: bool,
}
#[async_trait]
impl Runtime for FakeRuntime {
    async fn validate(&mut self, _: &Store, _: &str) -> Result<()> {
        self.counts.validated.fetch_add(1, Ordering::SeqCst);
        if self.reject_config {
            return Err(Error::Invalid("Rejected config".into()));
        }
        Ok(())
    }
    async fn start(&mut self, _: &Store, _: &str) -> Result<()> {
        self.counts.started.fetch_add(1, Ordering::SeqCst);
        if self.fail_start {
            return Err(Error::Invalid("start refused".into()));
        }
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(true)
    }
}
struct Fine;
#[async_trait]
impl Probe for Fine {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        Ok(1)
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        Ok(1)
    }
    async fn select(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

struct Harness {
    app: Arc<Application>,
    registry: Arc<Registry>,
    counts: Arc<Counts>,
    _dir: tempfile::TempDir,
}
fn harness(store: Store, registry_has_entry: bool, runtime: fn(&mut FakeRuntime)) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let counts = Arc::new(Counts::default());
    let mut fake = FakeRuntime {
        counts: counts.clone(),
        reject_config: false,
        fail_start: false,
    };
    runtime(&mut fake);
    let app = Application::with_probe(
        store,
        dir.path().into(),
        Box::new(fake),
        Arc::new(Fine),
        SECRET.into(),
    );
    let registry = Arc::new(Registry::default());
    registry.present.store(registry_has_entry, Ordering::SeqCst);
    app.set_autostart(registry.clone());
    Harness {
        app,
        registry,
        counts,
        _dir: dir,
    }
}

#[tokio::test]
async fn saving_the_autostart_setting_registers_without_touching_the_connection() {
    let h = harness(store(false, true, false), false, |_| {});
    h.app.execute(Action::Connect).await.unwrap();
    assert_eq!(h.counts.started.load(Ordering::SeqCst), 1);

    let mut settings = h.app.snapshot().settings;
    assert!(
        !settings.autostart && !settings.auto_connect,
        "off by default"
    );
    settings.autostart = true;
    settings.auto_connect = true;
    h.app
        .execute(Action::SaveSettings { settings })
        .await
        .unwrap();
    assert_eq!(h.registry.calls(), ["set(true)"]);
    assert!(h.registry.present.load(Ordering::SeqCst));
    let saved = h.app.snapshot().settings;
    assert!(saved.autostart && saved.auto_connect);
    assert_eq!(h.counts.started.load(Ordering::SeqCst), 1, "no restart");
    assert_eq!(
        h.counts.validated.load(Ordering::SeqCst),
        0,
        "no config check"
    );

    // Auto-connect alone does not touch the registry.
    let mut settings = h.app.snapshot().settings;
    settings.auto_connect = false;
    h.app
        .execute(Action::SaveSettings { settings })
        .await
        .unwrap();
    assert_eq!(h.registry.calls(), ["set(true)"]);

    let mut settings = h.app.snapshot().settings;
    settings.autostart = false;
    h.app
        .execute(Action::SaveSettings { settings })
        .await
        .unwrap();
    assert_eq!(h.registry.calls(), ["set(true)", "set(false)"]);
    assert!(!h.registry.present.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_failed_registration_keeps_the_settings_and_reports_the_failure() {
    let h = harness(store(false, true, false), false, |_| {});
    h.registry.fail.store(true, Ordering::SeqCst);
    let mut settings = h.app.snapshot().settings;
    settings.autostart = true;
    settings.theme = "dark".into();
    let error = h
        .app
        .execute(Action::SaveSettings { settings })
        .await
        .unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::Autostart));
    let snapshot = h.app.snapshot();
    assert!(
        !snapshot.settings.autostart,
        "the switch returns to what is real"
    );
    assert_eq!(
        snapshot.settings.theme, "system",
        "nothing of the save was applied"
    );
    assert!(snapshot
        .journal
        .iter()
        .any(|entry| entry.failure_code == Some(FailureCode::Autostart)));
}

#[tokio::test]
async fn a_rejected_save_puts_the_registration_back() {
    let h = harness(store(false, true, false), false, |runtime| {
        runtime.reject_config = true;
    });
    let mut settings = h.app.snapshot().settings;
    settings.autostart = true;
    settings.proxy_port = 2090; // a new config, which the runtime rejects
    assert!(h
        .app
        .execute(Action::SaveSettings { settings })
        .await
        .is_err());
    assert_eq!(h.registry.calls(), ["set(true)", "set(false)"]);
    assert!(!h.registry.present.load(Ordering::SeqCst));
    assert!(!h.app.snapshot().settings.autostart);
}

#[tokio::test]
async fn the_registration_is_reconciled_when_rift_starts() {
    // Enabled, but the entry is missing or points elsewhere: rewritten.
    let mut enabled = store(false, true, false);
    enabled.settings.autostart = true;
    let h = harness(enabled.clone(), false, |_| {});
    h.app.start();
    assert_eq!(h.registry.calls(), ["is_enabled", "set(true)"]);

    // Enabled and current: untouched.
    let h = harness(enabled, true, |_| {});
    h.app.start();
    assert_eq!(h.registry.calls(), ["is_enabled"]);

    // Disabled: a leftover entry is removed.
    let h = harness(store(false, true, false), true, |_| {});
    h.app.start();
    assert_eq!(h.registry.calls(), ["set(false)"]);
    assert!(!h.registry.present.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_registry_failure_at_start_is_reported_and_keeps_the_setting() {
    let mut enabled = store(false, true, false);
    enabled.settings.autostart = true;
    let h = harness(enabled, false, |_| {});
    h.registry.fail.store(true, Ordering::SeqCst);
    h.app.start();
    let snapshot = h.app.snapshot();
    assert!(snapshot.settings.autostart, "the next launch tries again");
    assert!(snapshot
        .journal
        .iter()
        .any(|entry| entry.level == "error" && entry.failure_code == Some(FailureCode::Autostart)));
}

#[tokio::test]
async fn an_imported_profile_does_not_change_this_computers_autostart() {
    let h = harness(store(false, true, false), false, |_| {});
    let mut imported = store(true, true, false);
    imported.settings.autostart = true;
    h.app.import_profile(imported).await.unwrap();
    let settings = h.app.snapshot().settings;
    assert!(!settings.autostart, "the registration is local");
    assert!(
        settings.auto_connect,
        "the launch connection travels with the profile"
    );
    assert!(h.registry.calls().is_empty());
}

#[tokio::test]
async fn the_launch_connection_connects_once_with_the_saved_mode() {
    let h = harness(store(true, true, true), false, |_| {});
    h.app.auto_connect().await;
    assert_eq!(h.counts.started.load(Ordering::SeqCst), 1);
    assert!(h.app.snapshot().runtime_active);
    assert!(h
        .app
        .snapshot()
        .journal
        .iter()
        .any(|entry| entry.message == "Автоподключение при запуске"));
}

#[tokio::test]
async fn the_launch_connection_stays_idle_when_off_or_impossible() {
    let off = harness(store(false, true, true), false, |_| {});
    off.app.auto_connect().await;
    assert_eq!(off.counts.started.load(Ordering::SeqCst), 0);
    assert!(off.app.snapshot().journal.is_empty());

    let nothing = harness(store(true, false, false), false, |_| {});
    nothing.app.auto_connect().await;
    assert_eq!(nothing.counts.started.load(Ordering::SeqCst), 0);
    assert!(!nothing.app.snapshot().runtime_active);
    let journal = nothing.app.snapshot().journal;
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0].level, "info");
    assert!(journal[0].message.contains("сервер не выбран"));
}

#[tokio::test]
async fn a_failed_launch_connection_is_an_ordinary_connect_error() {
    let h = harness(store(true, true, true), false, |runtime| {
        runtime.fail_start = true;
    });
    h.app.auto_connect().await;
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.status, "error");
    assert!(!snapshot.runtime_active);
    assert!(snapshot
        .journal
        .iter()
        .any(|entry| entry.failure_code == Some(FailureCode::SidecarStartExit)));
}
