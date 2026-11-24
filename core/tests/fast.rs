//! The fast check: one answer from each server of the pool and from the server in use, and a
//! second look at the ones that changed. Nothing is switched here, the verdict says what to do.
use async_trait::async_trait;
use rift_core::{
    failure::FailureCode,
    fast::{self, Watched},
    jitter::Jitter,
    model::{Server, Settings},
    parser::parse_uri,
    pool::{Health, Tolerance},
    selection::{Probe, Progress},
    Error, Result,
};
use std::{collections::HashMap, sync::Mutex, time::Duration};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

fn server(id: &str) -> Server {
    let mut s = parse_uri(
        &format!("trojan://secret@{}.example.com:443#{id}", id.to_lowercase()),
        None,
    )
    .unwrap();
    s.id = id.into();
    s
}
#[derive(Clone, Copy)]
enum Answer {
    Ms(u64),
    Timeout,
    Failed,
}
use Answer::{Failed, Ms, Timeout};
struct Scripted {
    answers: Mutex<HashMap<String, Vec<Answer>>>,
    calls: Mutex<Vec<(String, Instant)>>,
    running: Mutex<usize>,
    peak: Mutex<usize>,
    each_takes: Duration,
}
impl Scripted {
    fn new(script: &[(&str, &[Answer])]) -> Self {
        Self::taking(script, Duration::ZERO)
    }
    fn taking(script: &[(&str, &[Answer])], each_takes: Duration) -> Self {
        Self {
            answers: Mutex::new(
                script
                    .iter()
                    .map(|(id, a)| ((*id).to_owned(), a.to_vec()))
                    .collect(),
            ),
            calls: Mutex::new(vec![]),
            running: Mutex::new(0),
            peak: Mutex::new(0),
            each_takes,
        }
    }
    fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(c, _)| c.clone())
            .collect()
    }
    fn count(&self, id: &str) -> usize {
        self.calls().iter().filter(|c| *c == id).count()
    }
}
#[async_trait]
impl Probe for Scripted {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        panic!("a fast check measures no distances");
    }
    async fn proxy(&self, s: &Server) -> Result<u64> {
        self.calls
            .lock()
            .unwrap()
            .push((s.id.clone(), Instant::now()));
        {
            let mut running = self.running.lock().unwrap();
            *running += 1;
            let mut peak = self.peak.lock().unwrap();
            *peak = (*peak).max(*running);
        }
        if !self.each_takes.is_zero() {
            tokio::time::sleep(self.each_takes).await;
        }
        *self.running.lock().unwrap() -= 1;
        let answer = {
            let mut all = self.answers.lock().unwrap();
            let queue = all.get_mut(&s.id).unwrap();
            if queue.len() > 1 {
                queue.remove(0)
            } else {
                queue[0]
            }
        };
        match answer {
            Ms(ms) => Ok(ms),
            Timeout => Err(Error::failure(FailureCode::ProxyTimeout, "timeout")),
            Failed => Err(Error::Network),
        }
    }
    async fn select(&self, _: &str) -> Result<()> {
        panic!("a fast check switches nothing");
    }
}
fn tolerance() -> Tolerance {
    Tolerance::from_settings(&Settings::default()) // 50 %, 50 ms, 1500 ms
}
async fn check(
    probe: &Scripted,
    list: &[(&Server, Option<u64>)],
    current: Option<&str>,
) -> (rift_core::Result<fast::Report>, Vec<Progress>) {
    check_with(probe, list, current, 3, &Jitter::none()).await
}
async fn check_with(
    probe: &Scripted,
    list: &[(&Server, Option<u64>)],
    current: Option<&str>,
    concurrency: usize,
    jitter: &Jitter,
) -> (rift_core::Result<fast::Report>, Vec<Progress>) {
    let watched: Vec<Watched> = list
        .iter()
        .map(|(server, base_ms)| Watched {
            server,
            base_ms: *base_ms,
        })
        .collect();
    let mut events = vec![];
    let result = fast::run_fast(
        probe,
        &watched,
        current,
        tolerance(),
        concurrency,
        jitter,
        &CancellationToken::new(),
        |e| events.push(e),
    )
    .await;
    (result, events)
}
fn health(report: &fast::Report, id: &str) -> Health {
    report
        .readings
        .iter()
        .find(|r| r.id == id)
        .unwrap_or_else(|| panic!("no reading of {id}"))
        .judgement
        .health
}

