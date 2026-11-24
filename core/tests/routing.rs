//! Routing profiles: the syntax of a rule, the six orders, what each reaches the sing-box
//! config as, and (when a sing-box binary is at hand) that the real `sing-box check` accepts it.
use proptest::prelude::*;
use rift_core::{
    config::{self, generate_resolving, generate_with, Resolver},
    geodata::{GeoSet, SiteList},
    model::{Mode, Store},
    parser::parse_uri,
    persistence, profile,
    routing::{
        parse_rule, split_entries, Net, Route, Routing, Rule, DEFAULT_DIRECT, MAX_ENTRIES,
        PROTOCOLS,
    },
};
use serde_json::{json, Value};

const SECRET: &str = "12345678901234567890123456789012";

fn store(routing: Routing) -> Store {
    Store {
        servers: vec![parse_uri("trojan://secret@198.51.100.7:443#A", None).unwrap()],
        routing,
        ..Store::default()
    }
}
fn config_of(routing: Routing, geo: &GeoSet) -> Value {
    generate_with(&store(routing), SECRET, geo).unwrap()
}
fn rules(config: &Value) -> Vec<Value> {
    // The first two rules are always the sniffer and the DNS hijack.
    let all = config["route"]["rules"].as_array().unwrap();
    assert_eq!(all[0], json!({"action": "sniff"}));
    assert_eq!(all[1], json!({"protocol": "dns", "action": "hijack-dns"}));
    all[2..].to_vec()
}
fn text(entries: &[&str]) -> Vec<String> {
    entries.iter().map(|entry| entry.to_string()).collect()
}
fn routing(order: [Route; 3], block: &[&str], direct: &[&str], proxy: &[&str]) -> Routing {
    Routing {
        order,
        block: text(block),
        direct: text(direct),
        proxy: text(proxy),
    }
}
fn geo() -> GeoSet {
    let mut geo = GeoSet::none();
    geo.sites.insert(
        "category-ru".into(),
        SiteList {
            suffix: vec!["yandex.ru".into(), "vk.com".into()],
            full: vec!["www.example.ru".into()],
            ..SiteList::default()
        },
    );
    geo.ips.insert(
        "ru".into(),
        vec!["5.8.0.0/13".into(), "2a00:1450::/32".into()],
    );
    geo.ips.insert("private".into(), vec!["10.0.0.0/8".into()]);
    geo
}

// ---- the syntax of a rule ------------------------------------------------------------------

#[test]
fn every_form_of_a_rule_is_read() {
    use Rule::*;
    for (entry, expected) in [
        ("domain:example.com", Suffix("example.com".into())),
        ("domain:.ru", Suffix("ru".into())),
        ("DOMAIN:.XN--P1AI", Suffix("xn--p1ai".into())),
        ("domain:Пример.рф", Suffix("xn--e1afmkfd.xn--p1ai".into())),
        ("full:www.Example.com", Full("www.example.com".into())),
        ("keyword:Ads", Keyword("ads".into())),
        (r"regexp:^ad[0-9]{1,3}\.", Regex(r"^ad[0-9]{1,3}\.".into())),
        ("geosite:category-ru", Geosite("category-ru".into())),
        ("geosite:geolocation-!cn", Geosite("geolocation-!cn".into())),
        ("10.0.0.0/8", Cidr("10.0.0.0/8".into())),
        ("255.255.255.255", Cidr("255.255.255.255/32".into())),
        ("192.0.2.1", Cidr("192.0.2.1/32".into())),
        ("fc00::/7", Cidr("fc00::/7".into())),
        ("::1", Cidr("::1/128".into())),
        ("geoip:RU", Geoip("ru".into())),
        ("geoip:private", Geoip("private".into())),
        ("port:443", Port(443)),
        ("port:1000-2000", PortRange(1000, 2000)),
        ("udp:443", NetPort(Net::Udp, 443)),
        ("TCP:80", NetPort(Net::Tcp, 80)),
        ("udp:1000-2000", NetPortRange(Net::Udp, 1000, 2000)),
        ("tcp: 8080 ", NetPort(Net::Tcp, 8080)),
        ("protocol:bittorrent", Protocol("bittorrent".into())),
        ("Protocol:QUIC", Protocol("quic".into())),
        ("process:Chrome.exe", Process("Chrome.exe".into())),
        ("  domain:example.com  ", Suffix("example.com".into())),
    ] {
        assert_eq!(parse_rule(entry).unwrap(), expected, "{entry}");
    }
}

