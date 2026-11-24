use rift_core::{
    failure::FailureCode,
    model::Store,
    parser::parse_uri,
    probe::NetworkProbe,
    runtime::{Runtime, Sidecar},
    selection::Probe,
};
#[tokio::test]
async fn a_missing_sidecar_binary_is_a_start_failure_not_a_rejected_config() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::default();
    store
        .servers
        .push(parse_uri("trojan://credential@127.0.0.1:9", None).unwrap());
    let mut core = Sidecar::new(dir.path().join("no-such-sing-box"), dir.path().into());

    let error = core
        .validate(&store, "test-secret-012345678901234567890123")
        .await
        .unwrap_err();

    assert_eq!(error.failure_code(), Some(FailureCode::SidecarStartExit));
    assert!(!error.to_string().contains("no-such-sing-box"));
    let details = error.diagnostic_details();
    assert!(details.contains("spawn"), "{details}");
    assert!(details.contains("os_error="), "{details}");
    assert!(!details.contains("credential"), "{details}");
    assert!(!details.contains("test-secret"), "{details}");
}

/// The real `Sidecar` with `/bin/sh` standing in for sing-box: it is started as
/// `sh check -c stdin` / `sh run -c stdin` in the data directory, so the scripts `check` and
/// `run` placed there decide how the "sing-box" behaves. Nothing is executed from a file that
/// was just written, and the failures below need no real sing-box.
#[cfg(unix)]
mod scripted_child {
    use super::*;
    use rift_core::runtime::SidecarTimeouts;
    use std::time::Duration;

    const SECRET: &str = "test-secret-012345678901234567890123";
    const HANGS: &str = "exec sleep 30\n";
    const ACCEPTS_CONFIG: &str = "cat >/dev/null\n";