#[tokio::test]
async fn servers_that_answer_as_before_are_asked_once_each_and_nothing_else() {
    let (a, b, c) = (server("A"), server("B"), server("C"));
    let probe = Scripted::new(&[("A", &[Ms(100)]), ("B", &[Ms(130)]), ("C", &[Ms(170)])]);
    let (report, events) = check(
        &probe,
        &[(&a, Some(100)), (&b, Some(120)), (&c, Some(160))],
        Some("A"),
    )
    .await;
    let report = report.unwrap();
    assert!(report.all_fine());
    assert!(!report.current_lost);
    assert_eq!(probe.calls().len(), 3, "one answer from each");
    assert_eq!(probe.calls()[0], "A", "the server in use is asked first");
    for id in ["A", "B", "C"] {
        assert_eq!(health(&report, id), Health::Fine);
    }
    // The table learns the answers: the one in use as such, the others through their proxy.
    assert!(events
        .iter()
        .any(|e| matches!(e, Progress::Current(id, m) if id == "A" && m.latency_ms == Some(100))));
    for id in ["B", "C"] {
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Progress::Proxy(i, _) if i == id)),
            "{id}"
        );
    }
}

#[tokio::test]
async fn a_slow_first_answer_is_looked_at_twice_more_and_forgiven_when_the_middle_is_fine() {
    let (a, b) = (server("A"), server("B"));
    let probe = Scripted::new(&[("A", &[Ms(100)]), ("B", &[Ms(400), Ms(130), Ms(125)])]);
    let (report, events) = check(&probe, &[(&a, Some(100)), (&b, Some(120))], Some("A")).await;
    let report = report.unwrap();
    assert!(report.all_fine());
    assert_eq!(probe.count("B"), 3);
    assert_eq!(probe.count("A"), 1);
    // The table is told once, with the middle of the three, never with the first answer that the
    // other two overruled.
    let told: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, Progress::Proxy(id, _) if id == "B"))
        .collect();
    assert!(
        matches!(told.as_slice(), [Progress::Proxy(_, m)] if m.latency_ms == Some(130) && m.failure_code.is_none()),
        "{told:?}"
    );
    assert!(events
        .iter()
        .all(|e| !matches!(e, Progress::Proxy(_, m) if m.latency_ms == Some(400))));
}

#[tokio::test]
async fn a_first_miss_that_the_next_two_answers_overrule_never_marks_the_server_failed() {
    // The first answer is lost, the other two come: the server works. Nothing the check says of it
    // may be a failure, or the table and the list of servers that do not answer follow the first
    // probe instead of the verdict.
    let (a, b) = (server("A"), server("B"));
    let probe = Scripted::new(&[("A", &[Ms(100)]), ("B", &[Timeout, Ms(120), Ms(125)])]);
    let (report, events) = check(&probe, &[(&a, Some(100)), (&b, Some(120))], Some("A")).await;
    let report = report.unwrap();
    assert_eq!(probe.count("B"), 3);
    assert!(report.all_fine(), "{:?}", report.readings);
    let told: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Progress::Proxy(id, m) | Progress::Current(id, m) if id == "B" => Some(m.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(told.len(), 1, "{told:?}");
    assert_eq!(told[0].latency_ms, Some(125));
    assert_eq!(told[0].failure_code, None);
}

#[tokio::test]
async fn a_server_in_use_is_told_as_such_with_the_verdict_too() {
    let (a, b) = (server("A"), server("B"));
    let probe = Scripted::new(&[("A", &[Ms(400), Ms(105), Ms(110)]), ("B", &[Ms(120)])]);
    let (report, events) = check(&probe, &[(&a, Some(100)), (&b, Some(120))], Some("A")).await;
    assert!(report.unwrap().all_fine());
    let told: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, Progress::Current(id, _) | Progress::Proxy(id, _) if id == "A"))
        .collect();
    assert!(
        matches!(told.as_slice(), [Progress::Current(_, m)] if m.latency_ms == Some(110)),
        "{told:?}"
    );
}

