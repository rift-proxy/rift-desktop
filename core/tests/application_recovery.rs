//! Failure/recovery transitions of the coordinator: rollback after partial side effects,
//! Auto Select transitions through Settings, and crash detection ordering.
use async_trait::async_trait;
use rift_core::{
    application::{Action, Application, Snapshot},
    failure::FailureCode,
    model::{Server, Store, Subscription},
    parser::parse_uri,
    persistence, profile,
    runtime::{Runtime, Sidecar},
    selection::Probe,
    subscription::SubscriptionSource,
    Error, Result,
};
#[cfg(unix)]
use std::path::Path;
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

struct CountingRuntime {
    alive: Arc<AtomicBool>,
    starts: Arc<AtomicUsize>,
    checks: Arc<AtomicUsize>,
    /// Fault injection: the next `start` / `stop` call fails once.
    fail_start_once: Arc<AtomicBool>,
    fail_stop_once: Arc<AtomicBool>,
}
#[async_trait]
impl Runtime for CountingRuntime {
    async fn validate(&mut self, store: &Store, _: &str) -> Result<()> {
        self.checks.fetch_add(1, Ordering::SeqCst);
        if store.settings.proxy_port == 10 {
            return Err(Error::Invalid("Rejected imported config".into()));
        }
        Ok(())
    }
    async fn start(&mut self, _: &Store, _: &str) -> Result<()> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        if self.fail_start_once.swap(false, Ordering::SeqCst) {
            return Err(Error::Invalid("Injected start failure".into()));
        }
        self.alive.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        // Like the real sidecar, a failed stop has already let go of the child.
        self.alive.store(false, Ordering::SeqCst);
        if self.fail_stop_once.swap(false, Ordering::SeqCst) {
            return Err(Error::Invalid("Injected stop failure".into()));
        }
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(self.alive.load(Ordering::SeqCst))
    }
}

fn imported_store(port: u16) -> Store {
    let mut store = Store::default();
    store.settings.auto_select = false;
    store.settings.proxy_port = port;
    store
        .servers
        .push(parse_uri("trojan://imported-secret@imported.example:443", None).unwrap());
    store.selected = Some(store.servers[0].id.clone());
    store
}

