//! A subscription that answers in Xray JSON, through the application: the refresh turns it into
//! servers with the rules of their profiles, the journal counts what was refused, the switch of
//! the subscription decides whether the rules are kept, and a store never holds more different sets
//! of rules than it may.
use async_trait::async_trait;
use rift_core::{
    application::{Action, Application},
    config::generate_with,
    geodata::GeoSet,
    model::{Server, Store, Subscription},
    persistence,
    routing::MAX_ROUTE_SETS,
    runtime::Runtime,
    selection::Probe,
    subscription::SubscriptionSource,
    Result,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

const SECRET: &str = "12345678901234567890123456789012";
const ID: &str = "11111111-2222-3333-4444-555555555555";
const KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

struct Idle;
#[async_trait]
impl Runtime for Idle {
    async fn validate(&mut self, store: &Store, secret: &str) -> Result<()> {
        generate_with(store, secret, &GeoSet::none()).map(|_| ())
    }
    async fn start(&mut self, _: &Store, _: &str) -> Result<()> {
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(true)
    }
}
#[derive(Default)]
struct Probes(Mutex<Vec<String>>);
#[async_trait]
impl Probe for Probes {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        Ok(1)
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        Ok(1)
    }
    async fn select(&self, id: &str) -> Result<()> {
        self.0.lock().unwrap().push(format!("select {id}"));
        Ok(())
    }
    async fn routes(&self, mode: &str) -> Result<()> {
        self.0.lock().unwrap().push(format!("routes {mode}"));
        Ok(())
    }
}
/// Answers each address with the body it holds now.
struct Panel(Mutex<std::collections::HashMap<String, String>>);
impl Panel {
    fn serve(&self, number: usize, body: &Value) {
        self.serve_text(number, body.to_string());
    }
    fn serve_text(&self, number: usize, body: String) {
        self.0
            .lock()
            .unwrap()
            .insert(format!("https://panel{number}.example/s"), body);
    }
}
#[async_trait]
impl SubscriptionSource for Panel {
    async fn fetch(&self, url: &str, _: Option<u16>) -> Result<String> {
        self.0
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .ok_or(rift_core::Error::Network)
    }
}

fn reality(host: &str) -> Value {
    json!({
        "tag": "proxy", "protocol": "vless",
        "settings": {"vnext": [{"address": host, "port": 8443, "users": [
            {"id": ID, "encryption": "none", "flow": "xtls-rprx-vision"}]}]},
        "streamSettings": {"network": "tcp", "security": "reality",
            "realitySettings": {"serverName": "front.example", "publicKey": KEY,
                "shortId": "abcd1234", "fingerprint": "firefox"}},
    })
}
fn unsupported() -> Vec<Value> {
    vec![
        json!({"tag": "proxy", "protocol": "hysteria", "settings": {"address": "hy.example", "port": 1, "version": 2}}),
        json!({"tag": "proxy", "protocol": "vless",
            "settings": {"vnext": [{"address": "x.example", "port": 443, "users": [{"id": ID}]}]},
            "streamSettings": {"network": "xhttp", "security": "none"}}),
    ]
}
fn profile(remarks: &str, mut servers: Vec<Value>, direct: &[&str]) -> Value {
    servers.push(json!({"tag": "direct", "protocol": "freedom"}));
    servers.push(json!({"tag": "block", "protocol": "blackhole"}));
    json!({
        "remarks": remarks,
        "outbounds": servers,
        "routing": {"rules": [
            {"type": "field", "network": "udp", "port": "443", "outboundTag": "block"},
            {"type": "field", "outboundTag": "direct", "domain": direct},
        ]},
    })
}

