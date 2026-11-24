//! Selection failures keep their technical observations locally. A manual choice that stops
//! answering remains a warning, not an error toast.
mod common;

use common::fake::{pass, start_hosts, until};
use rift_core::{application::Action, failure::FailureCode, jitter::Jitter, notifications};

/// Toast subscriptions are process-wide.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test(start_paused = true)]
async fn a_connect_that_finds_nothing_says_how_far_it_got() {
    let _alone = ONE_AT_A_TIME.lock().await;
    let h = start_hosts(true, Jitter::none(), &[None, None, None]);
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "the outage", |s| s.error.is_some()).await;
    pass(1).await;
    let snapshot = h.app.snapshot();
    let entry = snapshot
        .journal
        .iter()
        .find(|entry| entry.failure_code == Some(FailureCode::NoAvailableServer))
        .expect("the outage is in the local journal");
    let detail = entry.details.as_deref().expect("selection details");
    for expected in ["no_available_server", "initial", "checked", "working"] {
        assert!(
            detail.contains(expected),
            "{expected} missing from {detail}"
        );
    }
    for (field, expected) in [("checked", 3), ("reachable", 3), ("working", 0)] {
        let prefix = format!("selection.{field}: ");
        let actual = detail.lines().find_map(|line| line.strip_prefix(&prefix));
        assert_eq!(
            actual.and_then(|value| value.parse::<u16>().ok()),
            Some(expected)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_check_the_owner_asked_for_keeps_its_local_observations() {
    let _alone = ONE_AT_A_TIME.lock().await;
    let h = start_hosts(true, Jitter::none(), &[Some(40), Some(60), Some(80)]);
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "a connection", |s| s.status == "connected").await;
    for port in 1..=3 {
        h.net.answer(port, None);
    }
    h.app.execute(Action::CheckNow).await.unwrap();
    until(&h.app, "the outage", |s| s.error.is_some()).await;
    pass(1).await;
    let snapshot = h.app.snapshot();
    let entry = snapshot
        .journal
        .iter()
        .rev()
        .find(|entry| entry.failure_code == Some(FailureCode::NoAvailableServer))
        .expect("the outage is in the local journal");
    let detail = entry.details.as_deref().expect("selection details");
    for expected in ["check_now", "working"] {
        assert!(
            detail.contains(expected),
            "{expected} missing from {detail}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_chosen_server_that_is_silent_is_a_line_in_the_journal_and_nothing_more() {
    let _alone = ONE_AT_A_TIME.lock().await;
    let mut toasts = notifications::subscribe();
    let h = start_hosts(false, Jitter::none(), &[Some(40), Some(60)]);
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "a connection", |s| s.status == "connected").await;
    while toasts.try_recv().is_ok() {}
    h.net.answer(1, None);
    h.app.execute(Action::CheckNow).await.unwrap();
    until(&h.app, "the line of the journal", |s| {
        s.journal
            .iter()
            .any(|entry| entry.message == "Выбранный сервер не отвечает")
    })
    .await;
    pass(1).await;
    let snapshot = h.app.snapshot();
    let line = snapshot
        .journal
        .iter()
        .find(|entry| entry.message == "Выбранный сервер не отвечает")
        .unwrap();
    assert_eq!(line.level, "warn");
    assert_eq!(snapshot.error, None, "no error is shown");
    assert_ne!(snapshot.status, "error");
    let chosen = snapshot.servers.iter().find(|v| v.id == h.ids[0]).unwrap();
    assert_eq!(chosen.status, "failed", "the list says it did not answer");
    assert!(toasts.try_recv().is_err(), "no toast is shown");
}