    fn store() -> Store {
        let mut store = Store::default();
        store.settings.proxy_port = super::free_port();
        store.settings.api_port = super::free_port();
        store
            .servers
            .push(parse_uri("trojan://credential@127.0.0.1:9", None).unwrap());
        store
    }
    fn sidecar(check: &str, run: &str, timeouts: SidecarTimeouts) -> (Sidecar, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("check"), check).unwrap();
        std::fs::write(dir.path().join("run"), run).unwrap();
        let core = Sidecar::new("/bin/sh".into(), dir.path().into()).with_timeouts(timeouts);
        (core, dir)
    }
    fn generous() -> SidecarTimeouts {
        SidecarTimeouts {
            check: Duration::from_secs(5),
            feed: Duration::from_secs(5),
            ready: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    async fn a_check_that_never_answers_is_a_config_timeout() {
        let timeouts = SidecarTimeouts {
            check: Duration::from_millis(300),
            ..generous()
        };
        let (mut core, _dir) = sidecar(HANGS, ACCEPTS_CONFIG, timeouts);
        let error = core.validate(&store(), SECRET).await.unwrap_err();
        assert_eq!(
            error.failure_code(),
            Some(FailureCode::SidecarConfigTimeout)
        );
    }

    #[tokio::test]
    async fn a_failing_check_is_a_rejected_config() {
        let (mut core, _dir) = sidecar("exit 1\n", ACCEPTS_CONFIG, generous());
        let error = core.validate(&store(), SECRET).await.unwrap_err();
        assert_eq!(
            error.failure_code(),
            Some(FailureCode::SidecarConfigRejected)
        );
    }

    #[tokio::test]
    async fn a_child_that_exits_at_start_is_a_start_failure() {
        let (mut core, _dir) = sidecar(ACCEPTS_CONFIG, "exit 3\n", generous());
        let error = core.start(&store(), SECRET).await.unwrap_err();
        assert_eq!(error.failure_code(), Some(FailureCode::SidecarStartExit));
        assert!(!core.alive().unwrap());
        let diagnostics = core.diagnostics();
        assert_eq!(diagnostics.generation, 1);
        assert_eq!(
            diagnostics.primary_exit,
            Some(rift_core::runtime::Engine::SingBox)
        );
        assert_eq!(diagnostics.exits.len(), 1);
        assert_eq!(diagnostics.exits[0].code, Some(3));
        assert_eq!(
            diagnostics.exits[0].stage,
            rift_core::runtime::ExitStage::Startup
        );
        assert_eq!(
            diagnostics.exits[0].termination,
            rift_core::runtime::Termination::Unexpected
        );
        core.stop().await.unwrap();
        assert_eq!(
            core.diagnostics(),
            diagnostics,
            "cleanup retains the original status"
        );
        let error = core.start(&store(), SECRET).await.unwrap_err();
        assert_eq!(error.failure_code(), Some(FailureCode::SidecarStartExit));
        assert_eq!(core.diagnostics().generation, 2);
        assert_eq!(
            core.diagnostics().exits.len(),
            1,
            "new generation replaces old exits"
        );
    }

    #[tokio::test]
    async fn a_denied_local_listener_names_only_its_verified_purpose_and_port() {
        for api in [false, true] {
            let store = store();
            let port = if api {
                store.settings.api_port
            } else {
                store.settings.proxy_port
            };
            let operation = if api {
                "finish-start clash server: external controller listen error"
            } else {
                "start inbound/mixed[mixed]"
            };
            let run = format!("{ACCEPTS_CONFIG}printf '%s' 'FATAL[0000] start service: {operation}: listen tcp 127.0.0.1:{port}: bind: An attempt was made to access a socket in a way forbidden by its access permissions.' >&2\nexit 19\n");
            let (mut core, _dir) = sidecar(ACCEPTS_CONFIG, &run, generous());
            let error = core.start(&store, SECRET).await.unwrap_err();
            assert_eq!(error.failure_code(), Some(FailureCode::LocalPortDenied));
            let message = error.to_string();
            assert!(message.contains(&port.to_string()));
            assert!(message.contains(if api { "Clash API" } else { "HTTP/SOCKS proxy" }));
            for misleading in ["TUN", "брандмауэр", "VPN", "127.0.0.1"] {
                assert!(!message.contains(misleading), "{message}");
            }
            let diagnostics = core.diagnostics();
            assert_eq!(diagnostics.listener.unwrap().port, port);
            assert_eq!(diagnostics.exits[0].code, Some(19));
        }
    }

    #[tokio::test]
    async fn local_listener_failures_stay_in_the_core_journal_category() {
        use rift_core::application::{Action, Application, Category};
        for (cause, code) in [
            ("address already in use", FailureCode::LocalPortInUse),
            ("permission denied", FailureCode::LocalPortDenied),
        ] {
            let store = store();
            let port = store.settings.proxy_port;
            let run = format!(
                "{ACCEPTS_CONFIG}printf '%s' 'FATAL[0000] start service: start inbound/mixed[mixed]: listen tcp 127.0.0.1:{port}: bind: {cause}' >&2\nexit 19\n"
            );
            let (core, dir) = sidecar(ACCEPTS_CONFIG, &run, generous());
            let app = Application::new(store, dir.path().into(), Box::new(core)).unwrap();
            let error = app.execute(Action::Connect).await.unwrap_err();
            assert_eq!(error.failure_code(), Some(code));
            let snapshot = app.snapshot();
            let entry = snapshot
                .journal
                .iter()
                .find(|entry| entry.failure_code == Some(code))
                .unwrap();
            assert_eq!(entry.category, Category::Core);
            assert_eq!(snapshot.status, "error");
            assert!(snapshot.error.as_ref().unwrap().contains(&port.to_string()));
            app.execute(Action::ExportDiagnostics).await.unwrap();
            let exported: serde_json::Value = serde_json::from_slice(
                &std::fs::read(dir.path().join("diagnostics.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(exported["runtime"]["listener"]["port"], port);
        }
    }

    #[tokio::test]
    async fn an_unknown_exit_or_unverified_listener_does_not_guess_a_port_conflict() {
        let store = store();
        let port = (1..=u16::MAX)
            .find(|port| *port != store.settings.proxy_port && *port != store.settings.api_port)
            .unwrap();
        for text in [
            "ERROR dial tcp secret-canary.example:443: permission denied".to_owned(),
            format!("FATAL[0000] start service: start inbound/mixed[mixed]: listen tcp 127.0.0.1:{port}: bind: address already in use"),
            "FATAL unrecognized startup failure password=secret-canary".to_owned(),
        ] {
            let run = format!("{ACCEPTS_CONFIG}printf '%s\\n' '{text}' >&2\nexit 23\n");
            let (mut core, _dir) = sidecar(ACCEPTS_CONFIG, &run, generous());
            let error = core.start(&store, SECRET).await.unwrap_err();
            assert_eq!(error.failure_code(), Some(FailureCode::SidecarStartExit));
            assert!(core.diagnostics().listener.is_none());
            let safe = format!("{} {:?}", error, core.diagnostics());
            assert!(!safe.contains("secret-canary") && !safe.contains("TUN"));
        }
    }
    /// A child that prints addresses and credentials, then dies: the application writes one
    /// `core` entry with the code, and nothing of what the child said reaches the journal, the
    /// banner or the snapshot.
    #[tokio::test]
    async fn what_a_dying_child_prints_never_reaches_the_journal() {
        use rift_core::application::{Action, Application, Category};
        let run = format!(
            "{ACCEPTS_CONFIG}echo 'dial tcp secret.example:443 uuid 11111111-1111-4111-8111-111111111111 password=hunter2'\necho 'vless://11111111-1111-4111-8111-111111111111@secret.example:443' >&2\nexit 3\n"
        );
        let (core, dir) = sidecar(ACCEPTS_CONFIG, &run, generous());
        let app = Application::new(store(), dir.path().into(), Box::new(core)).unwrap();
        let error = app.execute(Action::Connect).await.unwrap_err();
        assert_eq!(error.failure_code(), Some(FailureCode::SidecarStartExit));
        let snapshot = app.snapshot();
        let entry = snapshot
            .journal
            .iter()
            .find(|entry| entry.failure_code == Some(FailureCode::SidecarStartExit))
            .expect("the failed start is journaled");
        assert_eq!(entry.category, Category::Core);
        let everything = format!(
            "{} {:?} {}",
            serde_json::to_string(&snapshot.journal).unwrap(),
            snapshot.error,
            error
        );
        for secret in ["secret.example", "hunter2", "11111111", "dial tcp"] {
            assert!(
                !everything.contains(secret),
                "{secret} leaked: {everything}"
            );
        }
    }

    #[tokio::test]
    async fn an_api_that_never_answers_is_a_readiness_timeout_and_the_child_is_stopped() {
        let timeouts = SidecarTimeouts {
            ready: Duration::from_millis(500),
            ..generous()
        };
        let run = format!("{ACCEPTS_CONFIG}{HANGS}");
        let (mut core, _dir) = sidecar(ACCEPTS_CONFIG, &run, timeouts);
        let error = core.start(&store(), SECRET).await.unwrap_err();
        assert_eq!(
            error.failure_code(),
            Some(FailureCode::SidecarReadinessTimeout)
        );
        assert!(
            !core.alive().unwrap(),
            "a child without a usable API is stopped"
        );
        assert_eq!(core.pid(), None);
    }

    #[tokio::test]
    async fn a_proven_api_bind_failure_is_not_hidden_by_readiness_timeout() {
        let mut store = store();
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        store.settings.api_port = occupied.local_addr().unwrap().port();
        store.settings.proxy_timeout_ms = 30_000;
        let port = store.settings.api_port;
        let run = format!(
            "{ACCEPTS_CONFIG}printf '%s\\n' 'FATAL[0000] start service: finish-start clash server: external controller listen error: listen tcp 127.0.0.1:{port}: bind: address already in use' >&2\n{HANGS}"
        );
        let (mut core, _dir) = sidecar(
            ACCEPTS_CONFIG,
            &run,
            SidecarTimeouts {
                ready: Duration::from_millis(300),
                ..generous()
            },
        );
        let error = core.start(&store, SECRET).await.unwrap_err();
        assert_eq!(error.failure_code(), Some(FailureCode::LocalPortInUse));
        assert_eq!(core.diagnostics().listener.unwrap().port, port);
        assert!(error.to_string().contains("Clash API"));
        assert!(!core.alive().unwrap());
    }

    /// Xray stand-in for `xray_death_after_authentication_during_sing_box_readiness_keeps_its_cause`:
    /// it takes the config from stdin, completes the SOCKS5 username/password authentication
    /// Rift uses as Xray's readiness check, then dies only once the sing-box stand-in has
    /// started, so Xray's death falls inside sing-box's readiness window.
    const XRAY_STAND_IN: &str = r#"
import json, pathlib, socket, sys, time
config = json.load(sys.stdin)
inbound = config['inbounds'][0]
account = inbound['settings']['accounts'][0]
with socket.socket() as listener:
    listener.bind(('127.0.0.1', inbound['port']))
    listener.listen(1)
    connection, _ = listener.accept()
    with connection:
        def read(n):
            data = b''
            while len(data) < n:
                part = connection.recv(n - len(data))
                if not part:
                    raise EOFError()
                data += part
            return data
        assert read(3) == bytes([5, 1, 2])
        connection.sendall(bytes([5, 2]))
        assert read(1) == bytes([1])
        user = read(read(1)[0]).decode()
        password = read(read(1)[0]).decode()
        assert user == account['user'] and password == account['pass']
        connection.sendall(bytes([1, 0]))
    pathlib.Path('authenticated').touch()
    while not pathlib.Path('sing-started').exists():
        time.sleep(0.01)
sys.exit(1)
"#;

    /// `/bin/sh` stands in for both cores, told apart by their arguments: Xray runs
    /// `sh run -c stdin: -format json` (with `-test` for its config check), sing-box runs
    /// `sh run -c stdin`. Only the interpreter is executed; the Xray stand-in is a script it
    /// reads, not a file that was just written.
    #[tokio::test]
    async fn xray_death_after_authentication_during_sing_box_readiness_keeps_its_cause() {
        let run = format!(
            "case \"$2$5\" in\n\
             stdin:-test) cat >/dev/null; exit 0;;\n\
             stdin:*) exec python3 xray-stand-in.py;;\n\
             esac\n\
             {ACCEPTS_CONFIG}touch sing-started\n{HANGS}"
        );
        let (core, dir) = sidecar(ACCEPTS_CONFIG, &run, generous());
        std::fs::write(dir.path().join("xray-stand-in.py"), XRAY_STAND_IN).unwrap();
        let mut core = core.with_xray_binary("/bin/sh".into());
        let error = core.start(&store(), SECRET).await.unwrap_err();
        assert!(
            dir.path().join("authenticated").exists(),
            "actual SOCKS authentication completed"
        );
        assert!(dir.path().join("sing-started").exists());
        assert_eq!(error.failure_code(), Some(FailureCode::XrayCrashed));
        assert!(error.to_string().contains("Xray"));
        assert!(!error.to_string().contains("sing-box"));
        assert_eq!(core.crash_code(), FailureCode::XrayCrashed);
        assert!(core.pid().is_none() && core.xray_pid().is_none());
        assert!(core.proxy_channel().unwrap().read().unwrap().is_none());
        let diagnostics = core.diagnostics();
        assert_eq!(
            diagnostics.primary_exit,
            Some(rift_core::runtime::Engine::Xray)
        );
        let xray = diagnostics
            .exits
            .iter()
            .find(|exit| exit.engine == rift_core::runtime::Engine::Xray)
            .unwrap();
        assert_eq!(xray.code, Some(1));
        assert_eq!(xray.stage, rift_core::runtime::ExitStage::Startup);
        assert_eq!(
            xray.termination,
            rift_core::runtime::Termination::Unexpected
        );
        let sibling = diagnostics
            .exits
            .iter()
            .find(|exit| exit.engine == rift_core::runtime::Engine::SingBox)
            .unwrap();
        assert_eq!(
            sibling.termination,
            rift_core::runtime::Termination::SiblingFailure
        );
        core.stop().await.unwrap();
        assert_eq!(core.diagnostics(), diagnostics);
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
#[tokio::test]
#[ignore = "requires bundled sing-box; run cargo test -p rift-core --test sidecar -- --ignored"]
async fn starts_switches_and_stops_only_owned_process() {
    let suffix = if cfg!(windows) {
        "x86_64-pc-windows-msvc.exe"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let binary = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../desktop/src-tauri/binaries/sing-box-{suffix}"));
    let mut store = Store::default();
    store.settings.proxy_port = free_port();
    store.settings.api_port = free_port();
    // Distinctive, not just "test"/"other": a short common word would false-positive when
    // scanning file bytes for leftover credentials below (e.g. sing-box's own log text).
    let password_one = "outbound-password-one";
    let password_two = "outbound-password-two";
    store
        .servers
        .push(parse_uri(&format!("trojan://{password_one}@127.0.0.1:9"), None).unwrap());
    store
        .servers
        .push(parse_uri(&format!("trojan://{password_two}@127.0.0.1:10"), None).unwrap());
    let dir = tempfile::tempdir().unwrap();
    // Plain-text configs written by earlier versions are removed on start.
    for legacy in ["candidate.json", "sing-box.json"] {
        std::fs::write(dir.path().join(legacy), "{\"legacy\": true}").unwrap();
    }
    let mut core = Sidecar::new(binary.clone(), dir.path().into());
    let secret = "test-secret-012345678901234567890123";
    core.start(&store, secret).await.unwrap();
    assert!(core.alive().unwrap());
    // The config (outbound passwords, API secret) reaches sing-box only through stdin.
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        assert!(
            !path.ends_with("candidate.json") && !path.ends_with("sing-box.json"),
            "{path:?} left on disk"
        );
        if path.is_file() {
            let bytes = std::fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            assert!(!text.contains(secret), "{path:?} holds the API secret");
            for password in [password_one, password_two] {
                assert!(
                    !text.contains(password),
                    "{path:?} holds an outbound password"
                );
            }
        }
    }
    let pid = core.pid();
    #[cfg(target_os = "linux")]
    {
        let cmdline = std::fs::read(format!("/proc/{}/cmdline", pid.unwrap())).unwrap();
        assert!(
            String::from_utf8_lossy(&cmdline).contains("sing-box"),
            "pid must be the owned sing-box child"
        );
    }
    let mut bad = store.clone();
    bad.servers[0].outbound["type"] = serde_json::json!("unsupported-protocol");
    assert!(core.start(&bad, secret).await.is_err());
    assert_eq!(core.pid(), pid);
    assert!(core.alive().unwrap());
    let api = NetworkProbe::new(store.settings.clone(), secret.into()).unwrap();
    api.select(&store.servers[1].id).await.unwrap();
    assert!(api.proxy(&store.servers[0]).await.is_err());
    let selected: serde_json::Value = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!(
            "http://127.0.0.1:{}/proxies/main",
            store.settings.api_port
        ))
        .bearer_auth(secret)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(selected["now"], store.servers[1].id);
    let bad_api = NetworkProbe::new(store.settings.clone(), "wrong-token".into()).unwrap();
    assert!(bad_api.ready().await.is_err());
    let occupied = tempfile::tempdir().unwrap();
    let mut other = Sidecar::new(binary, occupied.path().into());
    let occupied_error = other
        .start(&store, "different-secret-01234567890123456")
        .await
        .unwrap_err();
    assert_eq!(
        occupied_error.failure_code(),
        Some(FailureCode::LocalPortInUse)
    );
    let listener = other
        .diagnostics()
        .listener
        .expect("actual bind error names its local listener");
    assert_eq!(listener.cause, rift_core::sidecar_log::ListenerCause::InUse);
    assert_eq!(
        listener.purpose,
        rift_core::sidecar_log::ListenerPurpose::Proxy
    );
    assert_eq!(listener.port, store.settings.proxy_port);
    assert!(occupied_error
        .to_string()
        .contains(&listener.port.to_string()));
    assert!(core.alive().unwrap());
    assert!(!other.alive().unwrap());
    core.stop().await.unwrap();
    assert!(!core.alive().unwrap());
    core.stop().await.unwrap();
}

/// Every name is the loopback address: the stand-in for the resolvers behind the bridge, so that
/// a name nobody can resolve is still dialed, and only through the bridge.
struct Loopback;
#[async_trait::async_trait]
impl rift_core::dns::Upstream for Loopback {
    fn name(&self) -> &'static str {
        "test"
    }
    async fn ask(&self, query: &[u8]) -> Result<Vec<u8>, rift_core::dns::Miss> {
        let question =
            rift_core::dns::parse_question(query).ok_or(rift_core::dns::Miss::Malformed)?;
        let addresses: &[std::net::IpAddr] = if question.qtype == 1 {
            &[std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)]
        } else {
            &[]
        };
        rift_core::dns::answer_addresses(query, addresses).ok_or(rift_core::dns::Miss::Malformed)
    }
}
fn loopback_chain() -> rift_core::dns::Chain {
    rift_core::dns::Chain::new(vec![Box::new(Loopback)])
}

/// The defect of a Windows machine whose VPN clients drop port 53 of every program but the
/// system's: sing-box could not resolve a name it dials itself. Its names now go to the bridge,
/// so a site that leaves directly (a `.ru` name, by the default profile) opens with no resolver
/// of sing-box's own reachable at all: the name below exists nowhere but in the bridge.
#[tokio::test]
#[ignore = "requires bundled sing-box; run cargo test -p rift-core --test sidecar -- --ignored"]
async fn a_name_sing_box_dials_directly_is_answered_by_the_bridge() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let binary = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../desktop/src-tauri/binaries/sing-box-x86_64-unknown-linux-gnu");
    if !binary.exists() {
        eprintln!("skipped: no sing-box binary for this machine");
        return;
    }
    // A site on the loopback address: whoever dials `rift-bridge-test.ru:PORT` reaches it.
    let site = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let site_port = site.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = site.accept().await {
            tokio::spawn(async move {
                let mut request = [0u8; 2048];
                let _ = stream.read(&mut request).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 6\r\nconnection: close\r\n\r\nbridge",
                    )
                    .await;
            });
        }
    });
    let mut store = Store::default();
    store.settings.proxy_port = free_port();
    store.settings.api_port = free_port();
    store
        .servers
        .push(parse_uri("trojan://credential@127.0.0.1:9", None).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let mut core = Sidecar::new(binary, dir.path().into()).with_chain(loopback_chain);
    core.start(&store, "test-secret-012345678901234567890123")
        .await
        .unwrap();
    let through_sing_box = reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::http(format!("http://127.0.0.1:{}", store.settings.proxy_port))
                .unwrap(),
        )
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap();
    let body = through_sing_box
        .get(format!("http://rift-bridge-test.ru:{site_port}/"))
        .send()
        .await
        .expect("sing-box dials the name it got from the bridge")
        .text()
        .await
        .unwrap();
    assert_eq!(body, "bridge");
    let stats = core.dns_stats();
    assert_eq!(stats.len(), 1);
    assert!(stats[0].answered >= 1, "the bridge answered: {stats:?}");
    core.stop().await.unwrap();
    assert!(
        core.dns_stats().is_empty(),
        "the bridge closes with the child"
    );
}

