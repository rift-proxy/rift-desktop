//! The light check of the selected server between cycles: two missed answers in a row are a
//! loss. Auto Select answers with a cycle at once; a manual choice only reports it.
mod common;

use common::fake::{pass, start, until};
use rift_core::{
    application::{Action, Application, Category},
    failure::FailureCode,
    jitter::Jitter,
};
use std::time::Duration;

fn lost_entries(app: &Application) -> usize {
    app.snapshot()
        .journal
        .iter()
        .filter(|e| e.failure_code == Some(FailureCode::ConnectionLost))
        .count()
}

#[tokio::test(start_paused = true)]
async fn the_selected_server_alone_is_asked_every_fifteen_seconds() {
    let h = start(false, Jitter::none());
    h.app.execute(Action::Connect).await.unwrap();
    // The worker ticks once a second, so a timer is met up to two seconds late.
    pass(14).await;
    assert!(h.net.asked().is_empty(), "nothing is asked before its time");
    pass(4).await;
    assert_eq!(
        h.net.asked(),
        [1],
        "one light check, of the selected server"
    );
    pass(12).await;
    assert_eq!(h.net.asked(), [1], "the next one is not due yet");
    pass(5).await;
    assert_eq!(h.net.asked(), [1, 1]);
    assert_eq!(h.app.snapshot().status, "connected");
}

#[tokio::test(start_paused = true)]
async fn the_light_checks_follow_the_spread() {
    for (offset, first_at) in [(10_000, 25), (-10_000, 5)] {
        let h = start(false, Jitter::fixed(offset));
        h.app.execute(Action::Connect).await.unwrap();
        pass(first_at - 2).await;
        assert!(h.net.asked().is_empty(), "offset {offset}");
        pass(4).await;
        assert_eq!(h.net.asked(), [1], "offset {offset}");
    }
}

#[tokio::test(start_paused = true)]
async fn one_missed_answer_is_not_a_loss() {
    let h = start(false, Jitter::none());
    h.app.execute(Action::Connect).await.unwrap();
    h.net.answer(1, None);
    pass(18).await;
    assert_eq!(h.net.count(1), 1);
    h.net.answer(1, Some(40));
    pass(7).await;
    assert_eq!(h.net.count(1), 2, "a missed answer is asked again soon");
    assert_eq!(h.app.snapshot().status, "connected");
    assert_eq!(lost_entries(&h.app), 0);
    // The count started over: a miss, then an answer, is again no loss.
    h.net.answer(1, None);
    pass(14).await;
    h.net.answer(1, Some(40));
    pass(10).await;
    assert_eq!(h.app.snapshot().status, "connected");
    assert_eq!(lost_entries(&h.app), 0);
}

#[tokio::test(start_paused = true)]
async fn a_manual_choice_is_only_reported_lost_once_and_never_switched() {
    let h = start(false, Jitter::none());
    h.app.execute(Action::Connect).await.unwrap();
    h.net.answer(1, None);
    until(&h.app, "the loss", |s| s.status == "lost").await;
    let snapshot = h.app.snapshot();
    assert_eq!(
        snapshot.error, None,
        "a loss is a status, not a failure popup"
    );
    assert_eq!(lost_entries(&h.app), 1);
    let entry = snapshot
        .journal
        .iter()
        .find(|e| e.failure_code == Some(FailureCode::ConnectionLost))
        .unwrap();
    assert_eq!(entry.category, Category::Checks);
    assert_eq!(
        entry.level, "warn",
        "not an error: nothing is sent anywhere"
    );
    // More missed checks add nothing; the other server is never asked or chosen.
    pass(60).await;
    assert_eq!(lost_entries(&h.app), 1);
    assert_eq!(h.net.count(2), 0);
    assert!(h
        .net
        .selects
        .lock()
        .unwrap()
        .iter()
        .all(|id| id == &h.ids[0]));
    assert_eq!(h.app.snapshot().status, "lost");

    // An answer returns: the status comes back and the journal says so.
    h.net.answer(1, Some(40));
    until(&h.app, "recovery", |s| s.status == "connected").await;
    assert!(h
        .app
        .snapshot()
        .journal
        .iter()
        .any(|e| e.message == "Соединение восстановлено"));
}

