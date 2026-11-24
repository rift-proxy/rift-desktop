//! The rules a provider sends with a server: how they are kept, what they add to the sing-box
//! config (each set in the Clash mode of its servers, after the user's own rules), how they are
//! refused when damaged, and that the application switches them with the server and says so in
//! the journal without ever writing the rules there.
use async_trait::async_trait;
use rift_core::{
    application::{Action, Application, Category},
    config::{generate_with, sanitized},
    geodata::{GeoSet, SiteList},
    model::{Server, Store},
    parser::parse_uri,
    persistence, profile,
    routing::{Routing, ServerRoutes, MAX_ENTRIES, MAX_ROUTE_SETS, NO_ROUTES_MODE},
    runtime::Runtime,
    selection::Probe,
    Result,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const SECRET: &str = "12345678901234567890123456789012";

fn routes(block: &[&str], direct: &[&str]) -> ServerRoutes {
    ServerRoutes {
        block: block.iter().map(|entry| entry.to_string()).collect(),
        direct: direct.iter().map(|entry| entry.to_string()).collect(),
    }
}
/// Rules like the ones the owner's provider sends.
fn provider() -> ServerRoutes {
    routes(
        &["udp:443"],
        &[
            "domain:vk.example",
            "geosite:category-ru",
            "protocol:bittorrent",
        ],
    )
}
fn other() -> ServerRoutes {
    routes(&[], &["domain:bank.example"])
}
fn server(port: u16, own: Option<ServerRoutes>) -> Server {
    let mut server = parse_uri(
        &format!("trojan://secret@198.51.100.7:{port}#S{port}"),
        None,
    )
    .unwrap();
    server.routes = own;
    server
}
fn store(servers: Vec<Server>) -> Store {
    Store {
        selected: servers.first().map(|server| server.id.clone()),
        servers,
        ..Store::default()
    }
}
fn config(store: &Store) -> Value {
    generate_with(store, SECRET, &GeoSet::none()).unwrap()
}
fn modes(rules: &[Value]) -> Vec<String> {
    rules
        .iter()
        .filter_map(|rule| rule["clash_mode"].as_str().map(str::to_owned))
        .collect()
}

// ---- what a server carries ------------------------------------------------------------------

#[test]
fn rules_are_not_part_of_the_identity_of_a_server() {
    let plain = server(443, None);
    let with = server(443, Some(provider()));
    assert_eq!(plain.id, with.id);
    assert_eq!(Server::fingerprint(&plain.outbound), with.id);
}

#[test]
fn the_mode_is_the_same_for_the_same_rules_and_names_nothing() {
    assert_eq!(provider().mode(), provider().mode());
    assert_ne!(provider().mode(), other().mode());
    // The order of the lists matters as little as it can: two lists, never merged.
    assert_ne!(
        routes(&["udp:443"], &[]).mode(),
        routes(&[], &["udp:443"]).mode()
    );
    // The entries are not run together into another set.
    assert_ne!(
        routes(&[], &["domain:a.example", "domain:b.example"]).mode(),
        routes(&[], &["domain:a.examp", "domain:le.b.example"]).mode()
    );
    let mode = provider().mode();
    assert!(mode.starts_with("routes-") && mode.len() == "routes-".len() + 12);
    assert!(!mode.contains("vk") && !mode.contains("udp"));
    // A server with nothing, or with empty lists, is in the mode with no rules.
    assert_eq!(server(1, None).routes_mode(), NO_ROUTES_MODE);
    assert_eq!(
        server(1, Some(ServerRoutes::default())).routes_mode(),
        NO_ROUTES_MODE
    );
    assert_eq!(server(1, Some(provider())).routes_mode(), mode);
}

#[test]
fn the_rules_are_kept_with_the_state_and_in_a_profile() {
    let original = store(vec![server(1, Some(provider())), server(2, None)]);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("state.json");
    persistence::save(&file, &original).unwrap();
    let loaded = persistence::load(&file).unwrap();
    assert_eq!(loaded.servers[0].routes, Some(provider()));
    assert_eq!(loaded.servers[1].routes, None);
    // A server without rules writes no field at all, so the files of earlier versions stay as they were.
    let written: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert!(written["servers"][0]["routes"].is_object());
    assert!(written["servers"][1].get("routes").is_none());
    // And a profile carries them.
    let decoded = profile::decode(&profile::encode(&original).unwrap()).unwrap();
    assert_eq!(decoded.servers[0].routes, Some(provider()));
    assert_eq!(decoded.servers[1].routes, None);
}

#[test]
fn a_state_file_from_before_the_rules_existed_loads_without_them() {
    let mut state = serde_json::to_value(store(vec![server(1, None)])).unwrap();
    state["servers"][0]
        .as_object_mut()
        .unwrap()
        .remove("routes");
    let loaded = persistence::decode(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(loaded.servers[0].routes, None);
}

// ---- what they make of the config -----------------------------------------------------------

#[test]
fn a_store_without_server_rules_has_no_mode_anywhere_but_the_default() {
    let config = config(&store(vec![server(1, None), server(2, None)]));
    let rules = config["route"]["rules"].as_array().unwrap();
    assert!(modes(rules).is_empty());
    assert!(config["dns"]["rules"]
        .as_array()
        .into_iter()
        .flatten()
        .all(|rule| rule.get("clash_mode").is_none()));
    assert_eq!(
        config["experimental"]["clash_api"]["default_mode"],
        NO_ROUTES_MODE
    );
}

#[test]
fn each_set_of_rules_acts_in_its_own_mode_after_the_rules_of_the_user() {
    let store = store(vec![
        server(1, Some(provider())),
        server(2, None),
        server(3, Some(other())),
    ]);
    let config = config(&store);
    let rules = config["route"]["rules"].as_array().unwrap();
    let (first, second) = (provider().mode(), other().mode());
    // What the user's profile makes comes first, with no mode, then the sets.
    let first_mode = rules
        .iter()
        .position(|r| r.get("clash_mode").is_some())
        .unwrap();
    assert!(rules[..first_mode]
        .iter()
        .all(|r| r.get("clash_mode").is_none()));
    assert!(
        first_mode >= 3,
        "sniff, DNS hijack and the default Direct list come first"
    );
    assert!(rules[first_mode..]
        .iter()
        .all(|r| r.get("clash_mode").is_some()));
    let mut seen = modes(rules);
    seen.sort();
    seen.dedup();
    let mut expected = vec![first.clone(), second.clone()];
    expected.sort();
    assert_eq!(seen, expected);
    // The three kinds of entry of the first set.
    let ours = |rule: &&Value| rule["clash_mode"] == first.as_str();
    let mine: Vec<&Value> = rules.iter().filter(ours).collect();
    assert!(mine.contains(&&json!({
        "network": "udp", "port": [443], "action": "reject", "clash_mode": first
    })));
    assert!(mine.contains(&&json!({
        "domain_suffix": ["vk.example"], "action": "route", "outbound": "direct", "clash_mode": first
    })));
    assert!(mine.contains(&&json!({
        "protocol": ["bittorrent"], "action": "route", "outbound": "direct", "clash_mode": first
    })));
    // The names it sends direct are answered by the local resolver, in that mode only.
    let dns = config["dns"]["rules"].as_array().unwrap();
    assert!(dns.contains(&json!({
        "domain_suffix": ["vk.example"], "server": "local", "clash_mode": first
    })));
    assert!(dns.contains(&json!({
        "domain_suffix": ["bank.example"], "server": "local", "clash_mode": second
    })));
    // The rest is the user's: the final route and the starting mode do not depend on them.
    assert_eq!(config["route"]["final"], "main");
    assert_eq!(
        config["experimental"]["clash_api"]["default_mode"],
        NO_ROUTES_MODE
    );
}

#[test]
fn servers_with_the_same_rules_share_one_set() {
    let many = (1..=5).map(|port| server(port, Some(provider()))).collect();
    let store = store(many);
    assert_eq!(store.route_sets().len(), 1);
    let config = config(&store);
    let rules = config["route"]["rules"].as_array().unwrap();
    // One rule per entry kind of the one set, not five.
    assert_eq!(
        modes(rules)
            .iter()
            .filter(|m| **m == provider().mode())
            .count(),
        3
    );
}

#[test]
fn the_mode_a_server_starts_in_is_the_mode_of_the_selected_server() {
    let mut store = store(vec![server(1, Some(provider())), server(2, Some(other()))]);
    assert_eq!(store.live_routes_mode(), provider().mode());
    store.selected = Some(store.servers[1].id.clone());
    assert_eq!(store.live_routes_mode(), other().mode());
    // A selection that cannot be used falls back to the first server, as the selector does.
    store.selected = Some("gone".into());
    assert_eq!(store.live_routes_mode(), provider().mode());
    store.servers.clear();
    assert_eq!(store.live_routes_mode(), NO_ROUTES_MODE);
}

#[test]
fn a_present_subscription_brings_its_rules() {
    use rift_core::model::Subscription;
    let mut brought = server(1, Some(provider()));
    brought.source = Some("sub".into());
    let mut store = store(vec![brought, server(2, None)]);
    store.subscriptions.push(Subscription {
        id: "sub".into(),
        name: "S".into(),
        url: "https://example.com/s".into(),
        ..Subscription::default()
    });
    assert_eq!(store.route_sets().len(), 1);
    assert!(!modes(config(&store)["route"]["rules"].as_array().unwrap()).is_empty());
}

#[test]
fn the_rules_never_reach_the_diagnostics_as_text() {
    let store = store(vec![server(1, Some(provider()))]);
    let clean = sanitized(&config(&store));
    let text = clean.to_string();
    for word in ["vk.example", "bittorrent", "category-ru"] {
        assert!(!text.contains(word), "{word}: {text}");
    }
}

#[test]
fn the_lists_the_rules_refer_to_are_wanted_with_the_users_own() {
    let store = store(vec![server(1, Some(provider()))]);
    let (sites, ips) = store.geo_names();
    // The default profile of the user already names category-ru: once.
    assert_eq!(sites, ["category-ru"]);
    assert!(ips.is_empty());
    let mut bare = store;
    bare.routing = Routing {
        direct: vec![],
        ..Routing::default()
    };
    assert_eq!(
        bare.geo_names().0,
        ["category-ru"],
        "only the server asks for it"
    );
    // With the database the list reaches the config in the mode of the server.
    let mut geo = GeoSet::none();
    geo.sites.insert(
        "category-ru".into(),
        SiteList {
            suffix: vec!["yandex.ru".into()],
            ..SiteList::default()
        },
    );
    let config = generate_with(&bare, SECRET, &geo).unwrap();
    let sets = config["route"]["rule_set"].as_array().unwrap();
    assert_eq!(sets.len(), 1);
    let rules = config["route"]["rules"].as_array().unwrap();
    assert!(rules
        .iter()
        .any(|rule| rule["rule_set"] == json!(["geosite-category-ru"])
            && rule["clash_mode"] == provider().mode().as_str()));
}

// ---- what is refused ------------------------------------------------------------------------

#[test]
fn damaged_rules_are_refused_wherever_they_come_from() {
    let bad: [ServerRoutes; 4] = [
        routes(&[], &["not a rule"]),
        routes(&["port:0"], &[]),
        // A provider does not know the programs of this machine.
        routes(&[], &["process:chrome.exe"]),
        routes(&[], &[&"domain:".repeat(300)]),
    ];
    for rules in bad {
        assert!(rules.validate().is_err(), "{rules:?}");
        let damaged = store(vec![server(1, Some(rules))]);
        assert!(persistence::validate(&damaged).is_err());
        assert!(profile::validate(&damaged).is_err());
    }
    assert!(provider().validate().is_ok());
}

#[test]
fn a_list_is_limited_like_the_lists_of_the_user() {
    let entries = |count: usize| -> Vec<String> {
        (0..count).map(|n| format!("domain:d{n}.example")).collect()
    };
    let at_limit = ServerRoutes {
        direct: entries(MAX_ENTRIES),
        block: vec![],
    };
    assert!(at_limit.validate().is_ok());
    let over = ServerRoutes {
        direct: entries(MAX_ENTRIES + 1),
        block: vec![],
    };
    assert!(over.validate().is_err());
    let over_block = ServerRoutes {
        direct: vec![],
        block: entries(MAX_ENTRIES + 1),
    };
    assert!(over_block.validate().is_err());
}

#[test]
fn only_so_many_different_sets_are_allowed_in_a_store() {
    let sets = |count: usize| -> Store {
        store(
            (0..count)
                .map(|n| {
                    server(
                        1000 + n as u16,
                        Some(routes(&[], &[&format!("domain:set{n}.example")])),
                    )
                })
                .collect(),
        )
    };
    assert!(persistence::validate(&sets(MAX_ROUTE_SETS)).is_ok());
    assert!(persistence::validate(&sets(MAX_ROUTE_SETS + 1)).is_err());
    // The same set on any number of servers is one.
    let same = store(
        (0..MAX_ROUTE_SETS + 5)
            .map(|n| server(2000 + n as u16, Some(other())))
            .collect(),
    );
    assert!(persistence::validate(&same).is_ok());
}

// ---- the application ------------------------------------------------------------------------

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
/// Says what it was asked to do, and can be made to fail to set the rules.
#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<String>>,
    refuse_rules: Mutex<bool>,
}
impl Recorder {
    fn calls(&self) -> Vec<String> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}
#[async_trait]
impl Probe for Recorder {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        Ok(1)
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        Ok(1)
    }
    async fn select(&self, id: &str) -> Result<()> {
        self.calls.lock().unwrap().push(format!("select {id}"));
        Ok(())
    }
    async fn routes(&self, mode: &str) -> Result<()> {
        self.calls.lock().unwrap().push(format!("routes {mode}"));
        if *self.refuse_rules.lock().unwrap() {
            return Err(rift_core::Error::failure(
                rift_core::failure::FailureCode::SelectorSwitch,
                "no",
            ));
        }
        Ok(())
    }
}
struct Harness {
    app: Arc<Application>,
    probe: Arc<Recorder>,
    a: Server,
    b: Server,
    c: Server,
    _dir: tempfile::TempDir,
}
fn harness() -> Harness {
    let (a, b, c) = (
        server(1, Some(provider())),
        server(2, None),
        server(3, Some(other())),
    );
    let mut store = store(vec![a.clone(), b.clone(), c.clone()]);
    store.settings.auto_select = false;
    let dir = tempfile::tempdir().unwrap();
    let probe = Arc::new(Recorder::default());
    let app = Application::with_probe(
        store,
        dir.path().into(),
        Box::new(Idle),
        probe.clone(),
        SECRET.into(),
    );
    Harness {
        app,
        probe,
        a,
        b,
        c,
        _dir: dir,
    }
}
fn notes(app: &Application) -> Vec<String> {
    app.snapshot()
        .journal
        .iter()
        .filter(|entry| entry.message.starts_with("Правила сервера"))
        .map(|entry| entry.message.clone())
        .collect()
}