/// The real log of sing-box 1.14.1, read as the classifier expects it: a connection through a
/// server that refuses it is a failure of the stage "dial", cause "refused", of the selected server
/// (with neither its address nor the site in what is kept).
#[tokio::test]
#[ignore = "requires bundled sing-box; run cargo test -p rift-core --test sidecar -- --ignored"]
async fn a_connection_sing_box_could_not_open_is_reduced_to_stage_cause_and_server() {
    use rift_core::sidecar_log::{Cause, Stage, Whom};
    let binary = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../desktop/src-tauri/binaries/sing-box-x86_64-unknown-linux-gnu");
    if !binary.exists() {
        eprintln!("skipped: no sing-box binary for this machine");
        return;
    }
    let mut store = Store::default();
    store.settings.proxy_port = free_port();
    store.settings.api_port = free_port();
    // Nothing listens on the port of the discard service: the connection is refused at once.
    let server = parse_uri("trojan://credential@127.0.0.1:9", None).unwrap();
    store.servers.push(server.clone());
    let dir = tempfile::tempdir().unwrap();
    let mut core = Sidecar::new(binary, dir.path().into()).with_chain(loopback_chain);
    core.start(&store, "test-secret-012345678901234567890123")
        .await
        .unwrap();
    // A site that is not in the direct list goes through the server.
    let through_sing_box = reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::http(format!("http://127.0.0.1:{}", store.settings.proxy_port))
                .unwrap(),
        )
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let _ = through_sing_box
        .get("http://rift-refused-test.example/")
        .send()
        .await;
    let mut summary = core.log_summary().unwrap();
    for _ in 0..50 {
        if summary.lines > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        summary = core.log_summary().unwrap();
    }
    let group = summary
        .groups
        .iter()
        .find(|g| g.whom == Whom::Selected)
        .unwrap_or_else(|| panic!("a failure of the selected server is in the log: {summary:?}"));
    assert_eq!(
        (group.stage, group.cause),
        (Stage::Dial, Cause::Refused),
        "{summary:?}"
    );
    let kept = format!("{summary:?}");
    for word in ["rift-refused-test", "127.0.0.1", "credential"] {
        assert!(!kept.contains(word), "{word} kept: {kept}");
    }
    core.stop().await.unwrap();
}

