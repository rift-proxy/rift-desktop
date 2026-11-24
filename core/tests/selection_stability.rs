//! A working server is not left for noise: the choice rests on the middle of three answers and
//! on a margin, and a server in use that merely timed out gets one more try.
use async_trait::async_trait;
use rift_core::{
    failure::FailureCode,
    model::Server,
    parser::parse_uri,
    selection::{self, Options, Probe, Progress},
    Error, Result,
};
use std::{collections::HashMap, sync::Mutex};
use tokio_util::sync::CancellationToken;

fn server(id: &str, port: u16) -> Server {
    // A host of its own: servers of one host are one reserve, not many (see `selection_pool`).
    let mut s = parse_uri(
        &format!(
            "trojan://secret@{}.example.com:{port}#{id}",
            id.to_lowercase()
        ),
        None,
    )
    .unwrap();
    s.id = id.into();
    s
}
fn servers(count: usize) -> Vec<Server> {
    (0..count)
        .map(|i| server(&format!("S{i}"), 443 + i as u16))
        .collect()
}

/// Answers of each server in order, then the last one forever; `None` is a timeout, `Err` codes
/// are written as `Failed`.
#[derive(Clone, Copy)]
enum Answer {
    Ms(u64),
    Timeout,
    Failed,
}
struct Scripted {
    answers: Mutex<HashMap<String, Vec<Answer>>>,
    calls: Mutex<Vec<String>>,
    /// Servers whose distance was measured, one entry per measurement.
    distances: Mutex<Vec<String>>,
}
impl Scripted {
    fn new(script: &[(&str, &[Answer])]) -> Self {
        Self {
            answers: Mutex::new(
                script
                    .iter()
                    .map(|(id, answers)| ((*id).to_owned(), answers.to_vec()))
                    .collect(),
            ),
            calls: Mutex::new(vec![]),
            distances: Mutex::new(vec![]),
        }
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn count(&self, id: &str) -> usize {
        self.calls()
            .iter()
            .filter(|c| *c == &format!("probe:{id}"))
            .count()
    }
}
#[async_trait]
impl Probe for Scripted {
    async fn endpoint(&self, s: &Server) -> Result<u64> {
        self.distances.lock().unwrap().push(s.id.clone());
        // Lower index, closer: the scan order is the numeric order of the ids.
        Ok(10 + s.id[1..].parse::<u64>().unwrap())
    }
    async fn proxy(&self, s: &Server) -> Result<u64> {
        self.calls.lock().unwrap().push(format!("probe:{}", s.id));
        let mut all = self.answers.lock().unwrap();
        let queue = all.get_mut(&s.id).unwrap();
        let answer = if queue.len() > 1 {
            queue.remove(0)
        } else {
            queue[0]
        };
        match answer {
            Answer::Ms(ms) => Ok(ms),
            Answer::Timeout => Err(Error::failure(FailureCode::ProxyTimeout, "timeout")),
            Answer::Failed => Err(Error::Network),
        }
    }
    async fn select(&self, id: &str) -> Result<()> {
        self.calls.lock().unwrap().push(format!("select:{id}"));
        Ok(())
    }
}
use Answer::{Failed, Ms, Timeout};

async fn run(
    probe: &Scripted,
    list: &[Server],
    current: Option<&str>,
) -> (rift_core::Result<selection::Outcome>, Vec<Progress>) {
    let mut events = vec![];
    let result = selection::run(probe, list, current, 10, &CancellationToken::new(), |e| {
        events.push(e)
    })
    .await;
    (result, events)
}

#[test]
fn a_challenger_must_be_faster_by_both_margins() {
    let o = Options::with_concurrency(1);
    // (candidate, in use, wins): 30 % and 50 ms, both inclusive.
    for (candidate, current, wins) in [
        (80, 100, false),  // 20 ms, 20 %
        (65, 100, false),  // 35 %, but 35 ms
        (240, 300, false), // 60 ms, but 20 %
        (250, 300, false), // exactly 50 ms, but 16 %
        (210, 300, true),  // exactly 30 % and 90 ms
        (211, 300, false), // 89 ms, 29.7 %
        (111, 160, false), // 49 ms, 30.6 %
        (110, 160, true),  // 50 ms, 31 %
        (150, 300, true),
        (50, 100, true), // exactly 50 ms and 50 %
        (51, 100, false),
        (100, 100, false),
        (120, 100, false),
        (0, 0, false),
    ] {
        assert_eq!(
            o.beats(candidate, current),
            wins,
            "{candidate} against {current}"
        );
    }
    let any = Options {
        threshold_percent: 0,
        threshold_ms: 0,
        ..o.clone()
    };
    assert!(any.beats(99, 100), "no margin: any gain counts");
    assert!(!any.beats(100, 100), "an equal answer never wins");
    let huge = Options {
        threshold_percent: 100,
        threshold_ms: 10_000,
        ..o
    };
    assert!(huge.beats(0, u64::MAX / 2), "no overflow at the extremes");
    assert!(!huge.beats(1, 10_000), "10 000 ms is the whole margin");
}

#[tokio::test]
async fn a_working_server_in_use_is_kept_unless_another_is_faster_by_the_margins() {
    for (in_use, other, switches) in [
        (100, 80, false),  // 20 %
        (100, 65, false),  // 35 ms
        (300, 240, false), // 20 %
        (300, 150, true),
        (100, 100, false),
        (100, 500, false),
    ] {
        let probe = Scripted::new(&[("S0", &[Ms(in_use)]), ("S1", &[Ms(other)])]);
        let (result, events) = run(&probe, &servers(2), Some("S0")).await;
        let result = result.unwrap();
        assert_eq!(
            result.selected,
            if switches { "S1" } else { "S0" },
            "{in_use} ms against {other} ms"
        );
        assert_eq!(result.last_switch.is_some(), switches);
        assert_eq!(
            probe.calls().iter().any(|c| c == "select:S1"),
            switches,
            "{in_use} against {other}"
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, Progress::Median { id, proxy_ms, .. } if id == "S0" && *proxy_ms == in_use)));
    }
}

