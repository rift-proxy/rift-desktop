//! Options of a subscription (interval, User-Agent, filters), what the provider reports
//! (traffic and term) and how Auto Select treats them; migration of stored data.
use proptest::prelude::*;
use rift_core::{
    model::{Availability, Filters, Server, Store, Subscription, Usage},
    parser::parse_uri,
    persistence, profile,
    subscription::{title_from_values, usage_from_userinfo},
};

fn server(name: &str, uri_scheme_host: &str, source: Option<&str>) -> Server {
    let mut server =
        parse_uri(&format!("{uri_scheme_host}#{name}"), source.map(Into::into)).unwrap();
    server.name = name.into();
    server
}
fn trojan(name: &str, host: &str, source: Option<&str>) -> Server {
    server(name, &format!("trojan://xy@{host}:443"), source)
}
fn vless(name: &str, host: &str, source: Option<&str>) -> Server {
    server(
        name,
        &format!("vless://11111111-1111-4111-8111-111111111111@{host}:443?security=tls"),
        source,
    )
}
fn source(id: &str, usage: Option<Usage>) -> Subscription {
    Subscription {
        id: id.into(),
        name: id.into(),
        url: format!("https://{id}.example/s"),
        usage,
        ..Default::default()
    }
}
fn filters(include: &[&str], exclude: &[&str], protocols: &[&str]) -> Filters {
    let list = |items: &[&str]| items.iter().map(|item| item.to_string()).collect();
    Filters {
        include: list(include),
        exclude: list(exclude),
        protocols: list(protocols),
    }
}

const NOW: u64 = 1_800_000_000;
const GIB: u64 = 1 << 30;

#[test]
fn availability_follows_the_term_and_the_quota() {
    let usage = |used, total, expire| Usage {
        used,
        total,
        expire,
    };
    assert_eq!(usage(0, 0, None).availability(NOW), Availability::Ok);
    assert_eq!(
        usage(5 * GIB, 0, None).availability(NOW),
        Availability::Ok,
        "no quota stated"
    );
    assert_eq!(
        usage(GIB - 1, GIB, None).availability(NOW),
        Availability::Ok
    );
    assert_eq!(
        usage(GIB, GIB, None).availability(NOW),
        Availability::Exhausted
    );
    assert_eq!(
        usage(2 * GIB, GIB, None).availability(NOW),
        Availability::Exhausted
    );
    assert_eq!(
        usage(0, GIB, Some(NOW + 1)).availability(NOW),
        Availability::Ok
    );
    assert_eq!(
        usage(0, GIB, Some(NOW)).availability(NOW),
        Availability::Expired
    );
    assert_eq!(
        usage(0, GIB, Some(NOW - 1)).availability(NOW),
        Availability::Expired
    );
    assert_eq!(
        usage(GIB, GIB, Some(NOW - 1)).availability(NOW),
        Availability::Expired
    );
}

#[test]
fn auto_select_skips_the_servers_of_a_used_up_or_expired_subscription() {
    let store = Store {
        subscriptions: vec![
            source(
                "fine",
                Some(Usage {
                    used: 1,
                    total: GIB,
                    expire: Some(NOW + 100),
                }),
            ),
            source(
                "used",
                Some(Usage {
                    used: GIB,
                    total: GIB,
                    expire: None,
                }),
            ),
            source(
                "old",
                Some(Usage {
                    used: 0,
                    total: 0,
                    expire: Some(NOW - 1),
                }),
            ),
            source("silent", None),
        ],
        servers: vec![
            trojan("a", "198.51.100.1", Some("fine")),
            trojan("b", "198.51.100.2", Some("used")),
            trojan("c", "198.51.100.3", Some("old")),
            trojan("d", "198.51.100.4", Some("silent")),
            trojan("m", "198.51.100.5", None),
        ],
        ..Store::default()
    };
    let names: Vec<_> = store
        .auto_candidates(NOW)
        .into_iter()
        .map(|server| server.name)
        .collect();
    assert_eq!(names, ["a", "d", "m"]);
    // The skipped servers are still stored and can be picked by hand.
    assert_eq!(store.available_servers().len(), 5);
}

#[test]
fn a_server_is_a_candidate_while_any_copy_of_it_is() {
    let shared = ("198.51.100.9", "twin");
    let store = Store {
        subscriptions: vec![
            source(
                "used",
                Some(Usage {
                    used: GIB,
                    total: GIB,
                    expire: None,
                }),
            ),
            source("fine", None),
        ],
        servers: vec![
            trojan(shared.1, shared.0, Some("used")),
            trojan(shared.1, shared.0, Some("fine")),
            trojan("only-used", "198.51.100.10", Some("used")),
            // The same outbound added by hand as well.
            trojan("by-hand", "198.51.100.10", None),
        ],
        ..Store::default()
    };
    let hosts: Vec<_> = store
        .auto_candidates(NOW)
        .into_iter()
        .map(|server| server.host)
        .collect();
    assert_eq!(
        hosts,
        ["198.51.100.9", "198.51.100.10"],
        "one entry per server id"
    );
}