#[tokio::test]
async fn a_server_that_stays_slow_is_worse() {
    let (a, b) = (server("A"), server("B"));
    let probe = Scripted::new(&[("A", &[Ms(100)]), ("B", &[Ms(400), Ms(380), Ms(390)])]);
    let (report, _) = check(&probe, &[(&a, Some(100)), (&b, Some(120))], Some("A")).await;
    let report = report.unwrap();
    assert!(!report.all_fine());
    assert_eq!(
        health(&report, "B"),
        Health::Worse {
            ms: 390,
            limit_ms: 180
        }
    );
    assert_eq!(health(&report, "A"), Health::Fine);
    assert!(report.lost().is_empty());
}

#[tokio::test]
async fn a_server_that_does_not_answer_is_lost_after_two_more_tries() {
    let (a, b, c) = (server("A"), server("B"), server("C"));
    let probe = Scripted::new(&[("A", &[Ms(100)]), ("B", &[Timeout]), ("C", &[Failed])]);
    let (report, events) = check(
        &probe,
        &[(&a, Some(100)), (&b, Some(120)), (&c, Some(140))],
        Some("A"),
    )
    .await;
    let report = report.unwrap();
    assert_eq!(health(&report, "B"), Health::Lost);
    assert_eq!(health(&report, "C"), Health::Lost);
    assert_eq!(probe.count("B"), 3);
    assert_eq!(probe.count("C"), 3);
    let mut lost = report.lost();
    lost.sort_unstable();
    assert_eq!(lost, ["B", "C"]);
    // The last thing the table is told of a lost server is that it failed, with the reason.
    let last = events
        .iter()
        .rev()
        .find_map(|e| match e {
            Progress::Proxy(id, m) if id == "B" => Some(m.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(last.latency_ms, None);
    assert_eq!(last.failure_code, Some(FailureCode::ProxyTimeout));
}

#[tokio::test]
async fn a_server_that_answered_only_once_of_three_is_lost() {
    let (a, b) = (server("A"), server("B"));
    let probe = Scripted::new(&[("A", &[Ms(100)]), ("B", &[Ms(500), Failed, Failed])]);
    let (report, events) = check(&probe, &[(&a, Some(100)), (&b, Some(120))], Some("A")).await;
    assert_eq!(health(&report.unwrap(), "B"), Health::Lost);
    // It did answer once, but the table must not show it as working in the end.
    let last = events
        .iter()
        .rev()
        .find_map(|e| match e {
            Progress::Proxy(id, m) | Progress::Current(id, m) if id == "B" => Some(m.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(last.latency_ms, None);
}

#[tokio::test]
async fn a_server_in_use_that_does_not_answer_ends_the_check_before_the_others_are_asked() {
    let (a, b, c) = (server("A"), server("B"), server("C"));
    let probe = Scripted::new(&[("A", &[Timeout]), ("B", &[Ms(100)]), ("C", &[Ms(100)])]);
    let (report, events) = check(
        &probe,
        &[(&a, Some(100)), (&b, Some(100)), (&c, Some(100))],
        Some("A"),
    )
    .await;
    let report = report.unwrap();
    assert!(report.current_lost);
    assert!(!report.all_fine());
    // A timeout is tried once more, as for any server in use; then it is over: the pool is for
    // the check that follows.
    assert_eq!(probe.calls(), ["A", "A"]);
    assert!(events
        .iter()
        .any(|e| matches!(e, Progress::Current(id, m) if id == "A" && m.latency_ms.is_none())));
}

#[tokio::test]
async fn a_server_in_use_with_another_failure_is_not_tried_twice() {
    let (a, b) = (server("A"), server("B"));
    let probe = Scripted::new(&[("A", &[Failed]), ("B", &[Ms(100)])]);
    let (report, _) = check(&probe, &[(&a, Some(100)), (&b, Some(100))], Some("A")).await;
    assert!(report.unwrap().current_lost);
    assert_eq!(probe.calls(), ["A"]);
}

#[tokio::test]
async fn a_server_in_use_outside_the_pool_is_watched_like_the_others() {
    // The pool is B, C, D; A was chosen by hand: four servers, the same rules.
    let (a, b, c, d) = (server("A"), server("B"), server("C"), server("D"));
    let probe = Scripted::new(&[
        ("A", &[Ms(500), Ms(520), Ms(510)]),
        ("B", &[Ms(100)]),
        ("C", &[Ms(100)]),
        ("D", &[Ms(100)]),
    ]);
    let (report, _) = check(
        &probe,
        &[
            (&b, Some(100)),
            (&c, Some(100)),
            (&d, Some(100)),
            (&a, Some(200)),
        ],
        Some("A"),
    )
    .await;
    let report = report.unwrap();
    assert_eq!(probe.calls().len(), 6, "A three times, the others once");
    assert_eq!(probe.calls()[0], "A", "the server in use first");
    assert_eq!(
        health(&report, "A"),
        Health::Worse {
            ms: 510,
            limit_ms: 300
        }
    );
    assert_eq!(health(&report, "B"), Health::Fine);
}

#[tokio::test]
async fn a_server_nobody_has_measured_gets_its_delay_from_three_answers() {
    let (a, b) = (server("A"), server("B"));
    let probe = Scripted::new(&[("A", &[Ms(100)]), ("B", &[Ms(300), Ms(310), Ms(320)])]);
    let (report, _) = check(&probe, &[(&a, Some(100)), (&b, None)], Some("A")).await;
    let report = report.unwrap();
    assert!(report.all_fine());
    let b_reading = report.readings.iter().find(|r| r.id == "B").unwrap();
    assert_eq!(
        b_reading.judgement.median_ms,
        Some(310),
        "the base for the checks that follow"
    );
    assert_eq!(probe.count("B"), 3);
}

#[tokio::test]
async fn nothing_in_use_is_no_reason_to_ask_for_one() {
    let (a, b) = (server("A"), server("B"));
    let probe = Scripted::new(&[("A", &[Ms(100)]), ("B", &[Ms(100)])]);
    let (report, _) = check(&probe, &[(&a, Some(100)), (&b, Some(100))], None).await;
    let report = report.unwrap();
    assert!(!report.current_lost);
    assert!(report.all_fine());
    assert_eq!(probe.calls().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn servers_are_asked_after_pauses_and_not_more_than_the_concurrency_at_once() {
    let list: Vec<Server> = ["A", "B", "C", "D", "E"]
        .iter()
        .map(|n| server(n))
        .collect();
    let script: Vec<(&str, &[Answer])> = ["A", "B", "C", "D", "E"]
        .iter()
        .map(|n| (*n, &[Ms(100)] as &[Answer]))
        .collect();
    let probe = Scripted::taking(&script, Duration::from_secs(30));
    let watched: Vec<(&Server, Option<u64>)> = list.iter().map(|s| (s, Some(100))).collect();
    let start = Instant::now();
    let (report, _) = check_with(
        &probe,
        &watched,
        Some("A"),
        2,
        &Jitter::pausing(Duration::from_secs(4)),
    )
    .await;
    assert!(report.unwrap().all_fine());
    assert!(
        *probe.peak.lock().unwrap() <= 2,
        "not more than two at once"
    );
    // The server in use first, then the other four one pause after another.
    let starts: Vec<Duration> = probe
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(_, at)| at.duration_since(start))
        .collect();
    assert_eq!(starts[0], Duration::ZERO);
    for pair in starts[1..].windows(2) {
        assert!(pair[1] - pair[0] >= Duration::from_secs(4), "{pair:?}");
    }
}

#[tokio::test]
async fn a_cancelled_check_is_cancelled() {
    let a = server("A");
    let probe = Scripted::new(&[("A", &[Ms(100)])]);
    let token = CancellationToken::new();
    token.cancel();
    let watched = [Watched {
        server: &a,
        base_ms: Some(100),
    }];
    let result = fast::run_fast(
        &probe,
        &watched,
        Some("A"),
        tolerance(),
        3,
        &Jitter::none(),
        &token,
        |_| {},
    )
    .await;
    assert!(matches!(result, Err(Error::Cancelled)));
}

#[tokio::test]
async fn zero_concurrency_is_refused() {
    let a = server("A");
    let probe = Scripted::new(&[("A", &[Ms(100)])]);
    let (report, _) = check_with(&probe, &[(&a, Some(100))], Some("A"), 0, &Jitter::none()).await;
    assert!(matches!(report, Err(Error::Invalid(_))));
}