/// The rules a provider sends with a server follow the selected server through the Clash API: the
/// same child, no restart. Server A brings "this address goes straight out"; B brings nothing, so
/// through B the same address goes to B, which refuses it.
#[tokio::test]
#[ignore = "requires bundled sing-box; run cargo test -p rift-core --test sidecar -- --ignored"]
async fn the_rules_of_a_server_follow_it_without_restarting_sing_box() {
    use rift_core::routing::ServerRoutes;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let binary = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../desktop/src-tauri/binaries/sing-box-x86_64-unknown-linux-gnu");
    if !binary.exists() {
        eprintln!("skipped: no sing-box binary for this machine");
        return;
    }
    let site = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let site_port = site.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = site.accept().await {
            tokio::spawn(async move {
                let mut request = [0u8; 2048];
                let _ = stream.read(&mut request).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\nconnection: close\r\n\r\nsite",
                    )
                    .await;
            });
        }
    });
    let mut a = parse_uri("trojan://credential@127.0.0.1:9#A", None).unwrap();
    a.routes = Some(ServerRoutes {
        block: vec![],
        direct: vec!["127.0.0.1/32".into()],
    });
    let b = parse_uri("trojan://credential@127.0.0.1:10#B", None).unwrap();
    let mut store = Store::default();
    store.settings.proxy_port = free_port();
    store.settings.api_port = free_port();
    store.servers = vec![a.clone(), b.clone()];
    store.selected = Some(a.id.clone());
    let secret = "test-secret-012345678901234567890123";
    let dir = tempfile::tempdir().unwrap();
    let mut core = Sidecar::new(binary, dir.path().into());
    core.start(&store, secret).await.unwrap();
    let pid = core.pid();
    assert!(pid.is_some());
    let probe = NetworkProbe::new(store.settings.clone(), secret.into()).unwrap();
    let through_sing_box = reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::http(format!("http://127.0.0.1:{}", store.settings.proxy_port))
                .unwrap(),
        )
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let url = format!("http://127.0.0.1:{site_port}/");
    let reaches = || async {
        match through_sing_box.get(&url).send().await {
            Ok(answer) if answer.status().is_success() => {
                Some(answer.text().await.unwrap_or_default())
            }
            _ => None,
        }
    };
    // Started on A: its rules are the ones in force from the first connection.
    assert_eq!(reaches().await.as_deref(), Some("site"));
    // To B: its selector, then its (no) rules.
    probe.select(&b.id).await.unwrap();
    probe.routes(&b.routes_mode()).await.unwrap();
    assert_eq!(reaches().await, None, "B has no rule for the address");
    // Back to A.
    probe.select(&a.id).await.unwrap();
    probe.routes(&a.routes_mode()).await.unwrap();
    assert_eq!(reaches().await.as_deref(), Some("site"));
    // A mode that is nowhere in the config is refused, not ignored.
    assert!(probe.routes("routes-000000000000").await.is_err());
    // A set whose entries name only lists the databases do not hold (yet) is no mode that is
    // missing: its mode is known to sing-box all the same.
    let mut only_lists = parse_uri("trojan://credential@127.0.0.1:11#C", None).unwrap();
    only_lists.routes = Some(ServerRoutes {
        block: vec!["geosite:category-ads-all".into()],
        direct: vec!["geoip:ru".into()],
    });
    let mut with_list = store.clone();
    with_list.servers.push(only_lists.clone());
    let mut second = Sidecar::new(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../desktop/src-tauri/binaries/sing-box-x86_64-unknown-linux-gnu"),
        dir.path().into(),
    );
    with_list.settings.proxy_port = free_port();
    with_list.settings.api_port = free_port();
    with_list.selected = Some(only_lists.id.clone());
    // It starts on the server whose set made no rule, in the mode of that set.
    second.start(&with_list, secret).await.unwrap();
    second.stop().await.unwrap();
    assert_eq!(core.pid(), pid, "one child throughout");
    core.stop().await.unwrap();
}