#[tokio::test]
async fn import_replaces_profile_and_restarts_connected_runtime() {
    let h = harness(&[(443, 20, 40)], false);
    h.app.execute(Action::Connect).await.unwrap();
    let starts_before = h.starts.load(Ordering::SeqCst);
    let imported = imported_store(3080);

    h.app.validate_profile(&imported).await.unwrap();
    let snapshot = h.app.import_profile(imported.clone()).await.unwrap();

    assert_eq!(snapshot.settings.proxy_port, 3080);
    assert_eq!(snapshot.servers.len(), 1);
    assert_eq!(snapshot.selected, imported.selected);
    assert!(snapshot.runtime_active);
    assert!(h.alive.load(Ordering::SeqCst));
    assert!(h.starts.load(Ordering::SeqCst) > starts_before);
    // A store with content has had its welcome: the one thing the import adds to the file.
    let mut expected = imported;
    expected.welcome_dismissed = true;
    assert_eq!(
        serde_json::to_value(persistence::load(&h.dir.path().join("state.json")).unwrap()).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test]
async fn saving_the_process_lookup_restarts_the_connected_runtime_and_is_kept() {
    let h = harness(&[(443, 20, 40)], false);
    h.app.execute(Action::Connect).await.unwrap();
    let starts_before = h.starts.load(Ordering::SeqCst);
    let mut settings = h.app.snapshot().settings;
    assert!(!settings.find_process);
    settings.find_process = true;

    let snapshot = h
        .app
        .execute(Action::SaveSettings { settings })
        .await
        .unwrap();

    assert!(snapshot.settings.find_process);
    assert!(snapshot.runtime_active);
    assert!(
        h.starts.load(Ordering::SeqCst) > starts_before,
        "the lookup is read by sing-box at start: it needs the restart"
    );
    assert!(
        persistence::load(&h.dir.path().join("state.json"))
            .unwrap()
            .settings
            .find_process
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejected_import_keeps_working_profile() {
    let h = harness(&[(443, 20, 40)], false);
    h.app.execute(Action::Connect).await.unwrap();
    let before = h.state();
    let imported = imported_store(10);

    assert_eq!(
        h.app
            .validate_profile(&imported)
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::SidecarConfigRejected)
    );
    assert_eq!(
        h.app
            .import_profile(imported)
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::SidecarConfigRejected)
    );
    assert_eq!(h.state(), before);
    assert_eq!(h.app.snapshot().settings.proxy_port, 2080);
    assert!(h.app.snapshot().runtime_active);
    assert!(h.alive.load(Ordering::SeqCst));
    h.app.shutdown().await.unwrap();
}

/// A profile file that is fully self-consistent (id, host, port, protocol) yet carries an
/// outbound sing-box would run as a program: it must be refused by Rift's own allowlist,
/// before `sing-box check` or the running sidecar are involved at all.
fn hostile_store() -> Store {
    let mut store = imported_store(3080);
    let outbound = serde_json::json!({
        "type": "tor",
        "server": "imported.example",
        "server_port": 443,
        "executable_path": "C:\\Windows\\System32\\calc.exe",
        "extra_args": ["--flag"],
    });
    let server = &mut store.servers[0];
    server.id = Server::fingerprint(&outbound);
    server.protocol = "tor".into();
    server.outbound = outbound;
    store.selected = Some(store.servers[0].id.clone());
    store
}

#[tokio::test]
async fn hostile_outbound_is_rejected_on_inspect_and_import_without_touching_runtime() {
    let h = harness(&[(443, 20, 40)], false);
    h.app.execute(Action::Connect).await.unwrap();
    let before = h.state();
    let starts = h.starts.load(Ordering::SeqCst);
    let checks = h.checks.load(Ordering::SeqCst);

    for result in [
        h.app.validate_profile(&hostile_store()).await,
        h.app.import_profile(hostile_store()).await.map(|_| ()),
    ] {
        assert!(matches!(result, Err(Error::Invalid(_))));
    }

    assert_eq!(
        h.checks.load(Ordering::SeqCst),
        checks,
        "sing-box check ran"
    );
    assert_eq!(h.starts.load(Ordering::SeqCst), starts, "runtime restarted");
    assert_eq!(h.state(), before);
    assert_eq!(h.app.snapshot().settings.proxy_port, 2080);
    assert!(h.app.snapshot().runtime_active);
    h.app.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn import_save_failure_restores_runtime_and_profile() {
    let h = harness(&[(443, 20, 40)], false);
    h.app.execute(Action::Connect).await.unwrap();
    let before = h.state();
    if !block_writes(h.dir.path()) {
        return;
    }

    let result = h.app.import_profile(imported_store(3080)).await;
    allow_writes(h.dir.path());

    assert_eq!(
        result.unwrap_err().failure_code(),
        Some(FailureCode::Persistence)
    );
    assert_eq!(h.state(), before);
    assert_eq!(h.app.snapshot().settings.proxy_port, 2080);
    assert!(h.app.snapshot().runtime_active);
    assert!(h.alive.load(Ordering::SeqCst));
    h.app.shutdown().await.unwrap();
}

/// The profile a harness persisted, as the application itself would reload it.
fn stored(h: &Harness) -> Store {
    persistence::load(&h.dir.path().join("state.json")).unwrap()
}
fn last_error(h: &Harness) -> rift_core::application::Entry {
    let snapshot = h.app.snapshot();
    let entry = snapshot.journal.last().expect("journal entry").clone();
    assert_eq!(entry.level, "error", "{entry:?}");
    assert!(snapshot.error.is_some());
    entry
}

#[tokio::test]
async fn import_start_failure_restores_previous_runtime_and_reports_the_code() {
    let h = harness(&[(443, 20, 40)], false);
    h.app.execute(Action::Connect).await.unwrap();
    let before = h.state();
    let starts = h.starts.load(Ordering::SeqCst);
    h.fail_start_once.store(true, Ordering::SeqCst);

    let error = h
        .app
        .import_profile(imported_store(3080))
        .await
        .unwrap_err();

    assert_eq!(error.failure_code(), Some(FailureCode::SidecarStartExit));
    assert_eq!(h.state(), before);
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.settings.proxy_port, 2080);
    assert!(snapshot.runtime_active);
    assert_eq!(snapshot.status, "connected");
    assert!(h.alive.load(Ordering::SeqCst));
    assert_eq!(
        h.starts.load(Ordering::SeqCst),
        starts + 2,
        "the failed start and the rollback start"
    );
    assert_eq!(
        last_error(&h).failure_code,
        Some(FailureCode::SidecarStartExit)
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test]
async fn import_while_disconnected_replaces_the_profile_without_starting_sing_box() {
    let h = harness(&[(443, 20, 40)], false);
    let imported = imported_store(3080);

    h.app.validate_profile(&imported).await.unwrap();
    let snapshot = h.app.import_profile(imported.clone()).await.unwrap();

    assert_eq!(snapshot.settings.proxy_port, 3080);
    assert_eq!(snapshot.servers.len(), 1);
    assert!(!snapshot.runtime_active);
    assert_eq!(snapshot.status, "disconnected");
    assert!(!snapshot.pending);
    assert_eq!(h.starts.load(Ordering::SeqCst), 0);
    // A store with content has had its welcome: the one thing the import adds to the file.
    let mut expected = imported;
    expected.welcome_dismissed = true;
    assert_eq!(
        serde_json::to_value(stored(&h)).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test]
async fn import_without_available_servers_stops_the_connected_runtime() {
    let h = harness(&[(443, 20, 40)], false);
    h.app.execute(Action::Connect).await.unwrap();
    let mut empty = Store::default();
    empty.settings.proxy_port = 3080;

    let snapshot = h.app.import_profile(empty).await.unwrap();

    assert!(snapshot.servers.is_empty());
    assert!(!snapshot.runtime_active);
    assert_eq!(snapshot.status, "disconnected");
    assert!(!h.alive.load(Ordering::SeqCst));
    assert!(stored(&h).servers.is_empty());
    h.app.shutdown().await.unwrap();
}

#[tokio::test]
async fn import_stop_failure_restores_previous_runtime_and_profile() {
    let h = harness(&[(443, 20, 40)], false);
    h.app.execute(Action::Connect).await.unwrap();
    let before = h.state();
    let starts = h.starts.load(Ordering::SeqCst);
    h.fail_stop_once.store(true, Ordering::SeqCst);
    let mut empty = Store::default();
    empty.settings.proxy_port = 3080;

    let error = h.app.import_profile(empty).await.unwrap_err();

    assert_eq!(error.failure_code(), Some(FailureCode::SidecarStop));
    assert_eq!(h.state(), before);
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.servers.len(), 1);
    assert_eq!(snapshot.settings.proxy_port, 2080);
    assert!(snapshot.runtime_active, "the previous runtime is back");
    assert_eq!(snapshot.status, "connected");
    assert!(h.alive.load(Ordering::SeqCst));
    assert_eq!(h.starts.load(Ordering::SeqCst), starts + 1);
    assert_eq!(last_error(&h).failure_code, Some(FailureCode::SidecarStop));
    h.app.shutdown().await.unwrap();
}

/// A directory where `state.json` should be makes the atomic replace fail on every platform
/// (unlike a read-only directory, which root and Windows ignore).
fn make_state_unwritable(h: &Harness) {
    let state = h.dir.path().join("state.json");
    std::fs::remove_file(&state).unwrap();
    std::fs::create_dir(&state).unwrap();
}

#[tokio::test]
async fn import_persistence_failure_restores_runtime_on_every_platform() {
    let h = harness(&[(443, 20, 40)], false);
    h.app.execute(Action::Connect).await.unwrap();
    let starts = h.starts.load(Ordering::SeqCst);
    make_state_unwritable(&h);

    let error = h
        .app
        .import_profile(imported_store(3080))
        .await
        .unwrap_err();

    assert_eq!(error.failure_code(), Some(FailureCode::Persistence));
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.settings.proxy_port, 2080);
    assert_eq!(snapshot.servers.len(), 1);
    assert!(snapshot.runtime_active);
    assert_eq!(snapshot.status, "connected");
    assert!(h.alive.load(Ordering::SeqCst));
    assert_eq!(h.starts.load(Ordering::SeqCst), starts + 2);
    assert_eq!(last_error(&h).failure_code, Some(FailureCode::Persistence));
    h.app.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejected_import_is_journaled_with_its_typed_code() {
    let h = harness(&[(443, 20, 40)], false);
    h.app.execute(Action::Connect).await.unwrap();

    let error = h.app.import_profile(imported_store(10)).await.unwrap_err();

    assert_eq!(
        error.failure_code(),
        Some(FailureCode::SidecarConfigRejected)
    );
    let entry = last_error(&h);
    assert_eq!(entry.failure_code, Some(FailureCode::SidecarConfigRejected));
    assert!(!entry.message.contains("imported-secret"));
    assert!(!entry.message.contains("imported.example"));
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn import_with_auto_select_queues_the_initial_cycle() {
    let h = loopback_harness(&[(9, 20, 40), (10, 30, 50)], true);
    let a = h.ids[0].clone();
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "initial selection of A", |s| {
        s.selected.as_ref() == Some(&a) && !s.checking && s.status == "connected"
    })
    .await;
    let before = cycles(&h);
    let mut imported = stored(&h);
    imported.settings.proxy_port = 3080;

    let snapshot = h.app.import_profile(imported).await.unwrap();

    assert!(snapshot.pending, "Initial is queued behind the new runtime");
    until(&h.app, "the queued Initial to run", |s| !s.pending).await;
    until(&h.app, "the queued Initial to end", |s| !s.checking).await;
    assert_eq!(cycles(&h), before + 1);
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn import_without_restart_keeps_the_switch_cooldown() {
    // B works first by endpoint RTT, C has the lower proxy RTT: Initial switches B then C.
    let h = loopback_harness(&[(9, 20, 80), (10, 30, 40)], true);
    let c = h.ids[1].clone();
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "Initial to settle on C", |s| {
        s.selected.as_ref() == Some(&c) && !s.checking
    })
    .await;
    let starts = h.starts.load(Ordering::SeqCst);
    let before = cycles(&h);
    // Only the theme differs, so the generated sing-box config is identical: no restart.
    let mut imported = stored(&h);
    imported.settings.theme = "dark".into();

    let snapshot = h.app.import_profile(imported).await.unwrap();

    assert_eq!(
        h.starts.load(Ordering::SeqCst),
        starts,
        "no restart expected"
    );
    assert!(snapshot.pending, "the Initial request is kept");
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(
        cycles(&h),
        before,
        "a cycle ran inside the switch + interval cooldown"
    );
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(
        cycles(&h),
        before + 1,
        "the queued Initial runs after the cooldown"
    );
    h.app.shutdown().await.unwrap();
}

/// Connected, Auto Select on, a Check now request queued behind the switch + interval
/// cooldown, then a profile that differs only in Auto Select is imported (no restart).
async fn import_into_a_queued_cycle(auto_select: bool) -> (Harness, usize, Snapshot) {
    let h = loopback_harness(&[(9, 20, 80), (10, 30, 40)], true);
    let c = h.ids[1].clone();
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "Initial to settle on C", |s| {
        s.selected.as_ref() == Some(&c) && !s.checking
    })
    .await;
    let starts = h.starts.load(Ordering::SeqCst);
    h.app.execute(Action::CheckNow).await.unwrap();
    // The worker moves the request into the schedule, where the cooldown holds it back.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let before = cycles(&h);
    assert!(h.app.snapshot().pending, "premise: Check now is queued");
    let mut imported = stored(&h);
    imported.settings.auto_select = auto_select;

    let snapshot = h.app.import_profile(imported).await.unwrap();

    assert_eq!(
        h.starts.load(Ordering::SeqCst),
        starts,
        "no restart expected"
    );
    (h, before, snapshot)
}

#[tokio::test(start_paused = true)]
async fn import_turning_auto_select_off_drops_the_queued_cycle() {
    let (h, before, snapshot) = import_into_a_queued_cycle(false).await;

    assert!(!snapshot.pending, "the snapshot shows nothing queued");
    assert!(!snapshot.settings.auto_select);
    tokio::time::sleep(Duration::from_secs(120)).await;
    assert_eq!(
        cycles(&h),
        before,
        "a cycle the snapshot did not show ran after the cooldown"
    );
    assert!(!h.app.snapshot().pending);
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn import_keeping_auto_select_on_runs_one_initial_after_the_cooldown() {
    let (h, before, snapshot) = import_into_a_queued_cycle(true).await;

    assert!(snapshot.pending, "the Initial request is queued");
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(cycles(&h), before, "the cooldown still holds");
    tokio::time::sleep(Duration::from_secs(60)).await;
    assert_eq!(
        cycles(&h),
        before + 1,
        "the queued Check now and the Initial are one cycle"
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn import_during_a_selection_cycle_leaves_no_stuck_flags() {
    let h = loopback_harness(&[(9, 20, 80), (10, 30, 40)], true);
    let c = h.ids[1].clone();
    *h.probe.gate.lock().unwrap() = Some(c.clone());
    h.app.execute(Action::Connect).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), h.probe.gate_entered.notified())
        .await
        .expect("Initial did not reach C");
    assert!(h.app.snapshot().checking);
    let mut imported = stored(&h);
    imported.settings.proxy_port = 3080;

    tokio::time::timeout(Duration::from_secs(5), h.app.import_profile(imported))
        .await
        .expect("import blocked by the running cycle")
        .unwrap();

    until(&h.app, "cycle flags to settle", |s| {
        !s.checking && !s.pending && !s.refreshing
    })
    .await;
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.settings.proxy_port, 3080);
    assert!(snapshot.runtime_active);
    assert!(
        cycles(&h) >= 2,
        "the interrupted Initial restarts after the import"
    );
    h.app.shutdown().await.unwrap();
}

struct BlockedSource {
    entered: Notify,
}
#[async_trait]
impl SubscriptionSource for BlockedSource {
    async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
        self.entered.notify_one();
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn import_during_a_refresh_leaves_no_stuck_refreshing_flag() {
    let mut store = Store::default();
    store.settings.auto_select = false;
    store.subscriptions.push(Subscription {
        id: "sub".into(),
        name: "Test".into(),
        url: "https://example.com/private".into(),
        updated_at: None,
        error: None,
        failure_code: None,
        ..Default::default()
    });
    let server = parse_uri("trojan://credential@example.com:443", Some("sub".into())).unwrap();
    store.servers.push(server);
    let source = Arc::new(BlockedSource {
        entered: Notify::new(),
    });
    let h = build(store, HashMap::new(), Some(source.clone()));
    let refresh = {
        let app = h.app.clone();
        tokio::spawn(async move {
            app.execute(Action::Refresh {
                id: Some("sub".into()),
            })
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), source.entered.notified())
        .await
        .expect("refresh did not start");
    assert!(h.app.snapshot().refreshing);

    tokio::time::timeout(
        Duration::from_secs(5),
        h.app.import_profile(imported_store(3080)),
    )
    .await
    .expect("import blocked by the running refresh")
    .unwrap();

    until(&h.app, "refresh flag to clear", |s| !s.refreshing).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), refresh)
        .await
        .expect("refresh did not end")
        .unwrap();
    assert_eq!(h.app.snapshot().settings.proxy_port, 3080);
    assert_eq!(h.app.snapshot().servers.len(), 1);
    h.app.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_profile_write_keeps_the_existing_destination() {
    let h = harness(&[(443, 20, 40)], false);
    let exported = h.app.export_profile().await.unwrap();
    // A directory in place of the file: the atomic replace fails everywhere, and nothing it
    // holds may be touched or left behind as a temporary file.
    let destination = h.dir.path().join("rift-profile.json");
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("keep.txt"), b"working").unwrap();
    let listing = |path: &std::path::Path| {
        let mut names: Vec<_> = std::fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let (parent_before, inside_before) = (listing(h.dir.path()), listing(&destination));

    let error = h
        .app
        .write_profile(&destination, &exported)
        .await
        .unwrap_err();

    assert_eq!(error.failure_code(), Some(FailureCode::Persistence));
    // Nothing in the data directory changed except the journal that records the failure.
    let parent_after = listing(h.dir.path());
    let added: Vec<_> = parent_after
        .iter()
        .filter(|name| !parent_before.contains(name))
        .collect();
    assert_eq!(added, ["journal.json"], "a temporary file was left behind");
    assert_eq!(listing(&destination), inside_before);
    assert_eq!(
        std::fs::read(destination.join("keep.txt")).unwrap(),
        b"working"
    );
    assert_eq!(last_error(&h).failure_code, Some(FailureCode::Persistence));
    h.app.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn failed_profile_write_leaves_the_previous_file_bytes_intact() {
    let h = harness(&[(443, 20, 40)], false);
    let exported = h.app.export_profile().await.unwrap();
    let out = tempfile::tempdir().unwrap();
    let destination = out.path().join("rift-profile.json");
    std::fs::write(&destination, b"previous export").unwrap();
    if !block_writes(out.path()) {
        return;
    }

    let result = h.app.write_profile(&destination, &exported).await;
    allow_writes(out.path());

    assert_eq!(
        result.unwrap_err().failure_code(),
        Some(FailureCode::Persistence)
    );
    assert_eq!(std::fs::read(&destination).unwrap(), b"previous export");
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 1);
    h.app.shutdown().await.unwrap();
}

// The two tests below make a profile failure and a foreign failure carry the same text
// through the public `report_failure`, which takes any core error: no real profile command
// produces these texts today, so this pins that the banner belongs to its last writer.

#[tokio::test(start_paused = true)]
async fn a_crash_with_the_same_text_keeps_its_banner() {
    const CRASH_TEXT: &str = "sing-box неожиданно завершился";
    let h = harness(&[(443, 20, 40)], false);
    let exported = h.app.export_profile().await.unwrap();
    let out = tempfile::tempdir().unwrap();
    h.app.execute(Action::Connect).await.unwrap();
    h.app
        .report_failure(&Error::Invalid(CRASH_TEXT.into()))
        .await;
    h.alive.store(false, Ordering::SeqCst);
    crashed(&h).await;
    assert_eq!(
        h.app.snapshot().error.as_deref(),
        Some(CRASH_TEXT),
        "premise: the crash banner has the profile failure's text"
    );

    h.app
        .write_profile(&out.path().join("rift-profile.json"), &exported)
        .await
        .unwrap();

    assert_eq!(h.app.snapshot().error.as_deref(), Some(CRASH_TEXT));
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_selection_failure_with_the_same_text_keeps_its_banner() {
    let h = harness(&[(443, 20, 40)], true);
    let exported = h.app.export_profile().await.unwrap();
    let out = tempfile::tempdir().unwrap();
    *h.probe.gate.lock().unwrap() = Some(h.ids[0].clone());
    *h.probe.fail_select.lock().unwrap() = Some(h.ids[0].clone());
    h.app.execute(Action::Connect).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), h.probe.gate_entered.notified())
        .await
        .expect("Initial did not reach the gated server");
    // While the cycle runs, a profile failure shows the text the selector failure will have.
    let text = Error::Network.to_string();
    h.app.report_failure(&Error::Network).await;
    h.probe.gate_release.notify_one();
    until(&h.app, "the selection to fail", |s| {
        !s.checking && s.status == "error"
    })
    .await;
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.error.as_deref(), Some(text.as_str()), "premise");
    assert!(snapshot
        .journal
        .iter()
        .any(|entry| entry.failure_code == Some(FailureCode::SelectorSwitch)));

    h.app
        .write_profile(&out.path().join("rift-profile.json"), &exported)
        .await
        .unwrap();

    assert_eq!(h.app.snapshot().error.as_deref(), Some(text.as_str()));
    h.app.shutdown().await.unwrap();
}

