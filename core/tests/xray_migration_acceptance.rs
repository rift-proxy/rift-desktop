//! Real production Runtime tests. Missing/unverified binaries are failures, never skips.
use rift_core::{
    dns::Chain,
    failure::FailureCode,
    model::Store,
    parser::parse_uri,
    probe::NetworkProbe,
    runtime::{Runtime, Sidecar},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    process::{Child, Command},
    time::{sleep, timeout},
};

const SECRET: &str = "synthetic-runtime-api-secret-01234567890123456789";
const ID: &str = "11111111-1111-4111-8111-111111111111";
fn binary(name: &str) -> PathBuf {
    let (triple, suffix) = if cfg!(windows) {
        ("x86_64-pc-windows-msvc", ".exe")
    } else {
        ("x86_64-unknown-linux-gnu", "")
    };
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../desktop/src-tauri/binaries/{name}-{triple}{suffix}"
    ));
    let bytes = std::fs::read(&path).expect("verified binaries required: node tooling/sidecar.mjs");
    let expected = match (name, cfg!(windows)) {
        ("xray", false) => "a27e3f95ab6ce270956d0cecd32eda4686a811afc71c64a6a734a3dede14883e",
        ("xray", true) => "c77a00eb84fb93ae044c2d771a1cabcc033b81e487253fbc35b12df6879c1e15",
        ("sing-box", false) => "9334a1c1fe97234911afaedd37226667ebdb3dab2abe383f2c4ae1387706dad9",
        ("sing-box", true) => "b838de45bd0b2e6ddbed1977e4745622f7dffab3b293807ff4c6b1b640fed909",
        _ => panic!("unknown core"),
    };
    assert_eq!(
        format!("{:x}", Sha256::digest(bytes)),
        expected,
        "unverified test binary"
    );
    path
}
fn free_port() -> u16 {
    // bind(0) then drop can hand the same ephemeral port to the next client connect before
    // sing-box binds its API. Stay below the OS ephemeral ranges and never reuse a fixture port.
    static NEXT: AtomicUsize = AtomicUsize::new(20_000);
    loop {
        let value = NEXT.fetch_add(1, Ordering::SeqCst);
        assert!(value < 30_000, "synthetic port allocator exhausted");
        let port = u16::try_from(value).unwrap();
        let address = (std::net::Ipv4Addr::LOCALHOST, port);
        if let (Ok(_tcp), Ok(_udp)) = (
            std::net::TcpListener::bind(address),
            std::net::UdpSocket::bind(address),
        ) {
            return port;
        }
    }
}

struct Remote {
    child: Child,
    port: u16,
}
impl Remote {
    async fn start() -> Self {
        Self::with_stream(None).await
    }
    async fn with_stream(stream: Option<Value>) -> Self {
        Self::with_protocol(
            "vless",
            json!({"clients":[{"id":ID}],"decryption":"none"}),
            stream,
        )
        .await
    }
    async fn with_protocol(protocol: &str, settings: Value, stream: Option<Value>) -> Self {
        Self::with_udp_redirect(protocol, settings, stream, None).await
    }
    async fn with_udp_redirect(
        protocol: &str,
        settings: Value,
        stream: Option<Value>,
        redirect: Option<u16>,
    ) -> Self {
        let port = free_port();
        let mut config = json!({"log":{"loglevel":"none"},"inbounds":[{
            "listen":"127.0.0.1","port":port,"protocol":protocol,
            "settings":settings}],
            // The synthetic remote is allowed to reach only this test's loopback target.
            // Xray 26.10.21 otherwise blocks private destinations by default.
            "outbounds":[{"protocol":"freedom","settings":{"finalRules":[{
                "action":"allow","ip":["127.0.0.0/8"]},{"action":"block"}]}}]});
        if let Some(stream) = stream {
            config["inbounds"][0]["streamSettings"] = stream;
        }
        if let Some(port) = redirect {
            // The client still encodes UDP destination 443. Only this synthetic remote maps
            // it to an unprivileged echo listener; host routing and privileged ports stay alone.
            config["outbounds"].as_array_mut().unwrap().push(json!({"protocol":"freedom","tag":"udp-echo","settings":{
                "redirect":format!("127.0.0.1:{port}"),"finalRules":[{"action":"allow","ip":["127.0.0.0/8"]},{"action":"block"}]}}));
            config["routing"] = json!({"rules":[{"type":"field","network":"udp","port":"443","outboundTag":"udp-echo"}]});
        }
        let mut child = Command::new(binary("xray"))
            .args(["run", "-c", "stdin:", "-format", "json"])
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&config).unwrap())
            .await
            .unwrap();
        timeout(Duration::from_secs(5), async {
            loop {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "synthetic remote exited"
                );
                if TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                    .await
                    .is_ok()
                {
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        Self { child, port }
    }
    async fn stop(&mut self) {
        self.child.kill().await.unwrap();
        self.child.wait().await.unwrap();
    }
}
struct Target {
    root_der: Vec<u8>,
    port: u16,
    hits: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Target {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Target {
    async fn start() -> Self {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec!["127.0.0.1".into(), "localhost".into()])
                .unwrap();
        let root_der = cert.der().to_vec();
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(signing_key.serialize_der().into()),
        )
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let seen = seen.clone();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let mut bytes = [0; 4096];
                    if timeout(Duration::from_secs(3), stream.read(&mut bytes))
                        .await
                        .is_ok_and(|r| r.is_ok_and(|n| n > 0))
                    {
                        seen.fetch_add(1, Ordering::SeqCst);
                        let _=stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\nsynthetic-xray-proof").await;
                    }
                });
            }
        });
        Self {
            port,
            hits,
            task,
            root_der,
        }
    }
    fn url(&self) -> String {
        format!("https://127.0.0.1:{}/synthetic", self.port)
    }
}
fn store(remotes: &[&Remote], target: &Target) -> Store {
    let mut s = Store::default();
    s.settings.api_port = free_port();
    loop {
        s.settings.proxy_port = free_port();
        if s.settings.proxy_port != s.settings.api_port {
            break;
        }
    }
    s.settings.connectivity_url = target.url();
    s.settings.proxy_timeout_ms = 1000;
    s.routing.direct.clear();
    for r in remotes {
        s.servers.push(
            parse_uri(
                &format!("vless://{ID}@127.0.0.1:{}?security=none&type=tcp", r.port),
                None,
            )
            .unwrap(),
        );
    }
    s.selected = s.servers.first().map(|s| s.id.clone());
    s
}
fn core(dir: &std::path::Path, _target: &Target) -> Sidecar {
    Sidecar::new(binary("sing-box"), dir.into())
        .with_xray_binary(binary("xray"))
        .with_chain(|| Chain::new(Vec::new()))
}
fn client(s: &Store, target: &Target) -> reqwest::Client {
    reqwest::Client::builder()
        .tls_certs_only([reqwest::Certificate::from_der(&target.root_der).unwrap()])
        .no_proxy()
        .proxy(reqwest::Proxy::all(format!("http://127.0.0.1:{}", s.settings.proxy_port)).unwrap())
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap()
}

fn legacy_fixture() -> Value {
    let bytes = include_bytes!("fixtures/xray-store-v1.json");
    assert_eq!(
        format!("{:x}", Sha256::digest(bytes)),
        "1bdae06b96c72c7c4a7914c8f48f860bf3d9634e3e23c79ddd23c522f2367067"
    );
    serde_json::from_slice(bytes).unwrap()
}

