use async_trait::async_trait;
use rift_core::{
    failure::FailureCode,
    model::Server,
    parser::parse_uri,
    routing::{ServerRoutes, NO_ROUTES_MODE},
    selection::{self, Probe, Progress},
    Error, Result,
};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    time::Duration,
};
use tokio::{sync::Notify, time::Instant};
use tokio_util::sync::CancellationToken;
struct Fake {
    calls: Mutex<Vec<String>>,
    release: Option<Notify>,
}
fn servers() -> Vec<Server> {
    ["A", "B", "C", "D"]
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let mut s = parse_uri(
                &format!("trojan://secret@example.com:{}#{n}", 443 + i),
                None,
            )
            .unwrap();
            s.id = n.to_string();
            s
        })
        .collect()
}
#[async_trait]
impl Probe for Fake {
    async fn endpoint(&self, s: &Server) -> Result<u64> {
        Ok(match s.id.as_str() {
            "A" => 20,
            "B" => 30,
            "C" => 40,
            _ => 50,
        })
    }
    async fn proxy(&self, s: &Server) -> Result<u64> {
        self.calls.lock().unwrap().push(format!("probe:{}", s.id));
        if let Some(n) = &self.release {
            n.notified().await;
        }
        match s.id.as_str() {
            "A" => Err(Error::Network),
            "B" => Ok(80),
            "C" => Ok(60),
            _ => Ok(90),
        }
    }
    async fn select(&self, id: &str) -> Result<()> {
        self.calls.lock().unwrap().push(format!("select:{id}"));
        Ok(())
    }
}
#[tokio::test]
async fn initial_immediate_then_best() {
    let f = Fake {
        calls: Mutex::new(vec![]),
        release: None,
    };
    let result = selection::run(&f, &servers(), None, 10, &CancellationToken::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(result.selected, "C");
    assert_eq!(
        *f.calls.lock().unwrap(),
        [
            "probe:A", "probe:B", "select:B", "probe:C", "probe:D",
            // the fastest by one answer are asked twice more before the choice
            "probe:C", "probe:C", "probe:B", "probe:B", "probe:D", "probe:D", "select:C"
        ]
    );
    assert!(result.last_switch.is_some());
}
#[tokio::test]
async fn a_working_server_in_use_does_not_end_the_check_the_others_are_asked_too() {
    let f = Fake {
        calls: Mutex::new(vec![]),
        release: None,
    };
    let mut events = vec![];
    let result = selection::run(
        &f,
        &servers(),
        Some("C"),
        10,
        &CancellationToken::new(),
        |e| events.push(e),
    )
    .await
    .unwrap();
    // The fastest server is the one in use: nothing to switch.
    assert_eq!(result.selected, "C");
    assert!(result.last_switch.is_none());
    assert_eq!(
        *f.calls.lock().unwrap(),
        [
            // the server in use first, then the others by distance
            "probe:C", "probe:A", "probe:B", "probe:D",
            // the fastest by one answer are asked twice more before the choice
            "probe:C", "probe:C", "probe:B", "probe:B", "probe:D", "probe:D"
        ]
    );
    // The server in use is measured first, its distance and its answer through it; every other
    // server has its distance measured once, and an answer through it when it was reachable.
    assert!(matches!(
        events.as_slice(),
        [Progress::Endpoint(a, e), Progress::Current(b, m), ..]
            if a == "C" && b == "C" && e.latency_ms == Some(40) && m.latency_ms == Some(60)
    ));
    for id in ["A", "B", "D"] {
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Progress::Endpoint(i, _) if i == id))
                .count(),
            1,
            "{id}"
        );
    }
}
#[tokio::test]
async fn the_first_working_server_is_taken_at_once_when_the_one_in_use_fails_and_the_fastest_follows(
) {
    let f = Fake {
        calls: Mutex::new(vec![]),
        release: None,
    };
    let mut events = vec![];
    let result = selection::run(
        &f,
        &servers(),
        Some("A"),
        10,
        &CancellationToken::new(),
        |e| events.push(e),
    )
    .await
    .unwrap();
    assert_eq!(result.selected, "C");
    // A is asked once: it is not asked again among the others. B answers first and is taken at
    // once; the check goes on and the fastest, C, takes its place at the end.
    assert_eq!(
        *f.calls.lock().unwrap(),
        [
            "probe:A", "probe:B", "select:B", "probe:C", "probe:D", "probe:C", "probe:C",
            "probe:B", "probe:B", "probe:D", "probe:D", "select:C"
        ]
    );
    // The distance of each server is measured once, A's among them.
    for (id, count) in [("A", 1), ("B", 1), ("C", 1), ("D", 1)] {
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, Progress::Endpoint(i, _) if i == id))
                .count(),
            count,
            "{id}"
        );
    }
}

