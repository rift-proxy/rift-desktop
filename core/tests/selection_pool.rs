//! What a full check hands over besides the choice: the pool of the best servers. And what the
//! check does with what it is told about the pool: try the pool first when nothing in use works,
//! leave out servers just found dead, and take the best server while the pool is not yet complete.
use async_trait::async_trait;
use rift_core::{
    failure::FailureCode,
    model::Server,
    parser::parse_uri,
    selection::{self, Options, Probe, Progress},
    Error, Result,
};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;

fn server(id: &str, host: &str, port: u16) -> Server {
    let mut s = parse_uri(&format!("trojan://secret@{host}:{port}#{id}"), None).unwrap();
    s.id = id.into();
    s
}
/// Each server on a host of its own, closer the lower the number.
fn servers(count: usize) -> Vec<Server> {
    (0..count)
        .map(|i| server(&format!("S{i}"), &format!("h{i}.example.com"), 443))
        .collect()
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
    calls: Mutex<Vec<String>>,
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
        }
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn count(&self, what: &str) -> usize {
        self.calls().iter().filter(|c| *c == what).count()
    }
    fn first(&self, what: &str) -> Option<usize> {
        self.calls().iter().position(|c| c == what)
    }
}
#[async_trait]
impl Probe for Scripted {
    async fn endpoint(&self, s: &Server) -> Result<u64> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("endpoint:{}", s.id));
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
async fn run(
    probe: &Scripted,
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
fn ids(pool: &[rift_core::pool::Ranked]) -> Vec<&str> {
    pool.iter().map(|r| r.id.as_str()).collect()
}
fn ids_of(set: &[&str]) -> HashSet<String> {
    set.iter().map(|s| (*s).to_owned()).collect()
}

#[tokio::test]
async fn a_full_check_hands_over_the_pool_of_the_best_servers_by_their_middle_answers() {
    let probe = Scripted::new(&[
        ("S0", &[Ms(300)]),
        ("S1", &[Ms(100)]),
        ("S2", &[Ms(120)]),
        ("S3", &[Ms(140)]),
        ("S4", &[Ms(500)]),
    ]);
    let (result, _) = run(&probe, &servers(5), None, Options::with_concurrency(3)).await;
    let outcome = result.unwrap();
    assert_eq!(outcome.selected, "S1");
    assert_eq!(ids(&outcome.pool), ["S1", "S2", "S3"]);
    let medians: Vec<u64> = outcome.pool.iter().map(|r| r.ms).collect();
    assert_eq!(medians, [100, 120, 140]);
}

#[tokio::test]
async fn the_pool_is_as_big_as_the_settings_say() {
    let script: Vec<(String, Vec<Answer>)> = (0..8)
        .map(|i| (format!("S{i}"), vec![Ms(100 + i as u64 * 10)]))
        .collect();
    let refs: Vec<(&str, &[Answer])> = script
        .iter()
        .map(|(id, a)| (id.as_str(), a.as_slice()))
        .collect();
    for (size, expected) in [(1, 1), (3, 3), (5, 5)] {
        let probe = Scripted::new(&refs);
        let options = Options {
            pool_size: size,
            ..Options::with_concurrency(3)
        };
        let (result, _) = run(&probe, &servers(8), None, options).await;
        assert_eq!(result.unwrap().pool.len(), expected, "size {size}");
    }
}

#[tokio::test]
async fn the_pool_has_one_server_per_host_and_goes_deeper_into_the_list_for_it() {
    // S0 and S1 are two rows of one machine, so are S2 and S3: three hosts need five servers.
    let list = vec![
        server("S0", "a.example.com", 443),
        server("S1", "a.example.com", 8443),
        server("S2", "b.example.com", 443),
        server("S3", "b.example.com", 8443),
        server("S4", "c.example.com", 443),
        server("S5", "d.example.com", 443),
        server("S6", "e.example.com", 443),
    ];
    let probe = Scripted::new(&[
        ("S0", &[Ms(100)]),
        ("S1", &[Ms(101)]),
        ("S2", &[Ms(102)]),
        ("S3", &[Ms(103)]),
        ("S4", &[Ms(104)]),
        ("S5", &[Ms(105)]),
        ("S6", &[Ms(106)]),
    ]);
    let (result, _) = run(&probe, &list, None, Options::with_concurrency(3)).await;
    assert_eq!(ids(&result.unwrap().pool), ["S0", "S2", "S4"]);
}

#[tokio::test]
async fn a_single_working_server_is_a_pool_of_one() {
    let probe = Scripted::new(&[("S0", &[Failed]), ("S1", &[Ms(70)]), ("S2", &[Failed])]);
    let (result, _) = run(&probe, &servers(3), None, Options::with_concurrency(3)).await;
    let outcome = result.unwrap();
    assert_eq!(ids(&outcome.pool), ["S1"]);
    assert_eq!(
        outcome.pool[0].ms, 70,
        "no middle of answers without a comparison: the first one"
    );
}

#[tokio::test]
async fn a_server_that_answered_once_of_three_is_not_in_the_pool() {
    let probe = Scripted::new(&[
        ("S0", &[Ms(100)]),
        ("S1", &[Ms(90), Failed, Failed]),
        ("S2", &[Ms(120)]),
    ]);
    let (result, _) = run(&probe, &servers(3), None, Options::with_concurrency(3)).await;
    assert_eq!(ids(&result.unwrap().pool), ["S0", "S2"]);
}

#[tokio::test]
async fn a_server_in_use_among_the_best_is_left_only_for_a_clearly_faster_one() {
    // Being one of the three best is not a reason to stay when the first is far ahead...
    let probe = Scripted::new(&[
        ("S0", &[Ms(80)]),
        ("S1", &[Ms(100)]),
        ("S2", &[Ms(200)]),
        ("S3", &[Ms(900)]),
    ]);
    let (result, _) = run(
        &probe,
        &servers(4),
        Some("S2"),
        Options::with_concurrency(3),
    )
    .await;
    let outcome = result.unwrap();
    assert_eq!(outcome.selected, "S0");
    assert_eq!(ids(&outcome.pool), ["S0", "S1", "S2"]);
    // ...and it is a reason when the difference is within the margins: S0 is faster by 40 ms.
    let probe = Scripted::new(&[
        ("S0", &[Ms(100)]),
        ("S1", &[Ms(110)]),
        ("S2", &[Ms(140)]),
        ("S3", &[Ms(900)]),
    ]);
    let (result, _) = run(
        &probe,
        &servers(4),
        Some("S2"),
        Options::with_concurrency(3),
    )
    .await;
    let outcome = result.unwrap();
    assert_eq!(outcome.selected, "S2");
    assert!(outcome.last_switch.is_none());
}

#[tokio::test]
async fn a_server_in_use_outside_the_pool_stays_unless_the_best_beats_it_by_the_margins() {
    // S3 is not among the best three. S0 is faster by 50 ms and 31 %: S3 gives way.
    let probe = Scripted::new(&[
        ("S0", &[Ms(110)]),
        ("S1", &[Ms(120)]),
        ("S2", &[Ms(130)]),
        ("S3", &[Ms(160)]),
    ]);
    let (result, _) = run(
        &probe,
        &servers(4),
        Some("S3"),
        Options::with_concurrency(3),
    )
    .await;
    assert_eq!(result.unwrap().selected, "S0");
    // The same, but faster by 40 ms only: S3 stays.
    let probe = Scripted::new(&[
        ("S0", &[Ms(110)]),
        ("S1", &[Ms(120)]),
        ("S2", &[Ms(130)]),
        ("S3", &[Ms(150)]),
    ]);
    let (result, _) = run(
        &probe,
        &servers(4),
        Some("S3"),
        Options::with_concurrency(3),
    )
    .await;
    assert_eq!(result.unwrap().selected, "S3");
}

#[tokio::test]
async fn while_the_pool_is_incomplete_the_best_server_replaces_whichever_is_in_use() {
    let script: &[(&str, &[Answer])] = &[
        ("S0", &[Ms(100)]),
        ("S1", &[Ms(110)]),
        ("S2", &[Ms(120)]),
        ("S3", &[Ms(130)]),
    ];
    // A server of the pool in use, faster than the margins would ask to leave...
    let options = Options {
        pool_incomplete: true,
        ..Options::with_concurrency(3)
    };
    let probe = Scripted::new(script);
    let (result, _) = run(&probe, &servers(4), Some("S2"), options.clone()).await;
    let outcome = result.unwrap();
    assert_eq!(
        outcome.selected, "S0",
        "the best, the others stay in reserve"
    );
    assert!(outcome.last_switch.is_some());
    assert_eq!(ids(&outcome.pool), ["S0", "S1", "S2"]);
    // ...and a server outside the pool, chosen by hand.
    let probe = Scripted::new(script);
    let (result, _) = run(&probe, &servers(4), Some("S3"), options).await;
    assert_eq!(result.unwrap().selected, "S0");
    // With a complete pool the same choice is left alone.
    let probe = Scripted::new(script);
    let (result, _) = run(
        &probe,
        &servers(4),
        Some("S3"),
        Options::with_concurrency(3),
    )
    .await;
    assert_eq!(result.unwrap().selected, "S3");
}

#[tokio::test]
async fn the_best_server_is_already_in_use_while_the_pool_is_incomplete_nothing_is_switched() {
    let probe = Scripted::new(&[("S0", &[Ms(100)]), ("S1", &[Ms(110)])]);
    let options = Options {
        pool_incomplete: true,
        ..Options::with_concurrency(3)
    };
    let (result, _) = run(&probe, &servers(2), Some("S0"), options).await;
    let outcome = result.unwrap();
    assert_eq!(outcome.selected, "S0");
    assert!(outcome.last_switch.is_none());
}

#[tokio::test]
async fn equal_answers_keep_the_server_in_use_once_the_pool_is_complete() {
    // A pool smaller than the setting is complete when it holds what there was to build: the
    // answers are equal, and the smaller id does not win from the server in use.
    let script: &[(&str, &[Answer])] = &[("S0", &[Ms(100)]), ("S1", &[Ms(100)])];
    let probe = Scripted::new(script);
    let (result, _) = run(
        &probe,
        &servers(2),
        Some("S1"),
        Options::with_concurrency(3),
    )
    .await;
    let outcome = result.unwrap();
    assert_eq!(outcome.selected, "S1");
    assert!(outcome.last_switch.is_none());
    // While the pool is incomplete the best is taken, the order of equals being the id's.
    let probe = Scripted::new(script);
    let options = Options {
        pool_incomplete: true,
        ..Options::with_concurrency(3)
    };
    let (result, _) = run(&probe, &servers(2), Some("S1"), options).await;
    assert_eq!(result.unwrap().selected, "S0");
}

#[tokio::test]
async fn the_pool_is_tried_first_when_the_server_in_use_does_not_work() {
    // S1 is the closest, but S3 and S2 are the pool: one of them is taken at once.
    let probe = Scripted::new(&[
        ("S0", &[Failed]),
        ("S1", &[Ms(90)]),
        ("S2", &[Ms(110)]),
        ("S3", &[Ms(100)]),
    ]);
    let options = Options {
        prefer: vec!["S3".into(), "S2".into()],
        ..Options::with_concurrency(1)
    };
    let (result, _) = run(&probe, &servers(4), Some("S0"), options).await;
    let outcome = result.unwrap();
    // The first switch is to the first of the pool, before S1 was asked at all.
    let switch = probe.first("select:S3").expect("S3 taken at once");
    assert_eq!(
        probe.first("select:S3"),
        probe.calls().iter().position(|c| c.starts_with("select:"))
    );
    assert!(probe.first("probe:S1").unwrap() > switch);
    // S3 was only a stand-in to get a connection: when the check is done the best, S1, takes over.
    assert_eq!(outcome.selected, "S1");
    assert_eq!(probe.count("select:S3"), 1);
    assert_eq!(probe.count("select:S1"), 1);
}

#[tokio::test]
async fn the_pool_is_not_measured_twice_in_the_same_check() {
    let probe = Scripted::new(&[
        ("S0", &[Failed]),
        ("S1", &[Ms(90)]),
        ("S2", &[Ms(110)]),
        ("S3", &[Failed]),
    ]);
    let options = Options {
        prefer: vec!["S3".into(), "S2".into()],
        ..Options::with_concurrency(1)
    };
    let (result, _) = run(&probe, &servers(4), Some("S0"), options).await;
    // S2 was taken at once, S1 is faster and takes over at the end.
    assert_eq!(result.unwrap().selected, "S1");
    // S3 did not answer when it was tried first: it is not asked again as an ordinary candidate.
    assert_eq!(probe.count("endpoint:S3"), 1);
    assert_eq!(probe.count("probe:S3"), 1);
}

#[tokio::test]
async fn the_pool_is_ignored_while_the_server_in_use_works() {
    let probe = Scripted::new(&[("S0", &[Ms(100)]), ("S1", &[Ms(110)]), ("S2", &[Ms(120)])]);
    let options = Options {
        prefer: vec!["S2".into(), "S1".into()],
        ..Options::with_concurrency(3)
    };
    let (result, _) = run(&probe, &servers(3), Some("S0"), options).await;
    let outcome = result.unwrap();
    assert_eq!(outcome.selected, "S0");
    assert_eq!(probe.count("select:S2"), 0);
    assert!(
        probe.first("probe:S0").unwrap() < probe.first("probe:S1").unwrap(),
        "the server in use is asked first as always"
    );
}

#[tokio::test]
async fn servers_to_skip_are_not_measured_at_all() {
    let probe = Scripted::new(&[("S0", &[Ms(100)]), ("S1", &[Ms(50)]), ("S2", &[Ms(120)])]);
    let options = Options {
        skip: ids_of(&["S1"]),
        ..Options::with_concurrency(3)
    };
    let (result, _) = run(&probe, &servers(3), Some("S0"), options).await;
    assert_eq!(result.unwrap().selected, "S0");
    assert_eq!(probe.count("endpoint:S1"), 0);
    assert_eq!(probe.count("probe:S1"), 0);
}

#[tokio::test]
async fn a_server_to_skip_that_is_in_use_is_still_asked() {
    // The one in use is never left out: skipping is for candidates.
    let probe = Scripted::new(&[("S0", &[Ms(100)]), ("S1", &[Ms(120)])]);
    let options = Options {
        skip: ids_of(&["S0"]),
        ..Options::with_concurrency(3)
    };
    let (result, _) = run(&probe, &servers(2), Some("S0"), options).await;
    assert_eq!(result.unwrap().selected, "S0");
    assert!(probe.count("probe:S0") >= 1);
}

#[tokio::test]
async fn a_server_in_use_just_found_dead_is_not_asked_again() {
    let probe = Scripted::new(&[("S0", &[Timeout]), ("S1", &[Ms(70)]), ("S2", &[Ms(90)])]);
    let options = Options {
        current_lost: true,
        ..Options::with_concurrency(1)
    };
    let (result, events) = run(&probe, &servers(3), Some("S0"), options).await;
    let outcome = result.unwrap();
    assert_eq!(outcome.selected, "S1");
    assert_eq!(probe.count("probe:S0"), 0);
    assert_eq!(probe.count("endpoint:S0"), 0);
    assert!(!events.iter().any(|e| matches!(e, Progress::Current(id, _) | Progress::Endpoint(id, _) | Progress::Proxy(id, _) if id == "S0")));
    // The first that answered was taken at once.
    assert!(probe.first("select:S1").unwrap() < probe.first("probe:S2").unwrap());
}

#[tokio::test]
async fn when_nobody_works_the_pool_is_empty_and_the_check_fails_as_before() {
    let probe = Scripted::new(&[("S0", &[Failed]), ("S1", &[Failed])]);
    let (result, _) = run(
        &probe,
        &servers(2),
        Some("S0"),
        Options::with_concurrency(3),
    )
    .await;
    assert!(matches!(result, Err(Error::Unavailable)));
}

// ---- what the check decided, for the journal ---------------------------------------------------

use rift_core::selection::Decision;

async fn decided(
    script: &[(&str, &[Answer])],
    count: usize,
    current: Option<&str>,
    options: Options,
) -> Decision {
    let probe = Scripted::new(script);
    let (result, _) = run(&probe, &servers(count), current, options).await;
    result.unwrap().decision
}

#[tokio::test]
async fn a_check_with_nothing_in_use_says_it_chose_the_best() {
    let d = decided(
        &[("S0", &[Ms(100)]), ("S1", &[Ms(80)])],
        2,
        None,
        Options::with_concurrency(3),
    )
    .await;
    assert_eq!(d, Decision::Chose { ms: 80 });
}

#[tokio::test]
async fn the_server_in_use_that_is_the_best_is_said_to_be_the_best() {
    let d = decided(
        &[("S0", &[Ms(100)]), ("S1", &[Ms(150)])],
        2,
        Some("S0"),
        Options::with_concurrency(3),
    )
    .await;
    assert_eq!(d, Decision::Best { ms: 100 });
}

#[tokio::test]
async fn a_server_in_use_the_others_do_not_beat_by_the_margins_says_by_how_much_they_fell_short() {
    // 40 ms is more than the share, less than the margin of 50 ms.
    let d = decided(
        &[("S0", &[Ms(140)]), ("S1", &[Ms(100)])],
        2,
        Some("S0"),
        Options::with_concurrency(3),
    )
    .await;
    assert_eq!(
        d,
        Decision::Kept {
            ms: 140,
            best_ms: 100
        }
    );
}

#[tokio::test]
async fn a_clearly_faster_server_replaces_the_one_in_use_and_it_is_said() {
    let d = decided(
        &[("S0", &[Ms(300)]), ("S1", &[Ms(100)])],
        2,
        Some("S0"),
        Options::with_concurrency(3),
    )
    .await;
    assert_eq!(
        d,
        Decision::Faster {
            from_ms: 300,
            to_ms: 100
        }
    );
}

#[tokio::test]
async fn a_server_in_use_that_does_not_answer_is_replaced_and_it_is_said() {
    let d = decided(
        &[("S0", &[Failed]), ("S1", &[Ms(100)]), ("S2", &[Ms(120)])],
        3,
        Some("S0"),
        Options::with_concurrency(3),
    )
    .await;
    assert_eq!(d, Decision::Lost { to_ms: 100 });
}

#[tokio::test]
async fn a_server_in_use_that_answers_only_now_and_then_gives_way() {
    let d = decided(
        &[
            ("S0", &[Ms(100), Failed, Failed]),
            ("S1", &[Ms(150)]),
            ("S2", &[Ms(160)]),
        ],
        3,
        Some("S0"),
        Options::with_concurrency(3),
    )
    .await;
    assert_eq!(d, Decision::Unreliable { to_ms: 150 });
}

#[tokio::test]
async fn while_the_pool_is_incomplete_the_best_is_taken_and_it_is_said_why() {
    let options = Options {
        pool_incomplete: true,
        ..Options::with_concurrency(3)
    };
    let d = decided(
        &[("S0", &[Ms(110)]), ("S1", &[Ms(100)]), ("S2", &[Ms(130)])],
        3,
        Some("S0"),
        options,
    )
    .await;
    assert_eq!(
        d,
        Decision::Pool {
            from_ms: Some(110),
            to_ms: 100
        }
    );
}

#[tokio::test]
async fn an_incomplete_pool_with_nothing_in_use_is_a_plain_choice() {
    let options = Options {
        pool_incomplete: true,
        ..Options::with_concurrency(3)
    };
    let d = decided(&[("S0", &[Ms(110)]), ("S1", &[Ms(100)])], 2, None, options).await;
    assert_eq!(d, Decision::Chose { ms: 100 });
}

#[tokio::test]
async fn a_server_in_use_that_left_the_list_is_replaced_by_a_plain_choice_when_the_pool_is_complete(
) {
    let d = decided(
        &[("S0", &[Ms(110)]), ("S1", &[Ms(100)])],
        2,
        Some("gone"),
        Options::with_concurrency(3),
    )
    .await;
    assert_eq!(d, Decision::Chose { ms: 100 });
}

#[tokio::test]
async fn a_single_working_server_in_use_is_the_best_there_is() {
    let d = decided(
        &[("S0", &[Ms(100)]), ("S1", &[Failed])],
        2,
        Some("S0"),
        Options::with_concurrency(3),
    )
    .await;
    assert_eq!(d, Decision::Best { ms: 100 });
}