#[tokio::test]
async fn one_lucky_answer_does_not_move_a_working_connection() {
    // S1 answers 10 ms once and 400 ms after that: the middle of three is 400.
    let probe = Scripted::new(&[("S0", &[Ms(200)]), ("S1", &[Ms(10), Ms(400), Ms(400)])]);
    let (result, events) = run(&probe, &servers(2), Some("S0")).await;
    let result = result.unwrap();
    assert_eq!(result.selected, "S0");
    assert!(result.last_switch.is_none());
    assert!(!probe.calls().iter().any(|c| c.starts_with("select:")));
    assert!(events
        .iter()
        .any(|e| matches!(e, Progress::Median { id, proxy_ms: 400, .. } if id == "S1")));
}

#[tokio::test]
async fn one_unlucky_answer_of_the_server_in_use_does_not_move_it_either() {
    // S0 is 100 ms; its first answer was an outlier of 900. S1 is steady at 300.
    let probe = Scripted::new(&[("S0", &[Ms(900), Ms(100), Ms(100)]), ("S1", &[Ms(300)])]);
    let (result, _) = run(&probe, &servers(2), Some("S0")).await;
    assert_eq!(result.unwrap().selected, "S0");
}

#[tokio::test]
async fn a_server_that_answered_only_once_is_not_trusted_as_the_best() {
    // S1 answers once, then fails twice: it is left out, S2 wins over the in-use S0.
    let probe = Scripted::new(&[
        ("S0", &[Ms(400)]),
        ("S1", &[Ms(20), Failed, Failed]),
        ("S2", &[Ms(100)]),
    ]);
    let (result, events) = run(&probe, &servers(3), Some("S0")).await;
    assert_eq!(result.unwrap().selected, "S2");
    assert!(!events
        .iter()
        .any(|e| matches!(e, Progress::Median { id, .. } if id == "S1")));
}

#[tokio::test]
async fn a_server_in_use_that_answers_unreliably_gives_way_to_a_steady_one() {
    // S0: one answer in three. S1 is steady and no faster by the margins, yet it replaces S0.
    let probe = Scripted::new(&[("S0", &[Ms(100), Failed, Failed]), ("S1", &[Ms(95)])]);
    let (result, _) = run(&probe, &servers(2), Some("S0")).await;
    assert_eq!(result.unwrap().selected, "S1");
}

#[tokio::test]
async fn when_nothing_is_steady_the_server_in_use_stays() {
    let probe = Scripted::new(&[
        ("S0", &[Ms(100), Failed, Failed]),
        ("S1", &[Ms(20), Failed, Failed]),
    ]);
    let (result, _) = run(&probe, &servers(2), Some("S0")).await;
    assert_eq!(result.unwrap().selected, "S0");
}