#[test]
fn store_v1_migration_preserves_links() {
    use rift_core::{
        connection::Connection,
        history::History,
        model::Server,
        persistence, pool, profile,
        selection::{Measurement, Progress},
    };
    let fixture = legacy_fixture();
    let rows = fixture["cases"].as_array().unwrap();
    assert_eq!(rows.len(), 114);
    for row in rows {
        let server: Server = serde_json::from_value(row["server"].clone()).unwrap();
        let canonical = Connection::from_legacy(&server.outbound)
            .unwrap()
            .legacy_value();
        assert_eq!(
            canonical, row["server"]["outbound"],
            "case {}",
            row["case_id"]
        );
        assert_eq!(Server::fingerprint(&canonical), server.id);
        let reread: Server =
            serde_json::from_value(serde_json::to_value(&server).unwrap()).unwrap();
        assert_eq!(reread.outbound, canonical);
        assert_eq!(reread.id, server.id);
    }
    let original = serde_json::to_vec(&fixture["state"]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, &original).unwrap();
    let migrated = persistence::load(&path).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "read must not rewrite v1"
    );
    assert_eq!(migrated.version, 2);
    assert_eq!(
        migrated.selected.as_deref(),
        fixture["state"]["selected"].as_str()
    );
    assert_eq!(json!(migrated.excluded), fixture["state"]["excluded"]);
    let mut history = History::default();
    for server in &migrated.servers {
        history.record(
            &Progress::Proxy(
                server.id.clone(),
                Measurement {
                    latency_ms: Some(7),
                    failure_code: None,
                },
            ),
            123,
        );
    }
    let pool = pool::build(
        &migrated
            .servers
            .iter()
            .map(|s| pool::Ranked {
                id: s.id.clone(),
                host: s.host.clone(),
                ms: 7,
            })
            .collect::<Vec<_>>(),
        3,
    );
    persistence::save(&path, &migrated).unwrap();
    let v2: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(v2["version"], 2);
    assert!(v2["servers"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s.get("outbound").is_none() && s["connection"]["format"] == "rift-connection"));
    let backups: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p != &path)
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
    let loaded = persistence::load(&path).unwrap();
    assert_eq!(
        serde_json::to_value(&loaded).unwrap(),
        serde_json::to_value(&migrated).unwrap()
    );
    history.begin_cycle(|id| loaded.servers.iter().any(|s| s.id == id));
    assert!(pool
        .iter()
        .all(|p| loaded.servers.iter().any(|s| s.id == p.id)));
    assert!(loaded.servers.iter().all(|s| history.of(&s.id).len() == 1));
    let imported = profile::decode(&serde_json::to_vec(&fixture["profile"]).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&imported).unwrap(),
        serde_json::to_value(&loaded).unwrap()
    );
    assert_eq!(
        serde_json::to_value(profile::decode(&profile::encode(&imported).unwrap()).unwrap())
            .unwrap(),
        serde_json::to_value(&imported).unwrap()
    );
    persistence::save(&path, &loaded).unwrap();
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        2,
        "v2 saves do not multiply v1 backups"
    );
}

#[tokio::test]
async fn retired_server_preserves_store() {
    use rift_core::{persistence, profile};
    let fixture = legacy_fixture();
    let original = serde_json::to_vec(&fixture["state"]).unwrap();
    let s = persistence::decode(&original).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, &original).unwrap();
    let target = Target::start().await;
    let mut runtime = core(dir.path(), &target);
    assert_eq!(
        runtime.start(&s, SECRET).await.unwrap_err().failure_code(),
        Some(FailureCode::UnsupportedLegacyParameters)
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(runtime.pid().is_none() && runtime.xray_pid().is_none());
    persistence::save(&path, &s).unwrap();
    let exported = profile::encode(&s).unwrap();
    let restored = profile::decode(&exported).unwrap();
    assert_eq!(
        serde_json::to_value(&restored).unwrap(),
        serde_json::to_value(&s).unwrap()
    );
    // A supported neighbour must still connect while both retired records remain in storage.
    let remote = Remote::start().await;
    let mut connected = store(&[&remote], &target);
    connected
        .servers
        .extend(restored.servers.iter().skip(1).cloned());
    let before = serde_json::to_value(&connected).unwrap();
    runtime.start(&connected, SECRET).await.unwrap();
    assert_eq!(
        client(&connected, &target)
            .get(target.url())
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "synthetic-xray-proof"
    );
    assert_eq!(serde_json::to_value(&connected).unwrap(), before);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn production_runtime_uses_xray() {
    let remote = Remote::start().await;
    let target = Target::start().await;
    let s = store(&[&remote], &target);
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = core(dir.path(), &target);
    runtime.start(&s, SECRET).await.unwrap();
    assert!(runtime.pid().is_some() && runtime.xray_pid().is_some());
    assert_ne!(runtime.pid(), runtime.xray_pid());
    let body = client(&s, &target)
        .get(target.url())
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!(
                "synthetic transfer: {error}; hits={}; summary={:?}",
                target.hits.load(Ordering::SeqCst),
                runtime.log_summary()
            )
        })
        .text()
        .await
        .unwrap();
    assert_eq!(body, "synthetic-xray-proof");
    assert_eq!(target.hits.load(Ordering::SeqCst), 1);
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_file() {
            assert!(
                !String::from_utf8_lossy(&std::fs::read(entry.path()).unwrap()).contains(SECRET)
            );
        }
    }
    runtime.stop().await.unwrap();
    assert!(!runtime.alive().unwrap());
    assert!(runtime.xray_pid().is_none());
}

#[tokio::test]
async fn candidate_probes_are_isolated() {
    let a = Remote::start().await;
    let mut b = Remote::start().await;
    let target = Target::start().await;
    let s = store(&[&a, &b], &target);
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = core(dir.path(), &target);
    runtime.start(&s, SECRET).await.unwrap();
    let probe = NetworkProbe::new(s.settings.clone(), SECRET.into()).unwrap();
    probe.ready().await.unwrap();
    delay(&s, &s.servers[0].id, &target, runtime.proxy_channel())
        .await
        .unwrap_or_else(|e| {
            panic!(
                "synthetic probe {e}, hits={}, log={:?}",
                target.hits.load(Ordering::SeqCst),
                runtime.log_summary()
            )
        });
    delay(&s, &s.servers[1].id, &target, runtime.proxy_channel())
        .await
        .unwrap();
    assert_eq!(target.hits.load(Ordering::SeqCst), 2);
    b.stop().await;
    assert!(
        delay(&s, &s.servers[1].id, &target, runtime.proxy_channel())
            .await
            .is_err()
    );
    delay(&s, &s.servers[0].id, &target, runtime.proxy_channel())
        .await
        .unwrap();
    assert_eq!(target.hits.load(Ordering::SeqCst), 3);
    let selected: Value = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!(
            "http://127.0.0.1:{}/proxies/main",
            s.settings.api_port
        ))
        .bearer_auth(SECRET)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(selected["now"], s.servers[0].id);
    runtime.stop().await.unwrap();
}