#[tokio::test]
async fn choosing_a_server_sets_the_rules_it_brings_and_the_journal_says_how_many() {
    let h = harness();
    h.app.execute(Action::Connect).await.unwrap();
    // The server it started on brings rules: the first note, counts only.
    assert_eq!(
        notes(&h.app),
        ["Правила сервера включены: Direct 3, Block 1"]
    );
    h.probe.calls();
    h.app
        .execute(Action::Select { id: h.b.id.clone() })
        .await
        .unwrap();
    assert_eq!(
        h.probe.calls(),
        [
            format!("select {}", h.b.id),
            format!("routes {NO_ROUTES_MODE}")
        ]
    );
    h.app
        .execute(Action::Select { id: h.c.id.clone() })
        .await
        .unwrap();
    assert_eq!(
        h.probe.calls(),
        [
            format!("select {}", h.c.id),
            format!("routes {}", other().mode())
        ]
    );
    h.app
        .execute(Action::Select { id: h.a.id.clone() })
        .await
        .unwrap();
    assert_eq!(
        notes(&h.app),
        [
            "Правила сервера включены: Direct 3, Block 1",
            "Правила сервера сняты",
            "Правила сервера включены: Direct 1, Block 0",
            "Правила сервера включены: Direct 3, Block 1",
        ]
    );
    // No rule of the provider is in any entry of the journal.
    let journal = h.app.snapshot().journal;
    assert!(journal
        .iter()
        .all(|entry| !entry.message.contains("vk.example") && !entry.message.contains("udp")));
    assert!(journal
        .iter()
        .filter(|entry| entry.message.starts_with("Правила сервера"))
        .all(|entry| entry.category == Category::Core));
}