#[tokio::test]
async fn inspect_with_an_unusable_data_directory_does_not_blame_the_profile_file() {
    let dir = tempfile::tempdir().unwrap();
    // The directory sing-box works from is a regular file: the check cannot even start.
    let data_dir = dir.path().join("data-dir-is-a-file");
    std::fs::write(&data_dir, b"x").unwrap();
    let app = Application::with_probe(
        Store::default(),
        dir.path().into(),
        Box::new(Sidecar::new(dir.path().join("sing-box"), data_dir)),
        Arc::new(ScriptedProbe::default()),
        SECRET.into(),
    );
    let mut store = Store::default();
    store
        .servers
        .push(parse_uri("trojan://credential@example.com:443", None).unwrap());
    let file = dir.path().join("rift-profile.json");
    std::fs::write(&file, profile::encode(&store).unwrap()).unwrap();

    let Err(error) = app.inspect_profile(&file).await else {
        panic!("the profile was accepted without a check");
    };

    assert_eq!(error.failure_code(), Some(FailureCode::Persistence));
    let text = profile::inspect_error_text(&error);
    assert_eq!(text, profile::check_unavailable());
    assert!(!text.contains("прочитать профиль"), "{text}");
    // A file that cannot be read still says so.
    let Err(missing) = app.inspect_profile(&dir.path().join("missing.json")).await else {
        panic!("a missing profile was accepted");
    };
    assert_eq!(missing.failure_code(), Some(FailureCode::Persistence));
    assert_eq!(
        profile::inspect_error_text(&missing),
        "Не удалось прочитать профиль"
    );
    // Both are typed in the journal, with static texts that carry no path.
    let snapshot = app.snapshot();
    let errors: Vec<_> = snapshot
        .journal
        .iter()
        .filter(|entry| entry.level == "error")
        .collect();
    assert_eq!(errors.len(), 2);
    assert!(errors
        .iter()
        .all(|entry| entry.failure_code == Some(FailureCode::Persistence)
            && !entry.message.contains("data-dir")
            && !entry.message.contains("missing")));
    assert_eq!(errors[0].message, profile::check_unavailable());
    app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn profile_failures_are_reported_while_a_selection_cycle_holds_the_coordinator() {
    let h = harness(&[(443, 20, 80), (444, 30, 40)], true);
    *h.probe.gate.lock().unwrap() = Some(h.ids[1].clone());
    h.app.execute(Action::Connect).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), h.probe.gate_entered.notified())
        .await
        .expect("Initial did not reach the gated server");
    assert!(
        h.app.snapshot().checking,
        "premise: the cycle holds the coordinator"
    );
    let dir = tempfile::tempdir().unwrap();
    let damaged = dir.path().join("damaged-profile.json");
    std::fs::write(&damaged, b"{").unwrap();
    let blocked = dir.path().join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    let short = Duration::from_secs(1);

    let Ok(Err(content)) = tokio::time::timeout(short, h.app.inspect_profile(&damaged)).await
    else {
        panic!("inspect_profile of a damaged file did not fail promptly");
    };
    assert_eq!(h.app.snapshot().error, Some(content.to_string()));
    let Ok(Err(write)) = tokio::time::timeout(short, h.app.write_profile(&blocked, b"{}")).await
    else {
        panic!("write_profile to a blocked path did not fail promptly");
    };
    assert_eq!(write.failure_code(), Some(FailureCode::Persistence));
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.error, Some(write.to_string()));
    assert!(snapshot.checking, "the cycle ran the whole time");
    let errors: Vec<_> = snapshot
        .journal
        .iter()
        .filter(|entry| entry.level == "error")
        .collect();
    assert_eq!(errors.len(), 2);
    assert_eq!(errors[1].failure_code, Some(FailureCode::Persistence));
    for entry in errors {
        assert!(!entry.message.contains("damaged-profile") && !entry.message.contains("blocked"));
    }

    h.probe.gate_release.notify_one();
    until(&h.app, "the cycle to finish", |s| !s.checking).await;
    h.app.shutdown().await.unwrap();
}

