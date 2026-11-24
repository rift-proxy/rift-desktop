//! The latest measurements of each server: twenty rows, memory only, asked for by id and never
//! carried by the snapshot.
mod common;

use common::fake::{pass, start, until};
use rift_core::{
    application::{Action, Application},
    failure::FailureCode,
    history::{History, KEEP},
    jitter::Jitter,
    selection::{Measurement, Progress},
};

fn found(ms: u64) -> Measurement {
    Measurement {
        latency_ms: Some(ms),
        failure_code: None,
    }
}
fn failed(code: FailureCode) -> Measurement {
    Measurement {
        latency_ms: None,
        failure_code: Some(code),
    }
}

#[test]
fn the_stages_of_one_check_are_one_row() {
    let mut history = History::default();
    history.begin_cycle(|_| true);
    history.record(&Progress::Endpoint("a".into(), found(12)), 100);
    history.record(&Progress::Proxy("a".into(), found(80)), 101);
    let rows = history.of("a");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].at, rows[0].endpoint_ms, rows[0].proxy_ms),
        (100, Some(12), Some(80)),
        "the time is that of the first measurement"
    );
    assert_eq!(rows[0].failure_code, None);

    history.begin_cycle(|_| true);
    history.record(&Progress::Endpoint("a".into(), found(14)), 200);
    assert_eq!(history.of("a").len(), 2, "the next check is a new row");
    assert_eq!(history.of("a")[1].proxy_ms, None, "nothing carried over");
}

#[test]
fn the_middle_of_the_samples_replaces_the_first_answer_of_the_same_check() {
    let mut history = History::default();
    history.begin_cycle(|_| true);
    history.record(&Progress::Endpoint("a".into(), found(10)), 1);
    history.record(&Progress::Proxy("a".into(), found(30)), 1);
    history.record(
        &Progress::Median {
            id: "a".into(),
            proxy_ms: 95,
            endpoint_ms: Some(11),
        },
        2,
    );
    let rows = history.of("a");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].endpoint_ms, rows[0].proxy_ms),
        (Some(11), Some(95))
    );
    // Without a distance sampled the first one stays.
    history.begin_cycle(|_| true);
    history.record(&Progress::Endpoint("a".into(), found(10)), 3);
    history.record(
        &Progress::Median {
            id: "a".into(),
            proxy_ms: 70,
            endpoint_ms: None,
        },
        3,
    );
    assert_eq!(history.of("a")[1].endpoint_ms, Some(10));
}

#[test]
fn the_last_stage_decides_whether_a_row_failed() {
    let mut history = History::default();
    history.begin_cycle(|_| true);
    // Distance failed, answer through the server came: nothing failed.
    history.record(
        &Progress::Endpoint("a".into(), failed(FailureCode::EndpointTimeout)),
        1,
    );
    history.record(&Progress::Current("a".into(), found(50)), 1);
    assert_eq!(history.of("a")[0].failure_code, None);
    // Distance failed and the answer was never asked: the distance's code stays.
    history.record(
        &Progress::Endpoint("b".into(), failed(FailureCode::EndpointTimeout)),
        1,
    );
    assert_eq!(
        history.of("b")[0].failure_code,
        Some(FailureCode::EndpointTimeout)
    );
    // Answer failed: its code.
    history.record(&Progress::Endpoint("c".into(), found(9)), 1);
    history.record(
        &Progress::Proxy("c".into(), failed(FailureCode::ProxyTimeout)),
        1,
    );
    assert_eq!(
        history.of("c")[0].failure_code,
        Some(FailureCode::ProxyTimeout)
    );
    // A switch that failed.
    history.record(
        &Progress::SelectionFailed("d".into(), FailureCode::SelectorSwitch),
        1,
    );
    assert_eq!(
        history.of("d")[0].failure_code,
        Some(FailureCode::SelectorSwitch)
    );
}

#[test]
fn twenty_rows_are_kept_and_the_oldest_go_first() {
    let mut history = History::default();
    for cycle in 0..(KEEP as u64 + 7) {
        history.begin_cycle(|_| true);
        history.record(&Progress::Proxy("a".into(), found(cycle)), cycle);
    }
    let rows = history.of("a");
    assert_eq!(rows.len(), KEEP);
    assert_eq!(rows.first().unwrap().proxy_ms, Some(7));
    assert_eq!(rows.last().unwrap().proxy_ms, Some(KEEP as u64 + 6));
}

#[test]
fn servers_that_left_the_list_are_forgotten() {
    let mut history = History::default();
    history.begin_cycle(|_| true);
    history.record(&Progress::Proxy("a".into(), found(1)), 1);
    history.record(&Progress::Proxy("b".into(), found(2)), 1);
    history.begin_cycle(|id| id == "a");
    assert_eq!(history.of("a").len(), 1);
    assert!(history.of("b").is_empty());
    assert!(history.of("never seen").is_empty());
}

