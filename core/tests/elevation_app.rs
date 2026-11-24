//! The coordinator with the elevated TUN helper: UAC at each TUN connection, a refusal
//! that falls back to Proxy without an error, restarts without a second prompt, and a helper that
//! dies telling itself apart from sing-box.
mod common;

use common::{
    fake::{pass, until, Net, NoFeed},
    helper::{tun_store, FakeRuntime, Prompt, World},
};
use rift_core::{
    application::{Action, Application, Category, Snapshot},
    elevation::{
        client::{HelperRuntime, HelperTimeouts},
        helper::Exit,
        mode::ModeRuntime,
    },
    failure::FailureCode,
    jitter::Jitter,
    model::{Mode, Store},
    notifications,
    parser::parse_uri,
    persistence,
    runtime::Runtime,
};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::sync::broadcast::Receiver;

/// The notice and error channels are process-wide.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const SECRET: &str = "12345678901234567890123456789012";
const DECLINED: &str = "Подтверждение Windows не получено: включён режим Proxy";

struct Rig {
    app: Arc<Application>,
    world: Arc<World>,
    dir: tempfile::TempDir,
    notices: Receiver<String>,
    errors: Receiver<String>,
}
fn rig(prompt: Prompt, mode: Mode, elevated: bool, edit: impl FnOnce(&mut Store)) -> Rig {
    let world = World::new(prompt);
    let timeouts = HelperTimeouts {
        consent: Duration::from_secs(5),
        connect: Duration::from_secs(5),
        hello: Duration::from_secs(5),
        start: Duration::from_secs(5),
        stop: Duration::from_secs(5),
    };
    let runtime = ModeRuntime::new(
        Box::new(FakeRuntime(world.plain_calls.clone())),
        HelperRuntime::new(world.elevator(), world.transport()).with_timeouts(timeouts),
        move || elevated,
    );
    let mut store = Store::default();
    store.settings.auto_select = false;
    store.settings.mode = mode;
    store
        .servers
        .push(parse_uri("trojan://p@example.com:1#One", None).unwrap());
    store.selected = Some(store.servers[0].id.clone());
    edit(&mut store);
    let net = Net::default();
    net.answer(1, Some(40));
    let dir = tempfile::tempdir().unwrap();
    let app = Application::with_jitter(
        store,
        dir.path().into(),
        Box::new(runtime),
        Arc::new(net),
        SECRET.into(),
        Arc::new(NoFeed),
        Jitter::none(),
    );
    app.start();
    Rig {
        app,
        world,
        dir,
        notices: notifications::subscribe_notices(),
        errors: notifications::subscribe(),
    }
}
impl Rig {
    async fn connect(&self) {
        self.app.execute(Action::Connect).await.unwrap();
    }
    fn mode(&self) -> Mode {
        self.app.snapshot().settings.mode
    }
    fn saved_mode(&self) -> Mode {
        persistence::load(&self.dir.path().join("state.json"))
            .unwrap()
            .settings
            .mode
    }
    async fn save(&self, change: impl FnOnce(&mut rift_core::model::Settings)) {
        let mut settings = self.app.snapshot().settings;
        change(&mut settings);
        self.app
            .execute(Action::SaveSettings { settings })
            .await
            .unwrap();
    }
    fn notices(&mut self) -> Vec<String> {
        let mut heard = vec![];
        while let Ok(text) = self.notices.try_recv() {
            heard.push(text);
        }
        heard
    }
    fn errors(&mut self) -> Vec<String> {
        let mut heard = vec![];
        while let Ok(text) = self.errors.try_recv() {
            heard.push(text);
        }
        heard
    }
}
fn errors_in_journal(snapshot: &Snapshot) -> Vec<(Category, Option<FailureCode>)> {
    snapshot
        .journal
        .iter()
        .filter(|entry| entry.level == "error")
        .map(|entry| (entry.category, entry.failure_code))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn connecting_in_tun_asks_once_and_runs_in_the_helper() {
    let _serial = SERIAL.lock().await;
    let rig = rig(Prompt::Accept, Mode::Tun, false, |_| {});
    rig.connect().await;
    let snapshot = rig.app.snapshot();
    assert!(snapshot.runtime_active);
    assert_eq!(snapshot.status, "connected");
    assert_eq!(rig.mode(), Mode::Tun);
    assert_eq!(rig.world.log(), ["prompt", "helper:start"]);
}

#[tokio::test(start_paused = true)]
async fn a_refusal_connects_in_proxy_with_a_notice_and_no_error_anywhere() {
    let _serial = SERIAL.lock().await;
    let mut rig = rig(Prompt::Decline, Mode::Tun, false, |_| {});
    rig.connect().await;
    let snapshot = rig.app.snapshot();
    assert!(snapshot.runtime_active);
    assert_eq!(snapshot.status, "connected");
    assert_eq!(snapshot.error, None);
    // The mode of the store is Proxy now, on disk too: the next start does not ask again.
    assert_eq!((rig.mode(), rig.saved_mode()), (Mode::Proxy, Mode::Proxy));
    assert_eq!(rig.world.log(), ["prompt", "plain:start"]);
    assert_eq!(rig.world.helpers_alive(), 0);
    assert!(
        snapshot
            .journal
            .iter()
            .any(|e| e.message == DECLINED && e.level == "info"),
        "{:?}",
        snapshot.journal
    );
    assert_eq!(errors_in_journal(&snapshot), []);
    assert_eq!(rig.notices(), [DECLINED]);
    assert_eq!(
        rig.errors(),
        Vec::<String>::new(),
        "no error toast, nothing to report"
    );
}

#[tokio::test(start_paused = true)]
async fn other_failures_are_errors_and_the_mode_is_not_changed() {
    let _serial = SERIAL.lock().await;
    let mut rig = rig(Prompt::Fail, Mode::Tun, false, |_| {});
    let error = rig.app.execute(Action::Connect).await.unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::ElevationFailed));
    let snapshot = rig.app.snapshot();
    assert!(!snapshot.runtime_active);
    assert_eq!(snapshot.status, "error");
    assert_eq!(rig.mode(), Mode::Tun, "only a refusal falls back to Proxy");
    assert_eq!(rig.notices(), Vec::<String>::new());
    assert_eq!(
        errors_in_journal(&snapshot),
        [(Category::Core, Some(FailureCode::ElevationFailed))]
    );
    assert!(!rig.errors().is_empty(), "the toast shows it");
}

