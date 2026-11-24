//! The pace of a comparison: a random pause between servers while a connection works, no more than
//! the allowed number of connections at once, a random order, and the server in use asked again
//! while a long comparison goes on. The clock is paused, so the pauses cost nothing.
use async_trait::async_trait;
use rift_core::{
    jitter::{Jitter, Source},
    model::Server,
    parser::parse_uri,
    selection::{self, Options, Probe, Progress},
    Error, Result,
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

fn server(id: &str, port: u16) -> Server {
    let mut s = parse_uri(&format!("trojan://secret@example.com:{port}#{id}"), None).unwrap();
    s.id = id.into();
    s
}
fn servers(count: usize) -> Vec<Server> {
    (0..count)
        .map(|i| server(&format!("S{i}"), 443 + i as u16))
        .collect()
}

/// Every call with the time since the start; the answers of the proxy per server in order, then
/// the last one forever (`None` fails). Distances: the closer the lower the number.
struct Timed {
    start: Instant,
    calls: Mutex<Vec<(String, Duration)>>,
    proxy: Mutex<HashMap<String, Vec<Option<u64>>>>,
    /// How long one connection to a server takes.
    connect_takes: Duration,
    in_flight: AtomicUsize,
    most_in_flight: AtomicUsize,
    /// Servers whose distance cannot be measured.
    unreachable: Mutex<Vec<String>>,
    /// Distances of a server in order, then the last one forever; the others answer 10 + number.
    distances: Mutex<HashMap<String, Vec<u64>>>,
}
impl Timed {
    fn new(connect_takes: Duration, proxy: &[(&str, &[Option<u64>])]) -> Self {
        Self {
            start: Instant::now(),
            calls: Mutex::new(vec![]),
            proxy: Mutex::new(
                proxy
                    .iter()
                    .map(|(id, answers)| ((*id).to_owned(), answers.to_vec()))
                    .collect(),
            ),
            connect_takes,
            in_flight: AtomicUsize::new(0),
            most_in_flight: AtomicUsize::new(0),
            unreachable: Mutex::new(vec![]),
            distances: Mutex::new(HashMap::new()),
        }
    }
    fn record(&self, what: String) {
        self.calls
            .lock()
            .unwrap()
            .push((what, Instant::now() - self.start));
    }
    fn at(&self, what: &str) -> Vec<Duration> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(call, _)| call == what)
            .map(|(_, at)| *at)
            .collect()
    }
    fn order(&self, prefix: &str) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(call, _)| call.strip_prefix(prefix).map(str::to_owned))
            .collect()
    }
}
#[async_trait]
impl Probe for Timed {
    async fn endpoint(&self, s: &Server) -> Result<u64> {
        self.record(format!("endpoint:{}", s.id));
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.most_in_flight.fetch_max(now, Ordering::SeqCst);
        // The server in use is near: asking it first costs no time.
        if s.id != "S0" {
            tokio::time::sleep(self.connect_takes).await;
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        if self.unreachable.lock().unwrap().contains(&s.id) {
            return Err(Error::Network);
        }
        if let Some(queue) = self.distances.lock().unwrap().get_mut(&s.id) {
            return Ok(if queue.len() > 1 {
                queue.remove(0)
            } else {
                queue[0]
            });
        }
        Ok(10 + s.id[1..].parse::<u64>().unwrap())
    }
    async fn proxy(&self, s: &Server) -> Result<u64> {
        self.record(format!("proxy:{}", s.id));
        let mut all = self.proxy.lock().unwrap();
        let queue = all.get_mut(&s.id).unwrap();
        let answer = if queue.len() > 1 {
            queue.remove(0)
        } else {
            queue[0]
        };
        answer.ok_or(Error::Network)
    }
    async fn select(&self, id: &str) -> Result<()> {
        self.record(format!("select:{id}"));
        Ok(())
    }
}

async fn run(
    probe: &Timed,
    list: &[Server],
    current: Option<&str>,
    options: Options,
) -> (Result<selection::Outcome>, Vec<Progress>) {
    let mut events = vec![];
    let result = selection::run_with(
        probe,
        list,
        current,
        options,
        &CancellationToken::new(),
        |e| events.push(e),
    )
    .await;
    (result, events)
}
fn paused(concurrency: usize, pause: Duration) -> Options {
    Options {
        jitter: Jitter::pausing(pause),
        ..Options::with_concurrency(concurrency)
    }
}
fn secs(list: &[Duration]) -> Vec<u64> {
    list.iter().map(|d| d.as_secs()).collect()
}
fn endpoint_names(list: &[Server]) -> Vec<String> {
    list.iter().map(|s| format!("endpoint:{}", s.id)).collect()
}

#[tokio::test(start_paused = true)]
async fn while_a_connection_works_each_server_starts_one_pause_after_the_one_before() {
    let list = servers(5);
    let probe = Timed::new(Duration::ZERO, &[("S0", &[Some(50)]), ("S1", &[Some(80)])]);
    probe
        .proxy
        .lock()
        .unwrap()
        .extend((2..5).map(|i| (format!("S{i}"), vec![Some(90)])));
    // S0 is in use and answers, so a connection works from the start.
    let (result, _) = run(&probe, &list, Some("S0"), paused(3, Duration::from_secs(2))).await;
    assert_eq!(result.unwrap().selected, "S0");
    let starts: Vec<u64> = (1..5)
        .map(|i| secs(&probe.at(&format!("endpoint:S{i}")))[0])
        .collect();
    assert_eq!(starts, [2, 4, 6, 8], "one pause before each, one at a time");
    // The same pause before each proxy measurement that follows.
    let proxied = secs(
        &(1..5)
            .map(|i| probe.at(&format!("proxy:S{i}"))[0])
            .collect::<Vec<_>>(),
    );
    assert_eq!(proxied, [10, 12, 14, 16]);
}

#[tokio::test(start_paused = true)]
async fn no_more_connections_than_allowed_are_open_at_once_and_the_pauses_still_follow_each_other()
{
    let list = servers(7);
    // Each connection takes longer than the pauses of three servers together.
    let probe = Timed::new(Duration::from_secs(20), &[("S0", &[Some(50)])]);
    probe
        .proxy
        .lock()
        .unwrap()
        .extend((1..7).map(|i| (format!("S{i}"), vec![Some(90)])));
    let (result, _) = run(&probe, &list, Some("S0"), paused(3, Duration::from_secs(2))).await;
    assert_eq!(result.unwrap().selected, "S0");
    assert_eq!(probe.most_in_flight.load(Ordering::SeqCst), 3);
    let starts = secs(
        &(1..7)
            .map(|i| probe.at(&format!("endpoint:S{i}"))[0])
            .collect::<Vec<_>>(),
    );
    // Three start two seconds apart; the next waits for a free place (the first ends at 22),
    // and its own pause begins then.
    assert_eq!(&starts[..3], [2, 4, 6]);
    assert!(starts[3] >= 24, "{starts:?}");
    assert!(
        starts.windows(2).all(|pair| pair[1] > pair[0]),
        "{starts:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn with_nothing_working_there_is_no_pause_until_the_first_server_that_answers_is_taken() {
    let list = servers(4);
    let probe = Timed::new(
        Duration::ZERO,
        &[
            ("S0", &[None]), // in use, dead
            ("S1", &[Some(50)]),
            ("S2", &[Some(60)]),
            ("S3", &[Some(70)]),
        ],
    );
    let (result, _) = run(&probe, &list, Some("S0"), paused(3, Duration::from_secs(2))).await;
    assert_eq!(result.unwrap().selected, "S1");
    // Every distance at once, the first proxy answer at once, the switch at once...
    assert_eq!(
        secs(
            &(1..4)
                .map(|i| probe.at(&format!("endpoint:S{i}"))[0])
                .collect::<Vec<_>>()
        ),
        [0, 0, 0]
    );
    assert_eq!(secs(&probe.at("proxy:S1"))[0], 0);
    assert_eq!(secs(&probe.at("select:S1")), [0]);
    // ...and only then, with a connection, the pace.
    assert_eq!(secs(&probe.at("proxy:S2"))[0], 2);
    assert_eq!(secs(&probe.at("proxy:S3"))[0], 4);
}

#[tokio::test(start_paused = true)]
async fn the_servers_are_measured_in_the_order_the_source_gives_not_the_order_they_came_in() {
    // Later servers get lower keys: the reverse order.
    struct Reverse(AtomicU64);
    impl Source for Reverse {
        fn offset_ms(&self) -> i64 {
            0
        }
        fn order_key(&self) -> u64 {
            u64::MAX - self.0.fetch_add(1, Ordering::SeqCst)
        }
        fn orders(&self) -> bool {
            true
        }
    }
    let list = servers(4);
    let probe = Timed::new(Duration::ZERO, &[("S0", &[Some(50)])]);
    probe
        .proxy
        .lock()
        .unwrap()
        .extend((1..4).map(|i| (format!("S{i}"), vec![Some(90)])));
    let options = Options {
        jitter: Jitter::with_source(Arc::new(Reverse(AtomicU64::new(0)))),
        ..Options::with_concurrency(1)
    };
    run(&probe, &list, Some("S0"), options).await.0.unwrap();
    assert_eq!(
        probe.order("endpoint:")[..4],
        ["S0", "S3", "S2", "S1"],
        "S0 is in use and was asked first"
    );
    // Without a source that orders them, the order is the one they came in.
    let probe = Timed::new(Duration::ZERO, &[("S0", &[Some(50)])]);
    probe
        .proxy
        .lock()
        .unwrap()
        .extend((1..4).map(|i| (format!("S{i}"), vec![Some(90)])));
    run(&probe, &list, Some("S0"), Options::with_concurrency(1))
        .await
        .0
        .unwrap();
    let asked: Vec<String> = probe
        .order("endpoint:")
        .into_iter()
        .take(4)
        .map(|id| format!("endpoint:{id}"))
        .collect();
    assert_eq!(asked[1..], endpoint_names(&list[1..])[..]);
}

#[tokio::test(start_paused = true)]
async fn the_server_in_use_is_asked_again_during_a_long_comparison_and_its_loss_ends_the_pauses() {
    let list = servers(6);
    let probe = Timed::new(
        Duration::ZERO,
        &[
            // Answers at the start, then nothing: the connection is lost during the comparison.
            ("S0", &[Some(50), None]),
            ("S1", &[Some(60)]),
            ("S2", &[Some(70)]),
            ("S3", &[Some(80)]),
            ("S4", &[Some(90)]),
            ("S5", &[Some(95)]),
        ],
    );
    let (result, events) = run(
        &probe,
        &list,
        Some("S0"),
        paused(1, Duration::from_secs(10)),
    )
    .await;
    // S0 is asked again once 15 s have passed since it was asked (at the third pause, after 20 s),
    // twice in a row it does not answer, and from then on there are no pauses.
    let starts = secs(
        &(1..6)
            .map(|i| probe.at(&format!("endpoint:S{i}"))[0])
            .collect::<Vec<_>>(),
    );
    assert_eq!(starts, [10, 20, 20, 20, 20], "{starts:?}");
    assert_eq!(
        probe.at("proxy:S0").len(),
        3,
        "once at the start, twice at the check"
    );
    // The first server that answers takes its place at once, and the result is never S0.
    let outcome = result.unwrap();
    assert_ne!(outcome.selected, "S0");
    assert!(events
        .iter()
        .any(|e| matches!(e, Progress::Selected(id, _) if id != "S0")));
    assert_eq!(secs(&probe.at("select:S1")), [20]);
}

#[tokio::test(start_paused = true)]
async fn a_server_in_use_that_still_answers_is_not_replaced_by_the_checks() {
    let list = servers(5);
    let probe = Timed::new(Duration::ZERO, &[("S0", &[Some(50)])]);
    probe
        .proxy
        .lock()
        .unwrap()
        .extend((1..5).map(|i| (format!("S{i}"), vec![Some(90)])));
    let (result, _) = run(
        &probe,
        &list,
        Some("S0"),
        paused(1, Duration::from_secs(10)),
    )
    .await;
    assert_eq!(result.unwrap().selected, "S0");
    assert!(probe.at("proxy:S0").len() > 1, "asked again meanwhile");
    assert!(probe.at("select:S1").is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_cycle_does_not_wait_for_the_pause() {
    let list = servers(3);
    let probe = Timed::new(Duration::ZERO, &[("S0", &[Some(50)])]);
    probe
        .proxy
        .lock()
        .unwrap()
        .extend((1..3).map(|i| (format!("S{i}"), vec![Some(90)])));
    let cancel = CancellationToken::new();
    let later = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        later.cancel();
    });
    let started = Instant::now();
    let result = selection::run_with(
        &probe,
        &list,
        Some("S0"),
        paused(3, Duration::from_secs(1000)),
        &cancel,
        |_| {},
    )
    .await;
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(started.elapsed(), Duration::from_secs(1));
}

/// What a pass of rechecking told: (server, distance answered, answer through it, if asked).
fn told(events: &[Progress]) -> Vec<(String, bool, Option<bool>)> {
    let mut told: Vec<(String, bool, Option<bool>)> = vec![];
    for event in events {
        match event {
            Progress::Endpoint(id, m) => told.push((id.clone(), m.latency_ms.is_some(), None)),
            Progress::Proxy(id, m) => {
                told.iter_mut().find(|(s, _, _)| s == id).unwrap().2 = Some(m.latency_ms.is_some())
            }
            _ => panic!("a recheck only measures: {event:?}"),
        }
    }
    told.sort();
    told
}

#[tokio::test(start_paused = true)]
async fn a_recheck_asks_the_answer_through_a_server_only_when_its_distance_answered() {
    let list = servers(4);
    let probe = Timed::new(
        Duration::ZERO,
        &[("S1", &[Some(50)]), ("S2", &[Some(50)]), ("S3", &[None])],
    );
    probe.unreachable.lock().unwrap().push("S1".into());
    let mut events = vec![];
    selection::recheck(
        &probe,
        &list[1..],
        3,
        &Jitter::none(),
        &CancellationToken::new(),
        |e| events.push(e),
    )
    .await
    .unwrap();
    assert_eq!(
        told(&events),
        [
            ("S1".into(), false, None),
            ("S2".into(), true, Some(true)),
            ("S3".into(), true, Some(false)),
        ]
    );
    // One question each; no retry, no second sample, no selection.
    assert_eq!(probe.at("proxy:S2").len(), 1);
    assert_eq!(probe.at("proxy:S3").len(), 1);
    assert!(probe.at("proxy:S1").is_empty());
    assert!(probe.order("select:").is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_recheck_has_its_own_three_connections_and_a_pause_before_each() {
    let list = servers(8);
    let probe = Timed::new(Duration::from_secs(20), &[]);
    probe
        .proxy
        .lock()
        .unwrap()
        .extend((1..8).map(|i| (format!("S{i}"), vec![Some(90)])));
    selection::recheck(
        &probe,
        &list[1..],
        3,
        &Jitter::pausing(Duration::from_secs(2)),
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(probe.most_in_flight.load(Ordering::SeqCst), 3);
    let starts = secs(
        &(1..8)
            .map(|i| probe.at(&format!("endpoint:S{i}"))[0])
            .collect::<Vec<_>>(),
    );
    assert_eq!(&starts[..3], [2, 4, 6]);
    assert!(
        starts.windows(2).all(|pair| pair[1] > pair[0]),
        "{starts:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_recheck_ends_at_once_and_zero_connections_are_refused() {
    let list = servers(4);
    let probe = Timed::new(Duration::ZERO, &[]);
    probe
        .proxy
        .lock()
        .unwrap()
        .extend((1..4).map(|i| (format!("S{i}"), vec![Some(90)])));
    let cancel = CancellationToken::new();
    let later = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        later.cancel();
    });
    let started = Instant::now();
    let result = selection::recheck(
        &probe,
        &list[1..],
        3,
        &Jitter::pausing(Duration::from_secs(1000)),
        &cancel,
        |_| {},
    )
    .await;
    assert!(matches!(result, Err(Error::Cancelled)));
    assert_eq!(started.elapsed(), Duration::from_secs(1));
    assert!(matches!(
        selection::recheck(
            &probe,
            &list[1..],
            0,
            &Jitter::none(),
            &CancellationToken::new(),
            |_| {}
        )
        .await,
        Err(Error::Invalid(_))
    ));
}

fn holding(ids: &[&str]) -> Options {
    Options {
        held_back: ids.iter().map(|id| (*id).to_owned()).collect(),
        ..Options::with_concurrency(3)
    }
}

#[tokio::test(start_paused = true)]
async fn held_back_servers_wait_while_the_server_in_use_works_or_others_are_left() {
    let list = servers(4);
    let all_work = [
        ("S0", [Some(50)]),
        ("S1", [Some(60)]),
        ("S2", [Some(70)]),
        ("S3", [Some(80)]),
    ];
    let probe_for = |dead_in_use: bool| {
        let probe = Timed::new(Duration::ZERO, &[]);
        probe
            .proxy
            .lock()
            .unwrap()
            .extend(all_work.iter().map(|(id, answers)| {
                let answers = if dead_in_use && *id == "S0" {
                    vec![None]
                } else {
                    answers.to_vec()
                };
                ((*id).to_owned(), answers)
            }));
        probe
    };
    // The one in use works: the comparison leaves S3 alone.
    let probe = probe_for(false);
    let (result, _) = run(&probe, &list, Some("S0"), holding(&["S3"])).await;
    assert_eq!(result.unwrap().selected, "S0");
    assert!(probe.at("endpoint:S3").is_empty());
    // It does not work, but S1 and S2 are there to try: S3 still waits.
    let probe = probe_for(true);
    let (result, _) = run(&probe, &list, Some("S0"), holding(&["S3"])).await;
    assert_eq!(result.unwrap().selected, "S1");
    assert!(probe.at("endpoint:S3").is_empty());
}

#[tokio::test(start_paused = true)]
async fn held_back_servers_are_measured_when_nothing_in_use_works_and_nobody_else_is_left() {
    let list = servers(3);
    let probe = Timed::new(
        Duration::ZERO,
        &[("S0", &[None]), ("S1", &[Some(60)]), ("S2", &[Some(70)])],
    );
    let (result, _) = run(&probe, &list, Some("S0"), holding(&["S1", "S2"])).await;
    assert_eq!(result.unwrap().selected, "S1");
    assert_eq!(
        probe.at("endpoint:S1").len(),
        3,
        "the first answer and the two more of the comparison"
    );
    // With nothing selected at all it is the same.
    let probe = Timed::new(
        Duration::ZERO,
        &[
            ("S0", &[Some(50)]),
            ("S1", &[Some(60)]),
            ("S2", &[Some(70)]),
        ],
    );
    let (result, _) = run(&probe, &list, None, holding(&["S0", "S1", "S2"])).await;
    assert_eq!(result.unwrap().selected, "S0");
    // The server in use is never held back, even when it is named.
    let probe = Timed::new(
        Duration::ZERO,
        &[
            ("S0", &[Some(50)]),
            ("S1", &[Some(60)]),
            ("S2", &[Some(70)]),
        ],
    );
    let (result, _) = run(&probe, &list, Some("S0"), holding(&["S0"])).await;
    assert_eq!(result.unwrap().selected, "S0");
    assert_eq!(
        probe.at("proxy:S0").len(),
        3,
        "asked once as the one in use, then twice more for the comparison, not as a candidate too"
    );
}

#[tokio::test(start_paused = true)]
async fn the_server_in_use_is_asked_again_every_fifteen_seconds_and_not_at_every_pause() {
    // Pauses of 10 s: the checks fall at the pauses that come 15 s or more after the last one.
    let list = servers(6);
    let probe = Timed::new(Duration::ZERO, &[("S0", &[Some(50)])]);
    probe
        .proxy
        .lock()
        .unwrap()
        .extend((1..6).map(|i| (format!("S{i}"), vec![Some(90)])));
    run(
        &probe,
        &list,
        Some("S0"),
        paused(1, Duration::from_secs(10)),
    )
    .await
    .0
    .unwrap();
    let asks = secs(&probe.at("proxy:S0"));
    assert_eq!(asks[..3], [0, 20, 40], "{asks:?}");
}

#[tokio::test(start_paused = true)]
async fn a_pause_that_ends_exactly_fifteen_seconds_after_the_last_question_asks_again() {
    let list = servers(6);
    let probe = Timed::new(Duration::ZERO, &[("S0", &[Some(50)])]);
    probe
        .proxy
        .lock()
        .unwrap()
        .extend((1..6).map(|i| (format!("S{i}"), vec![Some(90)])));
    run(
        &probe,
        &list,
        Some("S0"),
        paused(1, Duration::from_secs(15)),
    )
    .await
    .0
    .unwrap();
    let asks = secs(&probe.at("proxy:S0"));
    assert_eq!(asks[..3], [0, 15, 30], "{asks:?}");
}

#[tokio::test(start_paused = true)]
async fn the_distance_of_each_server_is_the_middle_of_its_own_three_measures() {
    let list = servers(3);
    let probe = Timed::new(Duration::ZERO, &[("S0", &[Some(50)])]);
    probe.proxy.lock().unwrap().extend([
        ("S1".to_owned(), vec![Some(60)]),
        ("S2".to_owned(), vec![Some(70)]),
    ]);
    // The first answer of S1 is the middle one: only that server's own first answer gives 40.
    probe
        .distances
        .lock()
        .unwrap()
        .insert("S1".into(), vec![40, 10, 90]);
    probe
        .distances
        .lock()
        .unwrap()
        .insert("S2".into(), vec![11]);
    let (_, events) = run(&probe, &list, Some("S0"), Options::with_concurrency(1)).await;
    let median_of = |id: &str| {
        events.iter().find_map(|e| match e {
            Progress::Median {
                id: found,
                endpoint_ms,
                ..
            } if found == id => Some(*endpoint_ms),
            _ => None,
        })
    };
    assert_eq!(median_of("S1"), Some(Some(40)));
    assert_eq!(median_of("S2"), Some(Some(11)));
}

struct RevokedGateway;
#[async_trait]
impl Probe for RevokedGateway {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        Ok(1)
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        Err(Error::Cancelled)
    }
    async fn select(&self, _: &str) -> Result<()> {
        Ok(())
    }
}
#[tokio::test]
async fn revoked_gateway_cancels_recheck_without_remote_failure_progress() {
    let mut events = Vec::new();
    let result = selection::recheck(
        &RevokedGateway,
        &servers(1),
        1,
        &Jitter::none(),
        &CancellationToken::new(),
        |event| events.push(event),
    )
    .await;
    assert!(matches!(result, Err(Error::Cancelled)));
    assert!(events.is_empty());
}
