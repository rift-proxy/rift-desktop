//! The elevated helper: what it accepts, what it refuses and when it ends.
mod common;

use common::helper::{tun_store, Calls, FakeRuntime, SECRET};
use proptest::prelude::*;
use rift_core::{
    elevation::{
        helper::{serve, vet, Exit},
        protocol::{
            decode, frame_length, read_frame, write_frame, FromHelper, ToHelper, MAX_FRAME, VERSION,
        },
    },
    failure::FailureCode,
    model::Mode,
};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use tokio::io::{duplex, AsyncWriteExt, DuplexStream};

const TOKEN: &str = "launch-token-1";
const POLL: Duration = Duration::from_millis(20);

struct Session {
    rift: DuplexStream,
    calls: Arc<Calls>,
    helper: tokio::task::JoinHandle<Exit>,
}
async fn session() -> Session {
    let (rift, helper_end) = duplex(64 * 1024);
    let calls = Arc::new(Calls::default());
    let mut runtime = FakeRuntime(calls.clone());
    let helper = tokio::spawn(async move { serve(helper_end, TOKEN, &mut runtime, POLL).await });
    let mut session = Session {
        rift,
        calls,
        helper,
    };
    // The first frame is always the greeting.
    assert_eq!(
        session.next().await,
        Some(FromHelper::Hello {
            version: VERSION,
            token: TOKEN.into()
        })
    );
    session
}
impl Session {
    async fn send(&mut self, message: &ToHelper) {
        write_frame(&mut self.rift, message).await.unwrap();
    }
    async fn next(&mut self) -> Option<FromHelper> {
        tokio::time::timeout(Duration::from_secs(3), read_frame(&mut self.rift))
            .await
            .expect("the helper said nothing")
            .unwrap()
    }
    async fn start(&mut self) -> Option<FromHelper> {
        self.send(&ToHelper::Start {
            store: Box::new(tun_store()),
            secret: SECRET.into(),
        })
        .await;
        self.next().await
    }
    async fn exit(self) -> Exit {
        tokio::time::timeout(Duration::from_secs(3), self.helper)
            .await
            .expect("the helper did not end")
            .unwrap()
    }
}

#[tokio::test]
async fn a_valid_tun_store_starts_sing_box_and_stop_ends_the_helper() {
    let mut s = session().await;
    assert_eq!(s.start().await, Some(FromHelper::Started));
    assert_eq!(s.calls.starts.load(Ordering::SeqCst), 1);
    assert_eq!(s.calls.secrets.lock().unwrap().as_slice(), [SECRET]);
    assert_eq!(s.calls.stores.lock().unwrap()[0].settings.mode, Mode::Tun);
    assert!(s.calls.alive.load(Ordering::SeqCst));

    s.send(&ToHelper::Stop).await;
    assert_eq!(s.next().await, Some(FromHelper::Stopped));
    let calls = s.calls.clone();
    assert_eq!(s.exit().await, Exit::Stopped);
    assert!(!calls.alive.load(Ordering::SeqCst), "no sing-box is left");
}

#[tokio::test]
async fn a_second_start_replaces_the_run_without_ending_the_helper() {
    let mut s = session().await;
    assert_eq!(s.start().await, Some(FromHelper::Started));
    assert_eq!(s.start().await, Some(FromHelper::Started));
    assert_eq!(s.calls.starts.load(Ordering::SeqCst), 2);
    assert!(!s.helper.is_finished());
}

