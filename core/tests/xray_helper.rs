//! Helper handoff, bounded diagnostics and lifecycle; fakes remain in transport tests.
use async_trait::async_trait;
use rift_core::{
    elevation::{
        client::{BoxStream, Elevator, HelperRuntime, Launched, Listener, Transport},
        helper::{serve, Exit},
        protocol::{read_frame, write_frame, FromHelper, ToHelper, VERSION},
    },
    failure::FailureCode,
    model::{Mode, Store},
    parser::parse_uri,
    proxy_channel::{ProxyChannel, ProxyHandoff},
    runtime::{Runtime, RuntimeDiagnostics},
    Result,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tokio::io::DuplexStream;
const TOKEN: &str = "synthetic-channel-token-01234567890123456789";
const SECRET: &str = "0123456789abcdef0123456789abcdef";
fn store() -> Store {
    let mut s = Store::default();
    s.settings.mode = Mode::Tun;
    s.servers
        .push(parse_uri("trojan://synthetic@server.example:443", None).unwrap());
    s
}

struct GatewayRuntime {
    alive: Arc<AtomicBool>,
    channel: ProxyChannel,
    stopped: Arc<AtomicBool>,
}
#[async_trait]
impl Runtime for GatewayRuntime {
    async fn validate(&mut self, _: &Store, _: &str) -> Result<()> {
        Ok(())
    }
    async fn start(&mut self, s: &Store, _: &str) -> Result<()> {
        *self.channel.write().unwrap() = Some(ProxyHandoff::new(12345, SECRET.into(), s)?);
        self.alive.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        *self.channel.write().unwrap() = None;
        self.stopped.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(self.alive.load(Ordering::SeqCst))
    }
    fn crash_code(&mut self) -> FailureCode {
        FailureCode::XrayCrashed
    }
    fn proxy_channel(&self) -> Option<ProxyChannel> {
        Some(self.channel.clone())
    }
    fn diagnostics(&self) -> rift_core::runtime::RuntimeDiagnostics {
        helper_facts(!self.alive.load(Ordering::SeqCst))
    }
}

fn helper_facts(exited: bool) -> rift_core::runtime::RuntimeDiagnostics {
    use rift_core::runtime::{ChildExit, Engine, ExitStage, RuntimeDiagnostics, Termination};
    RuntimeDiagnostics {
        generation: 1,
        primary_exit: exited.then_some(Engine::Xray),
        exits: if exited {
            vec![
                ChildExit {
                    engine: Engine::Xray,
                    stage: ExitStage::Running,
                    termination: Termination::Unexpected,
                    code: Some(37),
                    signal: None,
                },
                ChildExit {
                    engine: Engine::SingBox,
                    stage: ExitStage::Running,
                    termination: Termination::SiblingFailure,
                    code: None,
                    signal: Some(9),
                },
            ]
        } else {
            Vec::new()
        },
        listener: None,
        warnings: vec![rift_core::xray_log::WarningCount {
            category: rift_core::xray_log::WarningCategory::GrpcDeprecated,
            count: 1,
        }],
    }
}

#[tokio::test]
async fn privileged_helper_exports_only_gateway_metadata_and_preserves_xray_exit_code() {
    let (mut parent, peer) = tokio::io::duplex(65536);
    let alive = Arc::new(AtomicBool::new(false));
    let stopped = Arc::new(AtomicBool::new(false));
    let mut runtime = GatewayRuntime {
        alive: alive.clone(),
        stopped: stopped.clone(),
        channel: Arc::new(std::sync::RwLock::new(None)),
    };
    let task = tokio::spawn(async move {
        serve(
            Box::new(peer),
            TOKEN,
            &mut runtime,
            std::time::Duration::from_millis(10),
        )
        .await
    });
    assert_eq!(
        read_frame::<_, FromHelper>(&mut parent).await.unwrap(),
        Some(FromHelper::Hello {
            version: VERSION,
            token: TOKEN.into()
        })
    );
    write_frame(
        &mut parent,
        &ToHelper::Start {
            store: Box::new(store()),
            secret: SECRET.into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        read_frame::<_, FromHelper>(&mut parent).await.unwrap(),
        Some(FromHelper::Diagnostics {
            diagnostics: helper_facts(false)
        })
    );
    let reply = read_frame::<_, FromHelper>(&mut parent)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        reply,
        FromHelper::StartedWithHandoff {
            port: 12345,
            secret: SECRET.into()
        }
    );
    assert!(!format!("{reply:?}").contains(SECRET));
    alive.store(false, Ordering::SeqCst);
    assert_eq!(
        read_frame::<_, FromHelper>(&mut parent).await.unwrap(),
        Some(FromHelper::Diagnostics {
            diagnostics: helper_facts(true)
        })
    );
    assert_eq!(
        read_frame::<_, FromHelper>(&mut parent).await.unwrap(),
        Some(FromHelper::ExitedWithCode {
            code: FailureCode::XrayCrashed
        })
    );
    assert_eq!(task.await.unwrap(), Exit::SingBoxExited);
    assert!(stopped.load(Ordering::SeqCst));
}

struct Scripted {
    peer: Mutex<Option<DuplexStream>>,
    port: u16,
    exit: Option<&'static str>,
    closed: Arc<AtomicBool>,
    updates: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<RuntimeDiagnostics>>>,
}
struct Ready {
    client: Option<DuplexStream>,
}
#[async_trait]
impl Listener for Ready {
    fn name(&self) -> String {
        "rift-helper-synthetic".into()
    }
    async fn accept(&mut self) -> std::io::Result<BoxStream> {
        Ok(Box::new(self.client.take().unwrap()))
    }
}
impl Transport for Scripted {
    fn listen(&self) -> Result<Box<dyn Listener>> {
        let (client, peer) = tokio::io::duplex(65536);
        *self.peer.lock().unwrap() = Some(peer);
        Ok(Box::new(Ready {
            client: Some(client),
        }))
    }
}
#[async_trait]
impl Elevator for Scripted {
    async fn launch(&self, _: &str, token: &str) -> Launched {
        let mut peer = self.peer.lock().unwrap().take().unwrap();
        let token = token.to_owned();
        let port = self.port;
        let exit = self.exit;
        let closed = self.closed.clone();
        let updates = self.updates.lock().unwrap().take();
        tokio::spawn(async move {
            write_frame(
                &mut peer,
                &FromHelper::Hello {
                    version: VERSION,
                    token,
                },
            )
            .await
            .unwrap();
            assert!(matches!(
                read_frame::<_, ToHelper>(&mut peer).await.unwrap(),
                Some(ToHelper::Start { .. })
            ));
            write_frame(
                &mut peer,
                &FromHelper::Diagnostics {
                    diagnostics: helper_facts(false),
                },
            )
            .await
            .unwrap();
            write_frame(
                &mut peer,
                &FromHelper::StartedWithHandoff {
                    port,
                    secret: SECRET.into(),
                },
            )
            .await
            .unwrap();
            if let Some(mut updates) = updates {
                loop {
                    tokio::select! {
                        update = updates.recv() => {
                            let Some(diagnostics) = update else {
                                break;
                            };
                            write_frame(&mut peer, &FromHelper::Diagnostics { diagnostics })
                                .await
                                .unwrap();
                        }
                        command = read_frame::<_, ToHelper>(&mut peer) => {
                            assert!(matches!(command.unwrap(), Some(ToHelper::Stop)));
                            write_frame(&mut peer, &FromHelper::Stopped).await.unwrap();
                            break;
                        }
                    }
                }
            } else if let Some(code) = exit {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                write_frame(
                    &mut peer,
                    &FromHelper::Diagnostics {
                        diagnostics: helper_facts(code == "xray_crashed"),
                    },
                )
                .await
                .unwrap();
                write_frame(
                    &mut peer,
                    &serde_json::json!({"type": "exited_with_code", "code": code}),
                )
                .await
                .unwrap();
            } else if matches!(
                read_frame::<_, ToHelper>(&mut peer).await.unwrap(),
                Some(ToHelper::Stop)
            ) {
                write_frame(&mut peer, &FromHelper::Stopped).await.unwrap();
            }
            closed.store(true, Ordering::SeqCst);
        });
        Launched::Started
    }
}
async fn eventually(mut f: impl FnMut() -> bool) {
    for _ in 0..100 {
        if f() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("condition not reached");
}
#[tokio::test]
async fn parent_validates_handoff_clears_it_on_stop_and_retains_typed_crash() {
    for exit in [false, true] {
        let peer = Arc::new(Scripted {
            peer: Mutex::new(None),
            port: 12345,
            exit: exit.then_some("xray_crashed"),
            closed: Arc::new(AtomicBool::new(false)),
            updates: Mutex::new(None),
        });
        let mut runtime = HelperRuntime::new(peer.clone(), peer.clone());
        runtime.start(&store(), SECRET).await.unwrap();
        assert_eq!(runtime.diagnostics(), helper_facts(false));
        assert_eq!(
            runtime.informational_notes(1),
            vec![rift_core::xray_log::WarningCategory::GrpcDeprecated
                .note()
                .to_owned()]
        );
        assert!(runtime.informational_notes(2).is_empty());
        assert!(
            runtime.log_notes(2).is_empty(),
            "deprecation is not a connection warning"
        );
        let channel = runtime.proxy_channel().unwrap();
        assert_eq!(channel.read().unwrap().as_ref().unwrap().port(), 12345);
        if exit {
            eventually(|| !runtime.alive().unwrap()).await;
            assert_eq!(runtime.crash_code(), FailureCode::XrayCrashed);
            assert_eq!(runtime.diagnostics(), helper_facts(true));
            runtime.stop().await.unwrap();
            assert_eq!(
                runtime.diagnostics(),
                helper_facts(true),
                "cleanup preserves the engine's original code"
            );
            assert_eq!(runtime.crash_code(), FailureCode::XrayCrashed);
        } else {
            runtime.stop().await.unwrap();
        }
        assert!(channel.read().unwrap().is_none());
        eventually(|| peer.closed.load(Ordering::SeqCst)).await;
    }
}

#[tokio::test]
async fn raw_terminal_codes_distinguish_status_failure_from_invalid_helper_frames() {
    for drain in [false, true] {
        for (code, expected) in [
            ("unknown", FailureCode::Unknown),
            ("future_failure", FailureCode::ElevationChannel),
            ("clash_api", FailureCode::ElevationChannel),
        ] {
            let peer = Arc::new(Scripted {
                peer: Mutex::new(None),
                port: 12345,
                exit: Some(code),
                closed: Arc::new(AtomicBool::new(false)),
                updates: Mutex::new(None),
            });
            let mut runtime = HelperRuntime::new(peer.clone(), peer.clone());
            runtime.start(&store(), SECRET).await.unwrap();
            let channel = runtime.proxy_channel().unwrap();
            if drain {
                eventually(|| !runtime.alive().unwrap()).await;
            } else {
                runtime.stop().await.unwrap();
            }
            assert_eq!(runtime.crash_code(), expected, "code={code}, drain={drain}");
            assert!(!runtime.has_session());
            assert!(channel.read().unwrap().is_none());
            assert_eq!(runtime.diagnostics(), helper_facts(false));
            assert_eq!(runtime.diagnostics().primary_exit, None);
            eventually(|| peer.closed.load(Ordering::SeqCst)).await;
        }
    }
}
async fn update_helper_diagnostics(
    updates: &tokio::sync::mpsc::UnboundedSender<RuntimeDiagnostics>,
    runtime: &mut HelperRuntime,
    diagnostics: &RuntimeDiagnostics,
) {
    updates.send(diagnostics.clone()).unwrap();
    eventually(|| {
        assert!(runtime.alive().unwrap());
        runtime.diagnostics() == *diagnostics
    })
    .await;
}

#[tokio::test]
async fn helper_informational_notes_keep_growth_until_each_category_quiet_interval_expires() {
    use rift_core::xray_log::{WarningCategory, WarningCount};

    let (updates, receiver) = tokio::sync::mpsc::unbounded_channel();
    let peer = Arc::new(Scripted {
        peer: Mutex::new(None),
        port: 12345,
        exit: None,
        closed: Arc::new(AtomicBool::new(false)),
        updates: Mutex::new(Some(receiver)),
    });
    let mut runtime = HelperRuntime::new(peer.clone(), peer.clone());
    runtime.start(&store(), SECRET).await.unwrap();
    let grpc_note = WarningCategory::GrpcDeprecated.note().to_owned();
    let websocket_note = WarningCategory::WebsocketDeprecated.note().to_owned();
    assert_eq!(runtime.informational_notes(1000), vec![grpc_note.clone()]);
    assert!(runtime.informational_notes(1000).is_empty());

    let mut diagnostics = helper_facts(false);
    diagnostics.warnings[0].count = 2;
    update_helper_diagnostics(&updates, &mut runtime, &diagnostics).await;
    assert!(runtime.informational_notes(1001).is_empty());
    assert_eq!(
        runtime.diagnostics(),
        diagnostics,
        "counts are not throttled"
    );

    diagnostics.warnings.push(WarningCount {
        category: WarningCategory::WebsocketDeprecated,
        count: 1,
    });
    update_helper_diagnostics(&updates, &mut runtime, &diagnostics).await;
    assert_eq!(
        runtime.informational_notes(1010),
        vec![websocket_note.clone()],
        "a new category does not inherit another category's quiet interval"
    );
    diagnostics.warnings[1].count = 2;
    update_helper_diagnostics(&updates, &mut runtime, &diagnostics).await;
    assert!(runtime.informational_notes(1011).is_empty());
    assert!(runtime.informational_notes(1299).is_empty());
    assert_eq!(
        runtime.diagnostics(),
        diagnostics,
        "suppressed growth remains observable"
    );
    assert_eq!(
        runtime.informational_notes(1300),
        vec![grpc_note.clone()],
        "growth is still pending exactly 300 seconds after the last emission"
    );
    assert!(runtime.informational_notes(1300).is_empty());
    diagnostics.warnings[0].count = 3;
    update_helper_diagnostics(&updates, &mut runtime, &diagnostics).await;
    assert!(
        runtime.informational_notes(1301).is_empty(),
        "every emission restarts its category's quiet interval"
    );
    assert!(runtime.informational_notes(1309).is_empty());
    assert_eq!(
        runtime.informational_notes(1310),
        vec![websocket_note.clone()]
    );
    assert!(runtime.informational_notes(1310).is_empty());
    assert!(runtime.informational_notes(1599).is_empty());
    assert_eq!(runtime.informational_notes(1600), vec![grpc_note.clone()]);
    assert!(runtime.informational_notes(1600).is_empty());
    assert!(
        runtime.informational_notes(1610).is_empty(),
        "unchanged counts never repeat, even after the interval expires"
    );

    diagnostics.generation = 2;
    for warning in &mut diagnostics.warnings {
        warning.count = 1;
    }
    update_helper_diagnostics(&updates, &mut runtime, &diagnostics).await;
    assert_eq!(
        runtime.informational_notes(1611),
        vec![grpc_note, websocket_note],
        "a new generation immediately emits lower fresh counts"
    );
    assert!(runtime.informational_notes(1611).is_empty());
    assert_eq!(runtime.diagnostics(), diagnostics);
    runtime.stop().await.unwrap();
    eventually(|| peer.closed.load(Ordering::SeqCst)).await;
}

#[tokio::test]
async fn invalid_gateway_closes_the_session_and_no_capability_is_published() {
    let peer = Arc::new(Scripted {
        peer: Mutex::new(None),
        port: 0,
        exit: None,
        closed: Arc::new(AtomicBool::new(false)),
        updates: Mutex::new(None),
    });
    let mut runtime = HelperRuntime::new(peer.clone(), peer.clone());
    assert_eq!(
        runtime
            .start(&store(), SECRET)
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::ElevationChannel)
    );
    assert!(!runtime.has_session());
    assert!(runtime.proxy_channel().unwrap().read().unwrap().is_none());
    eventually(|| peer.closed.load(Ordering::SeqCst)).await;
}
#[test]
fn hello_debug_cannot_expose_tokens_and_gateway_values_are_bounded() {
    assert!(!format!(
        "{:?}",
        FromHelper::Hello {
            version: VERSION,
            token: TOKEN.into()
        }
    )
    .contains(TOKEN));
    for (port, secret) in [
        (0, SECRET.to_owned()),
        (12345, "short".into()),
        (12345, "a".repeat(129)),
        (12345, "not-ascii-密".repeat(8)),
    ] {
        assert!(ProxyHandoff::new(port, secret, &store()).is_err());
    }
}

// Hold the next inbound helper frame so its real crash timer wins a replacement Start.
struct GatedInput {
    inner: DuplexStream,
    gate: Arc<AtomicBool>,
    wake: Arc<Mutex<Option<std::task::Waker>>>,
}
impl tokio::io::AsyncRead for GatedInput {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.gate.load(Ordering::SeqCst) {
            *self.wake.lock().unwrap() = Some(cx.waker().clone());
            return std::task::Poll::Pending;
        }
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl tokio::io::AsyncWrite for GatedInput {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
struct Serving {
    peer: Mutex<Option<DuplexStream>>,
    gate: Arc<AtomicBool>,
    wake: Arc<Mutex<Option<std::task::Waker>>>,
    alive: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    task: Mutex<Option<tokio::task::JoinHandle<Exit>>>,
}
impl Transport for Serving {
    fn listen(&self) -> Result<Box<dyn Listener>> {
        let (client, peer) = tokio::io::duplex(65536);
        *self.peer.lock().unwrap() = Some(peer);
        Ok(Box::new(Ready {
            client: Some(client),
        }))
    }
}
#[async_trait]
impl Elevator for Serving {
    async fn launch(&self, _: &str, token: &str) -> Launched {
        let inner = self.peer.lock().unwrap().take().unwrap();
        let token = token.to_owned();
        let gate = self.gate.clone();
        let wake = self.wake.clone();
        let mut runtime = GatewayRuntime {
            alive: self.alive.clone(),
            stopped: self.stopped.clone(),
            channel: Arc::new(std::sync::RwLock::new(None)),
        };
        *self.task.lock().unwrap() = Some(tokio::spawn(async move {
            serve(
                GatedInput { inner, gate, wake },
                &token,
                &mut runtime,
                std::time::Duration::from_millis(10),
            )
            .await
        }));
        Launched::Started
    }
}
#[tokio::test]
async fn restart_preserves_typed_exit_from_the_real_helper_crash_timer() {
    let peer = Arc::new(Serving {
        peer: Mutex::new(None),
        gate: Arc::new(AtomicBool::new(false)),
        wake: Arc::new(Mutex::new(None)),
        alive: Arc::new(AtomicBool::new(false)),
        stopped: Arc::new(AtomicBool::new(false)),
        task: Mutex::new(None),
    });
    let mut runtime = HelperRuntime::new(peer.clone(), peer.clone());
    runtime.start(&store(), SECRET).await.unwrap();
    let channel = runtime.proxy_channel().unwrap();
    assert!(channel.read().unwrap().is_some());
    peer.gate.store(true, Ordering::SeqCst);
    peer.alive.store(false, Ordering::SeqCst);
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        runtime.start(&store(), SECRET),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::XrayCrashed));
    assert_eq!(runtime.crash_code(), FailureCode::XrayCrashed);
    assert!(!runtime.has_session());
    assert!(!runtime.alive().unwrap());
    assert!(channel.read().unwrap().is_none());
    let task = peer.task.lock().unwrap().take().unwrap();
    assert_eq!(task.await.unwrap(), Exit::SingBoxExited);
    assert!(peer.stopped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn helper_replacement_revokes_pending_gateway_and_restores_rejected_validation() {
    let peer = Arc::new(Serving {
        peer: Mutex::new(None),
        gate: Arc::new(AtomicBool::new(false)),
        wake: Arc::new(Mutex::new(None)),
        alive: Arc::new(AtomicBool::new(false)),
        stopped: Arc::new(AtomicBool::new(false)),
        task: Mutex::new(None),
    });
    let mut runtime = HelperRuntime::new(peer.clone(), peer.clone());
    let state = store();
    runtime.start(&state, SECRET).await.unwrap();
    let channel = runtime.proxy_channel().unwrap();
    peer.gate.store(true, Ordering::SeqCst);
    {
        let restart = runtime.start(&state, SECRET);
        tokio::pin!(restart);
        tokio::select! {
            result = &mut restart => panic!("replacement did not wait: {result:?}"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
        assert!(channel.read().unwrap().is_none());
        peer.gate.store(false, Ordering::SeqCst);
        if let Some(wake) = peer.wake.lock().unwrap().take() {
            wake.wake();
        }
        restart.await.unwrap();
    }
    assert!(channel.read().unwrap().is_some());
    let mut bad = state;
    bad.settings.mode = Mode::Proxy;
    assert_eq!(
        runtime
            .start(&bad, SECRET)
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::SidecarConfigRejected)
    );
    assert!(channel.read().unwrap().is_some());
    assert!(runtime.alive().unwrap());
    runtime.stop().await.unwrap();
    let task = peer.task.lock().unwrap().take().unwrap();
    assert_eq!(task.await.unwrap(), Exit::Stopped);
}

#[cfg(unix)]
#[tokio::test]
async fn a_denied_owned_child_listener_survives_the_real_helper_protocol_and_cleanup() {
    struct Peer {
        stream: Mutex<Option<DuplexStream>>,
        runtime: Mutex<Option<rift_core::runtime::Sidecar>>,
    }
    impl Transport for Peer {
        fn listen(&self) -> Result<Box<dyn Listener>> {
            let (client, peer) = tokio::io::duplex(65536);
            *self.stream.lock().unwrap() = Some(peer);
            Ok(Box::new(Ready {
                client: Some(client),
            }))
        }
    }
    #[async_trait]
    impl Elevator for Peer {
        async fn launch(&self, _: &str, token: &str) -> Launched {
            let stream = self.stream.lock().unwrap().take().unwrap();
            let mut runtime = self.runtime.lock().unwrap().take().unwrap();
            let token = token.to_owned();
            tokio::spawn(async move {
                serve(
                    stream,
                    &token,
                    &mut runtime,
                    std::time::Duration::from_millis(10),
                )
                .await
            });
            Launched::Started
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let mut s = store();
    s.settings.mode = Mode::TunProxy;
    let port = s.settings.proxy_port;
    std::fs::write(dir.path().join("check"), "cat >/dev/null\n").unwrap();
    std::fs::write(dir.path().join("run"), format!(
        "cat >/dev/null\nprintf '%s\\n' 'FATAL[0000] start service: start inbound/mixed[mixed]: listen tcp 127.0.0.1:{port}: bind: permission denied' >&2\nexit 19\n"
    )).unwrap();
    let peer = Arc::new(Peer {
        stream: Mutex::new(None),
        runtime: Mutex::new(Some(
            rift_core::runtime::Sidecar::new("/bin/sh".into(), dir.path().into())
                .with_chain(|| rift_core::dns::Chain::new(Vec::new())),
        )),
    });
    let mut runtime = HelperRuntime::new(peer.clone(), peer);
    let error = runtime.start(&s, SECRET).await.unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::LocalPortDenied));
    assert!(error.to_string().contains(&port.to_string()));
    assert!(error.to_string().contains("HTTP/SOCKS proxy"));
    assert!(!error.to_string().contains("TUN"));
    let diagnostics = runtime.diagnostics();
    assert_eq!(diagnostics.listener.unwrap().port, port);
    assert_eq!(diagnostics.exits[0].code, Some(19));
    assert_eq!(
        diagnostics.primary_exit,
        Some(rift_core::runtime::Engine::SingBox)
    );
    runtime.stop().await.unwrap();
    assert_eq!(runtime.diagnostics(), diagnostics);
}