#[test]
fn every_protocol_that_can_be_named_is_read_and_dns_is_not_one_of_them() {
    for name in PROTOCOLS {
        assert_eq!(
            parse_rule(&format!("protocol:{name}")).unwrap(),
            Rule::Protocol(name.to_owned())
        );
    }
    assert!(!PROTOCOLS.contains(&"dns"));
}

#[test]
fn what_cannot_be_read_is_refused_with_the_entry_named() {
    for bad in [
        "example.com", // a bare word: suffix or substring? say which
        "",
        "domain:",
        "domain:not a domain",
        "domain:1.2.3.4", // an address is not a domain
        "full:a..b",
        "keyword:",
        "keyword:two words",
        "regexp:(",
        "regexp:",
        "geosite:",
        "geosite:!cn",       // negation is not supported
        "geosite:google@cn", // nor attributes
        "geoip:!ru",
        "geoip:",
        "10.0.0.0/33",
        "fc00::/129",
        "300.1.1.1",
        "port:0",
        "port:65536",
        "port:443-80",
        "port:a-b",
        "port:",
        "udp:",
        "udp:0",
        "udp:65536",
        "udp:abc",
        "udp:20-10",
        "tcp:1-",
        "sctp:443",
        "protocol:",
        "protocol:foo",
        "protocol:dns", // answered by Rift itself
        "process:",
        r"process:C:\Windows\x.exe",
        "process:a/b.exe",
        "process:*.exe",
        "unknown:value",
    ] {
        let error = parse_rule(bad).unwrap_err().to_string();
        assert!(
            bad.is_empty()
                || error.contains(bad.trim())
                || error.contains(bad.split(':').next().unwrap()),
            "{bad:?} → {error}"
        );
    }
    assert!(
        parse_rule(&format!("keyword:{}", "x".repeat(300))).is_err(),
        "too long"
    );
}

#[test]
fn typed_text_splits_into_entries() {
    // The owner's own default list, comma separated, and the same pasted one per line.
    let comma = DEFAULT_DIRECT.join(",");
    assert_eq!(split_entries(&comma), DEFAULT_DIRECT.map(str::to_owned));
    assert_eq!(
        split_entries(&DEFAULT_DIRECT.join("\n")),
        DEFAULT_DIRECT.map(str::to_owned)
    );
    assert_eq!(
        split_entries("a.com, b.com;c.com\n\n# a comment\n  d.com  \nB.COM"),
        ["a.com", "b.com", "c.com", "d.com"],
        "blanks, comments and repeats (ignoring case) go"
    );
    // A comma inside a regular expression is a part of it.
    assert_eq!(
        split_entries("regexp:^a{1,3}b$\ndomain:x.com, domain:y.com"),
        ["regexp:^a{1,3}b$", "domain:x.com", "domain:y.com"]
    );
}

#[test]
fn the_direct_outbound_is_there_even_when_no_rule_sends_anything_to_it() {
    // The check for a network at all goes through it, whatever the profile says.
    let only_proxy = routing([Route::Block, Route::Direct, Route::Proxy], &[], &[], &[]);
    let config = config_of(only_proxy, &geo());
    let outbounds = config["outbounds"].as_array().unwrap();
    assert_eq!(
        outbounds
            .iter()
            .filter(|o| o["tag"] == "direct")
            .collect::<Vec<_>>(),
        vec![&json!({"type": "direct", "tag": "direct"})]
    );
    assert_eq!(config["route"]["final"], "main", "nothing is routed to it");
}

#[test]
fn the_default_profile_is_the_owners() {
    let default = Routing::default();
    assert_eq!(default.order, [Route::Block, Route::Direct, Route::Proxy]);
    assert!(default.block.is_empty() && default.proxy.is_empty());
    assert_eq!(default.direct.len(), 12);
    assert_eq!(
        &default.direct[..3],
        ["domain:.ru", "domain:.xn--p1ai", "geosite:category-ru"]
    );
    assert!(default.direct.contains(&"255.255.255.255".to_owned()));
    assert!(
        default.direct.contains(&"fc00::/7".to_owned()),
        "the IPv6 ranges of the same kind"
    );
    default.validate().unwrap();
    assert_eq!(default.rest(), Route::Proxy);
}