#[tokio::test]
async fn stores_that_fail_the_checks_are_refused_and_nothing_is_run() {
    let mut bad_port = tun_store();
    bad_port.settings.proxy_port = 0;
    let mut proxy_mode = tun_store();
    proxy_mode.settings.mode = Mode::Proxy;
    let mut forged = serde_json::to_value(tun_store()).unwrap();
    // A key of a program-running outbound, which the allowlist never lets in.
    forged["version"] = serde_json::json!(1);
    forged["servers"][0]
        .as_object_mut()
        .unwrap()
        .remove("connection");
    forged["servers"][0]["outbound"] = tun_store().servers[0].outbound.clone();
    forged["servers"][0]["outbound"]["executable_path"] = serde_json::json!("C:\\evil.exe");
    let forged: rift_core::model::Store = serde_json::from_value(forged).unwrap();
    let mut renamed = tun_store();
    renamed.servers[0].id = "not-the-fingerprint".into();

    for (name, store, secret) in [
        ("invalid settings", bad_port, SECRET),
        ("a mode that needs no rights", proxy_mode, SECRET),
        ("an outbound outside the allowlist", forged, SECRET),
        ("an id that is not the outbound's", renamed, SECRET),
        ("a short secret", tun_store(), "abc"),
        (
            "a secret that is not hex",
            tun_store(),
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        ),
        (
            "an upper-case secret",
            tun_store(),
            "0123456789ABCDEF0123456789ABCDEF",
        ),
    ] {
        let mut s = session().await;
        s.send(&ToHelper::Start {
            store: Box::new(store),
            secret: secret.into(),
        })
        .await;
        assert!(
            matches!(s.next().await, Some(FromHelper::Failed { .. })),
            "{name}"
        );
        assert_eq!(s.calls.starts.load(Ordering::SeqCst), 0, "{name}");
        // The helper stays up: Rift decides what follows.
        assert!(!s.helper.is_finished(), "{name}");
    }
}

#[test]
fn vet_says_why_in_plain_checks() {
    assert!(vet(&tun_store(), SECRET).is_ok());
    let mut proxy = tun_store();
    proxy.settings.mode = Mode::Proxy;
    assert!(vet(&proxy, SECRET).is_err());
    let mut tun_proxy = tun_store();
    tun_proxy.settings.mode = Mode::TunProxy;
    assert!(vet(&tun_proxy, SECRET).is_ok());
}

#[tokio::test]
async fn a_failed_start_is_reported_with_its_code_and_a_later_start_may_succeed() {
    let mut s = session().await;
    *s.calls.fail_next_start.lock().unwrap() = Some(FailureCode::SidecarStartExit);
    assert_eq!(
        s.start().await,
        Some(FromHelper::Failed {
            code: FailureCode::SidecarStartExit,
            startup: None,
        })
    );
    assert_eq!(s.start().await, Some(FromHelper::Started));
}

#[tokio::test]
async fn sing_box_that_ends_by_itself_is_reported_and_ends_the_helper() {
    let mut s = session().await;
    assert_eq!(s.start().await, Some(FromHelper::Started));
    s.calls.alive.store(false, Ordering::SeqCst);
    assert_eq!(s.next().await, Some(FromHelper::Exited));
    assert_eq!(s.exit().await, Exit::SingBoxExited);
}

#[tokio::test]
async fn a_status_query_error_is_not_reported_as_an_engine_exit() {
    let mut s = session().await;
    assert_eq!(s.start().await, Some(FromHelper::Started));
    s.calls.status_error.store(true, Ordering::SeqCst);
    assert_eq!(
        s.next().await,
        Some(FromHelper::ExitedWithCode {
            code: FailureCode::Unknown
        })
    );
    let calls = s.calls.clone();
    assert_eq!(s.exit().await, Exit::SingBoxExited);
    assert!(!calls.alive.load(Ordering::SeqCst));
    assert!(calls.stops.load(Ordering::SeqCst) > 0);
}

#[test]
fn terminal_status_failure_accepts_only_defined_wire_code_names() {
    assert_eq!(
        decode::<FromHelper>(br#"{"type":"exited_with_code","code":"unknown"}"#).unwrap(),
        FromHelper::ExitedWithCode {
            code: FailureCode::Unknown
        }
    );
    let error = decode::<FromHelper>(br#"{"type":"exited_with_code","code":"future_failure"}"#)
        .unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::ElevationChannel));
}

#[tokio::test]
async fn a_closed_channel_stops_sing_box_and_ends_the_helper() {
    let mut s = session().await;
    assert_eq!(s.start().await, Some(FromHelper::Started));
    let calls = s.calls.clone();
    drop(s.rift);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), s.helper)
            .await
            .unwrap()
            .unwrap(),
        Exit::ChannelClosed
    );
    assert!(!calls.alive.load(Ordering::SeqCst));
}