#[tokio::test(start_paused = true)]
async fn switching_a_running_proxy_to_tun_is_refused_and_the_proxy_is_untouched() {
    let _serial = SERIAL.lock().await;
    let mut rig = rig(Prompt::Decline, Mode::Proxy, false, |_| {});
    rig.connect().await;
    rig.save(|s| s.mode = Mode::Tun).await;
    assert_eq!((rig.mode(), rig.saved_mode()), (Mode::Proxy, Mode::Proxy));
    assert!(rig.app.snapshot().runtime_active);
    // Not even restarted: the mode was the only thing that changed.
    assert_eq!(rig.world.log(), ["plain:start", "prompt"]);
    assert_eq!(rig.notices(), [DECLINED]);
    assert_eq!(errors_in_journal(&rig.app.snapshot()), []);
}

#[tokio::test(start_paused = true)]
async fn the_rest_of_the_change_is_applied_when_tun_was_refused() {
    let _serial = SERIAL.lock().await;
    let rig = rig(Prompt::Decline, Mode::Proxy, false, |_| {});
    rig.connect().await;
    rig.save(|s| {
        s.mode = Mode::Tun;
        s.proxy_port = 2090;
    })
    .await;
    let snapshot = rig.app.snapshot();
    assert_eq!(snapshot.settings.mode, Mode::Proxy);
    assert_eq!(
        snapshot.settings.proxy_port, 2090,
        "the port change took effect"
    );
    assert_eq!(rig.world.log(), ["plain:start", "prompt", "plain:start"]);
}

#[tokio::test(start_paused = true)]
async fn switching_a_running_proxy_to_tun_after_consent_hands_over_to_the_helper() {
    let _serial = SERIAL.lock().await;
    let rig = rig(Prompt::Accept, Mode::Proxy, false, |_| {});
    rig.connect().await;
    rig.save(|s| s.mode = Mode::TunProxy).await;
    assert_eq!(rig.mode(), Mode::TunProxy);
    assert_eq!(
        rig.world.log(),
        ["plain:start", "prompt", "plain:stop", "helper:start"]
    );
    assert!(rig.app.snapshot().runtime_active);
}

