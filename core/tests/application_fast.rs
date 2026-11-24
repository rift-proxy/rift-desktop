//! The two checks as the application runs them: the fast one asks the pool every minute and the
//! full one every ten, a pool that gets worse or loses a server is looked at in full, a server in
//! use that dies is replaced from the pool, and a list with no working server is said so once and
//! asked of its subscriptions until it has one. Time is the test's (`start_paused`).
mod common;

use async_trait::async_trait;
use common::fake::{pass, start_hosts, start_hosts_on, until, H};
use rift_core::{
    application::{Action, Application, Snapshot},
    jitter::Jitter,
    model::{Store, Subscription},
    subscription::SubscriptionSource,
    Result,
};
use std::sync::{Arc, Mutex};

const FIVE: [Option<u64>; 5] = [Some(40), Some(100), Some(60), Some(80), Some(200)];

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
/// What the full checks decided, one line each.
fn decisions(app: &Application) -> Vec<String> {
    lines(app)
        .into_iter()
        .filter(|m| m.starts_with("Выбор сервера:"))
        .collect()
}
fn fast_lines(app: &Application) -> Vec<String> {
    lines(app)
        .into_iter()
        .filter(|m| m.starts_with("Быстрая проверка"))
        .collect()
}
fn finished(s: &Snapshot) -> usize {
    s.journal
        .iter()
        .filter(|e| e.message.starts_with("Проверка завершена"))
        .count()
}
async fn full_checks_done(h: &H, count: usize) {
    until(&h.app, "a full check to end", |s| {
        finished(s) >= count && !s.checking
    })
    .await;
}
/// The ports asked since `from` entries of the log, one per question.
fn asked_since(h: &H, from: usize) -> Vec<u16> {
    h.net.asked()[from..].to_vec()
}
fn count_of(ports: &[u16], port: u16) -> usize {
    ports.iter().filter(|p| **p == port).count()
}