#[tokio::test]
async fn choosing_the_same_rules_again_is_not_news() {
    let h = harness();
    h.app.execute(Action::Connect).await.unwrap();
    h.app
        .execute(Action::Select { id: h.b.id.clone() })
        .await
        .unwrap();
    h.app
        .execute(Action::Select { id: h.b.id.clone() })
        .await
        .unwrap();
    assert_eq!(
        notes(&h.app),
        [
            "Правила сервера включены: Direct 3, Block 1",
            "Правила сервера сняты"
        ]
    );
}

#[tokio::test]
async fn rules_that_could_not_be_set_fail_the_choice_and_leave_the_old_one() {
    let h = harness();
    h.app.execute(Action::Connect).await.unwrap();
    h.probe.calls();
    *h.probe.refuse_rules.lock().unwrap() = true;
    let error = h
        .app
        .execute(Action::Select { id: h.c.id.clone() })
        .await
        .unwrap_err();
    assert_eq!(
        error.failure_code(),
        Some(rift_core::failure::FailureCode::SelectorSwitch)
    );
    // The choice was not made, and the selector was sent back to the server it was on.
    assert_eq!(h.app.snapshot().selected, Some(h.a.id.clone()));
    let calls = h.probe.calls();
    assert_eq!(calls[0], format!("select {}", h.c.id));
    assert!(calls.contains(&format!("select {}", h.a.id)), "{calls:?}");
    assert_eq!(
        notes(&h.app).len(),
        1,
        "no note for rules that did not take"
    );
}