#[tokio::test(start_paused = true)]
async fn restarts_while_tun_runs_do_not_ask_again() {
    let _serial = SERIAL.lock().await;
    let rig = rig(Prompt::Accept, Mode::Tun, false, |_| {});
    rig.connect().await;
    // The proxy port is not in a pure TUN config: nothing to restart for it.
    rig.save(|s| s.proxy_port = 2090).await;
    rig.save(|s| s.mode = Mode::TunProxy).await;
    rig.save(|s| s.proxy_port = 2091).await;
    rig.save(|s| s.api_port = 2095).await;
    assert_eq!(rig.world.prompts(), 1);
    assert_eq!(rig.world.helper_calls.starts.load(Ordering::SeqCst), 4);
    assert_eq!(rig.world.helpers_alive(), 1);
    assert!(rig.app.snapshot().runtime_active);
}

#[tokio::test(start_paused = true)]
async fn disconnecting_ends_the_helper_and_the_next_tun_connection_asks_again() {
    let _serial = SERIAL.lock().await;
    let rig = rig(Prompt::Accept, Mode::Tun, false, |_| {});
    rig.connect().await;
    rig.app.execute(Action::Disconnect).await.unwrap();
    assert_eq!(rig.world.last_exit().await, Exit::Stopped);
    assert!(!rig.world.helper_calls.alive.load(Ordering::SeqCst));
    assert_eq!(rig.app.snapshot().status, "disconnected");
    rig.connect().await;
    assert_eq!(rig.world.prompts(), 2);
}

