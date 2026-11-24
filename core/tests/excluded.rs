//! Servers the user keeps out of Auto Select: named by the source and the address, so that two
//! servers with one name are two, and a rotation of credentials changes nothing; kept in the saved
//! state, and forgotten when nothing in the list stands behind the key. At the end: the address of
//! a subscription, which leaves the core only for the dialog that edits it.
use async_trait::async_trait;
use rift_core::{
    application::{Action, Application},
    diagnostics::Stage,
    failure::FailureCode,
    model::{Server, Store, Subscription},
    parser::parse_uri,
    persistence,
    runtime::Runtime,
    selection::Probe,
    subscription::{merge, parse_subscription, SubscriptionSource},
    Error, Result,
};
use std::{path::Path, sync::Arc};

fn server(name: &str, host: &str, port: u16, source: Option<&str>) -> Server {
    // The credentials differ by source: a copy of one outbound in two sources would share an id.
    parse_uri(
        &format!(
            "trojan://{}@{host}:{port}#{name}",
            source.unwrap_or("manual")
        ),
        source.map(str::to_owned),
    )
    .unwrap()
}
fn subscription(id: &str) -> Subscription {
    Subscription {
        id: id.into(),
        name: id.into(),
        url: "https://example.com/s".into(),
        ..Default::default()
    }
}
fn store(servers: Vec<Server>) -> Store {
    let sources: std::collections::BTreeSet<String> =
        servers.iter().filter_map(|s| s.source.clone()).collect();
    Store {
        subscriptions: sources.iter().map(|s| subscription(s)).collect(),
        servers,
        ..Store::default()
    }
}
fn ids(servers: &[Server]) -> Vec<String> {
    servers.iter().map(|s| s.id.clone()).collect()
}

#[test]
fn an_excluded_server_is_not_a_candidate_and_servers_with_its_name_are() {
    // Two servers of one subscription share a name; a third sits at the same address in another
    // subscription and a fourth was added by hand.
    let a = server("Berlin", "a.example.com", 443, Some("s"));
    let b = server("Berlin", "b.example.com", 443, Some("s"));
    let elsewhere = server("Berlin", "a.example.com", 443, Some("t"));
    let manual = server("Berlin", "a.example.com", 443, None);
    let mut store = store(vec![
        a.clone(),
        b.clone(),
        elsewhere.clone(),
        manual.clone(),
    ]);
    store.excluded = vec![a.stable_key()];
    let candidates = store.auto_candidates(0);
    let keys: Vec<String> = candidates.iter().map(Server::stable_key).collect();
    assert!(!keys.contains(&a.stable_key()));
    assert!(
        keys.contains(&b.stable_key()),
        "the other server of that name"
    );
    assert!(
        keys.contains(&elsewhere.stable_key()),
        "the same address of another source"
    );
    assert!(
        keys.contains(&manual.stable_key()),
        "the same address added by hand"
    );
    assert_eq!(candidates.len(), 3);
}

#[test]
fn the_exclusion_follows_the_address_not_the_credentials() {
    let before = server("Berlin", "a.example.com", 443, Some("s"));
    let rotated = parse_uri("trojan://other@a.example.com:443#Berlin", Some("s".into())).unwrap();
    assert_ne!(before.id, rotated.id);
    let mut store = store(vec![rotated.clone()]);
    store.excluded = vec![before.stable_key()];
    assert!(store.auto_candidates(0).is_empty());
    assert!(store.is_excluded(&rotated));
}

#[test]
fn the_server_in_use_that_is_excluded_is_found_for_the_check_that_asks_it() {
    let a = server("A", "a.example.com", 443, None);
    let b = server("B", "b.example.com", 443, None);
    let mut store = store(vec![a.clone(), b.clone()]);
    store.excluded = vec![a.stable_key()];
    assert!(store.excluded_in_use().is_none(), "nothing selected");
    store.selected = Some(b.id.clone());
    assert!(
        store.excluded_in_use().is_none(),
        "the one selected is not excluded"
    );
    store.selected = Some(a.id.clone());
    assert_eq!(store.excluded_in_use().map(|s| s.id), Some(a.id));
    // Still not a candidate: it is asked as the one in use, not chosen among the others.
    assert_eq!(ids(&store.auto_candidates(0)), [b.id]);
}