#[tokio::test]
async fn disconnecting_forgets_the_rules_so_the_next_connection_says_so_again() {
    let h = harness();
    h.app.execute(Action::Connect).await.unwrap();
    h.app.execute(Action::Disconnect).await.unwrap();
    h.app.execute(Action::Connect).await.unwrap();
    assert_eq!(
        notes(&h.app),
        [
            "Правила сервера включены: Direct 3, Block 1",
            "Правила сервера включены: Direct 3, Block 1"
        ]
    );
}

// ---- the real sing-box ----------------------------------------------------------------------

/// The sing-box of the development checkout, when there is one for this machine.
fn sing_box() -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../desktop/src-tauri/binaries/sing-box-x86_64-unknown-linux-gnu");
    (cfg!(target_os = "linux") && path.exists()).then_some(path)
}
fn accepted(binary: &std::path::Path, config: &Value) -> std::result::Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.json");
    std::fs::write(&file, serde_json::to_vec(config).unwrap()).unwrap();
    let output = std::process::Command::new(binary)
        .args(["check", "-c"])
        .arg(&file)
        .output()
        .unwrap();
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

#[test]
fn the_real_sing_box_accepts_the_config_with_the_rules_of_servers() {
    use rift_core::config::{generate_resolving, Resolver};
    use rift_core::model::Mode;
    let Some(binary) = sing_box() else {
        eprintln!("skipped: no sing-box binary for this machine in desktop/src-tauri/binaries");
        return;
    };
    let everything = routes(
        &[
            "udp:443",
            "tcp:1000-2000",
            "port:25",
            "keyword:ads",
            "10.0.0.0/8",
        ],
        &[
            "domain:vk.example",
            "full:www.bank.example",
            r"regexp:^ad[0-9]+\.",
            "geosite:category-ru",
            "geoip:ru",
            "192.0.2.0/24",
            "protocol:bittorrent",
            "protocol:quic",
        ],
    );
    let mut geo = GeoSet::none();
    geo.sites.insert(
        "category-ru".into(),
        SiteList {
            suffix: vec!["yandex.ru".into()],
            ..SiteList::default()
        },
    );
    geo.ips.insert("ru".into(), vec!["5.8.0.0/13".into()]);
    for mode in [Mode::Proxy, Mode::Tun, Mode::TunProxy] {
        let mut store = store(vec![
            server(1, Some(everything.clone())),
            server(2, Some(provider())),
            server(3, Some(other())),
            server(4, None),
        ]);
        store.settings.mode = mode;
        for geo in [GeoSet::none(), geo.clone()] {
            for resolver in [Resolver::System, Resolver::Bridge(53535)] {
                let config = generate_resolving(&store, SECRET, &geo, resolver).unwrap();
                accepted(&binary, &config).unwrap_or_else(|error| {
                    panic!("{:?} {resolver:?}: {error}", store.settings.mode)
                });
            }
        }
    }
    // A control: the check does look at the mode of a rule, so the acceptance above means something.
    // (That sing-box takes a mode it is asked for is not a matter of `check`: the live test in
    // `sidecar.rs` asks the running process.)
    let mut config = config(&store(vec![server(1, Some(provider()))]));
    config["route"]["rules"][3]["clash_mode"] = json!(7);
    assert!(accepted(&binary, &config).is_err());
}