#[tokio::test]
async fn frames_that_are_not_the_protocol_end_the_helper_and_run_nothing() {
    for (name, bytes) in [
        ("rubbish", [&5u32.to_be_bytes()[..], b"hello"].concat()),
        ("an unknown type", {
            let body = br#"{"type":"run_this","command":"calc.exe"}"#;
            [&(body.len() as u32).to_be_bytes()[..], body].concat()
        }),
        ("a start without a store", {
            let body = br#"{"type":"start","secret":"x"}"#;
            [&(body.len() as u32).to_be_bytes()[..], body].concat()
        }),
        (
            "a frame over the limit",
            (MAX_FRAME as u32 + 1).to_be_bytes().to_vec(),
        ),
    ] {
        let mut s = session().await;
        s.rift.write_all(&bytes).await.unwrap();
        let calls = s.calls.clone();
        assert_eq!(s.exit().await, Exit::Protocol, "{name}");
        assert_eq!(calls.starts.load(Ordering::SeqCst), 0, "{name}");
    }
}

#[tokio::test]
async fn a_start_larger_than_a_pipe_buffer_still_arrives_whole() {
    let mut big = tun_store();
    for i in 0..300 {
        big.servers.push(
            rift_core::parser::parse_uri(
                &format!("trojan://secret{i}@example.com:{}#S{i}", 1000 + i),
                None,
            )
            .unwrap(),
        );
    }
    let mut s = session().await;
    s.send(&ToHelper::Start {
        store: Box::new(big),
        secret: SECRET.into(),
    })
    .await;
    assert_eq!(s.next().await, Some(FromHelper::Started));
    assert_eq!(s.calls.stores.lock().unwrap()[0].servers.len(), 301);
}

#[test]
fn a_frame_announcing_more_than_the_limit_is_refused_before_anything_is_allocated() {
    assert_eq!(frame_length(0u32.to_be_bytes()).unwrap(), 0);
    assert_eq!(
        frame_length((MAX_FRAME as u32).to_be_bytes()).unwrap(),
        MAX_FRAME
    );
    let error = frame_length((MAX_FRAME as u32 + 1).to_be_bytes()).unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::ElevationChannel));
    assert!(frame_length(u32::MAX.to_be_bytes()).is_err());
}

#[test]
fn the_debug_form_of_a_start_shows_no_store() {
    let message = ToHelper::Start {
        store: Box::new(tun_store()),
        secret: SECRET.into(),
    };
    let text = format!("{message:?}");
    assert!(
        !text.contains(SECRET) && !text.contains("secret@"),
        "{text}"
    );
}

proptest! {
    #[test]
    fn no_bytes_make_the_frame_reader_panic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        runtime.block_on(async {
            let mut reader = &bytes[..];
            let _ = read_frame::<_, ToHelper>(&mut reader).await;
            let mut reader = &bytes[..];
            let _ = read_frame::<_, FromHelper>(&mut reader).await;
        });
        let _ = decode::<ToHelper>(&bytes);
        if let Ok(ToHelper::Start { store, secret }) = decode::<ToHelper>(&bytes) {
            let _ = vet(&store, &secret);
        }
    }
}

#[tokio::test]
async fn a_rejected_replacement_still_observes_the_previous_child() {
    let mut s = session().await;
    assert_eq!(s.start().await, Some(FromHelper::Started));
    let mut invalid = tun_store();
    invalid.settings.mode = Mode::Proxy;
    s.send(&ToHelper::Start {
        store: Box::new(invalid),
        secret: SECRET.into(),
    })
    .await;
    assert_eq!(
        s.next().await,
        Some(FromHelper::Failed {
            code: FailureCode::SidecarConfigRejected,
            startup: None,
        })
    );
    assert!(s.calls.alive.load(Ordering::SeqCst));
    s.calls.alive.store(false, Ordering::SeqCst);
    assert_eq!(s.next().await, Some(FromHelper::Exited));
    assert_eq!(s.exit().await, Exit::SingBoxExited);
}

#[tokio::test]
async fn eof_is_clean_only_before_the_first_header_byte() {
    assert!(read_frame::<_, ToHelper>(&mut &b""[..])
        .await
        .unwrap()
        .is_none());
    for header in [&b"\0"[..], &b"\0\0"[..], &b"\0\0\0"[..]] {
        assert!(read_frame::<_, ToHelper>(&mut &header[..]).await.is_err());
    }
}