#[tokio::test]
async fn two_answers_of_three_are_judged_by_the_slower() {
    // S1: 10 and 300 (one failure): counted as 300, which is not a clear win over 320.
    let probe = Scripted::new(&[("S0", &[Ms(320)]), ("S1", &[Ms(10), Ms(300), Failed])]);
    let (result, events) = run(&probe, &servers(2), Some("S0")).await;
    assert_eq!(result.unwrap().selected, "S0");
    assert!(events
        .iter()
        .any(|e| matches!(e, Progress::Median { id, proxy_ms: 300, .. } if id == "S1")));
}

#[tokio::test]
async fn only_the_fastest_few_and_the_server_in_use_are_measured_again() {
    // S9 is in use and the slowest of ten; S0..S4 are the five fastest.
    let script: Vec<(String, Vec<Answer>)> = (0..10)
        .map(|i| (format!("S{i}"), vec![Ms(100 + i as u64 * 10)]))
        .collect();
    let refs: Vec<(&str, &[Answer])> = script
        .iter()
        .map(|(id, answers)| (id.as_str(), answers.as_slice()))
        .collect();
    let probe = Scripted::new(&refs);
    let (result, _) = run(&probe, &servers(10), Some("S9")).await;
    // 190 ms against 100 ms: 90 ms and 47 % — leaves S9 for S0.
    assert_eq!(result.unwrap().selected, "S0");
    for id in ["S0", "S1", "S2", "S3", "S4", "S9"] {
        assert_eq!(probe.count(id), 3, "{id}: first answer and two more");
    }
    for id in ["S5", "S6", "S7", "S8"] {
        assert_eq!(probe.count(id), 1, "{id}: asked once");
    }
}

#[tokio::test]
async fn a_timeout_of_the_server_in_use_is_tried_once_more() {
    // S1 is not faster by the margins: the server in use stays, and it was asked twice at first.
    let probe = Scripted::new(&[("S0", &[Timeout, Ms(90)]), ("S1", &[Ms(80)])]);
    let (result, events) = run(&probe, &servers(2), Some("S0")).await;
    let result = result.unwrap();
    assert_eq!(result.selected, "S0");
    assert!(result.last_switch.is_none());
    assert_eq!(&probe.calls()[..2], ["probe:S0", "probe:S0"]);
    assert!(matches!(
        events.as_slice(),
        [Progress::Endpoint(..), Progress::Current(id, m), ..] if id == "S0" && m.latency_ms == Some(90)
    ));
}

#[tokio::test]
async fn two_timeouts_in_a_row_count_as_a_failure_of_the_server_in_use() {
    let probe = Scripted::new(&[("S0", &[Timeout]), ("S1", &[Ms(70)])]);
    let (result, events) = run(&probe, &servers(2), Some("S0")).await;
    assert_eq!(result.unwrap().selected, "S1");
    assert_eq!(probe.count("S0"), 2, "one retry, no more");
    assert!(events.iter().any(|e| matches!(
        e,
        Progress::Current(id, m) if id == "S0" && m.failure_code == Some(FailureCode::ProxyTimeout)
    )));
}

#[tokio::test]
async fn other_failures_of_the_server_in_use_are_not_retried() {
    let probe = Scripted::new(&[("S0", &[Failed]), ("S1", &[Ms(70)])]);
    let (result, _) = run(&probe, &servers(2), Some("S0")).await;
    assert_eq!(result.unwrap().selected, "S1");
    assert_eq!(probe.count("S0"), 1);
}

#[tokio::test]
async fn timeouts_of_other_servers_are_not_retried() {
    let probe = Scripted::new(&[("S0", &[Timeout]), ("S1", &[Ms(70)])]);
    let (result, _) = run(&probe, &servers(2), None).await;
    assert_eq!(result.unwrap().selected, "S1");
    assert_eq!(probe.count("S0"), 1, "a candidate is asked once");
}

#[tokio::test]
async fn a_server_in_use_that_is_no_longer_a_candidate_is_replaced() {
    // An expired subscription's server is left out of the candidates but is still selected.
    let probe = Scripted::new(&[("S1", &[Ms(70)])]);
    let list = vec![server("S1", 444)];
    let (result, _) = run(&probe, &list, Some("S0")).await;
    let result = result.unwrap();
    assert_eq!(result.selected, "S1");
    assert!(result.last_switch.is_some());
}

