//! What a refresh of a subscription does to the server in use. A provider that gives every server
//! other credentials every hour must not take the selection away; one that drops the server must
//! not leave an unchecked first server of the list in its place; and what a refresh brings that is
//! new is looked at at once. Time is the test's (`start_paused`).
mod common;

use async_trait::async_trait;
use common::fake::{pass, start_hosts_on, until, H};
use rift_core::{
    application::{Action, Application},
    jitter::Jitter,
    model::{Store, Subscription},
    parser::parse_uri,
    subscription::SubscriptionSource,
    Result,
};
use std::sync::{Arc, Mutex};

const FIVE: [Option<u64>; 5] = [Some(40), Some(100), Some(60), Some(80), Some(200)];

/// A subscription whose body the test changes.
struct Feed(Mutex<String>);
#[async_trait]
impl SubscriptionSource for Feed {
    async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
        Ok(self.0.lock().unwrap().clone())
    }
}
/// The servers on `ports`, each on a host of its own, with `password` as their credentials.
fn list(password: &str, ports: &[u16]) -> String {
    let mut text = String::new();
    for port in ports {
        text.push_str(&format!("trojan://{password}@h{port}.example.com:{port}\n"));
    }
    text
}
/// The id the server of `list(password, ..)` on `port` has.
fn id_of(password: &str, port: u16) -> String {
    parse_uri(
        &format!("trojan://{password}@h{port}.example.com:{port}"),
        Some("s".into()),
    )
    .unwrap()
    .id
}
fn lines(app: &Application) -> Vec<String> {
    app.snapshot()
        .journal
        .iter()
        .map(|e| e.message.clone())
        .collect()
}
fn started(app: &Application) -> Vec<String> {
    lines(app)
        .into_iter()
        .filter(|m| m.starts_with("Проверка серверов запущена"))
        .collect()
}
async fn connected(auto: bool, feed: &Arc<Feed>) -> H {
    let h = start_hosts_on(
        auto,
        Jitter::none(),
        &FIVE,
        feed.clone(),
        |store: &mut Store| {
            for server in &mut store.servers {
                server.source = Some("s".into());
            }
            store.subscriptions.push(Subscription {
                id: "s".into(),
                name: "s".into(),
                url: "https://example.com/s".into(),
                updated_at: Some(rift_core::application::timestamp()),
                ..Default::default()
            });
        },
    );
    for port in 6..=8u16 {
        h.net.answer(port, Some(50 + u64::from(port)));
    }
    h.app.execute(Action::Connect).await.unwrap();
    // The first tick refreshes the subscription once; with Auto Select the check of the servers
    // follows.
    until(&h.app, "the first check", |s| {
        let wanted = if auto {
            "Проверка завершена"
        } else {
            "Подписка обновлена"
        };
        s.journal.iter().any(|e| e.message.starts_with(wanted)) && !s.checking
    })
    .await;
    h
}
async fn refresh(h: &H) {
    h.app.execute(Action::Refresh { id: None }).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn credentials_rotated_at_the_same_addresses_keep_the_selection_and_start_no_check() {
    let feed = Arc::new(Feed(Mutex::new(list("p", &[1, 2, 3, 4, 5]))));
    let h = connected(true, &feed).await;
    let checks = started(&h.app).len();
    assert_eq!(h.app.snapshot().selected, Some(id_of("p", 1)));
    *feed.0.lock().unwrap() = list("q", &[1, 2, 3, 4, 5]);
    refresh(&h).await;
    // Every id is new, the server in use has followed its address.
    assert_eq!(h.app.snapshot().selected, Some(id_of("q", 1)));
    assert!(h
        .app
        .snapshot()
        .servers
        .iter()
        .all(|s| s.id != id_of("p", 1)));
    assert!(lines(&h.app)
        .iter()
        .any(|m| m == "Выбранный сервер перенесён на обновлённый"));
    // Nothing new to look at: no check starts, and the pool still knows its servers by address.
    pass(5).await;
    assert_eq!(started(&h.app).len(), checks, "{:?}", lines(&h.app));
    pass(70).await;
    assert_eq!(
        started(&h.app).len(),
        checks,
        "the fast check is not a full one"
    );
    assert!(lines(&h.app)
        .iter()
        .any(|m| m.starts_with("Быстрая проверка: 3 из 3 в норме")));
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_servers_of_a_refresh_that_changed_nothing_are_left_alone() {
    let feed = Arc::new(Feed(Mutex::new(list("p", &[1, 2, 3, 4, 5]))));
    let h = connected(true, &feed).await;
    let checks = started(&h.app).len();
    refresh(&h).await;
    pass(5).await;
    assert_eq!(started(&h.app).len(), checks);
    assert_eq!(h.app.snapshot().selected, Some(id_of("p", 1)));
    assert!(!lines(&h.app)
        .iter()
        .any(|m| m == "Выбранный сервер перенесён на обновлённый"));
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_server_in_use_whose_address_is_gone_is_replaced_at_once_by_a_full_check() {
    let feed = Arc::new(Feed(Mutex::new(list("p", &[1, 2, 3, 4, 5]))));
    let h = connected(true, &feed).await;
    let checks = started(&h.app).len();
    *feed.0.lock().unwrap() = list("p", &[2, 3, 4, 5]);
    refresh(&h).await;
    // No wait for the minute of a fast check: the full check is begun by the next tick.
    until(&h.app, "the replacement", |s| {
        s.selected.as_ref().is_some_and(|id| *id != id_of("p", 1))
            && !s.checking
            && s.journal
                .iter()
                .filter(|e| e.message.starts_with("Проверка завершена"))
                .count()
                > checks
    })
    .await;
    assert_eq!(
        started(&h.app).last().unwrap(),
        "Проверка серверов запущена: подписка принесла другие серверы"
    );
    // The best of what is left, S3 (60 ms), not the first of the list.
    assert_eq!(h.app.snapshot().selected, Some(id_of("p", 3)));
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_new_address_in_the_list_is_looked_at_at_once() {
    let feed = Arc::new(Feed(Mutex::new(list("p", &[1, 2, 3, 4, 5]))));
    let h = connected(true, &feed).await;
    let checks = started(&h.app).len();
    h.net.answer(6, Some(10));
    *feed.0.lock().unwrap() = list("p", &[1, 2, 3, 4, 5, 6]);
    refresh(&h).await;
    // The new server was measured by the check that the refresh started, without the minute of a
    // fast check or the ten of a full one.
    let new = id_of("p", 6);
    until(&h.app, "the new server to be measured", |s| {
        s.servers
            .iter()
            .any(|v| v.id == new && v.proxy_ms == Some(10))
            && !s.checking
    })
    .await;
    assert_eq!(started(&h.app).len(), checks + 1);
    assert_eq!(
        started(&h.app).last().unwrap(),
        "Проверка серверов запущена: подписка принесла другие серверы"
    );
    // S1 (40 ms) keeps its place: S6 is faster by 30 ms, the margin is 50.
    assert_eq!(h.app.snapshot().selected, Some(id_of("p", 1)));
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn with_auto_select_off_a_server_that_is_gone_is_replaced_by_one_that_is_checked_not_by_the_first(
) {
    let feed = Arc::new(Feed(Mutex::new(list("p", &[1, 2, 3, 4, 5]))));
    let h = connected(false, &feed).await;
    assert_eq!(h.app.snapshot().selected, Some(id_of("p", 1)));
    *feed.0.lock().unwrap() = list("p", &[2, 3, 4, 5]);
    refresh(&h).await;
    let selected = h.app.snapshot().selected.expect("something is selected");
    assert_ne!(selected, id_of("p", 1));
    assert!(lines(&h.app)
        .iter()
        .any(|m| m == "Выбранный сервер исчез из подписки; выбран другой, он будет проверен"));
    // The one selected is looked at, once: the check of one server, as with a manual choice.
    until(&h.app, "the check of the new selection", |s| {
        s.journal
            .iter()
            .any(|e| e.message == "Проверка серверов запущена: изменился список серверов")
    })
    .await;
    pass(5).await;
    assert!(!h.app.snapshot().settings.auto_select);
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_refresh_says_how_many_addresses_came_and_went_and_says_nothing_when_none_did() {
    let feed = Arc::new(Feed(Mutex::new(list("p", &[1, 2, 3, 4, 5]))));
    let h = connected(true, &feed).await;
    let said = |h: &H| -> Vec<String> {
        lines(&h.app)
            .into_iter()
            .filter(|m| m.starts_with("Адреса в подписке"))
            .collect()
    };
    assert!(said(&h).is_empty(), "the first look found nothing new");
    refresh(&h).await;
    assert!(said(&h).is_empty(), "the same list says nothing");
    // Credentials rotated at the same addresses: no address came or went.
    *feed.0.lock().unwrap() = list("q", &[1, 2, 3, 4, 5]);
    refresh(&h).await;
    assert!(said(&h).is_empty());
    // One address is new, another is gone.
    *feed.0.lock().unwrap() = list("q", &[2, 3, 4, 5, 6]);
    refresh(&h).await;
    assert_eq!(said(&h), ["Адреса в подписке: новых 1, пропавших 1"]);
    h.app.shutdown().await.unwrap();
}