/// A write into a directory fails on every platform; returns the banner it leaves.
async fn banner_of_a_failed_write(h: &Harness, blocked: &std::path::Path, bytes: &[u8]) -> String {
    h.app.write_profile(blocked, bytes).await.unwrap_err();
    h.app.snapshot().error.expect("the failure is shown")
}

#[tokio::test]
async fn a_successful_profile_command_clears_the_banner_of_the_failed_one() {
    let h = harness(&[(443, 20, 40)], false);
    let exported = h.app.export_profile().await.unwrap();
    let out = tempfile::tempdir().unwrap();
    let blocked = out.path().join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    let file = out.path().join("rift-profile.json");
    let store = stored(&h);

    banner_of_a_failed_write(&h, &blocked, &exported).await;
    h.app.write_profile(&file, &exported).await.unwrap();
    assert_eq!(h.app.snapshot().error, None, "write");

    banner_of_a_failed_write(&h, &blocked, &exported).await;
    h.app.export_profile().await.unwrap();
    assert_eq!(h.app.snapshot().error, None, "export");

    banner_of_a_failed_write(&h, &blocked, &exported).await;
    h.app.inspect_profile(&file).await.unwrap();
    assert_eq!(h.app.snapshot().error, None, "inspect");

    banner_of_a_failed_write(&h, &blocked, &exported).await;
    h.app.validate_profile(&store).await.unwrap();
    assert_eq!(h.app.snapshot().error, None, "validate");

    banner_of_a_failed_write(&h, &blocked, &exported).await;
    let snapshot = h.app.import_profile(store).await.unwrap();
    assert_eq!(
        snapshot.error, None,
        "the returned snapshot is already clean"
    );
    assert_eq!(h.app.snapshot().error, None, "import");

    // The journal keeps the history: one entry per failure.
    let failures = h
        .app
        .snapshot()
        .journal
        .iter()
        .filter(|entry| entry.level == "error")
        .count();
    assert_eq!(failures, 5);
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_crash_banner_survives_a_successful_profile_command() {
    let h = harness(&[(443, 20, 40)], false);
    let exported = h.app.export_profile().await.unwrap();
    let out = tempfile::tempdir().unwrap();
    let blocked = out.path().join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    h.app.execute(Action::Connect).await.unwrap();
    banner_of_a_failed_write(&h, &blocked, &exported).await;
    h.alive.store(false, Ordering::SeqCst);
    crashed(&h).await;
    let crash = h.app.snapshot().error.expect("the crash is shown");

    h.app
        .write_profile(&out.path().join("rift-profile.json"), &exported)
        .await
        .unwrap();
    h.app.export_profile().await.unwrap();

    assert_eq!(h.app.snapshot().error.as_deref(), Some(crash.as_str()));
    h.app.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_action_failure_with_the_same_text_keeps_its_banner() {
    let h = harness(&[(443, 20, 40)], false);
    let exported = h.app.export_profile().await.unwrap();
    let out = tempfile::tempdir().unwrap();
    let blocked = out.path().join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    let profile_banner = banner_of_a_failed_write(&h, &blocked, &exported).await;
    // A directory in place of state.json: saving the settings fails with the same text.
    let state = h.dir.path().join("state.json");
    std::fs::remove_file(&state).unwrap();
    std::fs::create_dir(&state).unwrap();
    let mut settings = h.app.snapshot().settings;
    settings.theme = "dark".into();
    let error = h
        .app
        .execute(Action::SaveSettings { settings })
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), profile_banner, "premise: identical text");

    h.app
        .write_profile(&out.path().join("rift-profile.json"), &exported)
        .await
        .unwrap();

    assert_eq!(
        h.app.snapshot().error.as_deref(),
        Some(profile_banner.as_str()),
        "the settings failure owns the banner"
    );
    h.app.shutdown().await.unwrap();
}