#[test]
fn a_switch_is_not_a_measurement() {
    let mut history = History::default();
    history.begin_cycle(|_| true);
    history.record(
        &Progress::Selected("a".into(), tokio::time::Instant::now()),
        1,
    );
    assert!(history.of("a").is_empty());
}

fn snapshot_text(app: &Application) -> String {
    serde_json::to_string(&app.snapshot()).unwrap()
}
/// One more Check Now in manual mode, waited for to the end.
async fn check(app: &Application) {
    pass(61).await;
    app.execute(Action::CheckNow).await.unwrap();
    until(app, "the check", |s| !s.checking && !s.pending).await;
}

#[tokio::test(start_paused = true)]
async fn a_check_leaves_one_row_per_server_with_both_distances() {
    let h = start(true, Jitter::none());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "Initial", |s| {
        !s.checking && !s.pending && s.next_check.is_some()
    })
    .await;
    for (id, ms) in [(&h.ids[0], 40), (&h.ids[1], 100)] {
        let rows = h.app.history(id);
        assert_eq!(rows.len(), 1, "{id}");
        assert_eq!(
            (rows[0].endpoint_ms, rows[0].proxy_ms, rows[0].failure_code),
            (Some(10), Some(ms), None)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn the_rows_of_a_long_run_are_twenty_and_the_newest() {
    let h = start(false, Jitter::none());
    h.app.execute(Action::Connect).await.unwrap();
    for i in 0..30u64 {
        h.net.answer(1, Some(100 + i));
        check(&h.app).await;
    }
    let rows = h.app.history(&h.ids[0]);
    assert_eq!(rows.len(), KEEP);
    assert_eq!(rows.last().unwrap().proxy_ms, Some(129));
    assert_eq!(rows.first().unwrap().proxy_ms, Some(110));
    assert!(
        h.app.history(&h.ids[1]).is_empty(),
        "a manual choice asks only the chosen server"
    );
}

#[tokio::test(start_paused = true)]
async fn the_light_check_between_cycles_adds_no_rows() {
    let h = start(false, Jitter::none());
    h.app.execute(Action::Connect).await.unwrap();
    pass(50).await;
    assert!(
        h.net.count(1) >= 2,
        "premise: the selected server was asked"
    );
    assert!(h.app.history(&h.ids[0]).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_failed_check_is_a_row_with_its_reason() {
    let h = start(false, Jitter::none());
    h.app.execute(Action::Connect).await.unwrap();
    h.net.answer(1, None);
    check(&h.app).await;
    let rows = h.app.history(&h.ids[0]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].proxy_ms, None);
    assert!(rows[0].failure_code.is_some(), "{rows:?}");
}

#[tokio::test(start_paused = true)]
async fn removing_a_server_drops_its_rows_at_the_next_check() {
    let h = start(true, Jitter::none());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "Initial", |s| {
        !s.checking && !s.pending && s.next_check.is_some()
    })
    .await;
    assert_eq!(h.app.history(&h.ids[1]).len(), 1);
    h.app
        .execute(Action::RemoveServer {
            id: h.ids[1].clone(),
        })
        .await
        .unwrap();
    until(&h.app, "the check after the removal", |s| {
        !s.checking && !s.pending
    })
    .await;
    pass(61).await;
    h.app.execute(Action::CheckNow).await.unwrap();
    until(&h.app, "the check", |s| !s.checking && !s.pending).await;
    assert!(h.app.history(&h.ids[1]).is_empty());
    assert!(!h.app.history(&h.ids[0]).is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_snapshot_does_not_grow_with_the_history() {
    let h = start(false, Jitter::none());
    h.app.execute(Action::Connect).await.unwrap();
    check(&h.app).await;
    h.app.execute(Action::ClearJournal).await.unwrap();
    let before = snapshot_text(&h.app);
    for _ in 0..25 {
        check(&h.app).await;
    }
    h.app.execute(Action::ClearJournal).await.unwrap();
    let after = snapshot_text(&h.app);
    assert_eq!(h.app.history(&h.ids[0]).len(), KEEP);
    assert_eq!(
        before.len(),
        after.len(),
        "history is asked for, not published:\n{before}\n{after}"
    );
    assert!(!after.contains("history"));
}

#[test]
fn independent_rechecks_append_rows_without_changing_the_selection_cycle() {
    let mut history = History::default();
    history.begin_cycle(|_| true);
    history.record(&Progress::Endpoint("live".into(), found(10)), 1);
    for at in [2, 3] {
        let check = history.begin_check();
        history.record_check(&Progress::Endpoint("dead".into(), found(20)), at, check);
        history.record_check(
            &Progress::Proxy("dead".into(), failed(FailureCode::ProxyTimeout)),
            at,
            check,
        );
    }
    history.record(&Progress::Proxy("live".into(), found(40)), 4);
    assert_eq!(
        history
            .of("dead")
            .iter()
            .map(|row| row.at)
            .collect::<Vec<_>>(),
        [2, 3]
    );
    assert_eq!(history.of("live").len(), 1);
    assert_eq!(history.of("live")[0].proxy_ms, Some(40));
}
