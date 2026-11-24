//! The speed through the proxy: read from sing-box's `/traffic` only while the screen wants it
//! and Rift is connected, kept in memory and apart from the snapshot.
mod common;

use common::{
    clash::{serve, Api, Reply},
    fake::start_on,
};
use rift_core::{
    application::{Action, Category},
    clash::Speed,
    failure::FailureCode,
    jitter::Jitter,
};
use std::{sync::Arc, time::Duration};

const LINE: &str = r#"{"up":100,"down":2000}"#;

async fn api_with(reply: Reply) -> (Arc<Api>, u16) {
    let api = Arc::new(Api::default());
    api.route("/traffic", reply);
    let port = serve(api.clone()).await;
    (api, port)
}
fn lines(lines: &[&str], gap_ms: u64, hold: bool) -> Reply {
    Reply::Lines {
        lines: lines.iter().map(|l| (*l).to_owned()).collect(),
        gap: Duration::from_millis(gap_ms),
        hold,
    }
}
async fn settle(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}
async fn speed(app: &rift_core::application::Application) -> Option<Speed> {
    let mut rx = app.traffic();
    let seen = tokio::time::timeout(Duration::from_secs(3), rx.wait_for(|s| s.is_some()))
        .await
        .expect("no speed arrived")
        .unwrap()
        .to_owned();
    seen
}