#[test]
fn a_server_in_two_subscriptions_brings_the_rules_the_config_holds_to_the_cycle() {
    use rift_core::model::{Subscription, Usage};
    let sub = |id: &str, usage: Option<Usage>| Subscription {
        id: id.into(),
        name: id.into(),
        url: format!("https://{id}.example/s"),
        usage,
        ..Subscription::default()
    };
    // The same server in A (traffic used up, no rules) and B (with rules): the config holds A's
    // copy, the cycle can only use B's.
    let mut a = server(1, None);
    a.source = Some("A".into());
    let mut b = server(1, Some(provider()));
    b.source = Some("B".into());
    let store = Store {
        subscriptions: vec![
            sub(
                "A",
                Some(Usage {
                    used: 10,
                    total: 10,
                    expire: None,
                }),
            ),
            sub("B", None),
        ],
        servers: vec![a.clone(), b],
        ..Store::default()
    };
    let candidates = store.auto_candidates(0);
    assert_eq!(candidates.len(), 1);
    let held = store.routes_mode_of(&a.id).unwrap();
    assert_eq!(candidates[0].routes_mode(), held);
    assert_eq!(held, NO_ROUTES_MODE, "A's copy brings no rules");
    // That mode is one the generated config knows.
    let text = config(&store).to_string();
    assert!(text.contains(&format!("\"default_mode\":\"{NO_ROUTES_MODE}\"")));
    assert!(
        !text.contains(&provider().mode()),
        "B's set is not in the config"
    );
}