#[test]
fn nothing_is_left_when_every_source_is_used_up() {
    let store = Store {
        subscriptions: vec![source(
            "used",
            Some(Usage {
                used: 9,
                total: 9,
                expire: None,
            }),
        )],
        servers: vec![trojan("a", "198.51.100.1", Some("used"))],
        ..Store::default()
    };
    assert!(store.auto_candidates(NOW).is_empty());
}

#[test]
fn a_missing_subscription_never_offers_servers() {
    let store = Store {
        servers: vec![trojan("a", "198.51.100.1", Some("gone"))],
        ..Store::default()
    };
    assert!(store.auto_candidates(NOW).is_empty());
}

#[test]
fn expiry_is_judged_at_the_moment_of_the_cycle() {
    let store = Store {
        subscriptions: vec![source(
            "term",
            Some(Usage {
                used: 0,
                total: 0,
                expire: Some(NOW + 10),
            }),
        )],
        servers: vec![trojan("a", "198.51.100.1", Some("term"))],
        ..Store::default()
    };
    assert_eq!(store.auto_candidates(NOW).len(), 1);
    assert!(store.auto_candidates(NOW + 10).is_empty());
}

#[test]
fn filters_keep_by_name_and_protocol() {
    let frankfurt = trojan("Frankfurt 01", "198.51.100.1", None);
    let helsinki = vless("Helsinki 02 (video)", "198.51.100.2", None);
    let nothing = Filters::default();
    assert!(nothing.is_empty() && nothing.keeps(&frankfurt) && nothing.keeps(&helsinki));

    let include = filters(&["frankfurt", "oslo"], &[], &[]);
    assert!(
        include.keeps(&frankfurt) && !include.keeps(&helsinki),
        "case does not matter"
    );

    let exclude = filters(&[], &["VIDEO"], &[]);
    assert!(exclude.keeps(&frankfurt) && !exclude.keeps(&helsinki));

    let both = filters(&["01", "02"], &["helsinki"], &[]);
    assert!(
        both.keeps(&frankfurt) && !both.keeps(&helsinki),
        "exclude wins over include"
    );

    let protocol = filters(&[], &[], &["vless"]);
    assert!(!protocol.keeps(&frankfurt) && protocol.keeps(&helsinki));
    let several = filters(&[], &[], &["vless", "trojan"]);
    assert!(several.keeps(&frankfurt) && several.keeps(&helsinki));
}

#[test]
fn typed_filters_are_tidied_and_checked() {
    let tidy = filters(
        &["  DE ", "de", "", "NL"],
        &[" x ", "X"],
        &[" VLESS ", "vless"],
    )
    .normalized();
    assert_eq!(tidy.include, ["DE", "NL"]);
    assert_eq!(tidy.exclude, ["x"]);
    assert_eq!(tidy.protocols, ["vless"]);
    assert!(tidy.validate().is_ok());

    for (what, bad) in [
        ("too many entries", filters(&["a"; 51], &[], &[])),
        ("entry too long", filters(&[&"x".repeat(65)], &[], &[])),
        ("control character", filters(&["a\nb"], &[], &[])),
        ("blank entry", filters(&["  "], &[], &[])),
        ("unknown protocol", filters(&[], &[], &["socks"])),
    ] {
        assert!(bad.validate().is_err(), "{what}");
    }
    assert!(filters(&["a"; 50], &[], &[]).validate().is_ok());
    assert!(filters(&[&"x".repeat(64)], &[], &[]).validate().is_ok());
}

#[test]
fn subscription_options_are_checked() {
    let with = |change: &dyn Fn(&mut Subscription)| {
        let mut source = source("s", None);
        change(&mut source);
        source.validate_options()
    };
    assert!(with(&|_| {}).is_ok());
    assert!(with(&|s| s.interval_mins = Some(1)).is_ok());
    assert!(with(&|s| s.interval_mins = Some(10080)).is_ok());
    assert!(with(&|s| s.interval_mins = Some(0)).is_err());
    assert!(with(&|s| s.interval_mins = Some(10081)).is_err());
    assert!(with(&|s| s.user_agent = Some("v2rayN/7.1".into())).is_ok());
    assert!(with(&|s| s.user_agent = Some("x".repeat(256))).is_ok());
    for bad in [
        "",
        "a\r\nX-Injected: 1",
        "tab\there",
        "имя",
        "x\u{7f}",
        &"x".repeat(257),
    ] {
        assert!(
            with(&|s| s.user_agent = Some(bad.into())).is_err(),
            "user agent {bad:?}"
        );
    }
}

