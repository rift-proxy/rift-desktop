use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rift_core::{
    config,
    model::{Mode, Server, Store},
    parser::{decode64, parse_uri},
    persistence,
    subscription::{merge, parse_subscription},
};
const URI:&str="vless://11111111-1111-4111-8111-111111111111@example.com:443?security=tls&type=ws&path=%2Fproxy&host=cdn.example.com#Frankfurt";
#[test]
fn vless_transport_and_name() {
    let s = parse_uri(URI, None).unwrap();
    assert_eq!(s.name, "Frankfurt");
    assert_eq!(s.outbound["transport"]["path"], "/proxy");
    assert_eq!(s.outbound["tls"]["server_name"], "example.com");
}
#[test]
fn reality_ipv6() {
    let s=parse_uri("vless://11111111-1111-4111-8111-111111111111@[::1]:443?security=reality&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&sid=ab12&sni=example.com&type=grpc&serviceName=grpc",None).unwrap();
    assert_eq!(s.host, "::1");
    assert_eq!(s.protocol, "vless reality");
    assert_eq!(s.outbound["tls"]["reality"]["short_id"], "ab12");
}
#[test]
fn trojan_encoded_password() {
    let s = parse_uri("trojan://p%40ss%3Aword@example.com:443#Hello%20world", None).unwrap();
    assert_eq!(s.outbound["password"], "p@ss:word");
    assert_eq!(s.name, "Hello world");
}
#[test]
fn shadowsocks_formats() {
    for uri in [
        format!(
            "ss://{}@example.com:8388",
            URL_SAFE_NO_PAD.encode("aes-256-gcm:secret")
        ),
        format!(
            "ss://{}",
            URL_SAFE_NO_PAD.encode("aes-256-gcm:secret@example.com:8388")
        ),
        "ss://aes-256-gcm:secret@example.com:8388".into(),
    ] {
        assert_eq!(
            parse_uri(&uri, None).unwrap().outbound["password"],
            "secret"
        );
    }
}
#[test]
fn vmess_json() {
    let raw = r#"{"v":"2","ps":"Test","add":"example.com","port":"443","id":"11111111-1111-4111-8111-111111111111","net":"ws","path":"/ws","tls":"tls","sni":"example.com"}"#;
    let s = parse_uri(&format!("vmess://{}", URL_SAFE_NO_PAD.encode(raw)), None).unwrap();
    assert_eq!(s.outbound["transport"]["type"], "ws");
    assert_eq!(s.outbound["alter_id"], 0);
}
#[test]
fn unknown_duplicate_unsafe_rejected() {
    for suffix in ["&unknown=SECRET", "&type=tcp", "&allowInsecure=1"] {
        assert!(parse_uri(
            &format!("{}{}", URI.split('#').next().unwrap(), suffix),
            None
        )
        .is_err());
    }
}
#[test]
fn fingerprint_ignores_name_and_source() {
    let a = parse_uri(URI, None).unwrap();
    let b = parse_uri(&URI.replace("#Frankfurt", "#Paris"), Some("source".into())).unwrap();
    assert_eq!(a.id, b.id);
    let mut o = a.outbound.clone();
    o["tag"] = serde_json::json!("arbitrary");
    assert_eq!(Server::fingerprint(&o), a.id);
}
#[test]
fn subscriptions_partial_and_dedup() {
    let list = format!("{URI}\ninvalid\n{URI}\n");
    for data in [list.clone(), URL_SAFE_NO_PAD.encode(&list)] {
        let p = parse_subscription(&data, "sub").unwrap();
        assert_eq!(p.servers.len(), 1);
        assert_eq!(p.rejected, 1);
    }
}
#[test]
fn merge_preserves_manual_and_other_source() {
    let mut store = Store::default();
    store.servers.push(parse_uri(URI, None).unwrap());
    store
        .servers
        .push(parse_uri(URI, Some("other".into())).unwrap());
    let next = merge(&store, "sub", parse_subscription(URI, "sub").unwrap());
    assert_eq!(next.servers.len(), 3);
    assert_eq!(store.servers.len(), 2);
    let next = merge(&next, "sub", parse_subscription(URI, "sub").unwrap());
    assert_eq!(next.servers.len(), 3);
}
#[test]
fn empty_bad_subscription_is_error() {
    for input in ["", "garbage", "# comment"] {
        assert!(parse_subscription(input, "sub").is_err());
    }
}
#[test]
fn config_modes_and_redaction() {
    let mut s = Store::default();
    s.servers.push(parse_uri(URI, None).unwrap());
    for (mode, count) in [(Mode::Proxy, 1), (Mode::Tun, 1), (Mode::TunProxy, 2)] {
        s.settings.mode = mode;
        let c = config::generate(&s, "12345678901234567890123456789012").unwrap();
        assert_eq!(c["inbounds"].as_array().unwrap().len(), count);
        assert_eq!(c["route"]["final"], "main");
        assert!(!config::sanitized(&c).to_string().contains("11111111-1111"));
        assert!(!config::sanitized(&c)
            .to_string()
            .contains("12345678901234567890123456789012"));
    }
}
#[test]
fn config_inputs_cover_every_setting_that_changes_the_config() {
    // nosemgrep: generic.secrets.security.detected-generic-secret.detected-generic-secret -- synthetic test token
    let secret = "12345678901234567890123456789012";
    let mut base = Store::default();
    base.servers.push(parse_uri(URI, None).unwrap());
    let before = serde_json::to_value(&base.settings).unwrap();
    for (key, value) in before.as_object().unwrap() {
        let mutated = match value {
            serde_json::Value::Bool(flag) => serde_json::json!(!flag),
            // The pool holds at most five and three servers are measured at once: shrink those,
            // every other number grows.
            serde_json::Value::Number(number) if key == "pool_size" || key == "concurrency" => {
                serde_json::json!(number.as_u64().unwrap() - 1)
            }
            serde_json::Value::Number(number) => serde_json::json!(number.as_u64().unwrap() + 7),
            serde_json::Value::String(text) if key == "mode" => {
                serde_json::json!(if text == "tun" { "proxy" } else { "tun" })
            }
            serde_json::Value::String(text) if key == "theme" => {
                serde_json::json!(if text == "dark" { "light" } else { "dark" })
            }
            // The language is the interface's own and changes no configuration. `generate`
            // refuses an unknown one and there is only `ru` so far: leave it as it is, and
            // mutate it to the other language when one is added.
            serde_json::Value::String(text) if key == "language" => serde_json::json!(text),
            serde_json::Value::String(_) => serde_json::json!("https://example.org/check"),
            other => panic!("setting {key}: add a mutation for {other}"),
        };
        let mut next = base.clone();
        let mut settings = before.clone();
        settings[key] = mutated;
        next.settings = serde_json::from_value(settings).unwrap();
        let changes_config =
            config::generate(&base, secret).unwrap() != config::generate(&next, secret).unwrap();
        assert_eq!(
            config::config_inputs(&base.settings) != config::config_inputs(&next.settings),
            changes_config,
            "setting {key}: config_inputs must list exactly the settings the config reads"
        );
    }
}
#[test]
fn every_language_with_a_catalog_is_a_valid_setting_and_no_other_is() {
    use rift_core::{model::Settings, text};
    assert!(text::languages().any(|code| code == "ru"));
    for code in text::languages() {
        let settings = Settings {
            language: code.into(),
            ..Settings::default()
        };
        assert!(settings.validate().is_ok(), "{code}");
    }
}
#[test]
fn settings_of_an_older_file_read_the_language_as_russian() {
    use rift_core::model::Settings;
    let settings: Settings = serde_json::from_str(r#"{"theme":"dark"}"#).unwrap();
    assert_eq!(settings.language, "ru");
    assert!(settings.validate().is_ok());
}
#[test]
fn persistence_roundtrip_and_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.json");
    let mut s = persistence::load(&p).unwrap();
    s.servers.push(parse_uri(URI, None).unwrap());
    persistence::save(&p, &s).unwrap();
    s.settings.theme = "dark".into();
    persistence::save(&p, &s).unwrap();
    assert_eq!(persistence::load(&p).unwrap().servers.len(), 1);
    assert_eq!(persistence::load(&p).unwrap().settings.theme, "dark");
    std::fs::write(&p, b"broken").unwrap();
    assert!(persistence::load(&p).is_err());
    assert_eq!(std::fs::read(&p).unwrap(), b"broken");
}
#[test]
fn an_older_file_with_more_parallel_checks_is_read_as_the_largest_allowed_not_refused() {
    let encoded = |value: &serde_json::Value| serde_json::to_vec(value).unwrap();
    let mut old = serde_json::to_value(Store::default()).unwrap();
    for (saved, read) in [(10, 3), (64, 3), (4, 3), (3, 3), (2, 2), (1, 1)] {
        old["settings"]["concurrency"] = saved.into();
        assert_eq!(
            persistence::decode(&encoded(&old))
                .unwrap()
                .settings
                .concurrency,
            read,
            "saved {saved}"
        );
    }
    // Zero was never allowed and is still damage.
    old["settings"]["concurrency"] = 0.into();
    assert!(persistence::decode(&encoded(&old)).is_err());
    assert_eq!(rift_core::model::Settings::default().concurrency, 3);
}
#[test]
fn decode_accepts_only_valid_current_state() {
    let mut store = Store::default();
    store.servers.push(parse_uri(URI, None).unwrap());
    let encoded = |value: &serde_json::Value| serde_json::to_vec(value).unwrap();
    let valid = serde_json::to_value(&store).unwrap();
    assert_eq!(
        persistence::decode(&encoded(&valid)).unwrap().servers.len(),
        1
    );

    let mut other_version = valid.clone();
    other_version["version"] = 3.into();
    assert!(persistence::decode(&encoded(&other_version)).is_err());

    let mut bad_settings = valid;
    bad_settings["settings"]["proxy_port"] = 0.into();
    assert!(persistence::decode(&encoded(&bad_settings)).is_err());

    assert!(persistence::decode(b"").is_err());
    assert!(persistence::decode(b"{").is_err());
}
#[test]
fn a_server_outbound_that_is_not_an_object_is_an_error_not_a_panic() {
    // A hand-edited or damaged state.json can stay valid JSON while a server's outbound is a
    // string, number, bool, array or null. decode is the validation boundary (startup fails
    // closed and leaves the file alone); config::generate must not trust it either, because
    // Store values also come from merges and tests, not only from decode.
    let mut store = Store::default();
    store.servers.push(parse_uri(URI, None).unwrap());
    let valid = serde_json::to_value(&store).unwrap();
    for outbound in [
        serde_json::json!("x"),
        serde_json::json!(5),
        serde_json::json!(true),
        serde_json::json!([1]),
        serde_json::Value::Null,
    ] {
        let mut damaged = valid.clone();
        damaged["version"] = 1.into();
        damaged["servers"][0]
            .as_object_mut()
            .unwrap()
            .remove("connection");
        damaged["servers"][0]["outbound"] = outbound.clone();
        assert!(
            persistence::decode(&serde_json::to_vec(&damaged).unwrap()).is_err(),
            "decode accepted outbound {outbound}"
        );

        let mut store = store.clone();
        store.servers[0].outbound = outbound.clone();
        assert!(
            config::generate(&store, "12345678901234567890123456789012").is_err(),
            "generate accepted outbound {outbound}"
        );
    }
}
#[test]
fn persisted_numbers_in_outbound_survive_save_and_load_bit_for_bit() {
    // Server.outbound is a free-form Value. Without serde_json's float_roundtrip, a literal
    // with 16+ significant digits parsed to a neighbouring f64 on the second pass, so every
    // save/load cycle could change a stored number and the "decode(serialize(x)) serializes
    // identically" invariant of the persisted_state fuzz target was false for accepted input.
    let state = |extra: &str| {
        format!(
            r#"{{"version":1,"settings":{{}},"servers":[{{"id":"s1","name":"n","country":"","protocol":"vless","host":"example.com","port":443,"outbound":{{"type":"vless","tag":"s1","server":"example.com","server_port":443,"extra":{extra}}},"source":null}}],"subscriptions":[],"selected":null}}"#
        )
    };
    for literal in [
        "3.8548489968072214799843",
        "71927273177e-281",
        "1111111111111111111111",
        "0.96652203255087063",
        "-0.0",
        "1.7976931348623157e308",
    ] {
        let store = persistence::decode(state(literal).as_bytes())
            .unwrap_or_else(|e| panic!("{literal} is accepted JSON: {e}"));
        let first = serde_json::to_string(&store).unwrap();
        let again = persistence::decode(first.as_bytes()).unwrap();
        assert_eq!(
            serde_json::to_string(&again).unwrap(),
            first,
            "{literal} changed on the second serialisation"
        );
    }
}
proptest::proptest! {#[test]fn arbitrary_uri_never_panics(s in ".{0,500}"){let _=parse_uri(&s,None);}#[test]fn base64_round_trip(s in ".{0,500}"){proptest::prop_assert_eq!(decode64(&URL_SAFE_NO_PAD.encode(&s)).unwrap(),s);}}

#[test]
fn invalid_crypto_rows_do_not_poison_valid_subscription() {
    let body = format!("{}\nss://unknown:password@example.com:443\nvless://11111111-1111-4111-8111-111111111111@example.com:443?security=reality&pbk=bad\nss://2022-blake3-aes-128-gcm:bad@example.com:443",URI);
    let parsed = parse_subscription(&body, "sub").unwrap();
    assert_eq!(parsed.servers.len(), 1);
    assert_eq!(parsed.rejected, 3);
}
#[test]
fn unsupported_tls_fingerprint_rejects_only_its_row() {
    let vmess = |fp: &str| {
        let raw = format!(
            r#"{{"v":"2","add":"example.com","port":"443","id":"11111111-1111-4111-8111-111111111111","tls":"tls","fp":"{fp}"}}"#
        );
        format!("vmess://{}", URL_SAFE_NO_PAD.encode(raw))
    };
    let bad_uri = "vless://22222222-2222-4222-8222-222222222222@example.org:443?security=tls&fp=rift-invalid-fingerprint";
    let body = format!("{URI}\n{bad_uri}\n{}", vmess("rift-invalid-fingerprint"));
    let parsed = parse_subscription(&body, "sub").unwrap();
    assert_eq!(parsed.servers.len(), 1);
    assert_eq!(parsed.rejected, 2);
    let good = parse_uri(&URI.replace("#", "&fp=firefox#"), None).unwrap();
    assert_eq!(good.outbound["tls"]["utls"]["fingerprint"], "firefox");
    let good = parse_uri(&vmess("ios"), None).unwrap();
    assert_eq!(good.outbound["tls"]["utls"]["fingerprint"], "ios");
}
#[test]
fn config_needs_a_32_character_api_token() {
    let mut s = Store::default();
    s.servers.push(parse_uri(URI, None).unwrap());
    assert!(config::generate(&s, &"x".repeat(31)).is_err());
    assert!(config::generate(&s, &"x".repeat(32)).is_ok());
    assert!(config::generate(&s, &"x".repeat(64)).is_ok());
}
#[test]
fn sanitized_config_keeps_structure_without_credentials() {
    let mut s = Store::default();
    s.servers.push(parse_uri(URI, None).unwrap());
    let c = config::generate(&s, "12345678901234567890123456789012").unwrap();
    let clean = config::sanitized(&c);
    assert_eq!(clean["inbounds"], c["inbounds"]);
    // The route keeps its shape (how many rules of which kind, where they lead, what is left)
    // but not what the user typed into them: those are browsing patterns, not structure.
    assert_eq!(clean["route"]["final"], c["route"]["final"]);
    assert_eq!(
        clean["route"]["rules"].as_array().unwrap().len(),
        c["route"]["rules"].as_array().unwrap().len()
    );
    assert!(!clean["route"].to_string().contains("xn--p1ai"));
    let outbounds = clean["outbounds"].as_array().unwrap();
    assert_eq!(outbounds.len(), c["outbounds"].as_array().unwrap().len());
    assert_eq!(outbounds[0]["type"], "vless");
    assert_eq!(
        outbounds[0].as_object().unwrap().len(),
        2,
        "type and tag only"
    );
    assert_eq!(clean["experimental"]["clash_api"]["secret"], "[REDACTED]");
}
#[test]
fn unreadable_state_is_an_error_not_an_empty_store() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.json");
    std::fs::create_dir(&p).unwrap();
    assert!(
        persistence::load(&p).is_err(),
        "must not silently start empty"
    );
}
#[test]
fn every_settings_limit_is_enforced_on_its_own() {
    use rift_core::model::Settings;
    assert!(Settings::default().validate().is_ok());
    type Change = Box<dyn Fn(&mut Settings)>;
    let cases: Vec<(&str, Change)> = vec![
        (
            "http url",
            Box::new(|s| s.connectivity_url = "http://example.com/204".into()),
        ),
        (
            "user",
            Box::new(|s| s.connectivity_url = "https://u@example.com/".into()),
        ),
        (
            "password",
            Box::new(|s| s.connectivity_url = "https://u:p@example.com/".into()),
        ),
        (
            "not url",
            Box::new(|s| s.connectivity_url = "example".into()),
        ),
        ("proxy port 0", Box::new(|s| s.proxy_port = 0)),
        ("api port 0", Box::new(|s| s.api_port = 0)),
        ("same ports", Box::new(|s| s.api_port = s.proxy_port)),
        ("concurrency 0", Box::new(|s| s.concurrency = 0)),
        ("concurrency 4", Box::new(|s| s.concurrency = 4)),
        ("interval 49", Box::new(|s| s.interval_secs = 49)),
        ("interval 301", Box::new(|s| s.interval_secs = 301)),
        ("pool 0", Box::new(|s| s.pool_size = 0)),
        ("pool 6", Box::new(|s| s.pool_size = 6)),
        ("full interval 9", Box::new(|s| s.full_interval_mins = 9)),
        ("full interval 31", Box::new(|s| s.full_interval_mins = 31)),
        ("degrade percent 9", Box::new(|s| s.degrade_percent = 9)),
        ("degrade percent 301", Box::new(|s| s.degrade_percent = 301)),
        ("degrade cap 199", Box::new(|s| s.degrade_cap_ms = 199)),
        ("degrade cap 10001", Box::new(|s| s.degrade_cap_ms = 10001)),
        ("endpoint 99", Box::new(|s| s.endpoint_timeout_ms = 99)),
        (
            "endpoint 30001",
            Box::new(|s| s.endpoint_timeout_ms = 30001),
        ),
        ("proxy 99", Box::new(|s| s.proxy_timeout_ms = 99)),
        ("proxy 30001", Box::new(|s| s.proxy_timeout_ms = 30001)),
        (
            "subscription 0",
            Box::new(|s| s.subscription_interval_mins = 0),
        ),
        (
            "subscription week+1",
            Box::new(|s| s.subscription_interval_mins = 10081),
        ),
        (
            "threshold percent 101",
            Box::new(|s| s.switch_threshold_percent = 101),
        ),
        (
            "threshold ms 10001",
            Box::new(|s| s.switch_threshold_ms = 10001),
        ),
        ("theme", Box::new(|s| s.theme = "sepia".into())),
        ("language", Box::new(|s| s.language = "xx".into())),
        ("language empty", Box::new(|s| s.language = String::new())),
    ];
    for (name, change) in cases {
        let mut s = Settings::default();
        change(&mut s);
        assert!(s.validate().is_err(), "{name} must be rejected");
    }
    let edge = Settings {
        concurrency: 3,
        interval_secs: 300,
        pool_size: 5,
        full_interval_mins: 30,
        degrade_percent: 300,
        degrade_cap_ms: 10000,
        endpoint_timeout_ms: 30000,
        proxy_timeout_ms: 100,
        subscription_interval_mins: 10080,
        switch_threshold_percent: 100,
        switch_threshold_ms: 10000,
        ..Settings::default()
    };
    assert!(edge.validate().is_ok(), "inclusive limits");
    let low = Settings {
        interval_secs: 50,
        pool_size: 1,
        full_interval_mins: 10,
        degrade_percent: 10,
        degrade_cap_ms: 200,
        ..Settings::default()
    };
    assert!(low.validate().is_ok(), "inclusive lower limits");
    let none = Settings {
        switch_threshold_percent: 0,
        switch_threshold_ms: 0,
        ..Settings::default()
    };
    assert!(none.validate().is_ok(), "zero margins mean any gain counts");
}
#[test]
fn available_servers_require_source_presence_not_a_subscription_toggle() {
    use rift_core::model::Subscription;
    let sub = |id: &str| Subscription {
        id: id.into(),
        name: id.into(),
        url: "https://example.com/s".into(),
        updated_at: None,
        error: None,
        failure_code: None,
        ..Default::default()
    };
    let store = Store {
        subscriptions: vec![sub("on"), sub("off")],
        servers: vec![
            parse_uri("trojan://p@example.com:1", Some("on".into())).unwrap(),
            parse_uri("trojan://p@example.com:2", Some("off".into())).unwrap(),
            parse_uri("trojan://p@example.com:3", None).unwrap(),
            parse_uri("trojan://p@example.com:4", Some("gone".into())).unwrap(),
        ],
        ..Store::default()
    };
    let ports: Vec<u16> = store.available_servers().iter().map(|s| s.port).collect();
    assert_eq!(ports, vec![1, 2, 3]);
}
#[test]
fn subscription_size_limit_is_exactly_four_mebibytes() {
    use rift_core::subscription::MAX_SUBSCRIPTION_BYTES;
    let head = format!("{URI}\n#");
    let exact = format!("{head}{}", "x".repeat(MAX_SUBSCRIPTION_BYTES - head.len()));
    assert_eq!(exact.len(), MAX_SUBSCRIPTION_BYTES);
    assert_eq!(parse_subscription(&exact, "sub").unwrap().servers.len(), 1);
    assert!(parse_subscription(&format!("{exact}x"), "sub").is_err());
}
#[test]
fn subscription_server_limit_is_two_thousand() {
    let body = |n: usize| {
        (1..=n)
            .map(|p| format!("trojan://p@example.com:{p}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(
        parse_subscription(&body(2000), "sub")
            .unwrap()
            .servers
            .len(),
        2000
    );
    assert!(parse_subscription(&body(2001), "sub").is_err());
}
#[test]
fn comments_and_blank_lines_are_not_rejected_rows() {
    let body = format!("# provider header\n\n   \n{URI}\n# footer\n");
    let parsed = parse_subscription(&body, "sub").unwrap();
    assert_eq!((parsed.servers.len(), parsed.rejected), (1, 0));
}
#[test]
fn merge_keeps_country_only_from_the_same_subscription() {
    let body = "trojan://p@example.com:1";
    let mut store = merge(
        &Store::default(),
        "other",
        parse_subscription(body, "other").unwrap(),
    );
    store.servers[0].country = "FI".into();
    let merged = merge(&store, "sub", parse_subscription(body, "sub").unwrap());
    let fresh = merged
        .servers
        .iter()
        .find(|s| s.source.as_deref() == Some("sub"))
        .unwrap();
    assert_eq!(
        fresh.country, "",
        "another subscription's country is not copied"
    );
}
#[test]
fn merge_drops_selection_of_a_removed_server_only() {
    use rift_core::model::Subscription;
    let mut store = Store::default();
    store.subscriptions.push(Subscription {
        id: "sub".into(),
        name: "sub".into(),
        url: "https://example.com/s".into(),
        updated_at: None,
        error: None,
        failure_code: None,
        ..Default::default()
    });
    let both = "trojan://p@example.com:1\ntrojan://p@example.com:2";
    store = merge(&store, "sub", parse_subscription(both, "sub").unwrap());
    store.selected = Some(store.servers[1].id.clone());
    let kept = merge(&store, "sub", parse_subscription(both, "sub").unwrap());
    assert_eq!(kept.selected, store.selected);
    let removed = merge(
        &store,
        "sub",
        parse_subscription("trojan://p@example.com:1", "sub").unwrap(),
    );
    assert_eq!(removed.selected, None);
}
fn subscribed(body: &str) -> Store {
    use rift_core::model::Subscription;
    let mut store = Store::default();
    store.subscriptions.push(Subscription {
        id: "sub".into(),
        name: "sub".into(),
        url: "https://example.com/s".into(),
        ..Default::default()
    });
    merge(&store, "sub", parse_subscription(body, "sub").unwrap())
}
#[test]
fn merge_moves_the_selection_to_the_server_that_kept_its_address() {
    // The provider gives every server other credentials: the ids are new, the addresses are not.
    let mut store = subscribed("trojan://old1@example.com:1\ntrojan://old2@example.com:2");
    store.selected = Some(store.servers[1].id.clone());
    let rotated = merge(
        &store,
        "sub",
        parse_subscription(
            "trojan://new1@example.com:1\ntrojan://new2@example.com:2",
            "sub",
        )
        .unwrap(),
    );
    let selected = rotated.selected.clone().expect("the selection moved");
    assert_ne!(Some(&selected), store.selected.as_ref(), "a new id");
    let server = rotated.servers.iter().find(|s| s.id == selected).unwrap();
    assert_eq!(server.port, 2, "the one at the same address, not the first");
    assert_eq!(server.source.as_deref(), Some("sub"));
}
#[test]
fn merge_takes_the_same_protocol_when_one_address_has_several_servers() {
    let vless = "vless://11111111-1111-4111-8111-111111111111@example.com:1#v";
    let mut store = subscribed(&format!("trojan://p@example.com:1\n{vless}"));
    let vless_id = store
        .servers
        .iter()
        .find(|s| s.protocol.starts_with("vless"))
        .unwrap()
        .id
        .clone();
    store.selected = Some(vless_id);
    // Credentials rotated; the trojan row comes first in the new list.
    let vless_new = "vless://22222222-2222-4222-8222-222222222222@example.com:1#v";
    let rotated = merge(
        &store,
        "sub",
        parse_subscription(&format!("trojan://q@example.com:1\n{vless_new}"), "sub").unwrap(),
    );
    let selected = rotated.selected.clone().unwrap();
    let server = rotated.servers.iter().find(|s| s.id == selected).unwrap();
    assert!(server.protocol.starts_with("vless"), "{}", server.protocol);
}
#[test]
fn merge_leaves_a_selection_of_another_source_alone() {
    let mut store = subscribed("trojan://p@example.com:1");
    let manual = parse_uri("trojan://m@manual.example.com:9", None).unwrap();
    store.selected = Some(manual.id.clone());
    store.servers.push(manual.clone());
    let merged = merge(
        &store,
        "sub",
        parse_subscription("trojan://q@example.com:1", "sub").unwrap(),
    );
    assert_eq!(merged.selected, Some(manual.id));
    // A server of another subscription at the same address is not its successor either.
    let mut other = subscribed("trojan://p@example.com:1");
    other.selected = Some(other.servers[0].id.clone());
    let merged = merge(
        &other,
        "elsewhere",
        parse_subscription("trojan://z@example.com:1", "elsewhere").unwrap(),
    );
    assert_eq!(merged.selected, other.selected, "the selection is of 'sub'");
}
#[test]
fn merge_keeps_nothing_selected_when_the_address_is_gone() {
    let mut store = subscribed("trojan://p@example.com:1\ntrojan://p@example.com:2");
    store.selected = Some(store.servers[1].id.clone());
    let merged = merge(
        &store,
        "sub",
        parse_subscription("trojan://p@example.com:1\ntrojan://p@example.com:3", "sub").unwrap(),
    );
    assert_eq!(
        merged.selected, None,
        "no blind pick of a server the user never chose"
    );
}
#[test]
fn merge_picks_the_first_row_of_the_address_when_the_protocol_is_gone_too() {
    let mut store = subscribed("trojan://p@example.com:1");
    store.selected = Some(store.servers[0].id.clone());
    let vless = "vless://22222222-2222-4222-8222-222222222222@example.com:1#v";
    let merged = merge(&store, "sub", parse_subscription(vless, "sub").unwrap());
    let selected = merged
        .selected
        .clone()
        .expect("the same address, another protocol");
    assert!(merged.servers.iter().any(|s| s.id == selected));
}
#[test]
fn subscription_url_must_be_plain_https() {
    use rift_core::subscription::validate_url;
    assert!(validate_url("https://provider.example/p").is_ok());
    for url in [
        "http://provider.example/p",
        "https://user@provider.example/p",
        "https://user:pass@provider.example/p",
        "https://:pass@provider.example/p",
        "provider.example/p",
    ] {
        assert!(validate_url(url).is_err(), "{url}");
    }
}
#[tokio::test]
async fn download_failure_is_an_error_not_empty_content() {
    use rift_core::subscription::{download, HttpSubscriptionSource, SubscriptionSource};
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let url = format!("https://127.0.0.1:{port}/sub");
    assert!(download(&url, None).await.is_err());
    assert!(HttpSubscriptionSource.fetch(&url, None).await.is_err());
    assert!(download("http://127.0.0.1/sub", None).await.is_err());
}
fn vmess_uri(fields: &str) -> String {
    let raw = format!(
        r#"{{"v":"2","add":"example.com","port":"443","id":"11111111-1111-4111-8111-111111111111"{fields}}}"#
    );
    format!("vmess://{}", URL_SAFE_NO_PAD.encode(raw))
}
const VLESS: &str = "vless://11111111-1111-4111-8111-111111111111@example.com:443";
const PBK: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
#[test]
fn each_incompatible_parameter_is_rejected_on_its_own() {
    let reality = format!("{VLESS}?security=reality&pbk={PBK}");
    for uri in [
        format!("{VLESS}?type=tcp&path=%2Fx"),
        format!("{VLESS}?type=tcp&host=cdn.example.com"),
        format!("{VLESS}?type=tcp&serviceName=svc"),
        format!("{VLESS}?type=ws&serviceName=svc"),
        format!("{VLESS}?type=grpc&path=%2Fx"),
        format!("{VLESS}?type=grpc&host=cdn.example.com"),
        format!("{VLESS}?type=quic"),
        format!("trojan://p@example.com:443?security=reality&pbk={PBK}"),
        format!("{VLESS}?security=reality"),
        format!("{reality}&sid=00112233445566778899"),
        format!("{reality}&sid=abc"),
        format!("{reality}&sid=zz"),
        format!("{VLESS}?security=tls&pbk={PBK}"),
        format!("{VLESS}?security=tls&sid=ab"),
        format!("{VLESS}?security=xtls"),
        format!("{VLESS}?sni=example.com"),
        format!("{VLESS}?fp=chrome"),
        format!("{VLESS}?pbk={PBK}"),
        format!("{VLESS}?sid=ab"),
        format!("{VLESS}?alpn=h2"),
        "ss://aes-256-gcm:pw@example.com:8388?type=ws".to_owned(),
        "ss://aes-256-gcm:pw@example.com:8388?security=tls".to_owned(),
        VLESS.replace(":443", ":0"),
        vmess_uri(r#","tls":"xtls""#),
        vmess_uri(r#","sni":"example.com""#),
        vmess_uri(r#","fp":"chrome""#),
        vmess_uri(r#","alpn":"h2""#),
        vmess_uri(r#","port":"0""#).replace("\"port\":\"443\",", ""),
    ] {
        assert!(parse_uri(&uri, None).is_err(), "{uri}");
    }
    let no_host = vmess_uri("").replace(
        &URL_SAFE_NO_PAD.encode(r#"{"v":"2","add":"example.com","port":"443","id":"11111111-1111-4111-8111-111111111111"}"#),
        &URL_SAFE_NO_PAD.encode(r#"{"v":"2","add":"","port":"443","id":"11111111-1111-4111-8111-111111111111"}"#),
    );
    assert!(parse_uri(&no_host, None).is_err(), "empty VMess host");
}
#[test]
fn accepted_boundaries_and_optional_fields() {
    let reality = format!("{VLESS}?security=reality&pbk={PBK}");
    assert!(parse_uri(&format!("{reality}&sid=0011223344556677"), None).is_ok());
    let ws = parse_uri(&format!("{VLESS}?type=ws&host=cdn.example.com"), None).unwrap();
    assert_eq!(
        ws.outbound["transport"]["headers"]["Host"],
        "cdn.example.com"
    );
    let bare_ws = parse_uri(&format!("{VLESS}?type=ws"), None).unwrap();
    assert!(bare_ws.outbound["transport"].get("headers").is_none());
    assert_eq!(bare_ws.outbound["transport"]["path"], "/");
    let tls = parse_uri(&format!("{VLESS}?security=tls"), None).unwrap();
    assert!(tls.outbound["tls"].get("alpn").is_none());
    let alpn = parse_uri(&format!("{VLESS}?security=tls&alpn=h2,http/1.1"), None).unwrap();
    assert_eq!(
        alpn.outbound["tls"]["alpn"],
        serde_json::json!(["h2", "http/1.1"])
    );
    let vmess_tls = parse_uri(&vmess_uri(r#","tls":"tls""#), None).unwrap();
    assert!(vmess_tls.outbound["tls"].get("alpn").is_none());
    let vmess_alpn = parse_uri(&vmess_uri(r#","tls":"tls","alpn":"h2""#), None).unwrap();
    assert_eq!(
        vmess_alpn.outbound["tls"]["alpn"],
        serde_json::json!(["h2"])
    );
    // Longest accepted URI: 16384 bytes including the name.
    let head = format!("{VLESS}#");
    let longest = format!("{head}{}", "n".repeat(16384 - head.len()));
    assert!(parse_uri(&longest, None).is_ok());
    assert!(parse_uri(&format!("{longest}n"), None).is_err());
}
#[test]
fn shadowsocks_2022_key_sizes_follow_the_method() {
    let key16 = "A".repeat(22);
    let key32 = "A".repeat(43);
    let ss = |method: &str, key: &str| format!("ss://{method}:{key}@example.com:8388");
    assert!(parse_uri(&ss("2022-blake3-aes-128-gcm", &key16), None).is_ok());
    assert!(parse_uri(&ss("2022-blake3-aes-128-gcm", &key32), None).is_err());
    assert!(parse_uri(&ss("2022-blake3-aes-256-gcm", &key32), None).is_ok());
    assert!(parse_uri(&ss("2022-blake3-aes-256-gcm", &key16), None).is_err());
}
#[test]
fn vless_flow_and_shadowsocks_credentials() {
    let plain = parse_uri(VLESS, None).unwrap();
    assert!(plain.outbound.get("flow").is_none());
    let vision = parse_uri(
        &format!("{VLESS}?security=reality&pbk={PBK}&flow=xtls-rprx-vision"),
        None,
    )
    .unwrap();
    assert_eq!(vision.outbound["flow"], "xtls-rprx-vision");
    let encoded = |credentials: &str| {
        format!(
            "ss://{}@example.com:8388",
            base64::engine::general_purpose::STANDARD.encode(credentials)
        )
    };
    assert!(parse_uri(&encoded("aes-256-gcm:secret"), None).is_ok());
    assert!(
        parse_uri(&encoded("aes-256-gcm:"), None).is_err(),
        "empty password"
    );
}

#[test]
fn host_longer_than_a_dns_name_is_rejected_at_ingestion() {
    // Four 63-character labels are 255 bytes: more than any resolvable name, and more than a
    // portable profile accepts, so it must never become a stored row.
    let host = vec!["a".repeat(63); 4].join(".");
    assert!(parse_uri(&format!("trojan://pw@{host}:443"), None).is_err());
    let ok = [
        "a".repeat(63),
        "a".repeat(63),
        "a".repeat(63),
        "a".repeat(61),
    ]
    .join(".");
    assert_eq!(ok.len(), 253);
    assert_eq!(
        parse_uri(&format!("trojan://pw@{ok}:443"), None)
            .unwrap()
            .host,
        ok
    );
}
#[test]
fn legacy_country_is_preserved_in_state_but_not_exposed_in_server_view() {
    let mut server = parse_uri(URI, None).unwrap();
    server.country = "DE".into();
    let bytes = serde_json::to_vec(&server).unwrap();
    let restored: rift_core::model::Server = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(restored.country, "DE");
    let view = serde_json::to_value(restored.summary()).unwrap();
    assert!(view.get("country").is_none());
    assert_eq!(view["name"], "Frankfurt");
}

#[test]
fn protocols_are_normalized_and_deduplicated_across_the_entire_filter() {
    let filter = rift_core::model::Filters {
        protocols: vec![
            "VLESS".into(),
            "trojan".into(),
            "vless".into(),
            " vmess ".into(),
            "TROJAN".into(),
        ],
        ..Default::default()
    }
    .normalized();
    assert_eq!(filter.protocols, ["vless", "trojan", "vmess"]);
    filter.validate().unwrap();
}
#[test]
fn stable_key_names_source_and_address_not_credentials() {
    let rotated = URI.replace(
        "11111111-1111-4111-8111-111111111111",
        "22222222-2222-4222-8222-222222222222",
    );
    let before = parse_uri(URI, Some("sub-a".into())).unwrap();
    let after = parse_uri(&rotated, Some("sub-a".into())).unwrap();
    // A provider that rotates credentials keeps the address: the key is the same, the id is not.
    assert_ne!(before.id, after.id);
    assert_eq!(before.stable_key(), after.stable_key());
    // The same address in another subscription, or added by hand, is another server.
    let other = parse_uri(URI, Some("sub-b".into())).unwrap();
    let manual = parse_uri(URI, None).unwrap();
    assert_ne!(before.stable_key(), other.stable_key());
    assert_ne!(before.stable_key(), manual.stable_key());
    assert_ne!(other.stable_key(), manual.stable_key());
    // Another port of the same host is another server.
    let moved = parse_uri(&URI.replace(":443?", ":8443?"), Some("sub-a".into())).unwrap();
    assert_ne!(before.stable_key(), moved.stable_key());
}
#[test]
fn stable_key_ignores_the_case_of_the_host() {
    let mut loud = parse_uri(URI, Some("sub-a".into())).unwrap();
    let quiet = loud.clone();
    loud.host = loud.host.to_uppercase();
    assert_eq!(loud.stable_key(), quiet.stable_key());
}
#[test]
fn stable_key_does_not_confuse_source_and_host() {
    // A source id and a host that together spell the same text still differ.
    let mut a = parse_uri(URI, Some("x".into())).unwrap();
    let mut b = parse_uri(URI, Some("x|example".into())).unwrap();
    a.host = "y.example".into();
    b.host = "com".into();
    assert_ne!(a.stable_key(), b.stable_key());
}
#[test]
fn auto_select_settings_start_at_what_the_owner_chose() {
    use rift_core::model::Settings;
    let s = Settings::default();
    assert_eq!(
        (
            s.pool_size,
            s.interval_secs,
            s.full_interval_mins,
            s.degrade_percent,
            s.degrade_cap_ms
        ),
        (3, 60, 10, 50, 1500)
    );
}
#[test]
fn files_of_earlier_versions_are_read_with_the_new_settings_at_their_defaults() {
    // A state written before the fast check existed: no pool, no full interval, no tolerance.
    let old = br#"{"version":1,"settings":{"interval_secs":120,"switch_threshold_percent":20},"servers":[],"subscriptions":[]}"#;
    let store = persistence::decode(old).unwrap();
    let s = &store.settings;
    assert_eq!(s.interval_secs, 120, "a value in range is kept");
    assert_eq!(s.switch_threshold_percent, 20);
    assert_eq!(
        (
            s.pool_size,
            s.full_interval_mins,
            s.degrade_percent,
            s.degrade_cap_ms
        ),
        (3, 10, 50, 1500)
    );
}
#[test]
fn an_interval_from_earlier_versions_is_brought_into_the_new_range() {
    // Up to a day was allowed, the fast check wants 50-300 s. The file is read, not refused.
    for (saved, read) in [
        (600, 300),
        (86400, 300),
        (301, 300),
        (300, 300),
        (60, 60),
        (50, 50),
        (10, 50),
        (0, 50),
    ] {
        let text = format!(
            r#"{{"version":1,"settings":{{"interval_secs":{saved}}},"servers":[],"subscriptions":[]}}"#
        );
        let store = persistence::decode(text.as_bytes()).unwrap();
        assert_eq!(store.settings.interval_secs, read, "saved {saved}");
    }
}
#[test]
fn a_profile_of_an_earlier_version_is_brought_into_the_new_ranges_too() {
    // The same rule as for the saved state: a profile exported when a day was allowed between checks
    // and 64 servers were measured at once is read, not refused.
    let bytes = {
        let mut ok = Store::default();
        ok.settings.interval_secs = 60;
        let mut file: serde_json::Value =
            serde_json::from_slice(&rift_core::profile::encode(&ok).unwrap()).unwrap();
        file["profile"]["settings"]["interval_secs"] = 86400.into();
        file["profile"]["settings"]["concurrency"] = 64.into();
        serde_json::to_vec(&file).unwrap()
    };
    let read = rift_core::profile::decode(&bytes).unwrap();
    assert_eq!(read.settings.interval_secs, 300);
    assert_eq!(read.settings.concurrency, 3);
}
#[test]
fn settings_built_in_rust_or_sent_by_the_interface_are_not_brought_into_range() {
    use rift_core::model::Settings;
    let wire: Settings =
        serde_json::from_str(r#"{"interval_secs":86400,"concurrency":64}"#).unwrap();
    assert_eq!((wire.interval_secs, wire.concurrency), (86400, 64));
    assert!(wire.validate().is_err(), "validate sees what was sent");
    let mut brought = wire;
    brought.bring_into_range();
    assert_eq!((brought.interval_secs, brought.concurrency), (300, 3));
    assert!(brought.validate().is_ok());
}

#[test]
fn routing_databases_are_updated_by_themselves_unless_the_user_says_otherwise() {
    use rift_core::model::Settings;
    assert!(Settings::default().geodata_auto_update);
    // A state written before the setting existed.
    let old = br#"{"version":1,"settings":{"interval_secs":60},"servers":[],"subscriptions":[]}"#;
    assert!(
        persistence::decode(old)
            .unwrap()
            .settings
            .geodata_auto_update
    );
    let off = br#"{"version":1,"settings":{"geodata_auto_update":false},"servers":[],"subscriptions":[]}"#;
    assert!(
        !persistence::decode(off)
            .unwrap()
            .settings
            .geodata_auto_update
    );
}