/// Per-server (endpoint, proxy) latencies, a log of successful selector switches and
/// injectable failures.
#[derive(Default)]
struct ScriptedProbe {
    latency: HashMap<String, (u64, u64)>,
    /// Endpoint probes started: one per server per selection cycle.
    endpoint_probes: AtomicUsize,
    selects: Mutex<Vec<String>>,
    fail_select: Mutex<Option<String>>,
    fail_select_once: Mutex<Option<String>>,
    gate: Mutex<Option<String>>,
    gate_entered: Notify,
    gate_release: Notify,
}
impl ScriptedProbe {
    fn selects(&self) -> Vec<String> {
        self.selects.lock().unwrap().clone()
    }
}
#[async_trait]
impl Probe for ScriptedProbe {
    async fn endpoint(&self, server: &Server) -> Result<u64> {
        self.endpoint_probes.fetch_add(1, Ordering::SeqCst);
        Ok(self.latency[&server.id].0)
    }
    async fn proxy(&self, server: &Server) -> Result<u64> {
        let gated = {
            let mut gate = self.gate.lock().unwrap();
            if gate.as_deref() == Some(server.id.as_str()) {
                *gate = None;
                true
            } else {
                false
            }
        };
        if gated {
            self.gate_entered.notify_one();
            self.gate_release.notified().await;
        }
        Ok(self.latency[&server.id].1)
    }
    async fn select(&self, id: &str) -> Result<()> {
        if self.fail_select.lock().unwrap().as_deref() == Some(id) {
            return Err(Error::Network);
        }
        {
            let mut once = self.fail_select_once.lock().unwrap();
            if once.as_deref() == Some(id) {
                *once = None;
                return Err(Error::Network);
            }
        }
        self.selects.lock().unwrap().push(id.into());
        Ok(())
    }
}