#[test]
fn a_set_of_lists_the_databases_do_not_hold_still_makes_its_mode_known() {
    // Nothing of this set can be a rule while there is no database, but the mode of its servers must
    // be one the config knows, or choosing such a server would be refused.
    let geo_only = routes(&["geosite:category-ads-all"], &["geoip:private"]);
    let mode = geo_only.mode();
    let store = store(vec![server(1, Some(geo_only))]);
    let config = config(&store);
    let rules = config["route"]["rules"].as_array().unwrap();
    let carrying: Vec<&Value> = rules
        .iter()
        .filter(|rule| rule["clash_mode"] == mode.as_str())
        .collect();
    assert_eq!(carrying.len(), 1, "{rules:?}");
    assert_eq!(carrying[0]["action"], "route");
    // A rule that matches nothing: it asks for a protocol no sniffer reports.
    assert!(carrying[0]["protocol"].is_array());
    // A set with a rule of its own needs no such rule.
    let mixed = routes(&[], &["geoip:private", "domain:a.example"]);
    let store = Store {
        servers: vec![server(1, Some(mixed.clone()))],
        ..Store::default()
    };
    let rules = config_rules(&store);
    assert_eq!(
        rules
            .iter()
            .filter(|rule| rule["clash_mode"] == mixed.mode().as_str())
            .count(),
        1
    );
    if let Some(binary) = sing_box() {
        accepted(
            &binary,
            &self::config(&Store {
                servers: vec![server(
                    1,
                    Some(routes(&["geosite:category-ads-all"], &["geoip:private"])),
                )],
                ..Store::default()
            }),
        )
        .unwrap();
    }
}
fn config_rules(store: &Store) -> Vec<Value> {
    config(store)["route"]["rules"].as_array().unwrap().clone()
}