#[tokio::test]
async fn a_failed_server_in_use_leaves_the_first_working_one_selected_at_once() {
    let probe = Scripted::new(&[("S0", &[Failed]), ("S1", &[Ms(300)]), ("S2", &[Ms(100)])]);
    let (result, _) = run(&probe, &servers(3), Some("S0")).await;
    assert_eq!(result.unwrap().selected, "S2");
    let calls = probe.calls();
    let first_switch = calls.iter().position(|c| c.starts_with("select:")).unwrap();
    assert_eq!(
        calls[first_switch], "select:S1",
        "the first answer is taken at once"
    );
    assert_eq!(calls.last().unwrap(), "select:S2", "then the fastest");
    // No margin against a stand-in that was only taken to be connected.
}

#[tokio::test]
async fn a_server_in_use_that_is_the_only_candidate_is_not_measured_again() {
    let probe = Scripted::new(&[("S0", &[Ms(100)])]);
    let (result, events) = run(&probe, &servers(1), Some("S0")).await;
    assert_eq!(result.unwrap().selected, "S0");
    assert_eq!(probe.calls(), ["probe:S0"]);
    assert!(!events.iter().any(|e| matches!(e, Progress::Median { .. })));
}

#[tokio::test]
async fn the_thresholds_come_from_the_options() {
    let probe = Scripted::new(&[("S0", &[Ms(100)]), ("S1", &[Ms(80)])]);
    let options = Options {
        threshold_percent: 10,
        threshold_ms: 10,
        ..Options::with_concurrency(4)
    };
    let list = servers(2);
    let result = selection::run_with(
        &probe,
        &list,
        Some("S0"),
        options,
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .unwrap();
    assert_eq!(result.selected, "S1", "20 ms and 20 % clear 10 and 10");
}

#[tokio::test]
async fn cancelling_while_measuring_again_switches_nothing() {
    use tokio::sync::Notify;
    struct Slow {
        asked: Mutex<usize>,
        gate: Notify,
        switched: Mutex<bool>,
    }
    #[async_trait]
    impl Probe for Slow {
        async fn endpoint(&self, _: &Server) -> Result<u64> {
            Ok(10)
        }
        async fn proxy(&self, s: &Server) -> Result<u64> {
            let n = {
                let mut asked = self.asked.lock().unwrap();
                *asked += 1;
                *asked
            };
            // The first answers of both servers come at once; the extra samples wait.
            if n > 2 {
                self.gate.notified().await;
            }
            Ok(if s.id == "S0" { 400 } else { 100 })
        }
        async fn select(&self, _: &str) -> Result<()> {
            *self.switched.lock().unwrap() = true;
            Ok(())
        }
    }
    let probe = Slow {
        asked: Mutex::new(0),
        gate: Notify::new(),
        switched: Mutex::new(false),
    };
    let list = servers(2);
    let token = CancellationToken::new();
    let run = selection::run(&probe, &list, Some("S0"), 4, &token, |_| {});
    tokio::pin!(run);
    for _ in 0..20 {
        tokio::select! {_=&mut run=>panic!("must wait"),_=tokio::task::yield_now()=>{}}
    }
    token.cancel();
    assert!(matches!(run.await, Err(Error::Cancelled)));
    assert!(!*probe.switched.lock().unwrap());
}

#[tokio::test]
async fn the_distance_is_sampled_like_the_answer_for_the_servers_measured_again() {
    // Ten servers, S9 in use: the fastest five and S9 are measured again, in both ways.
    let script: Vec<(String, Vec<Answer>)> = (0..10)
        .map(|i| (format!("S{i}"), vec![Ms(100 + i as u64 * 10)]))
        .collect();
    let refs: Vec<(&str, &[Answer])> = script
        .iter()
        .map(|(id, answers)| (id.as_str(), answers.as_slice()))
        .collect();
    let probe = Scripted::new(&refs);
    let (result, events) = run(&probe, &servers(10), Some("S9")).await;
    result.unwrap();
    let distances = |id: &str| {
        probe
            .distances
            .lock()
            .unwrap()
            .iter()
            .filter(|d| *d == id)
            .count()
    };
    for id in ["S0", "S1", "S4", "S9"] {
        assert_eq!(distances(id), 3, "{id}: first measurement and two more");
    }
    for id in ["S5", "S8"] {
        assert_eq!(distances(id), 1, "{id}: measured once");
    }
    // The middle of the distances comes with the middle of the answers.
    assert!(events.iter().any(|e| matches!(
        e,
        Progress::Median { id, proxy_ms: 100, endpoint_ms: Some(10) } if id == "S0"
    )));
}