struct Harness {
    app: Arc<Application>,
    probe: Arc<ScriptedProbe>,
    dir: tempfile::TempDir,
    alive: Arc<AtomicBool>,
    starts: Arc<AtomicUsize>,
    checks: Arc<AtomicUsize>,
    fail_start_once: Arc<AtomicBool>,
    fail_stop_once: Arc<AtomicBool>,
    ids: Vec<String>,
}
impl Harness {
    fn state(&self) -> Vec<u8> {
        std::fs::read(self.dir.path().join("state.json")).unwrap()
    }
}
/// `servers`: (port, endpoint ms, proxy ms). The initial Store is already on disk.
fn harness(servers: &[(u16, u64, u64)], auto_select: bool) -> Harness {
    let mut store = Store::default();
    store.settings.auto_select = auto_select;
    let mut latency = HashMap::new();
    for &(port, endpoint, proxy) in servers {
        let server = parse_uri(&format!("trojan://credential@example.com:{port}"), None).unwrap();
        latency.insert(server.id.clone(), (endpoint, proxy));
        store.servers.push(server);
    }
    build(store, latency, None)
}
/// Like `harness`, but the servers are loopback addresses: after an import the application
/// swaps in its real `NetworkProbe`, and its probes must never leave the machine.
fn loopback_harness(servers: &[(u16, u64, u64)], auto_select: bool) -> Harness {
    let mut store = Store::default();
    store.settings.auto_select = auto_select;
    let mut latency = HashMap::new();
    for &(port, endpoint, proxy) in servers {
        let server = parse_uri(&format!("trojan://credential@127.0.0.1:{port}"), None).unwrap();
        latency.insert(server.id.clone(), (endpoint, proxy));
        store.servers.push(server);
    }
    build(store, latency, None)
}
/// Selection cycles started so far (each logs one entry).
fn cycles(h: &Harness) -> usize {
    h.app
        .snapshot()
        .journal
        .iter()
        .filter(|entry| entry.message.starts_with("Проверка серверов запущена"))
        .count()
}
fn build(
    store: Store,
    latency: HashMap<String, (u64, u64)>,
    subscriptions: Option<Arc<dyn SubscriptionSource>>,
) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    persistence::save(&dir.path().join("state.json"), &store).unwrap();
    let ids = store.servers.iter().map(|s| s.id.clone()).collect();
    let probe = Arc::new(ScriptedProbe {
        latency,
        ..Default::default()
    });
    let alive = Arc::new(AtomicBool::new(false));
    let starts = Arc::new(AtomicUsize::new(0));
    let checks = Arc::new(AtomicUsize::new(0));
    let fail_start_once = Arc::new(AtomicBool::new(false));
    let fail_stop_once = Arc::new(AtomicBool::new(false));
    let runtime = Box::new(CountingRuntime {
        alive: alive.clone(),
        starts: starts.clone(),
        checks: checks.clone(),
        fail_start_once: fail_start_once.clone(),
        fail_stop_once: fail_stop_once.clone(),
    });
    let app = match subscriptions {
        Some(source) => Application::with_services(
            store,
            dir.path().into(),
            runtime,
            probe.clone(),
            SECRET.into(),
            source,
        ),
        None => Application::with_probe(
            store,
            dir.path().into(),
            runtime,
            probe.clone(),
            SECRET.into(),
        ),
    };
    app.start();
    Harness {
        app,
        probe,
        dir,
        alive,
        starts,
        checks,
        fail_start_once,
        fail_stop_once,
        ids,
    }
}
async fn until(app: &Application, what: &str, predicate: impl Fn(&Snapshot) -> bool) {
    let mut rx = app.snapshots.subscribe();
    tokio::time::timeout(Duration::from_secs(5), rx.wait_for(|s| predicate(s)))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}: {:?}", app.snapshot()))
        .unwrap();
}