#[test]
fn provider_names_are_cleaned_and_cut() {
    assert_eq!(
        Subscription::clean_name("  My VPN \u{0}\n").as_deref(),
        Some("My VPN")
    );
    assert_eq!(Subscription::clean_name("\n\t "), None);
    assert_eq!(Subscription::clean_name(""), None);
    let long = "я".repeat(100); // 200 bytes
    let cut = Subscription::clean_name(&long).unwrap();
    assert_eq!(cut.len(), 120, "cut on a character boundary");
    assert!(cut.chars().all(|c| c == 'я'));
    let emoji = format!("{}😀", "a".repeat(119)); // the emoji would end at byte 123
    assert_eq!(Subscription::clean_name(&emoji).unwrap(), "a".repeat(119));
}

const OLD_STATE: &str = r#"{
  "version": 1,
  "settings": {"mode": "proxy", "theme": "dark", "auto_select": true},
  "servers": [],
  "subscriptions": [
    {"id": "old", "name": "Старая", "url": "https://old.example/s", "enabled": true,
     "updated_at": 1789742400, "error": null}
  ],
  "selected": null
}"#;

#[test]
fn a_state_file_from_before_the_options_loads_with_defaults() {
    let store = persistence::decode(OLD_STATE.as_bytes()).unwrap();
    let source = &store.subscriptions[0];
    assert_eq!(source.name, "Старая");
    assert_eq!(source.interval_mins, None, "the global interval applies");
    assert_eq!(source.user_agent, None, "Rift's own User-Agent applies");
    assert!(source.filters.is_empty(), "nothing is filtered");
    assert!(source.usage.is_none());
    assert_eq!(store.auto_candidates(NOW).len(), 0);
}

#[test]
fn options_survive_a_save_and_a_profile_round_trip() {
    let mut store = Store::default();
    let mut s = source(
        "kept",
        Some(Usage {
            used: 7,
            total: 9,
            expire: Some(NOW),
        }),
    );
    s.interval_mins = Some(15);
    s.user_agent = Some("Happ/1.0".into());
    s.filters = filters(&["de"], &["test"], &["vless"]);
    store.subscriptions = vec![s];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    persistence::save(&path, &store).unwrap();
    let loaded = persistence::load(&path).unwrap();
    let kept = &loaded.subscriptions[0];
    assert_eq!(kept.interval_mins, Some(15));
    assert_eq!(kept.user_agent.as_deref(), Some("Happ/1.0"));
    assert_eq!(kept.filters, filters(&["de"], &["test"], &["vless"]));
    assert_eq!(
        kept.usage,
        Some(Usage {
            used: 7,
            total: 9,
            expire: Some(NOW)
        })
    );

    let bytes = profile::encode(&store).unwrap();
    let decoded = profile::decode(&bytes).unwrap();
    assert_eq!(decoded.subscriptions[0].interval_mins, Some(15));
}

#[test]
fn damaged_options_stop_the_load_instead_of_being_used() {
    let mut store = Store::default();
    let mut s = source("bad", None);
    s.user_agent = Some("a\r\nb".into());
    store.subscriptions = vec![s];
    let json = serde_json::to_vec(&store).unwrap();
    assert!(persistence::decode(&json).is_err());
    assert!(profile::validate(&store).is_err());
}

proptest! {
    #[test]
    fn any_header_text_is_handled_without_panicking(
        title in proptest::collection::vec(any::<u8>(), 0..2100),
        disposition in proptest::collection::vec(any::<u8>(), 0..2100),
        info in ".{0,1500}",
    ) {
        if let Some(name) = title_from_values(Some(&title), Some(&disposition)) {
            // Whatever the provider sends, what is stored from it is a short clean line.
            if let Some(clean) = Subscription::clean_name(&name) {
                prop_assert!(clean.len() <= 120 && !clean.chars().any(char::is_control));
            }
        }
        if let Some(usage) = usage_from_userinfo(&info) {
            prop_assert!(usage.expire != Some(0));
        }
    }

    #[test]
    fn userinfo_with_numbers_is_read_back_exactly(
        upload in 0u64..u64::MAX / 2,
        download in 0u64..u64::MAX / 2,
        total in any::<u64>(),
        expire in 1u64..u64::MAX,
    ) {
        let header = format!("upload={upload}; download={download}; total={total}; expire={expire}");
        let usage = usage_from_userinfo(&header).unwrap();
        prop_assert_eq!(usage.used, upload + download);
        prop_assert_eq!(usage.total, total);
        prop_assert_eq!(usage.expire, Some(expire));
    }
}