#[test]
fn an_order_is_a_permutation_and_there_are_six() {
    let orders = Routing::all_orders();
    assert_eq!(orders.len(), 6);
    let unique: std::collections::HashSet<_> = orders.iter().collect();
    assert_eq!(unique.len(), 6);
    for order in orders {
        Routing {
            order,
            ..Routing::default()
        }
        .validate()
        .unwrap();
    }
    for bad in [
        [Route::Block, Route::Block, Route::Proxy],
        [Route::Direct, Route::Direct, Route::Direct],
    ] {
        assert!(Routing {
            order: bad,
            ..Routing::default()
        }
        .validate()
        .is_err());
    }
}

#[test]
fn lists_are_limited_and_one_bad_entry_refuses_the_whole_profile() {
    let many = (0..=MAX_ENTRIES)
        .map(|i| format!("port:{}", i % 60000 + 1))
        .collect::<Vec<_>>();
    let routing = Routing {
        proxy: many,
        ..Routing::default()
    };
    assert!(routing.validate().is_err());
    let error = Routing::from_text(Route::ALL, "domain:ok.com\nnot a rule", "", "")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("Block") && error.contains("not a rule"),
        "{error}"
    );
    // A bad entry in the list that is not applied is refused as well: it would come back
    // with another order.
    assert!(Routing::from_text(
        [Route::Block, Route::Direct, Route::Proxy],
        "",
        "",
        "broken entry"
    )
    .is_err());
}

#[test]
fn the_names_of_the_lists_a_profile_refers_to_come_from_the_applied_lists_only() {
    let profile = routing(
        [Route::Block, Route::Direct, Route::Proxy],
        &["geosite:ads", "geoip:private"],
        &["geosite:category-ru", "geoip:ru", "geosite:ads"],
        &["geosite:not-applied", "geoip:xx"],
    );
    let (sites, ips) = profile.geo_names();
    assert_eq!(sites, ["ads", "category-ru"]);
    assert_eq!(ips, ["private", "ru"]);
}

proptest! {
    #[test]
    fn any_entry_is_read_or_refused_without_panicking(entry in "(?s).{0,300}") {
        let _ = parse_rule(&entry);
        let _ = split_entries(&entry);
    }

    #[test]
    fn a_read_entry_reads_the_same_again(entry in "[a-z0-9:./!_\\-]{1,60}") {
        if let Ok(rule) = parse_rule(&entry) {
            // Re-reading what the generator would write gives the same rule.
            let again = match &rule {
                Rule::Suffix(d) => format!("domain:{d}"),
                Rule::Full(d) => format!("full:{d}"),
                Rule::Keyword(w) => format!("keyword:{w}"),
                Rule::Regex(r) => format!("regexp:{r}"),
                Rule::Geosite(n) => format!("geosite:{n}"),
                Rule::Geoip(c) => format!("geoip:{c}"),
                Rule::Cidr(c) => c.clone(),
                Rule::Port(p) => format!("port:{p}"),
                Rule::PortRange(a, b) => format!("port:{a}-{b}"),
                Rule::NetPort(n, p) => format!("{}:{p}", n.name()),
                Rule::NetPortRange(n, a, b) => format!("{}:{a}-{b}", n.name()),
                Rule::Protocol(p) => format!("protocol:{p}"),
                Rule::Process(p) => format!("process:{p}"),
            };
            prop_assert_eq!(parse_rule(&again).unwrap(), rule);
        }
    }
}

// ---- the config ------------------------------------------------------------------------------