#[tokio::test]
async fn a_watched_connection_publishes_the_speed_with_the_secret() {
    let (api, port) = api_with(lines(&[LINE, r#"{"up":5,"down":6}"#], 50, true)).await;
    let h = start_on(false, Jitter::none(), |s| s.settings.api_port = port);
    h.app.execute(Action::Connect).await.unwrap();
    h.app.watch_traffic(true);
    let mut rx = h.app.traffic();
    tokio::time::timeout(
        Duration::from_secs(3),
        rx.wait_for(|s| *s == Some(Speed { up: 5, down: 6 })),
    )
    .await
    .expect("the second line did not arrive")
    .unwrap();
    let head = api.heads().remove(0).to_lowercase();
    assert!(head.starts_with("get /traffic "), "{head}");
    // The secret is a bearer token and goes nowhere else: no query, no other header.
    assert!(head.contains("authorization: bearer "), "{head}");
    let authorization = head
        .lines()
        .find_map(|line| line.strip_prefix("authorization: bearer "))
        .unwrap();
    assert!(!authorization.is_empty());
    assert_eq!(
        head.matches(authorization).count(),
        1,
        "secret appears only in Authorization"
    );
}

#[tokio::test]
async fn nothing_is_read_until_the_screen_wants_it_and_nothing_after_it_leaves() {
    let (api, port) = api_with(lines(&[LINE], 50, true)).await;
    let h = start_on(false, Jitter::none(), |s| s.settings.api_port = port);
    h.app.execute(Action::Connect).await.unwrap();
    settle(400).await;
    assert_eq!(api.count("/traffic"), 0, "no screen, no request");
    assert_eq!(*h.app.traffic().borrow(), None);

    h.app.watch_traffic(true);
    assert_eq!(
        speed(&h.app).await,
        Some(Speed {
            up: 100,
            down: 2000
        })
    );
    assert_eq!(api.count("/traffic"), 1);
    assert_eq!(api.open.load(std::sync::atomic::Ordering::SeqCst), 1);

    h.app.watch_traffic(false);
    let mut rx = h.app.traffic();
    tokio::time::timeout(Duration::from_secs(3), rx.wait_for(|s| s.is_none()))
        .await
        .expect("the speed stayed on screen")
        .unwrap();
    settle(300).await;
    assert_eq!(
        api.open.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the stream is closed, not left running"
    );
    assert_eq!(api.count("/traffic"), 1, "and not opened again");
}

#[tokio::test]
async fn disconnecting_closes_the_stream_and_clears_the_speed() {
    let (api, port) = api_with(lines(&[LINE], 50, true)).await;
    let h = start_on(false, Jitter::none(), |s| s.settings.api_port = port);
    h.app.watch_traffic(true);
    settle(300).await;
    assert_eq!(api.count("/traffic"), 0, "not connected, nothing to read");
    h.app.execute(Action::Connect).await.unwrap();
    assert!(speed(&h.app).await.is_some());

    h.app.execute(Action::Disconnect).await.unwrap();
    let mut rx = h.app.traffic();
    tokio::time::timeout(Duration::from_secs(3), rx.wait_for(|s| s.is_none()))
        .await
        .expect("the speed stayed after Disconnect")
        .unwrap();
    settle(300).await;
    assert_eq!(api.open.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(api.count("/traffic"), 1, "no reconnect while disconnected");
}

#[tokio::test]
async fn fields_outside_up_and_down_are_ignored() {
    let (_api, port) = api_with(lines(
        &[r#"{"up":7,"down":8,"note":"ignored","nested":{"a":1}}"#],
        50,
        true,
    ))
    .await;
    let h = start_on(false, Jitter::none(), |s| s.settings.api_port = port);
    h.app.execute(Action::Connect).await.unwrap();
    h.app.watch_traffic(true);
    assert_eq!(speed(&h.app).await, Some(Speed { up: 7, down: 8 }));
}

#[tokio::test]
async fn a_line_of_another_shape_ends_the_stream_and_it_is_opened_again() {
    // A good line, then rubbish, then a line that must never be believed. The pause before the
    // second opening is at least a second, whatever the spread says.
    let (api, port) = api_with(lines(&[LINE, "not json", r#"{"up":1,"down":2}"#], 30, true)).await;
    let h = start_on(false, Jitter::fixed(-10_000), |s| {
        s.settings.api_port = port
    });
    h.app.execute(Action::Connect).await.unwrap();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut rx = h.app.traffic();
    let log = seen.clone();
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            log.lock().unwrap().push(*rx.borrow_and_update());
        }
    });
    h.app.watch_traffic(true);
    tokio::time::timeout(Duration::from_secs(5), async {
        while api.count("/traffic") < 2 {
            settle(50).await;
        }
    })
    .await
    .expect("the stream was not opened again");
    settle(300).await;
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.contains(&Some(Speed {
            up: 100,
            down: 2000
        })),
        "{seen:?}"
    );
    assert!(
        seen.contains(&None),
        "the speed is cleared when the stream breaks: {seen:?}"
    );
    assert!(
        !seen.contains(&Some(Speed { up: 1, down: 2 })),
        "nothing after a bad line is believed: {seen:?}"
    );
}

#[tokio::test]
async fn three_breaks_in_a_row_are_one_error() {
    // The API accepts and drops: every open fails at once.
    let (api, port) = api_with(lines(&[], 10, false)).await;
    let h = start_on(false, Jitter::fixed(-10_000), |s| {
        s.settings.api_port = port
    });
    h.app.execute(Action::Connect).await.unwrap();
    h.app.watch_traffic(true);
    tokio::time::timeout(Duration::from_secs(8), async {
        while api.count("/traffic") < 4 {
            settle(50).await;
        }
    })
    .await
    .expect("fewer than four opens");
    let reported: Vec<_> = h
        .app
        .snapshot()
        .journal
        .into_iter()
        .filter(|e| e.failure_code == Some(FailureCode::ClashApi))
        .collect();
    assert_eq!(
        reported.len(),
        1,
        "one error for the whole outage: {reported:?}"
    );
    assert_eq!(reported[0].level, "error");
    assert_eq!(reported[0].category, Category::Core);
    assert_eq!(reported[0].message, "Не удалось получить скорость трафика");
}

#[tokio::test]
async fn a_refusal_is_a_break_like_any_other() {
    let api = Arc::new(Api::default());
    api.route("/traffic", Reply::Body("401 Unauthorized", "{}".into()));
    let port = serve(api.clone()).await;
    let h = start_on(false, Jitter::fixed(-10_000), |s| {
        s.settings.api_port = port
    });
    h.app.execute(Action::Connect).await.unwrap();
    h.app.watch_traffic(true);
    tokio::time::timeout(Duration::from_secs(4), async {
        while api.count("/traffic") < 2 {
            settle(50).await;
        }
    })
    .await
    .expect("not opened again after a refusal");
    assert_eq!(*h.app.traffic().borrow(), None);
}

#[tokio::test]
async fn the_speed_is_not_part_of_the_snapshot_or_the_journal() {
    let (_api, port) = api_with(lines(&[r#"{"up":424242,"down":737373}"#], 30, true)).await;
    let h = start_on(false, Jitter::none(), |s| s.settings.api_port = port);
    h.app.execute(Action::Connect).await.unwrap();
    h.app.watch_traffic(true);
    assert!(speed(&h.app).await.is_some());
    let text = serde_json::to_string(&h.app.snapshot()).unwrap();
    assert!(
        !text.contains("424242") && !text.contains("737373"),
        "{text}"
    );
}

#[tokio::test]
async fn a_complete_oversized_line_is_rejected_before_publishing_speed() {
    let body = format!("{}{}\n", " ".repeat(8193), LINE);
    let (_api, port) = api_with(Reply::Body("200 OK", body)).await;
    let clash = rift_core::clash::Clash::new(port, "secret").unwrap();
    let mut seen = Vec::new();
    let error = clash
        .traffic(&tokio_util::sync::CancellationToken::new(), |speed| {
            seen.push(speed)
        })
        .await
        .unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::ClashApi));
    assert!(
        seen.is_empty(),
        "oversized input must not publish a valid-looking speed"
    );
}