/// Makes `dir` read-only. Returns false where permissions do not stop writes (root);
/// callers then skip, since the fault cannot be injected.
#[cfg(unix)]
fn block_writes(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(dir.join(".probe"), b"").is_ok() {
        let _ = std::fs::remove_file(dir.join(".probe"));
        allow_writes(dir);
        return false;
    }
    true
}
#[cfg(unix)]
fn allow_writes(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn manual_selection_save_failure_restores_live_selector_and_state() {
    let h = harness(&[(443, 20, 40), (444, 30, 50)], true);
    let (a, b) = (h.ids[0].clone(), h.ids[1].clone());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "initial selection of A", |s| {
        s.selected.as_ref() == Some(&a) && !s.checking && s.status == "connected"
    })
    .await;
    let before = h.state();
    if !block_writes(h.dir.path()) {
        return;
    }
    let result = h.app.execute(Action::Select { id: b.clone() }).await;
    allow_writes(h.dir.path());
    assert_eq!(
        result.unwrap_err().failure_code(),
        Some(FailureCode::Persistence)
    );
    assert_eq!(
        h.app.snapshot().journal.last().unwrap().failure_code,
        Some(FailureCode::Persistence)
    );
    assert_eq!(
        h.probe.selects().last(),
        Some(&a),
        "selector must return to A"
    );
    assert!(h.probe.selects().contains(&b));
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.selected, Some(a));
    assert!(snapshot.settings.auto_select);
    assert_eq!(snapshot.status, "connected");
    assert_eq!(h.state(), before);
    h.app.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn manual_selection_reports_failed_selector_rollback() {
    let h = harness(&[(443, 20, 40), (444, 30, 50)], false);
    let (a, b) = (h.ids[0].clone(), h.ids[1].clone());
    h.app.execute(Action::Connect).await.unwrap();
    *h.probe.fail_select.lock().unwrap() = Some(a.clone());
    if !block_writes(h.dir.path()) {
        return;
    }
    let result = h.app.execute(Action::Select { id: b }).await;
    allow_writes(h.dir.path());
    assert!(result.is_err());
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.status, "error");
    assert!(snapshot.error.is_some());
    assert_eq!(snapshot.selected, Some(a));
    h.app.shutdown().await.unwrap();
}

#[tokio::test]
async fn manual_connect_selector_failure_stops_runtime_and_allows_retry() {
    let h = harness(&[(443, 20, 40)], false);
    *h.probe.fail_select_once.lock().unwrap() = Some(h.ids[0].clone());
    assert!(h.app.execute(Action::Connect).await.is_err());
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.status, "error");
    assert!(!snapshot.runtime_active);
    assert!(!h.alive.load(Ordering::SeqCst));
    h.app.execute(Action::Connect).await.unwrap();
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.status, "connected");
    assert!(snapshot.runtime_active);
    assert_eq!(h.starts.load(Ordering::SeqCst), 2);
    assert_eq!(h.probe.selects(), vec![h.ids[0].clone()]);
    h.app.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn manual_connect_save_failure_stops_runtime_and_keeps_state() {
    let h = harness(&[(443, 20, 40)], false);
    let before = h.state();
    if !block_writes(h.dir.path()) {
        return;
    }
    let result = h.app.execute(Action::Connect).await;
    allow_writes(h.dir.path());
    assert!(result.is_err());
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.status, "error");
    assert!(!snapshot.runtime_active);
    assert!(!h.alive.load(Ordering::SeqCst));
    assert_eq!(h.state(), before);
    h.app.execute(Action::Connect).await.unwrap();
    assert_eq!(h.app.snapshot().status, "connected");
    h.app.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn removing_last_server_with_failed_save_restores_connected_status() {
    let h = harness(&[(443, 20, 40)], false);
    h.app.execute(Action::Connect).await.unwrap();
    let before = h.state();
    if !block_writes(h.dir.path()) {
        return;
    }
    let result = h
        .app
        .execute(Action::RemoveServer {
            id: h.ids[0].clone(),
        })
        .await;
    allow_writes(h.dir.path());
    assert!(result.is_err());
    let snapshot = h.app.snapshot();
    assert_eq!(snapshot.servers.len(), 1);
    assert!(snapshot.runtime_active);
    assert_eq!(snapshot.status, "connected");
    assert!(
        !snapshot.pending,
        "manual mode: the restored runtime queues no Initial"
    );
    assert!(h.alive.load(Ordering::SeqCst));
    assert_eq!(h.state(), before);
    h.app.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn clear_journal_save_failure_keeps_journal() {
    let h = harness(&[(443, 20, 40)], false);
    h.app.execute(Action::Connect).await.unwrap();
    let journal = h.dir.path().join("journal.json");
    let before = std::fs::read(&journal).unwrap();
    if !block_writes(h.dir.path()) {
        return;
    }
    let result = h.app.execute(Action::ClearJournal).await;
    allow_writes(h.dir.path());
    assert!(result.is_err());
    let started = |entries: &[rift_core::application::Entry]| {
        entries.iter().any(|e| e.message == "sing-box запущен")
    };
    assert!(started(&h.app.snapshot().journal));
    assert_eq!(std::fs::read(&journal).unwrap(), before);
    h.app.shutdown().await.unwrap();
    let reopened = Application::with_probe(
        persistence::load(&h.dir.path().join("state.json")).unwrap(),
        h.dir.path().into(),
        Box::new(CountingRuntime {
            alive: Arc::new(AtomicBool::new(false)),
            starts: Arc::new(AtomicUsize::new(0)),
            checks: Arc::new(AtomicUsize::new(0)),
            fail_start_once: Arc::new(AtomicBool::new(false)),
            fail_stop_once: Arc::new(AtomicBool::new(false)),
        }),
        h.probe.clone(),
        SECRET.into(),
    );
    assert!(started(&reopened.snapshot().journal));
}

#[tokio::test]
async fn clear_journal_persists_empty_journal() {
    let h = harness(&[(443, 20, 40)], false);
    h.app.execute(Action::Connect).await.unwrap();
    h.app.execute(Action::ClearJournal).await.unwrap();
    assert!(h.app.snapshot().journal.is_empty());
    let saved = std::fs::read(h.dir.path().join("journal.json")).unwrap();
    assert_eq!(saved, b"[]");
    h.app.shutdown().await.unwrap();
}

#[tokio::test]
async fn enabling_auto_select_in_settings_runs_initial_to_lowest_proxy_rtt() {
    // B works first by endpoint RTT; C has the lower proxy RTT by more than both margins.
    // Periodic would stop on B.
    let h = harness(&[(443, 20, 200), (444, 30, 40)], false);
    let (b, c) = (h.ids[0].clone(), h.ids[1].clone());
    h.app.execute(Action::Connect).await.unwrap();
    assert_eq!(h.app.snapshot().selected, Some(b));
    let mut settings = h.app.snapshot().settings;
    settings.auto_select = true;
    h.app
        .execute(Action::SaveSettings {
            settings: settings.clone(),
        })
        .await
        .unwrap();
    until(&h.app, "Initial to switch to C", |s| {
        s.selected.as_ref() == Some(&c) && !s.checking
    })
    .await;
    settings.auto_select = false;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        h.app.execute(Action::SaveSettings { settings }),
    )
    .await
    .expect("SaveSettings blocked by the running Initial")
    .unwrap();
    let snapshot = h.app.snapshot();
    assert!(!snapshot.pending);
    assert_eq!(snapshot.next_check, None);
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn config_change_during_initial_resumes_initial() {
    let h = harness(&[(443, 20, 200), (444, 30, 40)], true);
    let (b, c) = (h.ids[0].clone(), h.ids[1].clone());
    *h.probe.gate.lock().unwrap() = Some(c.clone());
    h.app.execute(Action::Connect).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), h.probe.gate_entered.notified())
        .await
        .expect("Initial did not reach C");
    assert_eq!(h.app.snapshot().selected, Some(b.clone()));
    let mut settings = h.app.snapshot().settings;
    // Only a change of the generated config interrupts the check; a theme alone does not.
    settings.proxy_port = 2090;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        h.app.execute(Action::SaveSettings { settings }),
    )
    .await
    .expect("SaveSettings blocked by the running Initial")
    .unwrap();
    // The resumed Initial still honours the cooldown after the switch to B.
    tokio::time::advance(Duration::from_secs(61)).await;
    until(&h.app, "resumed Initial to reach C", |s| {
        s.selected.as_ref() == Some(&c) && !s.checking
    })
    .await;
    assert_eq!(h.probe.selects(), vec![b, c]);
    h.app.shutdown().await.unwrap();
}