#[test]
fn the_default_profile_sends_the_russian_zones_and_local_ranges_straight_out() {
    let config = config_of(Routing::default(), &GeoSet::none());
    assert_eq!(
        rules(&config),
        [
            json!({"domain_suffix": ["ru", "xn--p1ai"], "action": "route", "outbound": "direct"}),
            json!({"ip_cidr": [
                "10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "169.254.0.0/16",
                "224.0.0.0/4", "255.255.255.255/32", "fc00::/7", "fe80::/10", "ff00::/8"
            ], "action": "route", "outbound": "direct"}),
        ],
        "geosite:category-ru needs its database, which is not there yet"
    );
    assert_eq!(
        config["route"]["final"], "main",
        "everything else goes through the server"
    );
    assert!(config["outbounds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o == &json!({"type": "direct", "tag": "direct"})));
    // The direct domains are resolved by the local resolver, not through the server.
    assert_eq!(
        config["dns"]["rules"],
        json!([{"domain_suffix": ["ru", "xn--p1ai"], "server": "local"}])
    );
    assert!(config["route"].get("rule_set").is_none());
    assert!(config["route"].get("find_process").is_none());
}

#[test]
fn the_process_lookup_is_off_unless_a_rule_or_the_setting_asks_for_it() {
    let mut store = store(Routing::default());
    let config = generate_with(&store, SECRET, &GeoSet::none()).unwrap();
    assert!(config["route"].get("find_process").is_none());
    // The setting alone (no rule by program) turns it on, for the connections page.
    store.settings.find_process = true;
    let config = generate_with(&store, SECRET, &GeoSet::none()).unwrap();
    assert_eq!(config["route"]["find_process"], true);
    // A rule by program turns it on with the setting off, as before.
    store.settings.find_process = false;
    store.routing = routing(
        [Route::Proxy, Route::Direct, Route::Block],
        &[],
        &[],
        &["process:chrome.exe"],
    );
    let config = generate_with(&store, SECRET, &GeoSet::none()).unwrap();
    assert_eq!(config["route"]["find_process"], true);
}

#[test]
fn a_saved_state_from_before_the_setting_loads_with_it_off() {
    let mut value = serde_json::to_value(Store::default().settings).unwrap();
    value.as_object_mut().unwrap().remove("find_process");
    let settings: rift_core::model::Settings = serde_json::from_value(value).unwrap();
    assert!(!settings.find_process);
    assert!(!rift_core::model::Settings::default().find_process);
}

#[test]
fn with_the_databases_the_lists_are_inline_rule_sets() {
    let config = config_of(Routing::default(), &geo());
    let all = rules(&config);
    assert_eq!(all.len(), 3);
    assert_eq!(
        all[2],
        json!({"rule_set": ["geosite-category-ru"], "action": "route", "outbound": "direct"})
    );
    assert_eq!(
        config["route"]["rule_set"],
        json!([{
            "type": "inline", "tag": "geosite-category-ru",
            "rules": [{"domain": ["www.example.ru"], "domain_suffix": ["yandex.ru", "vk.com"]}]
        }])
    );
    // The direct geosite domains are resolved locally too.
    assert!(config["dns"]["rules"]
        .as_array()
        .unwrap()
        .contains(&json!({"rule_set": ["geosite-category-ru"], "server": "local"})));
    let with_ip = config_of(
        routing(
            [Route::Proxy, Route::Direct, Route::Block],
            &[],
            &["geoip:ru"],
            &["geoip:private"],
        ),
        &geo(),
    );
    let sets = with_ip["route"]["rule_set"].as_array().unwrap();
    assert_eq!(sets.len(), 2);
    assert!(sets.contains(&json!({"type": "inline", "tag": "geoip-ru", "rules": [{"ip_cidr": ["5.8.0.0/13", "2a00:1450::/32"]}]})));
}

#[test]
fn rules_that_need_a_missing_database_or_list_are_left_out() {
    let only_geo = routing(
        Route::ALL,
        &[],
        &["geosite:category-ru", "geoip:ru", "geosite:unknown"],
        &[],
    );
    let none = config_of(only_geo.clone(), &GeoSet::none());
    assert!(rules(&none).is_empty());
    assert!(none["route"].get("rule_set").is_none());
    // A list the database does not have is skipped; the ones it has are applied, together in
    // one rule (either of them matches).
    let partial = config_of(only_geo, &geo());
    assert_eq!(
        rules(&partial),
        [
            json!({"rule_set": ["geosite-category-ru", "geoip-ru"], "action": "route", "outbound": "direct"})
        ]
    );
}

fn actions(config: &Value) -> Vec<(String, String)> {
    rules(config)
        .iter()
        .map(|rule| {
            (
                rule["action"].as_str().unwrap().to_owned(),
                rule["outbound"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect()
}

#[test]
fn each_of_the_six_orders_checks_two_lists_and_sends_the_rest_to_the_third() {
    use Route::*;
    let lists = |order| {
        routing(
            order,
            &["domain:blocked.example"],
            &["domain:direct.example"],
            &["domain:proxied.example"],
        )
    };
    // (order, the two checked lists as (action, outbound), what is left, tail rule, direct outbound)
    type Case = (
        [Route; 3],
        [(&'static str, &'static str); 2],
        &'static str,
        bool,
    );
    let expected: [Case; 6] = [
        (
            [Block, Direct, Proxy],
            [("reject", ""), ("route", "direct")],
            "main",
            false,
        ),
        (
            [Block, Proxy, Direct],
            [("reject", ""), ("route", "main")],
            "direct",
            false,
        ),
        (
            [Direct, Block, Proxy],
            [("route", "direct"), ("reject", "")],
            "main",
            false,
        ),
        (
            [Direct, Proxy, Block],
            [("route", "direct"), ("route", "main")],
            "main",
            true,
        ),
        (
            [Proxy, Block, Direct],
            [("route", "main"), ("reject", "")],
            "direct",
            false,
        ),
        (
            [Proxy, Direct, Block],
            [("route", "main"), ("route", "direct")],
            "main",
            true,
        ),
    ];
    for (order, checked, rest, tail) in expected {
        let config = config_of(lists(order), &GeoSet::none());
        let got = actions(&config);
        let want: Vec<(String, String)> = checked
            .iter()
            .map(|(a, o)| (a.to_string(), o.to_string()))
            .chain(tail.then(|| ("reject".to_string(), String::new())))
            .collect();
        assert_eq!(got, want, "{order:?}");
        assert_eq!(config["route"]["final"], rest, "{order:?}");
        // The domain each rule carries shows which list it came from, in the order given.
        let from: Vec<&str> = rules(&config)
            .iter()
            .filter_map(|rule| {
                rule["domain_suffix"][0].as_str().map(|s| {
                    if s.starts_with("block") {
                        "block"
                    } else if s.starts_with("direct") {
                        "direct"
                    } else {
                        "proxy"
                    }
                })
            })
            .collect();
        let expected_from: Vec<&str> = order[..2]
            .iter()
            .map(|route| match route {
                Block => "block",
                Direct => "direct",
                Proxy => "proxy",
            })
            .collect();
        assert_eq!(from, expected_from, "{order:?}");
        // The `direct` outbound exists exactly when something is sent to it.
        let sends_direct = got.iter().any(|(_, outbound)| outbound == "direct") || rest == "direct";
        let has_direct = config["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["tag"] == "direct");
        assert_eq!(has_direct, sends_direct, "{order:?}");
    }
}

#[test]
fn the_last_list_is_not_applied_and_keeps_its_entries() {
    let profile = routing(
        [Route::Block, Route::Direct, Route::Proxy],
        &[],
        &["domain:direct.example"],
        &["domain:never-applied.example"],
    );
    let config = config_of(profile, &GeoSet::none());
    assert!(!config.to_string().contains("never-applied"));
    // Reordering brings the entries back to work.
    let reordered = config_of(
        routing(
            [Route::Proxy, Route::Direct, Route::Block],
            &[],
            &["domain:direct.example"],
            &["domain:never-applied.example"],
        ),
        &GeoSet::none(),
    );
    assert!(reordered.to_string().contains("never-applied"));
}

#[test]
fn every_kind_of_entry_becomes_one_rule_of_its_own_kind() {
    let profile = routing(
        [Route::Proxy, Route::Block, Route::Direct],
        &[
            "keyword:ads",
            "full:tracker.example",
            r"regexp:^ad[0-9]+\.",
            "port:25",
            "port:6881-6889",
            "udp:443",
            "tcp:1000-2000",
            "protocol:bittorrent",
        ],
        &[],
        &[
            "process:chrome.exe",
            "domain:work.example",
            "192.0.2.0/24",
            "port:22",
        ],
    );
    let config = config_of(profile, &GeoSet::none());
    let all = rules(&config);
    // Proxy first: every rule that routes comes before the first that rejects.
    let first_reject = all
        .iter()
        .position(|rule| rule["action"] == "reject")
        .unwrap();
    assert!(all[..first_reject]
        .iter()
        .all(|rule| rule["outbound"] == "main"));
    assert!(all[first_reject..]
        .iter()
        .all(|rule| rule["action"] == "reject"));
    assert!(
        all.contains(&json!({"ip_cidr": ["192.0.2.0/24"], "action": "route", "outbound": "main"}))
    );
    assert!(all.contains(
        &json!({"domain_suffix": ["work.example"], "action": "route", "outbound": "main"})
    ));
    assert!(all.contains(&json!({"port": [22], "action": "route", "outbound": "main"})));
    assert!(all
        .contains(&json!({"process_name": ["chrome.exe"], "action": "route", "outbound": "main"})));
    // Then Block.
    assert!(all.contains(&json!({"domain_keyword": ["ads"], "action": "reject"})));
    assert!(all.contains(&json!({"domain": ["tracker.example"], "action": "reject"})));
    assert!(all.contains(&json!({"domain_regex": [r"^ad[0-9]+\."], "action": "reject"})));
    assert!(all.contains(&json!({"port": [25], "action": "reject"})));
    assert!(all.contains(&json!({"port_range": ["6881:6889"], "action": "reject"})));
    // A transport and its port are two conditions of one rule; a port alone names no transport.
    assert!(all.contains(&json!({"network": "udp", "port": [443], "action": "reject"})));
    assert!(
        all.contains(&json!({"network": "tcp", "port_range": ["1000:2000"], "action": "reject"}))
    );
    assert!(all.contains(&json!({"protocol": ["bittorrent"], "action": "reject"})));
    assert!(!all.contains(&json!({"port": [443], "action": "reject"})));
    // A process rule needs the process to be looked up.
    assert_eq!(config["route"]["find_process"], true);
    // The rest goes direct.
    assert_eq!(config["route"]["final"], "direct");
}

/// `sing-box check` does not look for a rule's outbound, so the generator must never name one
/// that is not there.
#[test]
fn every_outbound_a_rule_names_exists() {
    let entries = [
        "domain:a.example",
        "10.0.0.0/8",
        "port:80",
        "udp:443",
        "protocol:bittorrent",
        "geosite:category-ru",
    ];
    for order in Routing::all_orders() {
        let config = config_of(routing(order, &entries, &entries, &entries), &geo());
        let tags: Vec<&str> = config["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["tag"].as_str().unwrap())
            .collect();
        for rule in config["route"]["rules"].as_array().unwrap() {
            if let Some(outbound) = rule["outbound"].as_str() {
                assert!(
                    tags.contains(&outbound),
                    "{order:?}: {outbound} is not an outbound"
                );
            }
        }
        assert!(
            tags.contains(&config["route"]["final"].as_str().unwrap()),
            "{order:?}"
        );
        // Every rule set that a rule names is defined.
        let defined: Vec<&str> = config["route"]["rule_set"]
            .as_array()
            .map(|sets| sets.iter().map(|s| s["tag"].as_str().unwrap()).collect())
            .unwrap_or_default();
        for rule in config["route"]["rules"].as_array().unwrap() {
            for tag in rule["rule_set"].as_array().into_iter().flatten() {
                assert!(defined.contains(&tag.as_str().unwrap()), "{order:?}: {tag}");
            }
        }
    }
}

#[test]
fn a_profile_that_blocks_the_rest_ends_with_a_rule_that_rejects_everything() {
    let config = config_of(
        routing(
            [Route::Proxy, Route::Direct, Route::Block],
            &[],
            &[],
            &["domain:work.example"],
        ),
        &GeoSet::none(),
    );
    assert_eq!(rules(&config).last().unwrap(), &json!({"action": "reject"}));
}

#[test]
fn the_profile_does_not_depend_on_the_mode_of_work() {
    for mode in [Mode::Proxy, Mode::Tun, Mode::TunProxy] {
        let mut store = store(Routing::default());
        store.settings.mode = mode;
        let config = generate_with(&store, SECRET, &GeoSet::none()).unwrap();
        assert_eq!(rules(&config).len(), 2);
    }
}

#[test]
fn an_invalid_profile_in_the_store_stops_the_config() {
    let mut bad = Routing::default();
    bad.proxy.push("garbage".into());
    assert!(generate_with(&store(bad), SECRET, &GeoSet::none()).is_err());
}

#[test]
fn diagnostics_carry_the_shape_of_the_routing_not_its_entries() {
    let profile = routing(
        [Route::Block, Route::Direct, Route::Proxy],
        &["domain:secret-company.example"],
        &[
            "domain:private-site.example",
            "192.168.77.0/24",
            "geosite:category-ru",
            "geoip:ru",
        ],
        &[],
    );
    let config = config_of(profile, &geo());
    let report = config::sanitized(&config).to_string();
    for leaked in [
        "geosite-category-ru",
        "geoip-ru",
        "secret-company",
        "private-site",
        "192.168.77",
        "yandex.ru",
        "5.8.0.0",
    ] {
        assert!(!report.contains(leaked), "{leaked} in {report}");
    }
    assert!(
        report.contains("domain_suffix"),
        "the kinds are there: {report}"
    );
}

// ---- the resolver of the names sing-box dials ---------------------------------------------------

fn bridged(routing: Routing, geo: &GeoSet) -> Value {
    generate_resolving(&store(routing), SECRET, geo, Resolver::Bridge(41234)).unwrap()
}

#[test]
fn the_local_resolver_is_the_bridge_on_the_loopback_with_the_time_the_chain_needs() {
    let config = bridged(Routing::default(), &geo());
    let local = config["dns"]["servers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|server| server["tag"] == "local")
        .unwrap();
    assert_eq!(
        *local,
        json!({"type": "tcp", "tag": "local", "server": "127.0.0.1", "server_port": 41234})
    );
    // The names of the servers and of the sites that leave directly: one resolver, the bridge,
    // and the whole chain (every resolver at its limit) fits in what sing-box waits.
    assert_eq!(
        config["route"]["default_domain_resolver"],
        json!({"server": "local", "timeout": "15s"})
    );
    let waited = std::time::Duration::from_secs(15);
    let chain = rift_core::dns::Chain::public().len() as u32 * rift_core::dns::STEP_TIMEOUT;
    assert!(chain < waited, "{chain:?} does not fit in {waited:?}");
    let dns_rules = config["dns"]["rules"].as_array().unwrap();
    assert!(!dns_rules.is_empty());
    for rule in dns_rules {
        assert_eq!(rule["server"], "local");
        assert_eq!(rule["timeout"], "15s", "{rule}");
    }
    // The names that go through the tunnel are not the bridge's: their resolver is unchanged.
    let remote = config["dns"]["servers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|server| server["tag"] == "remote")
        .unwrap();
    assert_eq!(remote["detour"], "main");
    assert_eq!(config["dns"]["final"], "remote");
}

#[test]
fn without_a_bridge_the_config_is_what_it_was() {
    let config = config_of(Routing::default(), &geo());
    assert_eq!(config["route"]["default_domain_resolver"], "local");
    assert!(config["dns"]["servers"]
        .as_array()
        .unwrap()
        .contains(&json!({"type": "local", "tag": "local"})));
    for rule in config["dns"]["rules"].as_array().unwrap() {
        assert!(rule.get("timeout").is_none());
    }
}

// ---- the real sing-box ---------------------------------------------------------------------

/// The sing-box of the development checkout, when there is one for this machine.
fn sing_box() -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../desktop/src-tauri/binaries/sing-box-x86_64-unknown-linux-gnu");
    (cfg!(target_os = "linux") && path.exists()).then_some(path)
}
fn accepted(binary: &std::path::Path, config: &Value) -> Result<(), String> {
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
fn the_real_sing_box_accepts_every_order_with_every_kind_of_rule() {
    let Some(binary) = sing_box() else {
        eprintln!("skipped: no sing-box binary for this machine in desktop/src-tauri/binaries");
        return;
    };
    let entries = [
        "domain:example.com",
        "full:www.example.com",
        "keyword:ads",
        r"regexp:^ad[0-9]+\.",
        "geosite:category-ru",
        "10.0.0.0/8",
        "fc00::/7",
        "geoip:ru",
        "port:443",
        "port:1000-2000",
        "udp:443",
        "tcp:1000-2000",
        "protocol:bittorrent",
        "process:chrome.exe",
    ];
    for order in Routing::all_orders() {
        for mode in [Mode::Proxy, Mode::Tun, Mode::TunProxy] {
            let mut store = store(routing(order, &entries, &entries, &entries));
            store.settings.mode = mode;
            for geo in [GeoSet::none(), geo()] {
                for resolver in [Resolver::System, Resolver::Bridge(53535)] {
                    let config = generate_resolving(&store, SECRET, &geo, resolver).unwrap();
                    accepted(&binary, &config).unwrap_or_else(|error| {
                        panic!(
                            "{order:?} {:?} {resolver:?}: {error}\n{}",
                            store.settings.mode,
                            serde_json::to_string_pretty(&config["route"]).unwrap()
                        )
                    });
                }
            }
        }
    }
    // The defaults, as a new installation has them.
    accepted(&binary, &config_of(Routing::default(), &geo())).unwrap();
    accepted(&binary, &config_of(Routing::default(), &GeoSet::none())).unwrap();
}

#[test]
fn the_real_sing_box_accepts_every_protocol_a_rule_can_name() {
    let Some(binary) = sing_box() else {
        eprintln!("skipped: no sing-box binary for this machine in desktop/src-tauri/binaries");
        return;
    };
    for name in PROTOCOLS {
        let entry = format!("protocol:{name}");
        let config = config_of(
            routing(
                [Route::Block, Route::Direct, Route::Proxy],
                &[entry.as_str()],
                &[],
                &[],
            ),
            &GeoSet::none(),
        );
        accepted(&binary, &config).unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

#[test]
fn the_real_sing_box_refuses_what_the_generator_would_never_write() {
    let Some(binary) = sing_box() else {
        eprintln!("skipped: no sing-box binary for this machine in desktop/src-tauri/binaries");
        return;
    };
    // A control: the check does check the rules, so the acceptance above means something.
    let mut config = config_of(Routing::default(), &GeoSet::none());
    config["route"]["rules"][2] =
        json!({"domain_regex": ["("], "action": "route", "outbound": "direct"});
    assert!(accepted(&binary, &config).is_err());
}

// ---- stored data and profiles ------------------------------------------------------------------

#[test]
fn a_state_file_from_before_routing_gets_the_default_profile() {
    let old = r#"{"version":1,"settings":{},"servers":[],"subscriptions":[],"selected":null}"#;
    let store = persistence::decode(old.as_bytes()).unwrap();
    assert_eq!(store.routing, Routing::default());
}

#[test]
fn a_saved_profile_survives_a_save_and_a_profile_round_trip() {
    let mut original = store(routing(
        [Route::Proxy, Route::Direct, Route::Block],
        &["domain:blocked.example"],
        &["geosite:category-ru", "port:443"],
        &["process:chrome.exe"],
    ));
    original.welcome_dismissed = true;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    persistence::save(&path, &original).unwrap();
    assert_eq!(persistence::load(&path).unwrap().routing, original.routing);
    let bytes = profile::encode(&original).unwrap();
    assert_eq!(profile::decode(&bytes).unwrap().routing, original.routing);
}

#[test]
fn a_damaged_profile_in_a_file_stops_the_load() {
    let mut bad = store(Routing::default());
    bad.routing.direct.push("this is not a rule".into());
    let json = serde_json::to_vec(&bad).unwrap();
    assert!(persistence::decode(&json).is_err());
    assert!(profile::validate(&bad).is_err());
    bad.routing = Routing {
        order: [Route::Block, Route::Block, Route::Direct],
        ..Routing::default()
    };
    assert!(persistence::decode(&serde_json::to_vec(&bad).unwrap()).is_err());
}

#[test]
fn dns_domain_rules_keep_the_traffic_lists_priority() {
    for order in Routing::all_orders() {
        let config = bridged(
            routing(order, &["domain:ru"], &["domain:ru"], &["domain:ru"]),
            &GeoSet::none(),
        );
        let dns = config["dns"]["rules"].as_array().unwrap();
        assert_eq!(dns.len(), 2);
        for (rule, route) in dns.iter().zip(order[..2].iter()) {
            assert_eq!(rule["domain_suffix"], json!(["ru"]));
            match route {
                Route::Direct => {
                    assert_eq!(rule["server"], "local");
                    assert_eq!(rule["timeout"], "15s");
                }
                Route::Proxy => {
                    assert_eq!(rule["server"], "remote");
                    assert!(rule.get("timeout").is_none());
                }
                Route::Block => {
                    assert_eq!(rule["action"], "reject");
                    assert!(rule.get("server").is_none());
                    assert!(rule.get("timeout").is_none());
                }
            }
        }
    }
}

#[test]
fn bootstrap_resolvers_bypass_tun_without_rewriting_server_names() {
    for mode in [Mode::Tun, Mode::TunProxy] {
        let mut store = store(Routing::default());
        store.settings.mode = mode;
        store.servers = vec![parse_uri("trojan://secret@server.example:443", None).unwrap()];
        let config =
            generate_resolving(&store, SECRET, &GeoSet::none(), Resolver::Bridge(41234)).unwrap();
        let tun = config["inbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|inbound| inbound["type"] == "tun")
            .unwrap();
        assert_eq!(
            tun["route_exclude_address"],
            json!(rift_core::dns::DOH_IPS.map(|ip| format!("{ip}/32")))
        );
        assert!(config["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|outbound| outbound["server"] == "server.example"));
    }
}

#[test]
fn rust_only_regex_extensions_are_refused_before_saving_a_rule() {
    for expression in [
        "(?x)a b",
        "(?u)a",
        "(?-u:a)",
        "(?R)a",
        r"\u{41}",
        r"\p{Script=Greek}",
        "[a&&b]",
        "a++",
        "a{1001}",
        "(a{100}){100}",
    ] {
        assert!(
            parse_rule(&format!("regexp:{expression}")).is_err(),
            "{expression}"
        );
    }
    for expression in [
        "(?i)example",
        "(?ms:abc)",
        "a{1,1000}",
        r"\p{Greek}+",
        r"^ad[0-9]+\.",
        r"\(\?x\)",
    ] {
        assert!(
            parse_rule(&format!("regexp:{expression}")).is_ok(),
            "{expression}"
        );
    }
}