#[test]
fn keys_with_no_server_behind_them_are_dropped() {
    let a = server("A", "a.example.com", 443, Some("s"));
    let gone = server("Gone", "gone.example.com", 443, Some("s"));
    let mut store = store(vec![a.clone()]);
    store.excluded = vec![
        gone.stable_key(),
        a.stable_key(),
        a.stable_key(),
        "junk".into(),
    ];
    store.prune_excluded();
    assert_eq!(store.excluded, [a.stable_key()], "one key, once");
}

#[test]
fn a_refresh_that_no_longer_lists_a_server_forgets_its_key() {
    let mut store = store(vec![]);
    store = merge(
        &store,
        "s",
        parse_subscription(
            "trojan://p@a.example.com:1\ntrojan://p@b.example.com:2",
            "s",
        )
        .unwrap(),
    );
    store.subscriptions.push(subscription("s"));
    let a = store.servers[0].stable_key();
    let b = store.servers[1].stable_key();
    store.excluded = vec![a.clone(), b];
    // The provider rotates the credentials of a and drops b.
    let next = merge(
        &store,
        "s",
        parse_subscription("trojan://q@a.example.com:1", "s").unwrap(),
    );
    assert_eq!(
        next.excluded,
        [a],
        "a is still there under new credentials, b is gone"
    );
}

#[test]
fn a_refresh_of_one_subscription_leaves_the_keys_of_the_others_alone() {
    let mut store = store(vec![
        server("A", "a.example.com", 1, Some("s")),
        server("B", "b.example.com", 2, Some("t")),
    ]);
    store.excluded = vec![store.servers[0].stable_key(), store.servers[1].stable_key()];
    let next = merge(
        &store,
        "s",
        parse_subscription("trojan://p@c.example.com:3", "s").unwrap(),
    );
    assert_eq!(next.excluded, [store.servers[1].stable_key()]);
}

#[test]
fn a_saved_state_without_the_field_has_no_exclusions() {
    let mut state = serde_json::to_value(Store::default()).unwrap();
    state.as_object_mut().unwrap().remove("excluded");
    let store = persistence::decode(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(store.excluded.is_empty());
}

#[test]
fn reading_a_saved_state_drops_keys_that_match_nothing_and_refuses_a_flood() {
    let a = server("A", "a.example.com", 443, Some("s"));
    let mut state = store(vec![a.clone()]);
    state.excluded = vec![a.stable_key(), "s|gone.example.com:1".into()];
    let read = persistence::decode(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(read.excluded, [a.stable_key()]);
    // A file with more keys than a list can hold is damaged, not a reason to keep them all.
    state.excluded = (0..5000).map(|i| format!("s|h{i}.example.com:1")).collect();
    assert!(persistence::decode(&serde_json::to_vec(&state).unwrap()).is_err());
    state.excluded = vec!["x".repeat(2000)];
    assert!(persistence::decode(&serde_json::to_vec(&state).unwrap()).is_err());
}

struct Idle;
#[async_trait]
impl Runtime for Idle {
    async fn validate(&mut self, _: &Store, _: &str) -> Result<()> {
        Ok(())
    }
    async fn start(&mut self, _: &Store, _: &str) -> Result<()> {
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(false)
    }
}
struct Fine;
#[async_trait]
impl Probe for Fine {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        Ok(1)
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        Ok(1)
    }
    async fn select(&self, _: &str) -> Result<()> {
        Ok(())
    }
}
fn start(store: Store, dir: &Path) -> Arc<Application> {
    Application::with_probe(
        store,
        dir.into(),
        Box::new(Idle),
        Arc::new(Fine),
        "x".repeat(32),
    )
}
fn saved(dir: &Path) -> Store {
    persistence::load(&dir.join("state.json")).unwrap()
}

#[tokio::test]
async fn the_action_excludes_and_returns_a_server_and_the_choice_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let a = server("A", "a.example.com", 443, None);
    let b = server("A", "b.example.com", 443, None);
    let app = start(store(vec![a.clone(), b.clone()]), dir.path());
    let snapshot = app
        .execute(Action::SetServerExcluded {
            id: a.id.clone(),
            excluded: true,
        })
        .await
        .unwrap();
    let flags: Vec<(String, bool)> = snapshot
        .servers
        .iter()
        .map(|s| (s.id.clone(), s.excluded))
        .collect();
    assert_eq!(flags, [(a.id.clone(), true), (b.id.clone(), false)]);
    assert_eq!(saved(dir.path()).excluded, [a.stable_key()]);
    // Asking again changes nothing; returning it empties the list.
    app.execute(Action::SetServerExcluded {
        id: a.id.clone(),
        excluded: true,
    })
    .await
    .unwrap();
    assert_eq!(saved(dir.path()).excluded, [a.stable_key()]);
    let snapshot = app
        .execute(Action::SetServerExcluded {
            id: a.id.clone(),
            excluded: false,
        })
        .await
        .unwrap();
    assert!(snapshot.servers.iter().all(|s| !s.excluded));
    assert!(saved(dir.path()).excluded.is_empty());
}