struct ChangingSubscription(Arc<AtomicUsize>);
#[async_trait]
impl SubscriptionSource for ChangingSubscription {
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
fn subscription_harness() -> (Harness, Arc<AtomicUsize>) {
    let mut store = Store::default();
    store.settings.auto_select = false;
    store.subscriptions.push(Subscription {
        id: "sub".into(),
        name: "Test".into(),
        url: "https://example.com/private".into(),
        updated_at: None,
        error: None,
        failure_code: None,
        ..Default::default()
    });
    let mut latency = HashMap::new();
    for port in [443, 444] {
        let server = parse_uri(
            &format!("trojan://credential@example.com:{port}"),
            Some("sub".into()),
        )
        .unwrap();
        latency.insert(server.id.clone(), (20, 40));
        if port == 443 {
            store.servers.push(server);
        }
    }
    let fetches = Arc::new(AtomicUsize::new(0));
    let h = build(
        store,
        latency,
        Some(Arc::new(ChangingSubscription(fetches.clone()))),
    );
    (h, fetches)
}
async fn crashed(h: &Harness) {
    until(&h.app, "crash to be published", |s| {
        s.status == "error" && !s.runtime_active
    })
    .await;
    assert_eq!(
        h.starts.load(Ordering::SeqCst),
        1,
        "a crashed sidecar must not be restarted without explicit Connect"
    );
}

#[tokio::test(start_paused = true)]
async fn a_crashed_sidecar_is_journaled_with_its_typed_code() {
    let h = harness(&[(443, 20, 40)], false);
    h.app.execute(Action::Connect).await.unwrap();
    h.alive.store(false, Ordering::SeqCst);
    crashed(&h).await;
    let snapshot = h.app.snapshot();
    assert!(snapshot.error.is_some());
    assert!(snapshot.journal.iter().any(|entry| {
        entry.level == "error" && entry.failure_code == Some(FailureCode::SidecarCrashed)
    }));
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn scheduled_refresh_after_crash_does_not_restart_runtime() {
    let (h, fetches) = subscription_harness();
    h.app.execute(Action::Connect).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while fetches.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("first subscription refresh");
    tokio::time::sleep(Duration::from_secs(2)).await;
    h.alive.store(false, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(60 * 60)).await;
    crashed(&h).await;
    until(&h.app, "changed subscription to be stored", |s| {
        s.servers.iter().all(|v| v.id != h.ids[0])
    })
    .await;
    h.app.execute(Action::Connect).await.unwrap();
    assert_eq!(h.starts.load(Ordering::SeqCst), 2);
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn manual_refresh_after_crash_does_not_restart_runtime() {
    let (h, fetches) = subscription_harness();
    h.app.execute(Action::Connect).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while fetches.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("first subscription refresh");
    tokio::time::sleep(Duration::from_secs(2)).await;
    h.alive.store(false, Ordering::SeqCst);
    let _ = h
        .app
        .execute(Action::Refresh {
            id: Some("sub".into()),
        })
        .await;
    crashed(&h).await;
    h.app.shutdown().await.unwrap();
}

#[tokio::test]
async fn subscription_count_is_capped_at_what_a_profile_can_carry() {
    let mut store = Store::default();
    for i in 0..1024 {
        store.subscriptions.push(Subscription {
            id: format!("sub{i}"),
            name: format!("Источник {i}"),
            url: format!("https://subscriptions.example/{i}"),
            updated_at: None,
            error: None,
            failure_code: None,
            ..Default::default()
        });
    }
    let h = build(store, HashMap::new(), None);
    let save = |id: Option<String>| Action::SaveSubscription {
        id,
        url: "https://subscriptions.example/new".into(),
        interval_mins: None,
        user_agent: None,
        filters: Default::default(),
        provider_routes: true,
    };

    let refused = h.app.execute(save(None)).await.unwrap_err();
    assert!(matches!(refused, Error::Invalid(_)));
    assert_eq!(h.app.snapshot().subscriptions.len(), 1024);
    // Editing an existing source is not adding one.
    h.app.execute(save(Some("sub0".into()))).await.unwrap();
    assert_eq!(h.app.snapshot().subscriptions.len(), 1024);
    // Every store the application accepted still exports.
    h.app.export_profile().await.unwrap();
    h.app.shutdown().await.unwrap();
}