struct Harness {
    app: Arc<Application>,
    panel: Arc<Panel>,
    probe: Arc<Probes>,
    dir: tempfile::TempDir,
}
fn harness(body: &Value, subscriptions: usize) -> Harness {
    let store = Store {
        subscriptions: (0..subscriptions)
            .map(|n| Subscription {
                id: format!("sub{n}"),
                name: format!("Panel {n}"),
                url: format!("https://panel{n}.example/s"),
                ..Subscription::default()
            })
            .collect(),
        ..Store::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let panel = Arc::new(Panel(Mutex::new(Default::default())));
    for number in 0..subscriptions {
        panel.serve(number, body);
    }
    let probe = Arc::new(Probes::default());
    let app = Application::with_services(
        store,
        dir.path().into(),
        Box::new(Idle),
        probe.clone(),
        SECRET.into(),
        panel.clone(),
    );
    Harness {
        app,
        panel,
        probe,
        dir,
    }
}
async fn refresh(h: &Harness) {
    h.app.execute(Action::Refresh { id: None }).await.unwrap();
}
fn stored(h: &Harness) -> Store {
    persistence::load(&h.dir.path().join("state.json")).unwrap()
}
fn journal(h: &Harness, containing: &str) -> Vec<String> {
    h.app
        .snapshot()
        .journal
        .into_iter()
        .map(|entry| entry.message)
        .filter(|message| message.contains(containing))
        .collect()
}

#[tokio::test]
async fn a_refresh_makes_servers_of_the_profiles_with_their_rules_and_counts_what_it_refused() {
    let mut refused = unsupported();
    refused.insert(0, reality("wl.example"));
    let body = json!([
        profile(
            "Finland",
            vec![reality("fi.example")],
            &["domain:vk.example"]
        ),
        profile("Whitelist", refused, &["domain:vk.example"]),
    ]);
    let h = harness(&body, 1);
    refresh(&h).await;
    let snapshot = h.app.snapshot();
    let mut names: Vec<_> = snapshot.servers.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["Finland", "Whitelist", "Whitelist 2"]);
    assert_eq!(snapshot.subscriptions[0].count, 3);
    // XHTTP is now imported; one Hysteria outbound remains refused (servers, not lines).
    assert_eq!(
        journal(&h, "Подписка обновлена"),
        ["Подписка обновлена; отклонено серверов: 1"]
    );
    // The rules are kept with the servers, on disk too.
    let store = stored(&h);
    assert!(store.servers.iter().all(|server| {
        server.routes.as_ref().is_some_and(|routes| {
            routes.block == ["udp:443"] && routes.direct == ["domain:vk.example"]
        })
    }));
    assert_eq!(store.route_sets().len(), 1);
    // And they are in force once a server is chosen.
    let id = snapshot.servers[0].id.clone();
    h.app.execute(Action::Connect).await.unwrap();
    h.app.execute(Action::Select { id }).await.unwrap();
    let mode = store.servers[0].routes_mode();
    assert!(h
        .probe
        .0
        .lock()
        .unwrap()
        .contains(&format!("routes {mode}")));
}

#[tokio::test]
async fn without_the_switch_the_servers_come_without_the_rules() {
    let body = json!([profile(
        "Finland",
        vec![reality("fi.example")],
        &["domain:vk.example"]
    )]);
    let h = harness(&body, 1);
    refresh(&h).await;
    assert!(stored(&h).servers[0].routes.is_some());
    h.app
        .execute(Action::SaveSubscription {
            id: Some("sub0".into()),
            url: "https://panel0.example/s".into(),
            interval_mins: None,
            user_agent: None,
            filters: Default::default(),
            provider_routes: false,
        })
        .await
        .unwrap();
    assert!(!h.app.snapshot().subscriptions[0].provider_routes);
    refresh(&h).await;
    let store = stored(&h);
    assert_eq!(store.servers.len(), 1);
    assert!(store.servers[0].routes.is_none());
    assert!(!store.subscriptions[0].provider_routes);
    // And back.
    h.app
        .execute(Action::SaveSubscription {
            id: Some("sub0".into()),
            url: "https://panel0.example/s".into(),
            interval_mins: None,
            user_agent: None,
            filters: Default::default(),
            provider_routes: true,
        })
        .await
        .unwrap();
    refresh(&h).await;
    assert!(stored(&h).servers[0].routes.is_some());
}

#[tokio::test]
async fn a_new_subscription_keeps_the_rules_unless_it_says_otherwise() {
    let h = harness(&json!([]), 0);
    h.app
        .execute(Action::SaveSubscription {
            id: None,
            url: "https://panel.example/s".into(),
            interval_mins: None,
            user_agent: None,
            filters: Default::default(),
            provider_routes: true,
        })
        .await
        .unwrap();
    assert!(h.app.snapshot().subscriptions[0].provider_routes);
    // An action from a caller that does not know the field is read as "keep".
    let action: Action = serde_json::from_value(json!({
        "type": "save_subscription", "id": null, "url": "https://panel2.example/s"
    }))
    .unwrap();
    assert!(matches!(
        action,
        Action::SaveSubscription {
            provider_routes: true,
            ..
        }
    ));
}

#[tokio::test]
async fn a_list_of_links_is_counted_in_lines_as_before() {
    let h = harness(&json!([]), 1);
    h.panel
        .serve_text(0, "trojan://p@a.example:1#A\nnot a link\nanother\n".into());
    refresh(&h).await;
    assert_eq!(
        journal(&h, "Подписка обновлена"),
        ["Подписка обновлена; отклонено строк: 2"]
    );
    assert!(stored(&h).servers[0].routes.is_none());
}