#[tokio::test]
async fn one_outbound_in_two_sources_is_excluded_and_returned_as_a_whole_from_either_row() {
    // The same link in two subscriptions has one id and two keys (the source is part of the key).
    // The row clicked may be either, so the action takes both rows that share the id: otherwise a
    // click on the second row would exclude the first, and the second would stay a candidate.
    let link = "trojan://same@a.example.com:443#Berlin";
    let in_s = parse_uri(link, Some("s".into())).unwrap();
    let in_t = parse_uri(link, Some("t".into())).unwrap();
    assert_eq!(in_s.id, in_t.id);
    assert_ne!(in_s.stable_key(), in_t.stable_key());
    let other = server("B", "b.example.com", 443, Some("s"));
    let dir = tempfile::tempdir().unwrap();
    let app = start(
        store(vec![in_s.clone(), in_t.clone(), other.clone()]),
        dir.path(),
    );
    let snapshot = app
        .execute(Action::SetServerExcluded {
            id: in_t.id.clone(),
            excluded: true,
        })
        .await
        .unwrap();
    let flags: Vec<bool> = snapshot.servers.iter().map(|s| s.excluded).collect();
    assert_eq!(flags, [true, true, false]);
    let mut keys = saved(dir.path()).excluded;
    keys.sort();
    let mut both = vec![in_s.stable_key(), in_t.stable_key()];
    both.sort();
    assert_eq!(keys, both);
    // Once more: nothing changes, and no key is written twice.
    app.execute(Action::SetServerExcluded {
        id: in_s.id.clone(),
        excluded: true,
    })
    .await
    .unwrap();
    assert_eq!(saved(dir.path()).excluded.len(), 2);
    // Returned from the other row, both come back.
    let snapshot = app
        .execute(Action::SetServerExcluded {
            id: in_s.id.clone(),
            excluded: false,
        })
        .await
        .unwrap();
    assert!(snapshot.servers.iter().all(|s| !s.excluded));
    assert!(saved(dir.path()).excluded.is_empty());
}

#[tokio::test]
async fn a_copy_left_out_of_a_saved_exclusion_joins_it_when_the_server_is_excluded_again() {
    let link = "trojan://same@a.example.com:443#Berlin";
    let in_s = parse_uri(link, Some("s".into())).unwrap();
    let in_t = parse_uri(link, Some("t".into())).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut state = store(vec![in_s.clone(), in_t.clone()]);
    state.excluded = vec![in_s.stable_key()];
    let app = start(state, dir.path());
    let snapshot = app
        .execute(Action::SetServerExcluded {
            id: in_t.id.clone(),
            excluded: true,
        })
        .await
        .unwrap();
    assert!(snapshot.servers.iter().all(|s| s.excluded));
    assert_eq!(saved(dir.path()).excluded.len(), 2);
}

#[tokio::test]
async fn a_server_that_is_not_in_the_list_cannot_be_excluded() {
    let dir = tempfile::tempdir().unwrap();
    let app = start(Store::default(), dir.path());
    assert!(app
        .execute(Action::SetServerExcluded {
            id: "nope".into(),
            excluded: true
        })
        .await
        .is_err());
}

#[tokio::test]
async fn removing_a_server_or_a_subscription_forgets_its_key() {
    let dir = tempfile::tempdir().unwrap();
    let manual = server("M", "m.example.com", 443, None);
    let from_sub = server("S", "s.example.com", 443, Some("s"));
    let app = start(store(vec![manual.clone(), from_sub.clone()]), dir.path());
    for server in [&manual, &from_sub] {
        app.execute(Action::SetServerExcluded {
            id: server.id.clone(),
            excluded: true,
        })
        .await
        .unwrap();
    }
    app.execute(Action::RemoveServer {
        id: manual.id.clone(),
    })
    .await
    .unwrap();
    assert_eq!(saved(dir.path()).excluded, [from_sub.stable_key()]);
    app.execute(Action::RemoveSubscription { id: "s".into() })
        .await
        .unwrap();
    assert!(saved(dir.path()).excluded.is_empty());
}