#[tokio::test]
async fn a_child_that_exits_just_after_successful_start_is_still_reported() {
    struct Dies;
    #[async_trait::async_trait]
    impl rift_core::runtime::Runtime for Dies {
        async fn validate(
            &mut self,
            _: &rift_core::model::Store,
            _: &str,
        ) -> rift_core::Result<()> {
            Ok(())
        }
        async fn start(&mut self, _: &rift_core::model::Store, _: &str) -> rift_core::Result<()> {
            Ok(())
        }
        async fn stop(&mut self) -> rift_core::Result<()> {
            Ok(())
        }
        fn alive(&mut self) -> rift_core::Result<bool> {
            Ok(false)
        }
    }
    let (mut rift, helper) = duplex(64 * 1024);
    let task = tokio::spawn(async move { serve(helper, TOKEN, &mut Dies, POLL).await });
    let hello: FromHelper = read_frame(&mut rift).await.unwrap().unwrap();
    assert!(matches!(hello, FromHelper::Hello { .. }));
    write_frame(
        &mut rift,
        &ToHelper::Start {
            store: Box::new(tun_store()),
            secret: SECRET.into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        read_frame::<_, FromHelper>(&mut rift).await.unwrap(),
        Some(FromHelper::Started)
    );
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(1),
            read_frame::<_, FromHelper>(&mut rift)
        )
        .await
        .unwrap()
        .unwrap(),
        Some(FromHelper::Exited)
    );
    assert_eq!(task.await.unwrap(), Exit::SingBoxExited);
}

#[test]
fn startup_failure_frames_accept_only_bounded_closed_observations() {
    use serde_json::json;
    let valid = json!({
        "type": "failed",
        "code": "sidecar_start_exit",
        "startup": {
            "stage": "sing_box_config_check_spawn",
            "cause": { "kind": "io", "os_code": 2 }
        }
    });
    let decoded = decode::<FromHelper>(&serde_json::to_vec(&valid).unwrap()).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), valid);
    assert!(!format!("{decoded:?}").contains("os_code"));
    for startup in [
        json!({"stage": "raw-child-output", "cause": {"kind": "io", "os_code": 2}}),
        json!({"stage": "xray_spawn", "cause": {"kind": "io", "os_code": "token-canary"}}),
        json!({"stage": "xray_spawn", "cause": {"kind": "io", "os_code": 2147483648_i64}}),
        json!({"stage": "xray_spawn", "cause": {"kind": "io", "os_code": 2, "output": "raw-canary"}}),
        json!({"stage": "xray_spawn", "cause": {"kind": "exit", "code": 1, "signal": null}}),
        json!({"stage": "xray_config_check", "cause": {"kind": "exit", "code": 0, "signal": null}}),
        json!({"stage": "xray_config_check", "cause": {"kind": "exit", "code": 1, "signal": 9}}),
        json!({"stage": "xray_config_check", "cause": {"kind": "exit", "code": null, "signal": 128}}),
        json!({"stage": "xray_spawn", "cause": {"kind": "timeout"}}),
        json!({"stage": "xray_config_feed", "cause": {"kind": "timeout"}, "config": "config-canary"}),
    ] {
        let mut frame = valid.clone();
        frame["startup"] = startup;
        let error = decode::<FromHelper>(&serde_json::to_vec(&frame).unwrap()).unwrap_err();
        assert_eq!(error.failure_code(), Some(FailureCode::ElevationChannel));
        for canary in ["token-canary", "raw-canary", "config-canary"] {
            assert!(!error.diagnostic_details().contains(canary));
        }
    }
    let mut frame = valid;
    frame["code"] = json!("future_failure");
    assert!(decode::<FromHelper>(&serde_json::to_vec(&frame).unwrap()).is_err());
    for stage in ["sing_box_config_check", "xray_config_check"] {
        let frame = json!({
            "type": "failed",
            "code": if stage == "xray_config_check" { "xray_config_rejected" } else { "sidecar_config_rejected" },
            "startup": { "stage": stage, "cause": { "kind": "exit", "code": 23, "signal": null } }
        });
        let decoded = decode::<FromHelper>(&serde_json::to_vec(&frame).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), frame);
    }
}