#[test]
fn the_rules_of_all_servers_together_are_bounded_too() {
    use rift_core::{
        routing::MAX_ROUTE_ENTRIES,
        subscription::{merge, ParsedSubscription},
    };
    // One set of 4000 entries, brought by every server of a subscription: the sets are one, the
    // entries add up.
    let big = ServerRoutes {
        block: (0..MAX_ENTRIES)
            .map(|n| format!("domain:b{n}.example"))
            .collect(),
        direct: (0..MAX_ENTRIES)
            .map(|n| format!("domain:d{n}.example"))
            .collect(),
    };
    let fit = MAX_ROUTE_ENTRIES / big.len();
    let parsed = ParsedSubscription {
        rows: Default::default(),
        servers: (0..fit + 5)
            .map(|n| server(1000 + n as u16, Some(big.clone())))
            .collect(),
        rejected: 0,
        filtered: 0,
    };
    let merged = merge(&Store::default(), "sub", parsed);
    assert_eq!(merged.servers.len(), fit + 5);
    assert_eq!(
        merged.servers.iter().filter(|s| s.routes.is_some()).count(),
        fit
    );
    assert!(persistence::validate(&merged).is_ok());
    // A store that holds more than that is not a valid one.
    let over = Store {
        servers: (0..fit + 1)
            .map(|n| server(2000 + n as u16, Some(big.clone())))
            .collect(),
        ..Store::default()
    };
    assert!(persistence::validate(&over).is_err());
}

#[test]
fn what_is_said_of_a_refused_entry_does_not_repeat_it() {
    // The text of a provider's entry would reach the journal with the message.
    for (entry, says) in [
        ("process:top-secret-name.exe", "программа не допускается"),
        ("regexp:([", "запись не читается"),
        ("domain:bank-name-here.example extra", "запись не читается"),
    ] {
        let store = Store {
            servers: vec![server(1, Some(routes(&[], &[entry])))],
            ..Store::default()
        };
        for error in [
            persistence::validate(&store).unwrap_err().to_string(),
            profile::validate(&store).unwrap_err().to_string(),
        ] {
            assert!(error.contains(says), "{error}");
            assert!(!error.contains(entry), "{error}");
            assert!(
                !error.contains("top-secret") && !error.contains("bank-name"),
                "{error}"
            );
        }
    }
}

#[tokio::test]
async fn a_restart_that_changes_nothing_in_the_rules_is_not_news() {
    let h = harness();
    h.app.execute(Action::Connect).await.unwrap();
    assert_eq!(notes(&h.app).len(), 1);
    // A change of the config restarts sing-box; the server in use is the same, so are its rules.
    let mut settings = h.app.snapshot().settings;
    settings.find_process = !settings.find_process;
    h.app
        .execute(Action::SaveSettings { settings })
        .await
        .unwrap();
    assert_eq!(notes(&h.app).len(), 1, "{:?}", notes(&h.app));
}

#[tokio::test]
async fn a_restart_that_leaves_no_rules_in_force_says_so() {
    let h = harness();
    h.app.execute(Action::Connect).await.unwrap();
    // The server in use goes; the next one brings no rules of its own.
    h.app
        .execute(Action::RemoveServer { id: h.a.id.clone() })
        .await
        .unwrap();
    let notes = notes(&h.app);
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert_eq!(notes[1], "Правила сервера сняты");
}