#[tokio::test(start_paused = true)]
async fn the_first_full_check_builds_the_pool_and_takes_the_best_server() {
    let h = start_hosts(true, Jitter::none(), &FIVE);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    assert_eq!(
        h.app.snapshot().selected.as_deref(),
        Some(h.ids[0].as_str())
    );
    assert_eq!(started(&h.app), ["Проверка серверов запущена: подключение"]);
    assert_eq!(
        decisions(&h.app),
        ["Выбор сервера: остаёмся, текущий лучший, 40 мс"]
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_server_chosen_by_hand_outside_a_pool_that_is_not_there_yet_gives_way_to_the_best() {
    // Auto Select is on, nothing is known yet: whichever server was chosen, the best one is taken
    // and the others stay in reserve.
    let h = start_hosts_on(
        true,
        Jitter::none(),
        &FIVE,
        Arc::new(common::fake::NoFeed),
        |store| {
            store.selected = Some(store.servers[4].id.clone());
        },
    );
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    assert_eq!(
        h.app.snapshot().selected.as_deref(),
        Some(h.ids[0].as_str())
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_fast_check_asks_the_pool_and_nobody_else_and_says_what_it_found() {
    let h = start_hosts(true, Jitter::none(), &FIVE);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    let before = h.net.asked().len();
    pass(70).await;
    let asked = asked_since(&h, before);
    // The pool is the three fastest: S1, S3 and S4.
    assert_eq!(count_of(&asked, 3), 1, "{asked:?}");
    assert_eq!(count_of(&asked, 4), 1, "{asked:?}");
    assert!(
        count_of(&asked, 1) >= 1,
        "the server in use, also by the light check"
    );
    assert_eq!(count_of(&asked, 2), 0, "{asked:?}");
    assert_eq!(count_of(&asked, 5), 0, "{asked:?}");
    assert_eq!(
        fast_lines(&h.app),
        ["Быстрая проверка: 3 из 3 в норме. Задержки 40 / 60 / 80 мс, базы 40 / 60 / 80 мс"]
    );
    assert_eq!(started(&h.app).len(), 1, "no full check yet");
    assert!(!h.app.snapshot().checking);
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_pool_server_that_stays_slow_starts_a_full_check_that_asks_everyone() {
    let h = start_hosts(true, Jitter::none(), &FIVE);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    let before = h.net.asked().len();
    h.net.answer(3, Some(400));
    pass(70).await;
    full_checks_done(&h, 2).await;
    assert_eq!(
        started(&h.app).last().unwrap(),
        "Проверка серверов запущена: один из лучших серверов стал хуже"
    );
    assert_eq!(
        fast_lines(&h.app),
        ["Быстрая проверка: сервер 2 из 3 хуже: 400 мс при базе 60 мс, допуск 110 мс. Запускаю полную проверку"]
    );
    // The full check asked the servers outside the pool as well.
    let asked = asked_since(&h, before);
    assert!(
        count_of(&asked, 2) >= 1 && count_of(&asked, 5) >= 1,
        "{asked:?}"
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_noisy_answer_is_not_a_reason_for_a_full_check() {
    let h = start_hosts(true, Jitter::none(), &FIVE);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    // Slow once, then as before: the second look finds nothing.
    pass(5).await;
    h.net.answer(3, Some(400));
    // The fast check is due in 60 s; make the answer normal again after its first question.
    let net = h.net.clone();
    tokio::spawn(async move {
        loop {
            if net.count(3) >= 1 {
                net.answer(3, Some(60));
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    });
    pass(70).await;
    assert_eq!(started(&h.app).len(), 1, "{:?}", lines(&h.app));
    assert_eq!(fast_lines(&h.app).len(), 1);
    assert!(fast_lines(&h.app)[0].contains("3 из 3 в норме"));
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_pool_server_that_does_not_answer_is_left_out_of_the_full_check_that_follows() {
    let h = start_hosts(true, Jitter::none(), &FIVE);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    let before = h.net.asked().len();
    h.net.answer(4, None);
    pass(70).await;
    full_checks_done(&h, 2).await;
    assert_eq!(
        started(&h.app).last().unwrap(),
        "Проверка серверов запущена: один из лучших серверов не отвечает"
    );
    assert_eq!(
        fast_lines(&h.app),
        ["Быстрая проверка: сервер 3 из 3 не отвечает. Запускаю полную проверку"]
    );
    // Three questions in the fast check, none in the full one: it was just found silent.
    let asked = asked_since(&h, before);
    assert_eq!(count_of(&asked, 4), 3, "{asked:?}");
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_first_answer_lost_and_confirmed_by_the_next_two_leaves_the_server_working_in_the_list() {
    let h = start_hosts(true, Jitter::none(), &FIVE);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    // S3 is in the pool. In each of two fast checks its first answer is lost and the other two
    // come: it works, and neither the table nor the list of servers that do not answer may say
    // otherwise.
    h.net
        .answer_next(3, &[None, Some(60), Some(60), None, Some(60), Some(60)]);
    pass(130).await;
    let view = h
        .app
        .snapshot()
        .servers
        .into_iter()
        .find(|s| s.id == h.ids[2])
        .unwrap();
    assert_eq!(view.status, "online");
    assert_eq!(view.failure_code, None);
    assert_eq!(
        view.retry_at, None,
        "not in the list of servers that do not answer"
    );
    assert_eq!(view.proxy_ms, Some(60));
    assert_eq!(started(&h.app).len(), 1, "nothing was lost, no full check");
    // The next full check asks it too (it would be held back if the list had it).
    h.app.execute(Action::CheckNow).await.unwrap();
    full_checks_done(&h, 2).await;
    let before = h.net.count(3);
    assert!(before >= 7, "{before}: asked by both full checks");
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_server_in_use_that_dies_is_replaced_from_the_pool_at_once() {
    // A spread of 10 s: the light check comes every 25 s and the fast one after 70, so the fast
    // one is the first to find the server dead when it dies at 55 s.
    let h = start_hosts(true, Jitter::fixed(10_000), &FIVE);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    assert_eq!(
        h.app.snapshot().selected.as_deref(),
        Some(h.ids[0].as_str())
    );
    pass(55).await;
    h.net.answer(1, None);
    until(&h.app, "the replacement", |s| {
        s.selected.as_deref() != Some(h.ids[0].as_str()) && s.selected.is_some()
    })
    .await;
    // The first of the rest of the pool, S3 (60 ms), took its place, not the closest or the last.
    assert_eq!(
        h.app.snapshot().selected.as_deref(),
        Some(h.ids[2].as_str())
    );
    full_checks_done(&h, 2).await;
    assert!(fast_lines(&h.app)
        .iter()
        .any(|l| l.contains("используемый сервер не отвечает (сервер 1 из 3)")));
    assert_eq!(
        started(&h.app).last().unwrap(),
        "Проверка серверов запущена: используемый сервер не отвечает"
    );
    assert_eq!(
        decisions(&h.app).last().unwrap(),
        "Выбор сервера: смена, текущий не отвечает; новый 60 мс"
    );
    // The dead server was not asked again by the check that replaced it.
    assert_eq!(
        h.app.snapshot().selected.as_deref(),
        Some(h.ids[2].as_str())
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_list_no_longer_than_the_pool_is_looked_at_whole_every_time() {
    let h = start_hosts(true, Jitter::none(), &[Some(40), Some(100), Some(60)]);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    pass(70).await;
    full_checks_done(&h, 2).await;
    assert_eq!(
        started(&h.app).last().unwrap(),
        "Проверка серверов запущена: в списке не больше серверов, чем в запасе"
    );
    assert!(fast_lines(&h.app).is_empty());
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn fast_ticks_that_become_full_checks_keep_the_scheduled_full_check_away() {
    // Three servers: every fast tick is a full check. The full timer counts from the end of the
    // last of them, so it never runs out and no check is ever the scheduled one.
    let h = start_hosts(true, Jitter::none(), &[Some(40), Some(100), Some(60)]);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    pass(11 * 60).await;
    let reasons = started(&h.app);
    assert!(reasons.len() >= 10, "{reasons:?}");
    assert!(
        reasons.iter().all(|m| !m.ends_with("по расписанию")),
        "{reasons:?}"
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_full_check_comes_every_ten_minutes_after_the_last_one_ended_and_fast_ones_in_between()
{
    let h = start_hosts(true, Jitter::none(), &FIVE);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    pass(11 * 60).await;
    full_checks_done(&h, 2).await;
    assert_eq!(
        started(&h.app),
        [
            "Проверка серверов запущена: подключение",
            "Проверка серверов запущена: по расписанию"
        ]
    );
    let fast = fast_lines(&h.app).len();
    assert!(
        (8..=10).contains(&fast),
        "{fast} fast checks in ten minutes"
    );
    assert_eq!(decisions(&h.app).len(), 2, "one line for each full check");
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn no_working_server_is_said_once_however_long_it_lasts() {
    let h = start_hosts(true, Jitter::none(), &[None, None, None, None, None]);
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "the error", |s| s.error.is_some() && !s.checking).await;
    pass(5 * 60).await;
    let errors: Vec<_> = h
        .app
        .snapshot()
        .journal
        .into_iter()
        .filter(|e| e.level == "error")
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    let still = lines(&h.app)
        .into_iter()
        .filter(|m| m == "Рабочих серверов по-прежнему нет")
        .count();
    assert!(still >= 3, "the checks go on every minute: {still}");
    assert!(
        h.app.snapshot().error.is_some(),
        "the banner stays up while it lasts"
    );
    h.app.shutdown().await.unwrap();
}

/// A subscription whose body the test changes.
struct Feed {
    body: Mutex<String>,
    fetched: Mutex<usize>,
}
#[async_trait]
impl SubscriptionSource for Feed {
    async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
        *self.fetched.lock().unwrap() += 1;
        Ok(self.body.lock().unwrap().clone())
    }
}
fn list(ports: &[u16]) -> String {
    let mut text = String::new();
    for p in ports {
        text.push_str(&format!("trojan://p@h{p}.example.com:{p}\n"));
    }
    text
}

#[tokio::test(start_paused = true)]
async fn while_nothing_works_the_subscription_is_asked_for_new_servers_and_the_check_follows() {
    let feed = Arc::new(Feed {
        body: Mutex::new(list(&[1, 2, 3, 4])),
        fetched: Mutex::new(0),
    });
    let h = start_hosts_on(
        true,
        Jitter::none(),
        &[None, None, None, None],
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
    h.app.execute(Action::Connect).await.unwrap();
    until(&h.app, "no server", |s| s.error.is_some() && !s.checking).await;
    // The provider has new servers by now: the next question brings them, and they work.
    for port in 5..=8u16 {
        h.net.answer(port, Some(50 + u64::from(port)));
    }
    *feed.body.lock().unwrap() = list(&[5, 6, 7, 8]);
    until(&h.app, "a working server from the new list", |s| {
        s.selected.is_some() && s.error.is_none() && !s.checking && s.status == "connected"
    })
    .await;
    assert!(lines(&h.app)
        .iter()
        .any(|m| m == "Рабочие серверы снова есть"));
    let selected = h.app.snapshot().selected.unwrap();
    assert!(
        !h.ids.contains(&selected),
        "the selected server is one of the new list, not of the old"
    );
    assert!(*feed.fetched.lock().unwrap() >= 2);
    h.app.shutdown().await.unwrap();
}

/// A subscription that cannot be downloaded: when it was asked, in seconds from the start.
struct DeadFeed {
    start: tokio::time::Instant,
    asked_at: Mutex<Vec<u64>>,
}
#[async_trait]
impl SubscriptionSource for DeadFeed {
    async fn fetch(&self, _: &str, proxy_port: Option<u16>) -> Result<String> {
        // A download that fails directly is tried once more through the local proxy: one question
        // of the provider is the direct one.
        if proxy_port.is_none() {
            self.asked_at
                .lock()
                .unwrap()
                .push(self.start.elapsed().as_secs());
        }
        Err(rift_core::Error::Network)
    }
}

#[tokio::test(start_paused = true)]
async fn while_nothing_works_a_subscription_that_fails_keeps_its_pause_between_downloads() {
    let feed = Arc::new(DeadFeed {
        start: tokio::time::Instant::now(),
        asked_at: Mutex::new(vec![]),
    });
    let h = start_hosts_on(
        true,
        Jitter::none(),
        &[None, None, None],
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
    h.app.execute(Action::Connect).await.unwrap();
    pass(15 * 60).await;
    let asked = feed.asked_at.lock().unwrap().clone();
    // The pause after a failed download doubles from a minute (60, 120, 240 s...), outage or not:
    // asking every minute of the outage would hammer a provider that is already down.
    assert!(
        (3..=5).contains(&asked.len()),
        "downloads at {asked:?} seconds in a quarter of an hour"
    );
    for (n, pair) in asked.windows(2).enumerate() {
        let pause = 60u64 << n;
        assert!(
            pair[1] - pair[0] >= pause,
            "download {} came after {} s, the pause is {pause} s: {asked:?}",
            n + 2,
            pair[1] - pair[0]
        );
    }
    let errors = lines(&h.app)
        .into_iter()
        .filter(|m| m == "Не удалось обновить подписку; предыдущие серверы сохранены")
        .count();
    assert_eq!(errors, asked.len(), "one entry for each download");
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_hour_of_an_idle_connection_asks_far_fewer_servers_than_a_full_check_every_minute_would()
{
    // 46 servers, as in the subscription the load was measured on. The full check takes no time
    // here (no pauses), so the full checks come every ten minutes exactly: more than in life.
    let answers: Vec<Option<u64>> = (0..46).map(|i| Some(40 + i)).collect();
    let h = start_hosts(true, Jitter::none(), &answers);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    let before = h.net.asked().len();
    let started_before = started(&h.app).len();
    pass(3600).await;
    let asked = h.net.asked().len() - before;
    let fulls = started(&h.app).len() - started_before;
    let fasts = fast_lines(&h.app).len();
    // Every ten minutes a full check: all 46 and three more answers of the best five (about 57
    // questions each); every minute a fast check of three; the light check of the server in use
    // every ~15 s on top of that.
    assert!((5..=6).contains(&fulls), "{fulls} full checks");
    assert!((50..=55).contains(&fasts), "{fasts} fast checks");
    assert!(
        asked < 900,
        "{asked} questions in an hour; a full check every minute would be about 3400"
    );
    assert!(asked > 400, "{asked}: the checks did run");
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_manual_choice_leaves_auto_select_on_and_is_watched_like_the_pool() {
    let h = start_hosts(true, Jitter::none(), &FIVE);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    // S5 is the slowest, outside the pool of the three fastest.
    h.app
        .execute(Action::Select {
            id: h.ids[4].clone(),
        })
        .await
        .unwrap();
    let snapshot = h.app.snapshot();
    assert!(
        snapshot.settings.auto_select,
        "a choice by hand does not turn it off"
    );
    assert_eq!(snapshot.selected.as_deref(), Some(h.ids[4].as_str()));
    assert_eq!(h.net.selects.lock().unwrap().last(), Some(&h.ids[4]));
    assert!(lines(&h.app)
        .iter()
        .any(|m| m == "Сервер выбран вручную; следующие проверки сравнят его с остальными"));
    // The fast check asks four servers: the three of the pool and the one chosen.
    pass(70).await;
    assert_eq!(
        fast_lines(&h.app),
        ["Быстрая проверка: 4 из 4 в норме. Задержки 40 / 60 / 80 / 200 мс, базы 40 / 60 / 80 / 200 мс"]
    );
    assert_eq!(started(&h.app).len(), 1, "nothing else was started");
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_server_chosen_by_hand_that_gets_worse_starts_a_full_check_like_a_pool_server() {
    let h = start_hosts(true, Jitter::none(), &FIVE);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    h.app
        .execute(Action::Select {
            id: h.ids[4].clone(),
        })
        .await
        .unwrap();
    h.net.answer(5, Some(900));
    pass(70).await;
    full_checks_done(&h, 2).await;
    assert_eq!(
        fast_lines(&h.app),
        ["Быстрая проверка: выбранный вручную сервер хуже: 900 мс при базе 200 мс, допуск 300 мс. Запускаю полную проверку"]
    );
    assert_eq!(
        started(&h.app).last().unwrap(),
        "Проверка серверов запущена: один из лучших серверов стал хуже"
    );
    // The pool is complete, the chosen server is far slower than the best: the full check ends
    // with the best one in use.
    assert_eq!(
        h.app.snapshot().selected.as_deref(),
        Some(h.ids[0].as_str())
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_base_of_a_server_chosen_by_hand_does_not_slide_with_its_delays() {
    // Seven servers: S7 (300 ms) is outside the pool and got no second look in the full check, so
    // its base is the one answer it gave. Its delay grows in two steps, each within the tolerance
    // of the one before (450 ms at the base of 300, 600 ms at 400), but the second beyond the
    // tolerance of the base: the base stays where the full check left it.
    let h = start_hosts(
        true,
        Jitter::none(),
        &[
            Some(40),
            Some(50),
            Some(60),
            Some(70),
            Some(80),
            Some(90),
            Some(300),
        ],
    );
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    h.app
        .execute(Action::Select {
            id: h.ids[6].clone(),
        })
        .await
        .unwrap();
    pass(70).await;
    assert_eq!(started(&h.app).len(), 1, "{:?}", fast_lines(&h.app));
    h.net.answer(7, Some(400));
    pass(60).await;
    assert_eq!(started(&h.app).len(), 1, "{:?}", fast_lines(&h.app));
    h.net.answer(7, Some(500));
    pass(60).await;
    full_checks_done(&h, 2).await;
    assert!(
        fast_lines(&h.app)
            .iter()
            .any(|l| l.contains("выбранный вручную сервер хуже: 500 мс при базе 300 мс")),
        "{:?}",
        fast_lines(&h.app)
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_server_chosen_by_hand_that_dies_is_replaced_from_the_pool() {
    let h = start_hosts(true, Jitter::fixed(10_000), &FIVE);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    h.app
        .execute(Action::Select {
            id: h.ids[4].clone(),
        })
        .await
        .unwrap();
    // The light check comes every 25 s and the fast one 70 s after the choice (the cooldown of a
    // switch): at 55 s the fast check is the next to ask.
    pass(55).await;
    h.net.answer(5, None);
    until(&h.app, "the replacement", |s| {
        s.selected.as_deref() != Some(h.ids[4].as_str())
    })
    .await;
    assert_eq!(
        h.app.snapshot().selected.as_deref(),
        Some(h.ids[0].as_str())
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_choice_by_hand_with_auto_select_off_stays_a_choice_by_hand() {
    let h = start_hosts(false, Jitter::none(), &FIVE);
    h.app.execute(Action::Connect).await.unwrap();
    h.app
        .execute(Action::Select {
            id: h.ids[4].clone(),
        })
        .await
        .unwrap();
    pass(15 * 60).await;
    assert!(!h.app.snapshot().settings.auto_select);
    assert_eq!(
        h.app.snapshot().selected.as_deref(),
        Some(h.ids[4].as_str())
    );
    assert!(
        started(&h.app).is_empty(),
        "no check of the servers by itself"
    );
    assert!(lines(&h.app).iter().any(|m| m == "Сервер выбран вручную"));
    h.app.shutdown().await.unwrap();
}

fn exclude(h: &H, index: usize, excluded: bool) -> impl std::future::Future<Output = ()> + '_ {
    let id = h.ids[index].clone();
    async move {
        h.app
            .execute(Action::SetServerExcluded { id, excluded })
            .await
            .unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn a_server_kept_out_of_auto_select_is_asked_by_no_check_and_comes_back_when_returned() {
    let h = start_hosts(true, Jitter::none(), &FIVE);
    exclude(&h, 2, true).await; // S3, one of the three fastest
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    h.app.execute(Action::CheckNow).await.unwrap();
    full_checks_done(&h, 2).await;
    pass(70).await;
    assert_eq!(h.net.count(3), 0, "no check asked it");
    assert!(
        fast_lines(&h.app)[0].contains("Задержки 40 / 80 / 100 мс"),
        "the pool is S1, S4 and S2: {:?}",
        fast_lines(&h.app)
    );
    // Returned, it is a candidate again: the check by hand asks it.
    exclude(&h, 2, false).await;
    h.app.execute(Action::CheckNow).await.unwrap();
    full_checks_done(&h, 3).await;
    assert!(h.net.count(3) >= 1);
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_pool_is_made_up_again_at_once_when_a_server_of_it_is_kept_out() {
    let h = start_hosts(true, Jitter::none(), &FIVE);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    let before = h.net.asked().len();
    exclude(&h, 2, true).await;
    pass(70).await;
    // S3 left the pool, S2 took its place from the ranking of the last full check: no question
    // went to anybody for it, and no full check began.
    let asked = asked_since(&h, before);
    assert_eq!(count_of(&asked, 3), 0, "{asked:?}");
    assert_eq!(started(&h.app).len(), 1);
    assert_eq!(
        fast_lines(&h.app),
        ["Быстрая проверка: 3 из 3 в норме. Задержки 40 / 80 / 100 мс, базы 40 / 80 / 100 мс"]
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_server_kept_out_that_is_in_use_is_asked_while_it_is_and_not_after() {
    // S5 was chosen long ago and then kept out; Auto Select takes the best server at the first
    // full check, and the excluded one is not asked again.
    let h = start_hosts_on(
        true,
        Jitter::none(),
        &FIVE,
        Arc::new(common::fake::NoFeed),
        |store| {
            store.selected = Some(store.servers[4].id.clone());
        },
    );
    exclude(&h, 4, true).await;
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    assert!(h.net.count(5) >= 1, "asked while it was the one in use");
    assert_eq!(
        h.app.snapshot().selected.as_deref(),
        Some(h.ids[0].as_str())
    );
    let before = h.net.count(5);
    pass(12 * 60).await;
    assert_eq!(h.net.count(5), before, "not asked once it is not in use");
    // It never became part of the pool.
    assert!(fast_lines(&h.app)
        .iter()
        .all(|l| l.contains("3 из 3 в норме. Задержки 40 / 60 / 80 мс")));
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_list_no_longer_than_the_pool_keeps_the_server_in_use_against_a_gain_that_is_small() {
    // Two servers: the pool can only ever hold two (K_eff is 2), so it is complete after the first
    // full check and the thresholds apply. Five milliseconds are no reason to cut the connections.
    let h = start_hosts(true, Jitter::none(), &[Some(100), Some(105)]);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    assert_eq!(
        h.app.snapshot().selected.as_deref(),
        Some(h.ids[0].as_str())
    );
    let switches = h.net.selects.lock().unwrap().len();
    for round in 0..4u64 {
        // The server in use is the slower one every other minute.
        h.net
            .answer(1, Some(if round % 2 == 0 { 110 } else { 100 }));
        pass(70).await;
    }
    full_checks_done(&h, 4).await;
    assert_eq!(
        h.app.snapshot().selected.as_deref(),
        Some(h.ids[0].as_str())
    );
    assert_eq!(
        h.net.selects.lock().unwrap().len(),
        switches,
        "the selector did not move: {:?}",
        decisions(&h.app)
    );
    assert!(
        decisions(&h.app)
            .iter()
            .skip(1)
            .all(|d| d.starts_with("Выбор сервера: остаёмся")),
        "{:?}",
        decisions(&h.app)
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_list_no_longer_than_the_pool_still_leaves_a_server_that_is_clearly_slower() {
    let h = start_hosts(true, Jitter::none(), &[Some(100), Some(105)]);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    h.net.answer(1, Some(400));
    pass(70).await;
    full_checks_done(&h, 2).await;
    assert_eq!(
        h.app.snapshot().selected.as_deref(),
        Some(h.ids[1].as_str())
    );
    assert_eq!(
        decisions(&h.app).last().unwrap(),
        "Выбор сервера: смена, новый быстрее: 400 → 105 мс"
    );
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn fewer_working_servers_than_the_pool_size_make_a_complete_pool_of_them() {
    // Five servers, two of them work: the pool holds both (K_eff is 2) and is complete, so the
    // full check ten minutes later leaves the server in use for a gain of five milliseconds.
    let h = start_hosts(
        true,
        Jitter::none(),
        &[Some(100), Some(105), None, None, None],
    );
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    assert_eq!(
        h.app.snapshot().selected.as_deref(),
        Some(h.ids[0].as_str())
    );
    let switches = h.net.selects.lock().unwrap().len();
    h.net.answer(1, Some(110));
    pass(11 * 60).await;
    full_checks_done(&h, 2).await;
    assert_eq!(
        decisions(&h.app).last().unwrap(),
        "Выбор сервера: остаёмся на текущем, 110 мс: лучший, 105 мс, не быстрее его на 30 % и 50 мс"
    );
    assert_eq!(h.net.selects.lock().unwrap().len(), switches);
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_pool_that_lost_a_member_takes_the_best_server_without_the_thresholds() {
    // The server in use is chosen by hand from the pool: 60 ms against the best 40 ms is no clear
    // gain, and while the pool is complete it stays. Removing a member makes the pool incomplete,
    // and the full check that follows ends with the best server.
    let h = start_hosts(true, Jitter::none(), &FIVE);
    h.app.execute(Action::Connect).await.unwrap();
    full_checks_done(&h, 1).await;
    h.app
        .execute(Action::Select {
            id: h.ids[2].clone(),
        })
        .await
        .unwrap();
    h.app
        .execute(Action::RemoveServer {
            id: h.ids[3].clone(),
        })
        .await
        .unwrap();
    full_checks_done(&h, 2).await;
    assert_eq!(
        h.app.snapshot().selected.as_deref(),
        Some(h.ids[0].as_str()),
        "{:?}",
        decisions(&h.app)
    );
    assert_eq!(
        decisions(&h.app).last().unwrap(),
        "Выбор сервера: смена на лучший, пока запас не набран: 60 → 40 мс"
    );
    h.app.shutdown().await.unwrap();
}

/// A store with one subscription behind the servers, for the tests that hold a full check.
fn with_subscription(store: &mut Store) {
    for server in &mut store.servers {
        server.source = Some("s".into());
    }
    store.subscriptions.push(Subscription {
        id: "s".into(),
        name: "s".into(),
        url: "https://provider.example/sub/token".into(),
        updated_at: Some(rift_core::application::timestamp()),
        ..Default::default()
    });
}

#[tokio::test(start_paused = true)]
async fn the_address_of_a_subscription_is_given_while_a_full_check_holds_the_coordinator() {
    let h = start_hosts_on(
        true,
        Jitter::none(),
        &FIVE,
        Arc::new(common::fake::NoFeed),
        with_subscription,
    );
    // The first question of the first full check is held: the worker keeps the coordinator.
    *h.net.gate.lock().unwrap() = true;
    h.app.execute(Action::Connect).await.unwrap();
    h.net.entered.notified().await;
    assert!(h.app.snapshot().checking, "a full check is running");
    let address = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        h.app.subscription_url("s"),
    )
    .await
    .expect("the address does not wait for the check")
    .unwrap();
    assert_eq!(address, "https://provider.example/sub/token");
    assert!(h.app.subscription_url("other").await.is_err());
    h.net.release.notify_one();
    full_checks_done(&h, 1).await;
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_address_that_was_edited_is_the_one_given_afterwards() {
    let h = start_hosts_on(
        true,
        Jitter::none(),
        &FIVE,
        Arc::new(common::fake::NoFeed),
        with_subscription,
    );
    h.app
        .execute(Action::SaveSubscription {
            id: Some("s".into()),
            url: "https://provider.example/sub/edited".into(),
            interval_mins: None,
            user_agent: None,
            filters: Default::default(),
            provider_routes: true,
        })
        .await
        .unwrap();
    assert_eq!(
        h.app.subscription_url("s").await.unwrap(),
        "https://provider.example/sub/edited"
    );
    h.app
        .execute(Action::RemoveSubscription { id: "s".into() })
        .await
        .unwrap();
    assert!(h.app.subscription_url("s").await.is_err(), "removed");
    h.app.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn keeping_a_server_out_waits_for_the_full_check_that_runs_and_is_applied_after_it() {
    // What is not settled by this fix: every action but the ones that cancel the check waits for the
    // coordinator. The exclusion is not lost or half done meanwhile; it is applied when the check ends.
    let h = start_hosts(true, Jitter::none(), &FIVE);
    *h.net.gate.lock().unwrap() = true;
    h.app.execute(Action::Connect).await.unwrap();
    h.net.entered.notified().await;
    let id = h.ids[2].clone();
    let app = h.app.clone();
    let pending = tokio::spawn(async move {
        app.execute(Action::SetServerExcluded { id, excluded: true })
            .await
    });
    pass(10).await;
    assert!(!pending.is_finished(), "waits for the check");
    assert!(h.app.snapshot().servers.iter().all(|s| !s.excluded));
    h.net.release.notify_one();
    pending.await.unwrap().unwrap();
    assert!(h.app.snapshot().servers[2].excluded);
    h.app.shutdown().await.unwrap();
}