#[derive(Default)]
struct CountingProbe {
    endpoints: AtomicUsize,
    proxies: AtomicUsize,
    selects: AtomicUsize,
}
#[async_trait]
impl Probe for CountingProbe {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        self.endpoints.fetch_add(1, Ordering::SeqCst);
        Ok(10)
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        self.proxies.fetch_add(1, Ordering::SeqCst);
        Ok(20)
    }
    async fn select(&self, _: &str) -> Result<()> {
        self.selects.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn duplicate_outbounds_are_probed_once_in_a_full_check() {
    let mut representative = servers()[0].clone();
    representative.name = "first source".into();
    representative.source = Some("first".into());
    let mut duplicate = representative.clone();
    duplicate.name = "second source".into();
    duplicate.source = Some("second".into());
    let list = vec![representative.clone(), duplicate];

    let probe = CountingProbe::default();
    let mut events = Vec::new();
    let result = selection::run(
        &probe,
        &list,
        None,
        10,
        &CancellationToken::new(),
        |event| events.push(event),
    )
    .await
    .unwrap();
    assert_eq!(result.selected, representative.id);
    assert_eq!(probe.endpoints.load(Ordering::SeqCst), 1);
    assert_eq!(probe.proxies.load(Ordering::SeqCst), 1);
    assert_eq!(probe.selects.load(Ordering::SeqCst), 1);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Progress::Endpoint(..)))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Progress::Proxy(..)))
            .count(),
        1
    );
}
#[tokio::test]
async fn a_server_in_use_that_the_others_do_not_beat_by_the_thresholds_stays() {
    let f = Fake {
        calls: Mutex::new(vec![]),
        release: None,
    };
    let result = selection::run(
        &f,
        &servers(),
        Some("B"),
        2,
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    // C is faster than B, but by 20 ms: less than the 50 ms the thresholds ask for.
    assert_eq!(result.selected, "B");
    assert!(result.last_switch.is_none());
    assert!(!f
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|c| c.starts_with("select:")));
}
#[tokio::test]
async fn failure_and_empty() {
    let f = Fake {
        calls: Mutex::new(vec![]),
        release: None,
    };
    for servers in [vec![], vec![servers()[0].clone()]] {
        assert!(matches!(
            selection::run(&f, &servers, None, 10, &CancellationToken::new(), |_| {}).await,
            Err(Error::Unavailable)
        ));
    }
}