/// Real RFC1928 UDP through the product's mixed listener, not a TCP approximation of UDP.
struct UdpAssociation {
    _control: TcpStream,
    socket: UdpSocket,
    relay: std::net::SocketAddr,
}
impl UdpAssociation {
    async fn open(port: u16) -> Self {
        let mut control = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        control.write_all(&[5, 1, 0]).await.unwrap();
        let mut reply = [0; 2];
        control.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, [5, 0]);
        control
            .write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        let mut reply = [0; 10];
        control.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply[..4], &[5, 0, 0, 1]);
        let relay = std::net::SocketAddr::from((
            std::net::Ipv4Addr::new(reply[4], reply[5], reply[6], reply[7]),
            u16::from_be_bytes([reply[8], reply[9]]),
        ));
        Self {
            _control: control,
            socket: UdpSocket::bind("127.0.0.1:0").await.unwrap(),
            relay,
        }
    }
    async fn echo(&self, port: u16, nonce: u8) -> bool {
        let mut request = vec![0, 0, 0, 1, 127, 0, 0, 1];
        request.extend(port.to_be_bytes());
        request.extend([b'r', b'i', b'f', b't', nonce]);
        self.socket.send_to(&request, self.relay).await.unwrap();
        let mut reply = [0; 128];
        match timeout(Duration::from_secs(1), self.socket.recv_from(&mut reply)).await {
            Ok(Ok((size, _))) => size == request.len() && reply[..size] == request,
            _ => false,
        }
    }
}