#[tokio::test(start_paused = true)]
async fn leaving_tun_ends_the_helper_without_a_prompt() {
    let _serial = SERIAL.lock().await;
    let rig = rig(Prompt::Accept, Mode::Tun, false, |_| {});
    rig.connect().await;
    rig.save(|s| s.mode = Mode::Proxy).await;
    assert_eq!(rig.world.last_exit().await, Exit::Stopped);
    assert_eq!(rig.world.prompts(), 1);
    assert!(rig.app.snapshot().runtime_active);
    assert_eq!(rig.world.plain_calls.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn exiting_rift_ends_the_helper() {
    let _serial = SERIAL.lock().await;
    let rig = rig(Prompt::Accept, Mode::Tun, false, |_| {});
    rig.connect().await;
    rig.app.shutdown().await.unwrap();
    assert_eq!(rig.world.last_exit().await, Exit::Stopped);
    assert!(!rig.world.helper_calls.alive.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn a_rift_with_administrator_rights_goes_round_the_helper() {
    let _serial = SERIAL.lock().await;
    let rig = rig(Prompt::Decline, Mode::Tun, true, |_| {});
    rig.connect().await;
    assert_eq!(rig.world.prompts(), 0);
    assert_eq!(rig.mode(), Mode::Tun);
    assert_eq!(rig.world.log(), ["plain:start"]);
}

#[tokio::test(start_paused = true)]
async fn a_helper_that_dies_and_sing_box_that_dies_are_told_apart() {
    let _serial = SERIAL.lock().await;
    let rig = rig(Prompt::Accept, Mode::Tun, false, |_| {});
    rig.connect().await;
    rig.world.crash_last_helper();
    until(&rig.app, "the helper's crash", |s| s.status == "error").await;
    let snapshot = rig.app.snapshot();
    assert_eq!(
        snapshot.error.as_deref(),
        Some("Помощник TUN неожиданно завершился")
    );
    assert_eq!(
        errors_in_journal(&snapshot),
        [(Category::Core, Some(FailureCode::ElevationHelperCrashed))]
    );
    assert!(!snapshot.runtime_active);

    // Connecting again is a new prompt, and this time sing-box inside the helper dies.
    rig.connect().await;
    assert_eq!(rig.world.prompts(), 2);
    rig.world.helper_calls.alive.store(false, Ordering::SeqCst);
    until(&rig.app, "sing-box's crash", |s| {
        s.error.as_deref() == Some("sing-box неожиданно завершился")
    })
    .await;
    assert!(errors_in_journal(&rig.app.snapshot())
        .contains(&(Category::Core, Some(FailureCode::SidecarCrashed))));
}

#[tokio::test(start_paused = true)]
async fn status_query_errors_disconnect_without_claiming_an_engine_crash_in_either_mode() {
    let _serial = SERIAL.lock().await;
    for mode in [Mode::Proxy, Mode::Tun] {
        let tun = mode == Mode::Tun;
        let rig = rig(Prompt::Accept, mode, false, |_| {});
        rig.connect().await;
        let calls = if tun {
            rig.world.helper_calls.clone()
        } else {
            rig.world.plain_calls.clone()
        };
        calls.status_error.store(true, Ordering::SeqCst);
        until(&rig.app, "runtime status-query failure", |snapshot| {
            snapshot.status == "error"
        })
        .await;
        let snapshot = rig.app.snapshot();
        assert_eq!(
            snapshot.error.as_deref(),
            Some("Не удалось проверить состояние ядер")
        );
        assert!(!snapshot.runtime_active);
        assert_eq!(
            errors_in_journal(&snapshot),
            [(Category::App, Some(FailureCode::Unknown))]
        );
        assert!(!calls.alive.load(Ordering::SeqCst));
        assert!(calls.stops.load(Ordering::SeqCst) > 0);
        assert_eq!(calls.starts.load(Ordering::SeqCst), 1);
        if tun {
            assert_eq!(rig.world.last_exit().await, Exit::SingBoxExited);
        }
        rig.app.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn auto_connect_with_tun_asks_at_start_and_falls_back_to_proxy_when_refused() {
    let _serial = SERIAL.lock().await;
    let mut rig = rig(Prompt::Decline, Mode::Tun, false, |store| {
        store.settings.auto_connect = true;
    });
    rig.app.auto_connect().await;
    pass(1).await;
    assert_eq!(rig.world.prompts(), 1);
    assert!(rig.app.snapshot().runtime_active);
    assert_eq!(rig.saved_mode(), Mode::Proxy);
    assert_eq!(rig.notices(), [DECLINED]);
}

#[tokio::test(start_paused = true)]
async fn the_store_the_helper_gets_is_the_one_the_user_saved() {
    let _serial = SERIAL.lock().await;
    let rig = rig(Prompt::Accept, Mode::Tun, false, |store| {
        *store = {
            let mut s = tun_store();
            s.settings.auto_select = false;
            s.selected = Some(s.servers[0].id.clone());
            s
        };
    });
    rig.connect().await;
    let stores = rig.world.helper_calls.stores.lock().unwrap().clone();
    assert_eq!(stores.len(), 1);
    assert_eq!(stores[0].servers.len(), 1);
    assert_eq!(stores[0].settings.mode, Mode::Tun);
    assert_eq!(
        rig.world.helper_calls.secrets.lock().unwrap().as_slice(),
        [SECRET]
    );
}

/// Only launch/transport and the Windows rights check are adapted: the helper executes the
/// real Sidecar config check, including its actual OS spawn failure, on every host.
struct CheckingRuntime(rift_core::runtime::Sidecar);
#[async_trait::async_trait]
impl rift_core::runtime::Runtime for CheckingRuntime {
    async fn validate(&mut self, store: &Store, secret: &str) -> rift_core::Result<()> {
        self.0.validate(store, secret).await
    }
    async fn start(&mut self, store: &Store, secret: &str) -> rift_core::Result<()> {
        self.0.validate(store, secret).await
    }
    async fn stop(&mut self) -> rift_core::Result<()> {
        self.0.stop().await
    }
    fn alive(&mut self) -> rift_core::Result<bool> {
        self.0.alive()
    }
}
struct CheckingPeer {
    stream: std::sync::Mutex<Option<tokio::io::DuplexStream>>,
    runtime: std::sync::Mutex<Option<CheckingRuntime>>,
    task: std::sync::Mutex<Option<tokio::task::JoinHandle<Exit>>>,
}
struct CheckingListener(Option<tokio::io::DuplexStream>);
#[async_trait::async_trait]
impl rift_core::elevation::client::Listener for CheckingListener {
    fn name(&self) -> String {
        "synthetic-startup-check-channel".into()
    }
    async fn accept(&mut self) -> std::io::Result<rift_core::elevation::client::BoxStream> {
        Ok(Box::new(self.0.take().unwrap()))
    }
}
impl rift_core::elevation::client::Transport for CheckingPeer {
    fn listen(&self) -> rift_core::Result<Box<dyn rift_core::elevation::client::Listener>> {
        let (parent, helper) = tokio::io::duplex(65536);
        *self.stream.lock().unwrap() = Some(helper);
        Ok(Box::new(CheckingListener(Some(parent))))
    }
}
#[async_trait::async_trait]
impl rift_core::elevation::client::Elevator for CheckingPeer {
    async fn launch(&self, _: &str, token: &str) -> rift_core::elevation::client::Launched {
        let stream = self.stream.lock().unwrap().take().unwrap();
        let mut runtime = self.runtime.lock().unwrap().take().unwrap();
        let token = token.to_owned();
        *self.task.lock().unwrap() = Some(tokio::spawn(async move {
            rift_core::elevation::helper::serve(
                stream,
                &token,
                &mut runtime,
                Duration::from_millis(10),
            )
            .await
        }));
        rift_core::elevation::client::Launched::Started
    }
}

#[tokio::test]
async fn actual_missing_engine_checks_survive_helper_application_journal_and_manual_export() {
    use rift_core::runtime::Sidecar;
    let _serial = SERIAL.lock().await;
    for xray in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let helper_dir = tempfile::tempdir().unwrap();
        let missing = helper_dir.path().join("missing-executable-path-canary");
        let os_code = std::fs::File::open(&missing)
            .unwrap_err()
            .raw_os_error()
            .unwrap();
        let sidecar = Sidecar::new(missing.clone(), helper_dir.path().into());
        let mut sidecar = if xray {
            sidecar.with_xray_binary(missing)
        } else {
            sidecar
        };
        let mut store = tun_store();
        store.settings.auto_select = false;
        store.servers = vec![parse_uri(
            "trojan://startup-password-canary@startup-host-canary.example:443#startup-name-canary",
            None,
        )
        .unwrap()];
        store.selected = Some(store.servers[0].id.clone());
        let (code, stage) = if xray {
            (FailureCode::XrayStartExit, "xray_config_check_spawn")
        } else {
            (FailureCode::SidecarStartExit, "sing_box_config_check_spawn")
        };
        let expected = format!("stage={stage}; os_error={os_code}");
        let direct = sidecar
            .validate(&store, common::helper::SECRET)
            .await
            .unwrap_err();
        assert_eq!(direct.failure_code(), Some(code));
        assert!(direct.diagnostic_details().contains(&expected));
        let peer = Arc::new(CheckingPeer {
            stream: std::sync::Mutex::new(None),
            runtime: std::sync::Mutex::new(Some(CheckingRuntime(sidecar))),
            task: std::sync::Mutex::new(None),
        });
        let runtime = HelperRuntime::new(peer.clone(), peer.clone());
        let app = Application::with_jitter(
            store,
            dir.path().into(),
            Box::new(runtime),
            Arc::new(Net::default()),
            common::helper::SECRET.into(),
            Arc::new(NoFeed),
            Jitter::none(),
        );
        let error = app.execute(Action::Connect).await.unwrap_err();
        assert_eq!(error.failure_code(), Some(code));
        assert!(error.diagnostic_details().contains(&expected));
        let snapshot = app.snapshot();
        let entry = snapshot
            .journal
            .iter()
            .find(|entry| entry.failure_code == Some(code) && entry.level == "error")
            .expect("Application journals the concrete helper failure");
        let details = entry.details.as_deref().unwrap();
        assert!(details.contains(&expected), "{details}");
        assert!(details.len() <= rift_core::journal::MAX_DETAIL_BYTES);
        let journal = std::fs::read_to_string(dir.path().join("journal.json")).unwrap();
        assert!(journal.contains(&expected), "{journal}");
        assert!(
            !dir.path().join("diagnostics.json").exists(),
            "no automatic export"
        );
        app.execute(Action::ExportDiagnostics).await.unwrap();
        let export = std::fs::read_to_string(dir.path().join("diagnostics.json")).unwrap();
        assert!(export.contains(&expected), "{export}");
        for text in [journal, export] {
            for forbidden in [
                "startup-password-canary",
                "startup-host-canary",
                "startup-name-canary",
                "missing-executable-path-canary",
                common::helper::SECRET,
                "trojan://",
            ] {
                assert!(!text.contains(forbidden), "{forbidden}: {text}");
            }
        }
        assert_eq!(std::fs::read_dir(helper_dir.path()).unwrap().count(), 0);
        app.shutdown().await.unwrap();
        let task = peer.task.lock().unwrap().take().unwrap();
        assert_eq!(task.await.unwrap(), Exit::Stopped);
    }
}