struct StageFailures;
#[async_trait]
impl Probe for StageFailures {
    async fn endpoint(&self, server: &Server) -> Result<u64> {
        if server.id == "A" {
            Err(Error::failure(
                FailureCode::EndpointTimeout,
                "endpoint timeout",
            ))
        } else {
            Ok(10)
        }
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        Err(Error::Network)
    }
    async fn select(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn progress_keeps_safe_failure_codes_for_each_probe_stage() {
    let mut events = Vec::new();
    let result = selection::run(
        &StageFailures,
        &servers()[..2],
        None,
        2,
        &CancellationToken::new(),
        |event| events.push(event),
    )
    .await;
    assert!(matches!(result, Err(Error::Unavailable)));
    assert!(events.iter().any(|event| matches!(
        event,
        Progress::Endpoint(id, measurement)
            if id == "A" && measurement.failure_code == Some(FailureCode::EndpointTimeout)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        Progress::Proxy(id, measurement)
            if id == "B" && measurement.failure_code == Some(FailureCode::ProxyTransport)
    )));
}

struct SelectionFailure;
#[async_trait]
impl Probe for SelectionFailure {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        Ok(10)
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        Ok(20)
    }
    async fn select(&self, _: &str) -> Result<()> {
        Err(Error::Network)
    }
}

#[tokio::test]
async fn selector_failure_is_typed_in_progress_and_result() {
    let mut events = Vec::new();
    let error = selection::run(
        &SelectionFailure,
        &servers()[..1],
        None,
        1,
        &CancellationToken::new(),
        |event| events.push(event),
    )
    .await
    .unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::SelectorSwitch));
    assert!(events.iter().any(|event| matches!(
        event,
        Progress::SelectionFailed(id, FailureCode::SelectorSwitch) if id == "A"
    )));
}
/// Every server answers; the log says what the switches asked of the selector and of the rules.
struct Rules {
    calls: Mutex<Vec<String>>,
    refuse: bool,
}
#[async_trait]
impl Probe for Rules {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        Ok(10)
    }
    async fn proxy(&self, s: &Server) -> Result<u64> {
        // B is the faster one, so that the cycle ends on it.
        Ok(if s.id == "B" { 30 } else { 90 })
    }
    async fn select(&self, id: &str) -> Result<()> {
        self.calls.lock().unwrap().push(format!("select:{id}"));
        Ok(())
    }
    async fn routes(&self, mode: &str) -> Result<()> {
        self.calls.lock().unwrap().push(format!("routes:{mode}"));
        if self.refuse {
            return Err(Error::failure(FailureCode::SelectorSwitch, "no"));
        }
        Ok(())
    }
}
fn with_rules(mut servers: Vec<Server>) -> (Vec<Server>, String) {
    let routes = ServerRoutes {
        block: vec!["udp:443".into()],
        direct: vec!["domain:a.example".into()],
    };
    let mode = routes.mode();
    servers[0].routes = Some(routes);
    (servers, mode)
}
#[tokio::test]
async fn a_switch_sets_the_rules_of_the_server_right_after_the_selector() {
    let (servers, mode) = with_rules(servers());
    let probe = Rules {
        calls: Mutex::new(vec![]),
        refuse: false,
    };
    let result = selection::run(
        &probe,
        &servers[..2],
        None,
        10,
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(result.selected, "B");
    // First A, which brings rules; then B, which brings none: its mode is the one with no rules.
    assert_eq!(
        *probe.calls.lock().unwrap(),
        [
            "select:A".to_owned(),
            format!("routes:{mode}"),
            "select:B".to_owned(),
            format!("routes:{NO_ROUTES_MODE}"),
        ]
    );
}
#[tokio::test]
async fn rules_that_cannot_be_set_fail_the_switch_like_the_selector_does() {
    let (servers, _) = with_rules(servers());
    let probe = Rules {
        calls: Mutex::new(vec![]),
        refuse: true,
    };
    let mut events = Vec::new();
    let error = selection::run(
        &probe,
        &servers[..1],
        None,
        1,
        &CancellationToken::new(),
        |event| events.push(event),
    )
    .await
    .unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::SelectorSwitch));
    assert!(events.iter().any(|event| matches!(
        event,
        Progress::SelectionFailed(id, FailureCode::SelectorSwitch) if id == "A"
    )));
    // Nothing was announced as chosen.
    assert!(!events
        .iter()
        .any(|event| matches!(event, Progress::Selected(..))));
}
#[tokio::test]
async fn cancellation_interrupts_probe() {
    let f = Fake {
        calls: Mutex::new(vec![]),
        release: Some(Notify::new()),
    };
    let token = CancellationToken::new();
    let list = servers();
    let run = selection::run(&f, &list, None, 10, &token, |_| {});
    tokio::pin!(run);
    tokio::select! {_=&mut run=>panic!("must wait"),_=tokio::task::yield_now()=>{}}
    token.cancel();
    assert!(matches!(run.await, Err(Error::Cancelled)));
    assert!(!f
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|c| c.starts_with("select:")));
}
struct EqualProxy(Fake);
#[async_trait]
impl Probe for EqualProxy {
    async fn endpoint(&self, s: &Server) -> Result<u64> {
        self.0.endpoint(s).await
    }
    async fn proxy(&self, s: &Server) -> Result<u64> {
        self.0.proxy(s).await.map(|_| 60)
    }
    async fn select(&self, id: &str) -> Result<()> {
        self.0.select(id).await
    }
}
#[tokio::test]
async fn equal_proxy_latency_keeps_first_and_zero_concurrency_is_rejected() {
    let probe = EqualProxy(Fake {
        calls: Mutex::new(vec![]),
        release: None,
    });
    let mut list = servers();
    list.reverse();
    let result = selection::run(&probe, &list, None, 2, &CancellationToken::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(result.selected, "B");
    assert_eq!(
        *probe.0.calls.lock().unwrap(),
        [
            "probe:A", "probe:B", "select:B", "probe:C", "probe:D", "probe:B", "probe:B",
            "probe:C", "probe:C", "probe:D", "probe:D"
        ]
    );
    assert!(matches!(
        selection::run(&probe, &list, None, 0, &CancellationToken::new(), |_| {}).await,
        Err(Error::Invalid(_))
    ));
}

/// Cancels the cycle in the middle of a switch: right after the selector has moved.
struct CutsInBetween {
    calls: Mutex<Vec<String>>,
    token: CancellationToken,
}
#[async_trait]
impl Probe for CutsInBetween {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        Ok(10)
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        Ok(30)
    }
    async fn select(&self, id: &str) -> Result<()> {
        self.calls.lock().unwrap().push(format!("select:{id}"));
        self.token.cancel();
        // The reply takes a moment, as a real one does: the cancellation is seen in between.
        tokio::task::yield_now().await;
        Ok(())
    }
    async fn routes(&self, mode: &str) -> Result<()> {
        self.calls.lock().unwrap().push(format!("routes:{mode}"));
        Ok(())
    }
}
#[tokio::test]
async fn a_cycle_cancelled_after_the_selector_moved_still_sets_the_rules_that_go_with_it() {
    let (servers, mode) = with_rules(servers());
    let token = CancellationToken::new();
    let probe = CutsInBetween {
        calls: Mutex::new(vec![]),
        token: token.clone(),
    };
    let _ = selection::run(&probe, &servers[..1], None, 1, &token, |_| {}).await;
    assert_eq!(
        *probe.calls.lock().unwrap(),
        ["select:A".to_owned(), format!("routes:{mode}")],
        "the selector and its rules are never left apart by a cancellation"
    );
}

/// The API never answers the selector.
struct Hangs;
#[async_trait]
impl Probe for Hangs {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        Ok(10)
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        Ok(30)
    }
    async fn select(&self, _: &str) -> Result<()> {
        std::future::pending().await
    }
}
#[tokio::test(start_paused = true)]
async fn a_switch_that_does_not_answer_does_not_hold_the_cancellation() {
    let servers = servers();
    let token = CancellationToken::new();
    let cancel = {
        let token = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            token.cancel();
        })
    };
    let started = Instant::now();
    let result = selection::run(&Hangs, &servers[..1], None, 1, &token, |_| {}).await;
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    // The cancellation came after a second and the switch was given two more.
    assert!(
        started.elapsed() <= Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    cancel.await.unwrap();
}