#[tokio::test]
async fn udp_candidates_are_isolated() {
    use rift_core::selection::Probe;
    let a = Remote::start().await;
    let mut b = Remote::start().await;
    let target = Target::start().await;
    let s = store(&[&a, &b], &target);
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = socket.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = hits.clone();
    let echo = tokio::spawn(async move {
        let mut bytes = [0; 128];
        while let Ok((size, sender)) = socket.recv_from(&mut bytes).await {
            counted.fetch_add(1, Ordering::SeqCst);
            socket.send_to(&bytes[..size], sender).await.unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = core(dir.path(), &target);
    runtime.start(&s, SECRET).await.unwrap();
    let selector = NetworkProbe::new(s.settings.clone(), SECRET.into()).unwrap();
    let udp_a = UdpAssociation::open(s.settings.proxy_port).await;
    assert!(
        udp_a.echo(port, 1).await,
        "UDP must cross sing-box and Xray candidate A"
    );
    selector.select(&s.servers[1].id).await.unwrap();
    let udp_b = UdpAssociation::open(s.settings.proxy_port).await;
    assert!(udp_b.echo(port, 2).await, "UDP must cross candidate B");
    b.stop().await;
    // A new UDP association prevents a previous flow/cache from hiding candidate changes.
    let failed_b = UdpAssociation::open(s.settings.proxy_port).await;
    assert!(
        !failed_b.echo(port, 3).await,
        "B cannot fall back to A/direct"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    selector.select(&s.servers[0].id).await.unwrap();
    let working_a = UdpAssociation::open(s.settings.proxy_port).await;
    assert!(
        working_a.echo(port, 4).await,
        "A must remain independent of failed B"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    runtime.stop().await.unwrap();
    assert!(
        !working_a.echo(port, 5).await,
        "stopped cores cannot forward UDP directly"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    echo.abort();
}

struct SyntheticDns;
static DNS_QUESTIONS: AtomicUsize = AtomicUsize::new(0);
#[async_trait::async_trait]
impl rift_core::dns::Upstream for SyntheticDns {
    fn name(&self) -> &'static str {
        "test"
    }
    async fn ask(&self, query: &[u8]) -> Result<Vec<u8>, rift_core::dns::Miss> {
        let question =
            rift_core::dns::parse_question(query).ok_or(rift_core::dns::Miss::Malformed)?;
        assert_eq!(question.name, "only-in-rift.invalid");
        DNS_QUESTIONS.fetch_add(1, Ordering::SeqCst);
        let addresses: &[std::net::IpAddr] = if question.qtype == 1 {
            &[std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)]
        } else {
            &[]
        };
        rift_core::dns::answer_addresses(query, addresses).ok_or(rift_core::dns::Miss::Malformed)
    }
}
fn synthetic_dns() -> Chain {
    Chain::new(vec![Box::new(SyntheticDns)])
}

#[tokio::test]
async fn dns_bridge_preserves_hostnames() {
    let remote = Remote::start().await;
    let target = Target::start().await;
    let mut s = store(&[&remote], &target);
    s.servers = vec![parse_uri(
        &format!(
            "vless://{ID}@only-in-rift.invalid:{}?security=none",
            remote.port
        ),
        None,
    )
    .unwrap()];
    s.selected = Some(s.servers[0].id.clone());
    let original = serde_json::to_value(&s).unwrap();
    let plan = rift_core::launch::LaunchPlan::build(
        &s,
        SECRET,
        &rift_core::geodata::GeoSet::none(),
        rift_core::config::Resolver::Bridge(12344),
        12345,
        SECRET,
    )
    .unwrap();
    assert_eq!(
        plan.xray()["outbounds"][1]["settings"]["vnext"][0]["address"],
        "only-in-rift.invalid"
    );
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = Sidecar::new(binary("sing-box"), dir.path().into())
        .with_xray_binary(binary("xray"))
        .with_chain(synthetic_dns);
    runtime.start(&s, SECRET).await.unwrap();
    assert_eq!(
        client(&s, &target)
            .get(target.url())
            .send()
            .await
            .unwrap_or_else(|e| panic!(
                "DNS transfer: {e}; bridge={:?}; questions={}",
                runtime.dns_stats(),
                DNS_QUESTIONS.load(Ordering::SeqCst)
            ))
            .text()
            .await
            .unwrap(),
        "synthetic-xray-proof"
    );
    assert!(DNS_QUESTIONS.load(Ordering::SeqCst) >= 1);
    assert!(runtime.dns_stats().iter().any(|stats| stats.answered > 0));
    assert_eq!(target.hits.load(Ordering::SeqCst), 1);
    assert_eq!(serde_json::to_value(&s).unwrap(), original);
    runtime.stop().await.unwrap();
    assert!(runtime.dns_stats().is_empty());
}

#[tokio::test]
async fn xray_crash_has_no_fallback() {
    let remote = Remote::start().await;
    let target = Target::start().await;
    let s = store(&[&remote], &target);
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = core(dir.path(), &target);
    runtime.start(&s, SECRET).await.unwrap();
    assert_eq!(
        client(&s, &target)
            .get(target.url())
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "synthetic-xray-proof"
    );
    let pid = runtime.xray_pid().unwrap();
    assert!(runtime.alive().unwrap());
    kill_owned(pid).await;
    timeout(Duration::from_secs(3), async {
        while runtime.alive().unwrap() {
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(runtime.crash_code(), FailureCode::XrayCrashed);
    assert!(client(&s, &target).get(target.url()).send().await.is_err());
    assert_eq!(target.hits.load(Ordering::SeqCst), 1);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn sing_box_crash_stops_the_owned_xray_and_closes_its_capability() {
    let remote = Remote::start().await;
    let target = Target::start().await;
    let s = store(&[&remote], &target);
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = core(dir.path(), &target);
    runtime.start(&s, SECRET).await.unwrap();
    let channel = runtime.proxy_channel().unwrap();
    let gateway = channel.read().unwrap().as_ref().unwrap().port();
    kill_owned(runtime.pid().unwrap()).await;
    timeout(Duration::from_secs(3), async {
        loop {
            if !runtime.alive().unwrap()
                && TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, gateway))
                    .await
                    .is_err()
            {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(runtime.crash_code(), FailureCode::SidecarCrashed);
    assert!(channel.read().unwrap().is_none());
    assert_eq!(target.hits.load(Ordering::SeqCst), 0);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn rejected_replacement_keeps_both_working_children_and_data() {
    let remote = Remote::start().await;
    let target = Target::start().await;
    let s = store(&[&remote], &target);
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = core(dir.path(), &target);
    runtime.start(&s, SECRET).await.unwrap();
    let pids = (runtime.pid(), runtime.xray_pid());
    let mut retired = s.clone();
    retired.servers =
        vec![parse_uri("ss://aes-128-cfb:synthetic@legacy.example:443", None).unwrap()];
    retired.selected = Some(retired.servers[0].id.clone());
    let original = serde_json::to_value(&retired).unwrap();
    assert_eq!(
        runtime
            .start(&retired, SECRET)
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::UnsupportedLegacyParameters)
    );
    assert_eq!((runtime.pid(), runtime.xray_pid()), pids);
    assert_eq!(serde_json::to_value(&retired).unwrap(), original);
    assert!(runtime.alive().unwrap());
    assert_eq!(
        client(&s, &target)
            .get(target.url())
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "synthetic-xray-proof"
    );
    assert_eq!(target.hits.load(Ordering::SeqCst), 1);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn failed_sing_box_start_cleans_the_new_xray_and_keeps_foreign_listener() {
    let remote = Remote::start().await;
    let target = Target::start().await;
    let mut s = store(&[&remote], &target);
    let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
    s.settings.proxy_port = occupied.local_addr().unwrap().port();
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = core(dir.path(), &target);
    assert_eq!(
        runtime.start(&s, SECRET).await.unwrap_err().failure_code(),
        Some(FailureCode::LocalPortInUse)
    );
    let listener = runtime.diagnostics().listener.unwrap();
    assert_eq!(listener.port, s.settings.proxy_port);
    assert_eq!(
        listener.purpose,
        rift_core::sidecar_log::ListenerPurpose::Proxy
    );
    assert!(runtime.pid().is_none() && runtime.xray_pid().is_none());
    assert!(runtime.proxy_channel().unwrap().read().unwrap().is_none());
    assert!(runtime.dns_stats().is_empty());
    let client = TcpStream::connect(occupied.local_addr().unwrap())
        .await
        .unwrap();
    let accepted = timeout(Duration::from_secs(1), occupied.accept())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(client.local_addr().unwrap(), accepted.1);
}

fn tls_server_stream(network: &str, path: &str) -> (Value, String) {
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let pem = cert.pem();
    (
        json!({"network":network,"security":"tls","tlsSettings":{"alpn":["h2","http/1.1"],
        "certificates":[{"certificate":pem.lines().collect::<Vec<_>>(),"key":signing_key.serialize_pem().lines().collect::<Vec<_>>()}]},
        "xhttpSettings":{"path":path,"mode":"auto"}}),
        pem,
    )
}
async fn reality_keypair() -> (String, String) {
    let output = Command::new(binary("xray"))
        .arg("x25519")
        .output()
        .await
        .unwrap();
    assert!(output.status.success() && output.stdout.len() < 1024);
    let text = std::str::from_utf8(&output.stdout).unwrap();
    let field = |prefix: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(prefix))
            .unwrap()
            .trim()
            .to_owned()
    };
    (field("PrivateKey:"), field("Password (PublicKey):"))
}
fn fragment() -> String {
    json!({"tcp":[{"type":"fragment","settings":{"packets":"tlshello","length":"10-20","delay":"1-2"}}]}).to_string()
}

async fn hello_records(listener: TcpListener) -> Vec<usize> {
    timeout(Duration::from_secs(6), async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut hello = Vec::new();
        let mut records = Vec::new();
        loop {
            let mut header = [0; 5];
            stream.read_exact(&mut header).await.unwrap();
            assert_eq!(header[0], 22, "only actual TLS handshake records count");
            let size = u16::from_be_bytes([header[3], header[4]]) as usize;
            assert!(size > 0);
            let mut data = vec![0; size];
            stream.read_exact(&mut data).await.unwrap();
            records.push(size);
            hello.extend(data);
            if hello.len() >= 4 {
                assert_eq!(hello[0], 1, "ClientHello");
                let size =
                    ((hello[1] as usize) << 16) | ((hello[2] as usize) << 8) | hello[3] as usize;
                if hello.len() >= size + 4 {
                    return records;
                }
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn finalmask_fragment_changes_actual_tls_and_reality_clienthello_records() {
    let (_, key) = reality_keypair().await;
    let target = Target::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = core(dir.path(), &target);
    for security in ["tls", "reality"] {
        for enabled in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let mut params = vec![("sni", "localhost".into()), ("fp", "chrome".into())];
            if security == "reality" {
                params.extend([("pbk", key.clone()), ("sid", "ab12".into())]);
            }
            if enabled {
                params.push(("fm", fragment()));
            }
            let s = protocol_store_at(port, &target, "tcp", security, &params);
            runtime.start(&s, SECRET).await.unwrap();
            let c = client(&s, &target);
            let url = target.url();
            let (records, response) = tokio::join!(hello_records(listener), c.get(url).send());
            assert!(
                response.is_err(),
                "capture peer intentionally ends after ClientHello"
            );
            if enabled {
                assert!(
                    records.len() > 5,
                    "{security}: fragment must affect real bytes"
                );
                assert!(
                    records.iter().all(|size| *size <= 20),
                    "each fragment obeys length=10-20"
                );
            } else {
                assert_eq!(records.len(), 1, "negative control: no mask, one record");
                assert!(records[0] > 100);
            }
            runtime.stop().await.unwrap();
        }
    }
}

#[tokio::test]
async fn official_fragment_maxsplit_and_arrays_reach_actual_tls_and_reality_records() {
    let (_, key) = reality_keypair().await;
    let target = Target::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = core(dir.path(), &target);
    for security in ["tls", "reality"] {
        for arrays in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let settings = if arrays {
                json!({"packets":"tlshello","lengths":[13,29],"delays":[0]})
            } else {
                json!({"packets":"tlshello","length":17,"delay":0,"maxSplit":3})
            };
            let mask = json!({"tcp":[{"type":"fragment","settings":settings}]}).to_string();
            let mut params = vec![
                ("sni", "localhost".into()),
                ("fp", "chrome".into()),
                ("fm", mask),
            ];
            if security == "reality" {
                params.extend([("pbk", key.clone()), ("sid", "ab12".into())]);
            }
            if arrays {
                params.push(("extra", json!({"headers":{}}).to_string()));
            }
            let s = protocol_store_at(
                port,
                &target,
                if arrays { "xhttp" } else { "tcp" },
                security,
                &params,
            );
            runtime.start(&s, SECRET).await.unwrap();
            let c = client(&s, &target);
            let (records, response) =
                tokio::join!(hello_records(listener), c.get(target.url()).send());
            assert!(response.is_err(), "capture peer ends after ClientHello");
            if arrays {
                assert!(records.len() > 5);
                assert_eq!(records[0], 13);
                assert!(records[1..records.len() - 1].iter().all(|size| *size == 29));
                assert!(*records.last().unwrap() <= 29);
            } else {
                assert_eq!(records.len(), 3, "maxSplit must reach the executing core");
                assert_eq!(&records[..2], &[17, 17]);
                assert!(records[2] > 17, "last allowed split carries the remainder");
            }
            runtime.stop().await.unwrap();
        }
    }
}

#[tokio::test]
async fn reality_with_finalmask_transfers_through_the_owned_xray() {
    let (private, key) = reality_keypair().await;
    let front = Target::start().await;
    let target = Target::start().await;
    let remote=Remote::with_stream(Some(json!({"network":"tcp","security":"reality",
        "realitySettings":{"target":format!("127.0.0.1:{}",front.port),"serverNames":["localhost"],"privateKey":private,"shortIds":["ab12"]}}))).await;
    let s = protocol_store(
        &remote,
        &target,
        "tcp",
        "reality",
        &[
            ("sni", "localhost".into()),
            ("fp", "chrome".into()),
            ("pbk", key),
            ("sid", "ab12".into()),
            ("fm", fragment()),
        ],
    );
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = core(dir.path(), &target);
    runtime.start(&s, SECRET).await.unwrap();
    assert_eq!(
        client(&s, &target)
            .get(target.url())
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "synthetic-xray-proof"
    );
    assert_eq!(target.hits.load(Ordering::SeqCst), 1);
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn vision_preserves_tcp_and_udp443_for_tls_and_reality() {
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let echo_port = udp.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let seen = hits.clone();
    let echo = tokio::spawn(async move {
        let mut bytes = [0; 128];
        while let Ok((n, peer)) = udp.recv_from(&mut bytes).await {
            seen.fetch_add(1, Ordering::SeqCst);
            udp.send_to(&bytes[..n], peer).await.unwrap();
        }
    });
    let target = Target::start().await;
    let front = Target::start().await;
    let (private, key) = reality_keypair().await;
    for security in ["tls", "reality"] {
        let (stream, root) = if security == "tls" {
            let (mut stream, pem) = tls_server_stream("tcp", "/");
            stream.as_object_mut().unwrap().remove("xhttpSettings");
            (stream, Some(pem))
        } else {
            (
                json!({"network":"tcp","security":"reality","realitySettings":{"target":format!("127.0.0.1:{}",front.port),"serverNames":["localhost"],"privateKey":private,"shortIds":["ab12"]}}),
                None,
            )
        };
        let remote = Remote::with_udp_redirect(
            "vless",
            json!({"clients":[{"id":ID,"flow":"xtls-rprx-vision"}],"decryption":"none"}),
            Some(stream),
            Some(echo_port),
        )
        .await;
        let mut params = vec![
            ("flow", "xtls-rprx-vision".into()),
            ("sni", "localhost".into()),
        ];
        if security == "reality" {
            params.extend([("pbk", key.clone()), ("sid", "ab12".into())]);
        }
        let s = protocol_store(&remote, &target, "tcp", security, &params);
        let dir = tempfile::tempdir().unwrap();
        let mut runtime = core(dir.path(), &target);
        if let Some(root) = root {
            runtime = runtime.with_test_xray_ca(root).unwrap();
        }
        runtime.start(&s, SECRET).await.unwrap();
        assert_eq!(
            client(&s, &target)
                .get(target.url())
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "synthetic-xray-proof"
        );
        assert!(
            UdpAssociation::open(s.settings.proxy_port)
                .await
                .echo(443, 8)
                .await,
            "Vision must keep UDP443"
        );
        runtime.stop().await.unwrap();
    }
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    echo.abort();
}
fn protocol_store(
    remote: &Remote,
    target: &Target,
    network: &str,
    security: &str,
    params: &[(&str, String)],
) -> Store {
    protocol_store_at(remote.port, target, network, security, params)
}
fn protocol_store_at(
    port: u16,
    target: &Target,
    network: &str,
    security: &str,
    params: &[(&str, String)],
) -> Store {
    let mut s = store(&[], target);
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("type", network)
        .append_pair("security", security)
        .extend_pairs(params.iter().map(|(k, v)| (*k, v.as_str())))
        .finish();
    s.servers = vec![parse_uri(&format!("vless://{ID}@127.0.0.1:{}?{query}", port), None).unwrap()];
    s.selected = Some(s.servers[0].id.clone());
    s
}

#[tokio::test]
async fn every_xhttp_mode_transfers_through_production_runtime_with_verified_tls() {
    let (mut stream, root) = tls_server_stream("xhttp", "/synthetic");
    stream["xhttpSettings"]["xPaddingBytes"] = json!("17-31");
    let remote = Remote::with_stream(Some(stream)).await;
    let target = Target::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = core(dir.path(), &target).with_test_xray_ca(root).unwrap();
    for (index, mode) in ["auto", "packet-up", "stream-up", "stream-one"]
        .iter()
        .enumerate()
    {
        let s = protocol_store(
            &remote,
            &target,
            "xhttp",
            "tls",
            &[
                ("mode", (*mode).into()),
                ("path", "/synthetic".into()),
                ("sni", "localhost".into()),
                (
                    "extra",
                    json!({"xPaddingBytes":"17-31","xmux":{"maxConnections":1}}).to_string(),
                ),
            ],
        );
        runtime.start(&s, SECRET).await.unwrap();
        assert_eq!(
            client(&s, &target)
                .get(target.url())
                .send()
                .await
                .unwrap_or_else(|e| panic!("XHTTP {mode}: {e}"))
                .text()
                .await
                .unwrap(),
            "synthetic-xray-proof"
        );
        assert_eq!(target.hits.load(Ordering::SeqCst), index + 1);
    }
    // Wrong path is a protocol failure; the earlier successful HTTP target cannot hide it.
    let wrong = protocol_store(
        &remote,
        &target,
        "xhttp",
        "tls",
        &[
            ("mode", "stream-one".into()),
            ("path", "/wrong".into()),
            ("sni", "localhost".into()),
            (
                "extra",
                json!({"xPaddingBytes":"17-31","xmux":{"maxConnections":1}}).to_string(),
            ),
        ],
    );
    runtime.start(&wrong, SECRET).await.unwrap();
    assert!(client(&wrong, &target)
        .get(target.url())
        .send()
        .await
        .is_err());
    assert_eq!(target.hits.load(Ordering::SeqCst), 4);
    let bad_padding = protocol_store(
        &remote,
        &target,
        "xhttp",
        "tls",
        &[
            ("path", "/synthetic".into()),
            ("sni", "localhost".into()),
            (
                "extra",
                json!({"xPaddingBytes":"50-60","xmux":{"maxConnections":1}}).to_string(),
            ),
        ],
    );
    runtime.start(&bad_padding, SECRET).await.unwrap();
    assert!(client(&bad_padding, &target)
        .get(target.url())
        .send()
        .await
        .is_err());
    assert_eq!(
        target.hits.load(Ordering::SeqCst),
        4,
        "padding range must actually affect the wire"
    );
    runtime.stop().await.unwrap();
    let (_, wrong_root) = tls_server_stream("xhttp", "/synthetic");
    let mut untrusted = core(dir.path(), &target)
        .with_test_xray_ca(wrong_root)
        .unwrap();
    let s = protocol_store(
        &remote,
        &target,
        "xhttp",
        "tls",
        &[
            ("path", "/synthetic".into()),
            ("sni", "localhost".into()),
            (
                "extra",
                json!({"xPaddingBytes":"17-31","xmux":{"maxConnections":1}}).to_string(),
            ),
        ],
    );
    untrusted.start(&s, SECRET).await.unwrap();
    assert!(
        client(&s, &target).get(target.url()).send().await.is_err(),
        "untrusted proxy certificate must fail"
    );
    assert_eq!(target.hits.load(Ordering::SeqCst), 4);
    untrusted.stop().await.unwrap();
}

async fn tcp_udp_transfer(
    runtime: &mut Sidecar,
    s: &Store,
    target: &Target,
    udp_port: u16,
    nonce: u8,
) {
    runtime.start(s, SECRET).await.unwrap();
    assert_eq!(
        client(s, target)
            .get(target.url())
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "synthetic-xray-proof"
    );
    let association = UdpAssociation::open(s.settings.proxy_port).await;
    assert!(
        association.echo(udp_port, nonce).await,
        "protocol must carry actual UDP"
    );
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn every_remaining_cipher_and_proxy_family_transfers_tcp_and_udp() {
    use base64::{
        engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
        Engine,
    };
    let target = Target::start().await;
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let udp_port = udp.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = hits.clone();
    let echo = tokio::spawn(async move {
        let mut data = [0; 128];
        while let Ok((n, peer)) = udp.recv_from(&mut data).await {
            counted.fetch_add(1, Ordering::SeqCst);
            udp.send_to(&data[..n], peer).await.unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = core(dir.path(), &target);
    let mut cases = 0;
    for method in rift_core::connection::XRAY_SS_METHODS {
        let password = if method.starts_with("2022-") {
            STANDARD.encode(vec![
                7;
                if method == "2022-blake3-aes-128-gcm" {
                    16
                } else {
                    32
                }
            ])
        } else {
            "synthetic-password".into()
        };
        let remote = Remote::with_protocol(
            "shadowsocks",
            json!({"method":method,"password":password,"network":"tcp,udp"}),
            None,
        )
        .await;
        let mut s = store(&[&remote], &target);
        s.servers[0] = parse_uri(
            &format!("ss://{method}:{password}@127.0.0.1:{}", remote.port),
            None,
        )
        .unwrap();
        s.selected = Some(s.servers[0].id.clone());
        cases += 1;
        tcp_udp_transfer(&mut runtime, &s, &target, udp_port, cases).await;
    }
    for security in rift_core::connection::XRAY_VMESS_SECURITIES {
        let remote = Remote::with_protocol("vmess", json!({"clients":[{"id":ID}]}), None).await;
        let mut s = store(&[&remote], &target);
        let uri=format!("vmess://{}",URL_SAFE_NO_PAD.encode(json!({"v":"2","add":"127.0.0.1","port":remote.port,"id":ID,"aid":0,"scy":security,"net":"tcp"}).to_string()));
        s.servers[0] = parse_uri(&uri, None).unwrap();
        s.selected = Some(s.servers[0].id.clone());
        cases += 1;
        tcp_udp_transfer(&mut runtime, &s, &target, udp_port, cases).await;
    }
    let (mut stream, root) = tls_server_stream("tcp", "/");
    stream.as_object_mut().unwrap().remove("xhttpSettings");
    let remote = Remote::with_protocol(
        "trojan",
        json!({"clients":[{"password":"synthetic-password"}]}),
        Some(stream),
    )
    .await;
    let mut runtime = runtime.with_test_xray_ca(root).unwrap();
    let mut s = store(&[&remote], &target);
    s.servers[0] = parse_uri(
        &format!(
            "trojan://synthetic-password@127.0.0.1:{}?sni=localhost",
            remote.port
        ),
        None,
    )
    .unwrap();
    s.selected = Some(s.servers[0].id.clone());
    cases += 1;
    tcp_udp_transfer(&mut runtime, &s, &target, udp_port, cases).await;
    assert_eq!(cases, 11);
    assert_eq!(target.hits.load(Ordering::SeqCst), 11);
    assert_eq!(hits.load(Ordering::SeqCst), 11);
    echo.abort();
}

// Same Rust probe as production; trust is local to this HTTPS fixture, never OS/Go globals.
async fn delay(
    s: &Store,
    id: &str,
    target: &Target,
    channel: Option<rift_core::proxy_channel::ProxyChannel>,
) -> rift_core::Result<u64> {
    use rift_core::selection::Probe;
    NetworkProbe::new(s.settings.clone(), SECRET.into())?
        .with_channel(channel)
        .with_test_root(reqwest::Certificate::from_der(&target.root_der).unwrap())
        .proxy(s.servers.iter().find(|server| server.id == id).unwrap())
        .await
}

async fn kill_owned(pid: u32) {
    // Only a PID returned by this test's own Runtime, never a process name.
    let status = if cfg!(windows) {
        Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
    } else {
        Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .await
    };
    assert!(status.unwrap().success());
}

#[tokio::test]
async fn handoff_preserves_live_connection_labels_bytes_lifetime_and_close() {
    use rift_core::clash::{self, Clash};
    let remote = Remote::start().await;
    let target = Target::start().await;
    let mut s = store(&[&remote], &target);
    s.servers[0].name = "synthetic-handoff-label".into();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let destination = listener.local_addr().unwrap().port();
    let echo = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (mut read, mut write) = stream.into_split();
        tokio::io::copy(&mut read, &mut write).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = core(dir.path(), &target);
    runtime.start(&s, SECRET).await.unwrap();
    let api = Clash::new(s.settings.api_port, SECRET).unwrap();
    let cancel = tokio_util::sync::CancellationToken::new();
    let observed = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let traffic = {
        let (api, cancel, observed) = (api.clone(), cancel.clone(), observed.clone());
        tokio::spawn(async move {
            api.traffic(&cancel, |speed| {
                observed.fetch_max(speed.up.min(speed.down), Ordering::SeqCst);
            })
            .await
        })
    };
    let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, s.settings.proxy_port))
        .await
        .unwrap();
    stream.write_all(&[5, 1, 0]).await.unwrap();
    let mut auth = [0; 2];
    stream.read_exact(&mut auth).await.unwrap();
    assert_eq!(auth, [5, 0]);
    let mut connect = vec![5, 1, 0, 1, 127, 0, 0, 1];
    connect.extend(destination.to_be_bytes());
    stream.write_all(&connect).await.unwrap();
    let mut reply = [0; 10];
    stream.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply[..4], &[5, 0, 0, 1]);
    let payload = vec![b'q'; 65536];
    let mut received = vec![0; payload.len()];
    for _ in 0..2 {
        stream.write_all(&payload).await.unwrap();
        timeout(Duration::from_secs(3), stream.read_exact(&mut received))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received, payload);
        sleep(Duration::from_millis(1100)).await;
    }
    let names = vec![(s.servers[0].id.clone(), s.servers[0].name.clone())];
    let view = clash::view(
        api.connections().await.unwrap(),
        &names,
        std::time::SystemTime::now(),
    );
    let connection = view
        .items
        .iter()
        .find(|row| row.port == destination)
        .expect("actual handoff connection visible");
    assert_eq!(connection.route, "proxy");
    assert_eq!(
        connection.server.as_deref(),
        Some("synthetic-handoff-label")
    );
    assert_eq!(connection.network, "tcp");
    assert!(connection.upload >= 131072 && connection.download >= 131072);
    assert!(connection.seconds.is_some_and(|seconds| seconds >= 1));
    assert!(
        observed.load(Ordering::SeqCst) > 0,
        "traffic stream must observe real payload bytes"
    );
    drop(stream);
    timeout(Duration::from_secs(3), echo)
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(3), async {
        loop {
            let view = clash::view(
                api.connections().await.unwrap(),
                &names,
                std::time::SystemTime::now(),
            );
            if !view.items.iter().any(|row| row.port == destination) {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    timeout(Duration::from_secs(1), traffic)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn real_xray_tls_failure_is_reduced_and_new_run_resets_the_log() {
    use rift_core::sidecar_log::Cause;
    let (stream, _) = tls_server_stream("tcp", "/");
    let remote = Remote::with_stream(Some(stream)).await;
    let target = Target::start().await;
    let mut s = store(&[&remote], &target);
    s.servers[0] = parse_uri(
        &format!(
            "vless://{ID}@127.0.0.1:{}?security=tls&sni=localhost&type=tcp",
            remote.port
        ),
        None,
    )
    .unwrap();
    s.selected = Some(s.servers[0].id.clone());
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = core(dir.path(), &target);
    runtime.start(&s, SECRET).await.unwrap();
    assert!(
        client(&s, &target).get(target.url()).send().await.is_err(),
        "untrusted synthetic remote certificate must fail"
    );
    timeout(Duration::from_secs(3), async {
        loop {
            if runtime
                .xray_log_summary()
                .unwrap()
                .groups
                .iter()
                .any(|group| group.cause == Cause::Tls)
            {
                break;
            }
            sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("real Xray warning must reach bounded classifier");
    let kept = format!(
        "{:?} {:?}",
        runtime.xray_log_summary(),
        runtime.log_notes(1)
    );
    for value in ["127.0.0.1", ID, SECRET, "localhost", "certificate has"] {
        assert!(!kept.contains(value));
    }
    runtime.stop().await.unwrap();
    runtime.start(&s, SECRET).await.unwrap();
    assert!(
        !runtime
            .xray_log_summary()
            .unwrap()
            .groups
            .iter()
            .any(|group| group.cause == Cause::Tls),
        "prior run TLS failures must not enter a new collector"
    );
    runtime.stop().await.unwrap();
}

#[tokio::test]
async fn cancelled_probe_refresh_stop_and_reconnect_keep_one_owned_runtime() {
    use rift_core::selection::Probe;
    let remote = Remote::start().await;
    let target = Target::start().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let entered = Arc::new(tokio::sync::Notify::new());
    let blocked = {
        let entered = entered.clone();
        tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            entered.notify_one();
            std::future::pending::<()>().await;
        })
    };
    let mut initial = store(&[&remote], &target);
    initial.servers = vec![parse_uri(
        &format!("vless://{ID}@127.0.0.1:{port}?security=none&type=tcp"),
        None,
    )
    .unwrap()];
    initial.selected = Some(initial.servers[0].id.clone());
    let mut refreshed = store(&[&remote], &target);
    refreshed.settings.api_port = initial.settings.api_port;
    refreshed.settings.proxy_port = initial.settings.proxy_port;
    let dir = tempfile::tempdir().unwrap();
    let mut runtime = core(dir.path(), &target);
    runtime.start(&initial, SECRET).await.unwrap();
    let old_pids = (runtime.pid(), runtime.xray_pid());
    let channel = runtime.proxy_channel();
    let probe = NetworkProbe::new(initial.settings.clone(), SECRET.into())
        .unwrap()
        .with_channel(channel.clone())
        .with_test_root(reqwest::Certificate::from_der(&target.root_der).unwrap());
    let server = initial.servers[0].clone();
    let request = tokio::spawn(async move { probe.proxy(&server).await });
    timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    assert!(runtime.alive().unwrap());
    assert_eq!((runtime.pid(), runtime.xray_pid()), old_pids);
    runtime.start(&refreshed, SECRET).await.unwrap();
    assert_ne!(runtime.pid(), old_pids.0);
    assert_ne!(runtime.xray_pid(), old_pids.1);
    delay(
        &refreshed,
        &refreshed.servers[0].id,
        &target,
        channel.clone(),
    )
    .await
    .unwrap();
    assert!(
        delay(&initial, &initial.servers[0].id, &target, channel.clone())
            .await
            .is_err(),
        "removed candidate cannot use a refreshed gateway"
    );
    runtime.stop().await.unwrap();
    runtime.stop().await.unwrap();
    assert!(channel.as_ref().unwrap().read().unwrap().is_none());
    assert!(runtime.pid().is_none() && runtime.xray_pid().is_none());
    runtime.start(&refreshed, SECRET).await.unwrap();
    assert_eq!(
        client(&refreshed, &target)
            .get(target.url())
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "synthetic-xray-proof"
    );
    runtime.stop().await.unwrap();
    blocked.abort();
}

#[tokio::test]
async fn protocol_transport_security_matrix_transfers_tcp_and_udp() {
    let target = Target::start().await;
    let front = Target::start().await;
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let udp_port = udp.local_addr().unwrap().port();
    let echo = tokio::spawn(async move {
        let mut bytes = [0; 128];
        while let Ok((n, peer)) = udp.recv_from(&mut bytes).await {
            udp.send_to(&bytes[..n], peer).await.unwrap();
        }
    });
    let (private, key) = reality_keypair().await;
    let mut count = 0u8;
    for protocol in ["vless", "vmess", "trojan"] {
        for network in ["tcp", "ws", "grpc", "xhttp"] {
            for security in ["none", "tls", "reality"] {
                if security == "reality" && (network == "ws" || protocol != "vless") {
                    continue;
                } // Existing Rift policy: REALITY only VLESS; upstream rejects REALITY+WS.
                let (mut stream, root) = match security {
                    "tls" => tls_server_stream(network, "/synthetic"),
                    "reality" => (
                        json!({"network":network,"security":"reality","realitySettings":{"target":format!("127.0.0.1:{}", front.port),"serverNames":["localhost"],"privateKey":private,"shortIds":["ab12"]}}),
                        String::new(),
                    ),
                    _ => (json!({"network":network,"security":"none"}), String::new()),
                };
                stream.as_object_mut().unwrap().remove("xhttpSettings");
                match network {
                    "ws" => stream["wsSettings"] = json!({"path":"/synthetic"}),
                    "grpc" => stream["grpcSettings"] = json!({"serviceName":"synthetic"}),
                    "xhttp" => stream["xhttpSettings"] = json!({"path":"/synthetic","mode":"auto"}),
                    _ => {}
                }
                let settings = match protocol {
                    "vless" => json!({"clients":[{"id":ID}],"decryption":"none"}),
                    "vmess" => json!({"clients":[{"id":ID}]}),
                    _ => json!({"clients":[{"password":"synthetic-password"}]}),
                };
                let remote = Remote::with_protocol(protocol, settings, Some(stream)).await;
                let mut params = Vec::new();
                if security != "none" {
                    params.push(("sni", "localhost".into()));
                }
                if network == "ws" || network == "xhttp" {
                    params.push(("path", "/synthetic".into()));
                }
                if network == "grpc" {
                    params.push(("serviceName", "synthetic".into()));
                }
                if security == "reality" {
                    params.extend([
                        ("fp", "chrome".into()),
                        ("pbk", key.clone()),
                        ("sid", "ab12".into()),
                    ]);
                }
                let mut s = protocol_store(&remote, &target, network, security, &params);
                let mut outbound =
                    rift_core::connection::Connection::from_legacy(&s.servers[0].outbound)
                        .unwrap()
                        .xray_outbound("synthetic")
                        .unwrap();
                outbound["protocol"] = json!(protocol);
                outbound["settings"] = match protocol {
                    "vless" => {
                        json!({"vnext":[{"address":"127.0.0.1","port":remote.port,"users":[{"id":ID,"encryption":"none"}]}]})
                    }
                    "vmess" => {
                        json!({"vnext":[{"address":"127.0.0.1","port":remote.port,"users":[{"id":ID,"security":"auto"}]}]})
                    }
                    _ => {
                        json!({"servers":[{"address":"127.0.0.1","port":remote.port,"password":"synthetic-password"}]})
                    }
                };
                s.servers = rift_core::subscription::parse_subscription(
                    &json!({"outbounds":[outbound]}).to_string(),
                    "synthetic",
                )
                .unwrap()
                .servers;
                s.subscriptions.push(rift_core::model::Subscription {
                    id: "synthetic".into(),
                    url: "https://synthetic.example/subscription".into(),
                    ..Default::default()
                });
                s.selected = Some(s.servers[0].id.clone());
                let dir = tempfile::tempdir().unwrap();
                let mut runtime = core(dir.path(), &target);
                if security == "tls" {
                    runtime = runtime.with_test_xray_ca(root).unwrap();
                }
                count += 1;
                eprintln!("matrix case {count}: {protocol}/{network}/{security}");
                tcp_udp_transfer(&mut runtime, &s, &target, udp_port, count).await;
            }
        }
    }
    assert_eq!(count, 27);
    assert_eq!(target.hits.load(Ordering::SeqCst), 27);
    echo.abort();
}

#[tokio::test]
async fn actual_sing_box_bind_conflicts_name_only_the_configured_local_listener() {
    use rift_core::sidecar_log::{ListenerCause, ListenerPurpose};
    for purpose in [ListenerPurpose::Proxy, ListenerPurpose::ClashApi] {
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = occupied.local_addr().unwrap().port();
        let mut s = Store::default();
        s.settings.proxy_port = free_port();
        s.settings.api_port = free_port();
        match purpose {
            ListenerPurpose::Proxy => s.settings.proxy_port = port,
            ListenerPurpose::ClashApi => s.settings.api_port = port,
        }
        s.servers
            .push(parse_uri("trojan://synthetic@127.0.0.1:9", None).unwrap());
        s.selected = Some(s.servers[0].id.clone());
        let dir = tempfile::tempdir().unwrap();
        let mut runtime = Sidecar::new(binary("sing-box"), dir.path().into())
            .with_chain(|| Chain::new(Vec::new()));
        let error = runtime.start(&s, SECRET).await.unwrap_err();
        assert_eq!(error.failure_code(), Some(FailureCode::LocalPortInUse));
        let facts = runtime.diagnostics();
        let listener = facts.listener.unwrap();
        assert_eq!(listener.purpose, purpose);
        assert_eq!(listener.port, port);
        assert_eq!(listener.cause, ListenerCause::InUse);
        assert!(error.to_string().contains(&port.to_string()));
        assert_eq!(facts.exits.len(), 1);
        assert!(facts.exits[0].code.is_some());
        assert!(!runtime.alive().unwrap());
        runtime.stop().await.unwrap();
        assert_eq!(runtime.diagnostics(), facts);
    }
}

#[tokio::test]
async fn actual_pinned_xray_transport_deprecations_do_not_become_connection_errors() {
    use rift_core::xray_log::WarningCategory;
    let remote = Remote::start().await;
    let target = Target::start().await;
    for (transport, category) in [
        ("ws", WarningCategory::WebsocketDeprecated),
        ("grpc", WarningCategory::GrpcDeprecated),
    ] {
        let mut s = store(&[&remote], &target);
        s.servers[0] = parse_uri(
            &format!(
                "vless://{ID}@127.0.0.1:{}?security=none&type={transport}",
                remote.port
            ),
            None,
        )
        .unwrap();
        s.selected = Some(s.servers[0].id.clone());
        let dir = tempfile::tempdir().unwrap();
        let mut runtime = core(dir.path(), &target);
        runtime.start(&s, SECRET).await.unwrap();
        timeout(Duration::from_secs(3), async {
            while !runtime
                .diagnostics()
                .warnings
                .iter()
                .any(|warning| warning.category == category)
            {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("pinned core's exact transport warning reaches its closed category");
        assert!(runtime.xray_log_summary().unwrap().groups.is_empty());
        assert!(runtime
            .log_notes(1)
            .iter()
            .all(|note| !note.contains("Xray")));
        let info = runtime.informational_notes(1);
        assert_eq!(info, vec![category.note().to_owned()]);
        runtime.stop().await.unwrap();
        assert!(runtime
            .diagnostics()
            .warnings
            .iter()
            .any(|warning| warning.category == category));
        let facts = runtime.diagnostics();
        assert_eq!(facts.exits.len(), 2);
        assert_eq!(facts.primary_exit, None);
        assert!(facts
            .exits
            .iter()
            .all(|exit| exit.stage == rift_core::runtime::ExitStage::Running
                && exit.termination == rift_core::runtime::Termination::Requested));
        runtime.start(&s, SECRET).await.unwrap();
        assert_eq!(runtime.diagnostics().generation, facts.generation + 1);
        assert!(runtime.diagnostics().exits.is_empty());
        runtime.stop().await.unwrap();
    }
}

/// The application owns the runtime; this fixture only exposes its owned PIDs to the
/// existing crash injector, while every lifecycle/diagnostic operation is real.
struct PidTrackedSidecar {
    core: Sidecar,
    pids: Arc<[AtomicUsize; 2]>,
}
#[async_trait::async_trait]
impl Runtime for PidTrackedSidecar {
    async fn validate(&mut self, store: &Store, secret: &str) -> rift_core::Result<()> {
        self.core.validate(store, secret).await
    }
    async fn start(&mut self, store: &Store, secret: &str) -> rift_core::Result<()> {
        self.core.start(store, secret).await?;
        self.pids[0].store(self.core.pid().unwrap() as usize, Ordering::SeqCst);
        self.pids[1].store(self.core.xray_pid().unwrap() as usize, Ordering::SeqCst);
        Ok(())
    }
    async fn stop(&mut self) -> rift_core::Result<()> {
        self.core.stop().await
    }
    fn alive(&mut self) -> rift_core::Result<bool> {
        self.core.alive()
    }
    fn crash_code(&mut self) -> FailureCode {
        self.core.crash_code()
    }
    fn diagnostics(&self) -> rift_core::runtime::RuntimeDiagnostics {
        self.core.diagnostics()
    }
}

#[tokio::test]
async fn detected_application_crash_exports_the_primary_and_reaped_sibling() {
    use rift_core::{
        application::{Action, Application},
        runtime::{Engine, ExitStage, RuntimeDiagnostics, Termination},
    };
    for (index, engine) in [Engine::SingBox, Engine::Xray].into_iter().enumerate() {
        let remote = Remote::start().await;
        let target = Target::start().await;
        let mut s = store(&[&remote], &target);
        s.settings.auto_select = false;
        let probe = NetworkProbe::new(s.settings.clone(), SECRET.into()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let pids = Arc::new([AtomicUsize::new(0), AtomicUsize::new(0)]);
        let app = Application::with_probe(
            s,
            dir.path().into(),
            Box::new(PidTrackedSidecar {
                core: core(dir.path(), &target),
                pids: pids.clone(),
            }),
            Arc::new(probe),
            SECRET.into(),
        );
        app.start();
        app.execute(Action::Connect).await.unwrap();
        kill_owned(u32::try_from(pids[index].load(Ordering::SeqCst)).unwrap()).await;
        timeout(Duration::from_secs(5), async {
            while app.snapshot().runtime_active {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(app.snapshot().status, "error");
        app.execute(Action::ExportDiagnostics).await.unwrap();
        let report: Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("diagnostics.json")).unwrap())
                .unwrap();
        let facts: RuntimeDiagnostics = serde_json::from_value(report["runtime"].clone()).unwrap();
        assert_eq!(facts.primary_exit, Some(engine));
        assert_eq!(facts.exits.len(), 2);
        for exit in facts.exits {
            assert_eq!(exit.stage, ExitStage::Running);
            assert!(exit.code.is_some() || exit.signal.is_some());
            assert_eq!(
                exit.termination,
                if exit.engine == engine {
                    Termination::Unexpected
                } else {
                    Termination::SiblingFailure
                }
            );
        }
        app.shutdown().await.unwrap();
    }
}