#[test]
fn the_profile_carries_the_exclusions_with_the_ids_of_the_subscriptions() {
    use rift_core::profile;
    let a = server("A", "a.example.com", 443, Some("s"));
    let mut store = store(vec![a.clone()]);
    store.excluded = vec![a.stable_key(), "s|gone.example.com:1".into()];
    let bytes = profile::encode(&store).unwrap();
    let back = profile::decode(&bytes).unwrap();
    assert_eq!(
        back.excluded,
        [a.stable_key()],
        "the key that matches nothing is not carried"
    );
}

// ---- the address of a subscription ---------------------------------------------------------------

const CANARY: &str = "https://provider.example/sub/CANARY-9f3a-token";

fn with_address(dir: &Path) -> Arc<Application> {
    let mut store = store(vec![]);
    store.subscriptions.push(Subscription {
        url: CANARY.into(),
        ..subscription("s")
    });
    store
        .servers
        .push(server("A", "a.example.com", 443, Some("s")));
    start(store, dir)
}

#[tokio::test]
async fn the_address_of_a_subscription_is_given_when_asked_for_by_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let app = with_address(dir.path());
    assert_eq!(app.subscription_url("s").await.unwrap(), CANARY);
    assert!(
        app.subscription_url("other").await.is_err(),
        "no such subscription"
    );
}

/// A download that fails the way the real one does: a typed failure whose text names no address.
/// It remembers what it was asked for, so that the test knows the refresh really went to the
/// address.
#[derive(Default)]
struct Failing(std::sync::Mutex<Vec<String>>);
#[async_trait]
impl SubscriptionSource for Failing {
    async fn fetch(&self, url: &str, _: Option<u16>) -> Result<String> {
        self.0.lock().unwrap().push(url.to_owned());
        Err(Error::failure(
            FailureCode::SubscriptionTransport,
            "Сервер подписки не ответил",
        )
        .at_stage(Stage::SendRequest))
    }
}

#[tokio::test]
async fn the_address_is_in_no_snapshot_journal_or_diagnostics_export() {
    let dir = tempfile::tempdir().unwrap();
    let source = Arc::new(Failing::default());
    let mut state = store(vec![]);
    state.subscriptions.push(Subscription {
        url: CANARY.into(),
        ..subscription("s")
    });
    state
        .servers
        .push(server("A", "a.example.com", 443, Some("s")));
    let app = Application::with_services(
        state,
        dir.path().into(),
        Box::new(Idle),
        Arc::new(Fine),
        "x".repeat(32),
        source.clone(),
    );
    // Everything the interface and the user's files carry about the subscription: the address is
    // saved again, and the refresh that follows asks it and fails.
    app.execute(Action::SaveSubscription {
        id: Some("s".into()),
        url: CANARY.into(),
        interval_mins: None,
        user_agent: None,
        filters: Default::default(),
        provider_routes: true,
    })
    .await
    .unwrap();
    // A failed download is recorded for its subscription, not returned as an error of the action.
    app.execute(Action::Refresh {
        id: Some("s".into()),
    })
    .await
    .unwrap();
    assert_eq!(
        app.snapshot().subscriptions[0].failure_code,
        Some(FailureCode::SubscriptionTransport)
    );
    assert!(
        source.0.lock().unwrap().iter().any(|url| url == CANARY),
        "the refresh asked the address: the failure path was walked"
    );
    app.execute(Action::ExportDiagnostics).await.unwrap();
    let snapshot = serde_json::to_string(&app.snapshot()).unwrap();
    assert!(!snapshot.contains("CANARY"), "{snapshot}");
    let diagnostics = std::fs::read_to_string(dir.path().join("diagnostics.json")).unwrap();
    assert!(!diagnostics.contains("CANARY"));
    assert!(
        diagnostics.contains("subscription_transport"),
        "the failure is in the diagnostics too"
    );
    let journal = std::fs::read_to_string(dir.path().join("journal.json")).unwrap();
    assert!(!journal.contains("CANARY"));
    assert!(journal.contains("Не удалось обновить подписку"));
    // The view of a subscription has no field for it.
    let view = serde_json::to_value(&app.snapshot().subscriptions[0]).unwrap();
    assert!(view.get("url").is_none(), "{view}");
}