#[tokio::test]
async fn a_store_never_holds_more_different_sets_than_it_may() {
    let sets = |count: usize, from: usize| -> Value {
        Value::Array(
            (from..from + count)
                .map(|n| {
                    profile(
                        &format!("S{n}"),
                        vec![reality(&format!("h{n}.example"))],
                        &[&format!("domain:set{n}.example")],
                    )
                })
                .collect(),
        )
    };
    // One panel sends a different list with each of its servers: only so many keep theirs.
    let h = harness(&sets(MAX_ROUTE_SETS + 8, 0), 1);
    refresh(&h).await;
    let store = stored(&h);
    assert_eq!(store.servers.len(), MAX_ROUTE_SETS + 8);
    assert_eq!(store.route_sets().len(), MAX_ROUTE_SETS);
    let with = store.servers.iter().filter(|s| s.routes.is_some()).count();
    assert_eq!(
        with, MAX_ROUTE_SETS,
        "the first ones keep theirs, the rest are servers all the same"
    );
    assert!(persistence::validate(&store).is_ok());

    // The sets count over every panel: a second one with a set of its own adds a server only.
    let h = harness(&json!([]), 2);
    h.panel.serve(0, &sets(MAX_ROUTE_SETS, 0));
    h.panel.serve(1, &sets(1, 100));
    refresh(&h).await;
    let store = stored(&h);
    assert_eq!(store.servers.len(), MAX_ROUTE_SETS + 1);
    assert_eq!(store.route_sets().len(), MAX_ROUTE_SETS);
    assert!(persistence::validate(&store).is_ok());
    let second: Vec<_> = store
        .servers
        .iter()
        .filter(|s| s.source.as_deref() == Some("sub1"))
        .collect();
    assert_eq!(second.len(), 1);
    assert!(second[0].routes.is_none());
}

#[tokio::test]
async fn servers_that_do_not_fit_the_store_without_their_rules_are_counted_in_the_journal() {
    // A provider whose profile has 2000 + 2000 entries and many servers: the entries add up to more
    // than a store keeps, and the servers over that come without them.
    let domains = |letter: char| -> Vec<String> {
        (0..2000)
            .map(|n| format!("domain:{letter}{n}.example"))
            .collect()
    };
    let servers: Vec<Value> = (0..40).map(|n| reality(&format!("h{n}.example"))).collect();
    let mut outbounds = servers;
    outbounds.push(json!({"tag": "direct", "protocol": "freedom"}));
    outbounds.push(json!({"tag": "block", "protocol": "blackhole"}));
    let body = json!([{
        "remarks": "Big",
        "outbounds": outbounds,
        "routing": {"rules": [
            {"type": "field", "outboundTag": "block", "domain": domains('b')},
            {"type": "field", "outboundTag": "direct", "domain": domains('d')},
        ]},
    }]);
    let h = harness(&body, 1);
    refresh(&h).await;
    let store = stored(&h);
    assert_eq!(store.servers.len(), 40);
    let with = store.servers.iter().filter(|s| s.routes.is_some()).count();
    assert_eq!(with, 25, "100 000 entries of 4000 each");
    assert_eq!(
        journal(&h, "Правила провайдера не применены"),
        ["Правила провайдера не применены у серверов: 15 (предел наборов или записей)"]
    );
}

/// Answers once the test lets it: a download that is still going while the subscription is edited.
struct Slow {
    body: String,
    started: Arc<Notify>,
    gate: Arc<Notify>,
}
#[async_trait]
impl SubscriptionSource for Slow {
    async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
        self.started.notify_one();
        self.gate.notified().await;
        Ok(self.body.clone())
    }
}
fn save(provider_routes: bool) -> Action {
    Action::SaveSubscription {
        id: Some("sub0".into()),
        url: "https://panel0.example/s".into(),
        interval_mins: None,
        user_agent: None,
        filters: Default::default(),
        provider_routes,
    }
}

#[tokio::test]
async fn a_download_begun_before_the_switch_was_turned_off_does_not_bring_the_rules_back() {
    let body = json!([profile(
        "P",
        vec![reality("a.example")],
        &["domain:a.example"]
    )]);
    let (started, gate) = (Arc::new(Notify::new()), Arc::new(Notify::new()));
    let store = Store {
        subscriptions: vec![Subscription {
            id: "sub0".into(),
            name: "P".into(),
            url: "https://panel0.example/s".into(),
            ..Subscription::default()
        }],
        ..Store::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let app = Application::with_services(
        store,
        dir.path().into(),
        Box::new(Idle),
        Arc::new(Probes::default()),
        SECRET.into(),
        Arc::new(Slow {
            body: body.to_string(),
            started: started.clone(),
            gate: gate.clone(),
        }),
    );
    let refresh = {
        let app = app.clone();
        tokio::spawn(async move { app.execute(Action::Refresh { id: None }).await })
    };
    started.notified().await;
    // The owner turns the rules off while the answer is on its way.
    app.execute(save(false)).await.unwrap();
    gate.notify_one();
    refresh.await.unwrap().unwrap();
    let stored = persistence::load(&dir.path().join("state.json")).unwrap();
    assert!(!stored.subscriptions[0].provider_routes);
    assert!(
        stored.servers.iter().all(|server| server.routes.is_none()),
        "the answer read with the switch on must not be stored"
    );
}

#[tokio::test]
async fn turning_the_switch_off_takes_the_rules_off_the_servers_at_once() {
    let body = json!([profile(
        "Finland",
        vec![reality("fi.example")],
        &["domain:vk.example"]
    )]);
    let h = harness(&body, 1);
    refresh(&h).await;
    assert!(stored(&h).servers[0].routes.is_some());
    // The provider cannot be reached any more: there is no refresh to take them off.
    h.panel.0.lock().unwrap().clear();
    h.app.execute(save(false)).await.unwrap();
    assert!(stored(&h).servers[0].routes.is_none());
}