#[tokio::test(start_paused = true)]
async fn choosing_another_server_ends_the_loss_and_starts_the_count_over() {
    let h = start(false, Jitter::none());
    h.app.execute(Action::Connect).await.unwrap();
    h.net.answer(1, None);
    until(&h.app, "the loss", |s| s.status == "lost").await;
    h.app
        .execute(Action::Select {
            id: h.ids[1].clone(),
        })
        .await
        .unwrap();
    assert_eq!(h.app.snapshot().status, "connected");
    pass(18).await;
    assert_eq!(h.net.count(2), 1, "the new server is the one asked now");
    assert_eq!(h.app.snapshot().status, "connected");
    assert_eq!(lost_entries(&h.app), 1, "no new loss reported");
}

#[tokio::test(start_paused = true)]
async fn auto_select_starts_a_cycle_at_once_after_two_missed_answers() {
    let h = start(true, Jitter::none());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "Initial", |s| {
        !s.checking
            && !s.pending
            && s.selected.as_ref() == Some(&h.ids[0])
            && s.next_check.is_some()
    })
    .await;
    let started = tokio::time::Instant::now();
    let cycles_before = cycles(&h.app);
    h.net.answer(1, None);
    until(&h.app, "failover to the other server", |s| {
        s.selected.as_ref() == Some(&h.ids[1]) && !s.checking
    })
    .await;
    // The interval is 60 s and the last switch is fresh: only the loss can explain a cycle now.
    assert!(
        started.elapsed() < Duration::from_secs(40),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(cycles(&h.app), cycles_before + 1);
    assert_eq!(h.app.snapshot().status, "connected");
    assert_eq!(lost_entries(&h.app), 1);
    assert_eq!(h.net.selects.lock().unwrap().last(), Some(&h.ids[1]));
}

#[tokio::test(start_paused = true)]
async fn a_fruitless_failover_cycle_is_followed_by_the_regular_interval() {
    let h = start(true, Jitter::none());
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "Initial", |s| {
        !s.checking && !s.pending && s.next_check.is_some()
    })
    .await;
    h.net.answer(1, None);
    h.net.answer(2, None);
    until(&h.app, "the failover cycle to fail", |s| {
        s.status == "error" && !s.checking
    })
    .await;
    let after_failover = cycles(&h.app);
    // Light checks go on failing, but none of them starts another cycle for a whole interval.
    pass(40).await;
    assert_eq!(cycles(&h.app), after_failover);
    // Then the regular cycle comes, once.
    pass(40).await;
    assert!(cycles(&h.app) > after_failover);
}

#[tokio::test(start_paused = true)]
async fn nothing_is_asked_while_disconnected() {
    let h = start(false, Jitter::none());
    h.app.execute(Action::Connect).await.unwrap();
    h.app.execute(Action::Disconnect).await.unwrap();
    pass(60).await;
    assert!(h.net.asked().is_empty());
    // Connecting again starts the timing over.
    h.app.execute(Action::Connect).await.unwrap();
    pass(14).await;
    assert!(h.net.asked().is_empty());
    pass(2).await;
    assert_eq!(h.net.asked(), [1]);
}

#[tokio::test(start_paused = true)]
async fn disconnect_does_not_wait_for_a_light_check_in_flight() {
    let h = start(false, Jitter::none());
    h.app.execute(Action::Connect).await.unwrap();
    *h.net.gate.lock().unwrap() = true;
    tokio::time::timeout(Duration::from_secs(60), h.net.entered.notified())
        .await
        .expect("the light check did not start");
    tokio::time::timeout(Duration::from_secs(5), h.app.execute(Action::Disconnect))
        .await
        .expect("Disconnect blocked by the light check")
        .unwrap();
    assert_eq!(h.app.snapshot().status, "disconnected");
    assert_eq!(
        lost_entries(&h.app),
        0,
        "an interrupted check counts as nothing"
    );
}

/// Cycles started so far, by their journal line.
fn cycles(app: &Application) -> usize {
    app.snapshot()
        .journal
        .iter()
        .filter(|e| e.message.starts_with("Проверка серверов запущена"))
        .count()
}
