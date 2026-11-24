//! Subscriptions in the Xray JSON format: which outbounds become servers (the same ones a link
//! of the server gives, down to the id), how they are named, what is refused, and the rules a
//! profile routes by. The bodies are built here in the shape a panel answers; the keys, addresses
//! and ids in them are made up.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use proptest::prelude::*;
use rift_core::{
    model::{Filters, Server},
    parser::parse_uri,
    subscription::{apply_filters, parse_subscription, parse_subscription_with},
    xray,
};
use serde_json::{json, Value};

const ID: &str = "11111111-2222-3333-4444-555555555555";
/// 32 bytes of zeros, as Reality writes a public key.
const KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
/// The same for Shadowsocks 2022.
const SS_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

fn tcp_reality(host: &str, port: u16) -> Value {
    json!({
        "tag": format!("proxy-{host}"),
        "protocol": "vless",
        "settings": {"vnext": [{"address": host, "port": port, "users": [
            {"id": ID, "encryption": "none", "flow": "xtls-rprx-vision"}]}]},
        "streamSettings": {"network": "tcp", "tcpSettings": {}, "security": "reality",
            "realitySettings": {"serverName": "front.example", "publicKey": KEY,
                "shortId": "abcd1234", "fingerprint": "firefox"}},
    })
}
fn ws_tls(host: &str) -> Value {
    json!({
        "tag": "proxy-ws",
        "protocol": "vless",
        "settings": {"vnext": [{"address": host, "port": 443, "users": [
            {"id": ID, "encryption": "none", "flow": ""}]}]},
        "streamSettings": {"network": "ws",
            "wsSettings": {"path": "/feed/x", "headers": {"Host": "site.example"}},
            "security": "tls",
            "tlsSettings": {"serverName": "site.example", "fingerprint": "edge",
                "alpn": ["http/1.1"]}},
        "mux": {"enabled": true, "concurrency": 8, "xudpConcurrency": 16},
    })
}
fn xhttp(host: &str) -> Value {
    json!({
        "tag": "proxy-xhttp",
        "protocol": "vless",
        "settings": {"vnext": [{"address": host, "port": 443, "users": [
            {"id": ID, "encryption": "none", "flow": ""}]}]},
        "streamSettings": {"network": "xhttp",
            "xhttpSettings": {"mode": "stream-one", "path": "/x"},
            "security": "tls", "tlsSettings": {"serverName": "site.example"}},
    })
}
fn hysteria(host: &str) -> Value {
    json!({
        "tag": "proxy",
        "protocol": "hysteria",
        "settings": {"address": host, "port": 8449, "version": 2},
        "streamSettings": {"network": "hysteria",
            "hysteriaSettings": {"version": 2, "auth": "pw"}, "security": "tls",
            "tlsSettings": {"serverName": "site.example", "alpn": ["h3"]}},
    })
}
fn shadowsocks(host: &str) -> Value {
    json!({
        "tag": "proxy",
        "protocol": "shadowsocks",
        "settings": {"servers": [{"address": host, "port": 2030, "password": SS_KEY,
            "method": "2022-blake3-aes-256-gcm", "uot": false, "UoTVersion": 1}]},
        "streamSettings": {"network": "tcp", "tcpSettings": {}, "security": "none"},
    })
}
/// A profile as the panel writes it: the servers, then the direct and the blocking outbound,
/// and the rules a provider sends.
#[allow(clippy::needless_pass_by_value)]
fn profile(remarks: &str, servers: Vec<Value>, rules: Vec<Value>) -> Value {
    let mut outbounds = servers;
    outbounds.push(json!({"tag": "direct", "protocol": "freedom"}));
    outbounds.push(json!({"tag": "block", "protocol": "blackhole"}));
    json!({
        "remarks": remarks,
        "dns": {"servers": ["https://8.8.8.8/dns-query"]},
        "inbounds": [{"listen": "127.0.0.1", "port": 10808, "protocol": "socks", "tag": "socks"}],
        "outbounds": outbounds,
        "routing": {"domainStrategy": "IPIfNonMatch", "rules": rules},
    })
}
fn provider_rules() -> Vec<Value> {
    vec![
        json!({"type": "field", "network": "udp", "port": "443", "outboundTag": "block"}),
        json!({"type": "field", "outboundTag": "direct",
            "domain": ["domain:ru", "geosite:category-ru", "domain:vk.example"]}),
        json!({"type": "field", "protocol": ["bittorrent"], "outboundTag": "direct"}),
    ]
}
fn parse(body: &Value) -> rift_core::subscription::ParsedSubscription {
    parse_subscription(&body.to_string(), "source").unwrap()
}
fn by_name<'a>(servers: &'a [Server], name: &str) -> &'a Server {
    servers
        .iter()
        .find(|server| server.name == name)
        .unwrap_or_else(|| panic!("no server {name}: {:?}", names(servers)))
}
fn names(servers: &[Server]) -> Vec<&str> {
    servers.iter().map(|server| server.name.as_str()).collect()
}

// ---- which bodies are Xray JSON -----------------------------------------------------------

#[test]
fn a_byte_order_mark_in_front_of_the_body_does_not_hide_it() {
    let body = json!([profile("Mark", vec![tcp_reality("a.example", 443)], vec![])]).to_string();
    let marked = format!("\u{feff}{body}");
    assert!(xray::looks_like(&marked));
    assert_eq!(
        parse_subscription(&marked, "s").unwrap().servers[0].name,
        "Mark"
    );
}

#[test]
fn a_body_is_xray_json_when_an_array_or_an_object_begins_it() {
    for body in ["[]", " \n\t[{}]", "{}", "[1]"] {
        assert!(xray::looks_like(body), "{body:?}");
    }
    for body in [
        "vless://a@b:1",
        "dmxlc3M6Ly9hQGI6MQ==",
        "",
        "# comment\n[x]",
        "\"[\"",
    ] {
        assert!(!xray::looks_like(body), "{body:?}");
    }
}

// ---- servers ----------------------------------------------------------------------------------

#[test]
fn an_outbound_is_the_server_a_link_of_it_would_be() {
    let body = json!([profile(
        "Reality",
        vec![tcp_reality("a.example", 8443)],
        vec![]
    )]);
    let server = parse(&body).servers.remove(0);
    let link = format!(
        "vless://{ID}@a.example:8443?security=reality&sni=front.example&fp=firefox&pbk={KEY}&sid=abcd1234&flow=xtls-rprx-vision&type=tcp#Reality"
    );
    let expected = parse_uri(&link, Some("source".into())).unwrap();
    assert_eq!(server.id, expected.id);
    assert_eq!(server.outbound, expected.outbound);
    assert_eq!(server.name, "Reality");
    assert_eq!(server.protocol, "vless reality");
    assert_eq!((server.host.as_str(), server.port), ("a.example", 8443));
    assert_eq!(server.source.as_deref(), Some("source"));

    // Over WebSocket with TLS; the `mux` of the profile is not carried over, and an empty flow is none.
    let server = parse(&json!([profile("Web", vec![ws_tls("b.example")], vec![])]))
        .servers
        .remove(0);
    let link = format!(
        "vless://{ID}@b.example:443?security=tls&sni=site.example&fp=edge&alpn=http%2F1.1&type=ws&host=site.example&path=%2Ffeed%2Fx#Web"
    );
    assert_eq!(server.id, parse_uri(&link, None).unwrap().id);

    // Shadowsocks 2022.
    let server = parse(&json!([profile(
        "SS",
        vec![shadowsocks("c.example")],
        vec![]
    )]))
    .servers
    .remove(0);
    let user = URL_SAFE_NO_PAD.encode(format!("2022-blake3-aes-256-gcm:{SS_KEY}"));
    let link = format!("ss://{user}@c.example:2030#SS");
    assert_eq!(server.id, parse_uri(&link, None).unwrap().id);
}

#[test]
fn trojan_vmess_and_grpc_are_read_like_their_links() {
    let trojan = json!({
        "protocol": "trojan",
        "settings": {"servers": [{"address": "t.example", "port": 443, "password": "secret"}]},
        "streamSettings": {"network": "tcp", "security": "tls",
            "tlsSettings": {"serverName": "front.example"}},
    });
    let server = parse(&json!([profile("T", vec![trojan], vec![])]))
        .servers
        .remove(0);
    let link = "trojan://secret@t.example:443?sni=front.example";
    assert_eq!(server.id, parse_uri(link, None).unwrap().id);

    let grpc = json!({
        "protocol": "vless",
        "settings": {"vnext": [{"address": "g.example", "port": 443,
            "users": [{"id": ID, "encryption": "none"}]}]},
        "streamSettings": {"network": "grpc", "grpcSettings": {"serviceName": "svc"},
            "security": "tls", "tlsSettings": {"serverName": "front.example"}},
    });
    let server = parse(&json!([profile("G", vec![grpc], vec![])]))
        .servers
        .remove(0);
    let link = format!(
        "vless://{ID}@g.example:443?security=tls&sni=front.example&type=grpc&serviceName=svc"
    );
    assert_eq!(server.id, parse_uri(&link, None).unwrap().id);

    let vmess = json!({
        "protocol": "vmess",
        "settings": {"vnext": [{"address": "v.example", "port": 443,
            "users": [{"id": ID, "alterId": 0, "security": "auto"}]}]},
        "streamSettings": {"network": "ws", "wsSettings": {"path": "/v"},
            "security": "tls", "tlsSettings": {"serverName": "v.example"}},
    });
    let server = parse(&json!([profile("V", vec![vmess], vec![])]))
        .servers
        .remove(0);
    assert_eq!(
        server.outbound,
        json!({"type": "vmess", "server": "v.example", "server_port": 443, "uuid": ID,
            "alter_id": 0, "security": "auto",
            "transport": {"type": "ws", "path": "/v"},
            "tls": {"enabled": true, "server_name": "v.example"}})
    );
}

#[test]
fn the_flat_form_of_newer_xray_is_the_same_server() {
    let flat = json!({
        "protocol": "vless",
        "settings": {"address": "a.example", "port": 8443, "id": ID, "encryption": "none",
            "flow": "xtls-rprx-vision"},
        "streamSettings": tcp_reality("a.example", 8443)["streamSettings"].clone(),
    });
    let nested = tcp_reality("a.example", 8443);
    let a = parse(&json!([profile("A", vec![flat], vec![])]))
        .servers
        .remove(0);
    let b = parse(&json!([profile("A", vec![nested], vec![])]))
        .servers
        .remove(0);
    assert_eq!(a.id, b.id);
}

#[test]
fn a_single_profile_object_is_read_as_well_as_an_array() {
    let one = profile("One", vec![tcp_reality("a.example", 443)], vec![]);
    assert_eq!(parse(&one).servers.len(), 1);
    assert_eq!(parse(&json!([one])).servers.len(), 1);
}

// ---- what is refused --------------------------------------------------------------------------

#[test]
fn what_cannot_be_a_server_is_counted_and_the_rest_is_kept() {
    let good = tcp_reality("good.example", 443);
    let mut insecure = ws_tls("insecure.example");
    insecure["streamSettings"]["tlsSettings"]["allowInsecure"] = json!(true);
    let mut unknown_key = ws_tls("key.example");
    unknown_key["streamSettings"]["sockopt"] = json!({"dialerProxy": "other"});
    let mut two_servers = tcp_reality("two.example", 443);
    two_servers["settings"]["vnext"]
        .as_array_mut()
        .unwrap()
        .push(json!({"address": "other.example", "port": 1, "users": [{"id": ID}]}));
    let mut encrypted = tcp_reality("enc.example", 443);
    encrypted["settings"]["vnext"][0]["users"][0]["encryption"] =
        json!("mlkem768x25519plus.native.0rtt.key");
    let mut fingerprint = tcp_reality("fp.example", 443);
    fingerprint["streamSettings"]["realitySettings"]["fingerprint"] = json!("curl");
    let mut uot = shadowsocks("uot.example");
    uot["settings"]["servers"][0]["uot"] = json!(true);
    let mut header = tcp_reality("header.example", 443);
    header["streamSettings"]["tcpSettings"] = json!({"header": {"type": "http"}});
    let mut bad_key = tcp_reality("badkey.example", 443);
    bad_key["streamSettings"]["realitySettings"]["publicKey"] = json!("short");
    let mut extra = tcp_reality("extra.example", 443);
    extra["sendThrough"] = json!("1.2.3.4");
    let mut port = tcp_reality("port.example", 443);
    port["settings"]["vnext"][0]["port"] = json!(0);
    let rejected = vec![
        hysteria("hy.example"),
        insecure,
        unknown_key,
        two_servers,
        encrypted,
        fingerprint,
        uot,
        header,
        bad_key,
        extra,
        port,
        json!({"protocol": "socks", "settings": {}}),
    ];
    let count = rejected.len();
    let mut outbounds = rejected;
    outbounds.insert(0, good);
    let parsed = parse(&json!([profile("Mixed", outbounds, vec![])]));
    // Only the good one stays, whatever the others were.
    assert_eq!(names(&parsed.servers), ["Mixed"]);
    assert_eq!(parsed.servers[0].host, "good.example");
    assert_eq!(parsed.rejected, count);
}

#[test]
fn malformed_users_are_invalid_fields_not_unsupported_configurations() {
    use rift_core::diagnostics::Rejection;

    for protocol in ["vless", "vmess"] {
        for (users, why) in [
            (json!({}), Rejection::InvalidField),
            (json!("synthetic"), Rejection::InvalidField),
            (json!(true), Rejection::InvalidField),
            (json!(42), Rejection::InvalidField),
            (Value::Null, Rejection::InvalidField),
            (json!([false]), Rejection::InvalidField),
            (json!([{"id": ID}, {"id": ID}]), Rejection::UnsupportedParam),
        ] {
            let mut bad = tcp_reality("bad.example", 443);
            bad["protocol"] = json!(protocol);
            bad["settings"]["vnext"][0]["users"] = users;
            let parsed = parse(&json!({"outbounds": [tcp_reality("good.example", 443), bad]}));
            assert_eq!(parsed.servers.len(), 1);
            assert_eq!(parsed.servers[0].host, "good.example");
            assert_eq!(parsed.rejected, 1);
            assert_eq!(parsed.rows.rejected(), 1);
            assert_eq!(parsed.rows.rejected_by[why as usize], 1, "{protocol}");
        }
    }
}

#[test]
fn empty_server_lists_are_missing_fields_but_multiple_servers_remain_unsupported() {
    use rift_core::diagnostics::Rejection;

    for (protocol, list) in [
        ("vless", "vnext"),
        ("vmess", "vnext"),
        ("trojan", "servers"),
        ("shadowsocks", "servers"),
    ] {
        for (entries, why) in [
            (json!([]), Rejection::MissingField),
            (json!([{}, {}]), Rejection::UnsupportedParam),
            (json!("synthetic"), Rejection::InvalidField),
            (json!([false]), Rejection::InvalidField),
        ] {
            let mut bad = json!({"protocol": protocol, "settings": {}});
            bad["settings"][list] = entries;
            let parsed = parse(&json!({"outbounds": [tcp_reality("good.example", 443), bad]}));
            assert_eq!(parsed.servers.len(), 1);
            assert_eq!(parsed.servers[0].host, "good.example");
            assert_eq!(parsed.rejected, 1);
            assert_eq!(parsed.rows.rejected(), 1);
            assert_eq!(parsed.rows.rejected_by[why as usize], 1, "{protocol}");
        }
    }
}

#[test]
fn an_outbound_that_many_profiles_repeat_is_rejected_once() {
    let profiles: Vec<Value> = (0..5)
        .map(|n| {
            profile(
                &format!("P{n}"),
                vec![
                    tcp_reality(&format!("ok{n}.example"), 443),
                    hysteria("same.example"),
                ],
                vec![],
            )
        })
        .collect();
    let parsed = parse(&Value::Array(profiles));
    assert_eq!(parsed.servers.len(), 5);
    assert_eq!(parsed.rejected, 1);
}

#[test]
fn a_body_with_no_server_in_it_is_an_error() {
    for body in [
        "[]",
        "{}",
        r#"[{"remarks":"x"}]"#,
        r#"[{"outbounds":[]}]"#,
        "[1, 2]",
    ] {
        let error = parse_subscription(body, "s")
            .err()
            .expect("invalid body accepted");
        assert_eq!(
            error.failure_code(),
            Some(rift_core::failure::FailureCode::SubscriptionInvalidBody),
            "{body}"
        );
    }
    let only_hysteria = json!([profile("H", vec![hysteria("hy.example")], vec![])]);
    assert!(parse_subscription(&only_hysteria.to_string(), "s").is_err());
    for body in ["[", "[1,", "{\"a\":", "[] x", "{\"a\" 1}"] {
        let error = parse_subscription(body, "s")
            .err()
            .expect("invalid JSON accepted");
        assert_eq!(
            error.failure_code(),
            Some(rift_core::failure::FailureCode::SubscriptionInvalidBody),
            "{body}"
        );
    }
}

#[test]
fn there_is_a_limit_to_the_servers() {
    let many = |count: usize| -> Value {
        Value::Array(
            (0..count)
                .map(|n| {
                    profile(
                        &format!("S{n}"),
                        vec![tcp_reality(&format!("h{n}.example"), 443)],
                        vec![],
                    )
                })
                .collect(),
        )
    };
    assert_eq!(parse(&many(2000)).servers.len(), 2000);
    assert!(parse_subscription(&many(2001).to_string(), "s").is_err());
}

// ---- names and copies -------------------------------------------------------------------------

#[test]
fn a_server_is_named_by_its_own_profile_not_by_the_auto_one_that_lists_it() {
    // The "auto" profile comes first in the array and lists every server again.
    let auto = profile(
        "Auto",
        vec![
            tcp_reality("a.example", 443),
            tcp_reality("b.example", 443),
            tcp_reality("c.example", 443),
        ],
        vec![],
    );
    let body = json!([
        auto,
        profile("Finland", vec![tcp_reality("a.example", 443)], vec![]),
        profile("Sweden", vec![tcp_reality("b.example", 443)], vec![]),
    ]);
    let parsed = parse(&body);
    // "c" is only in the big profile: it takes its remark.
    let mut seen = names(&parsed.servers);
    seen.sort_unstable();
    assert_eq!(seen, ["Auto", "Finland", "Sweden"]);
    assert_eq!(by_name(&parsed.servers, "Finland").host, "a.example");
    assert_eq!(by_name(&parsed.servers, "Sweden").host, "b.example");
    assert_eq!(by_name(&parsed.servers, "Auto").host, "c.example");
}

#[test]
fn a_malformed_row_does_not_steal_the_individual_profiles_name_or_routes() {
    let body = json!([
        profile(
            "Auto",
            vec![tcp_reality("a.example", 443), tcp_reality("b.example", 443)],
            vec![],
        ),
        profile(
            "Individual",
            vec![tcp_reality("a.example", 443), json!({"settings": {}})],
            provider_rules(),
        ),
    ]);
    let parsed = parse(&body);
    let individual = by_name(&parsed.servers, "Individual");
    assert_eq!(individual.host, "a.example");
    assert_eq!(individual.routes.as_ref().unwrap().block, ["udp:443"]);
    assert_eq!(by_name(&parsed.servers, "Auto").host, "b.example");
    assert_eq!(parsed.rejected, 1);
}

#[test]
fn an_outbounds_field_with_the_wrong_shape_is_not_reported_as_missing() {
    for outbounds in [json!(null), json!({}), json!("not an array"), json!(42)] {
        let error = parse_subscription(&json!({"outbounds": outbounds}).to_string(), "s")
            .err()
            .unwrap();
        let mut cause: &dyn std::error::Error = &error;
        loop {
            if let Some(rift_core::Error::Rejected(reason)) =
                cause.downcast_ref::<rift_core::Error>()
            {
                assert_eq!(*reason, rift_core::diagnostics::Rejection::InvalidField);
                break;
            }
            cause = cause.source().expect("typed rejection cause");
        }
        assert!(error.diagnostic_details().contains("outbounds"));
    }
}

#[test]
fn what_a_bigger_profile_adds_takes_its_remark_numbered_when_there_are_several() {
    let body = json!([
        profile("Plain", vec![tcp_reality("a.example", 443)], vec![]),
        // A decoy that is the plain server, and the server the profile is for.
        profile(
            "Whitelist",
            vec![tcp_reality("a.example", 443), ws_tls("w.example")],
            vec![]
        ),
        profile(
            "Pair",
            vec![
                tcp_reality("p1.example", 443),
                tcp_reality("p2.example", 443)
            ],
            vec![]
        ),
        profile("", vec![shadowsocks("anon.example")], vec![]),
    ]);
    let parsed = parse(&body);
    assert_eq!(
        names(&parsed.servers),
        ["Plain", "Whitelist", "Pair", "Pair 2", "anon.example"]
    );
    assert_eq!(by_name(&parsed.servers, "Whitelist").host, "w.example");
}

#[test]
fn the_servers_keep_the_order_of_the_body() {
    let body = json!([
        profile(
            "Big",
            vec![tcp_reality("1.example", 443), tcp_reality("2.example", 443)],
            vec![]
        ),
        profile("Single", vec![tcp_reality("3.example", 443)], vec![]),
    ]);
    let hosts: Vec<String> = parse(&body).servers.into_iter().map(|s| s.host).collect();
    assert_eq!(hosts, ["1.example", "2.example", "3.example"]);
}

#[test]
fn filters_see_the_servers_like_any_others() {
    let body = json!([
        profile("A", vec![tcp_reality("a.example", 443)], vec![]),
        profile("B", vec![shadowsocks("b.example")], vec![]),
    ]);
    let filters = Filters {
        protocols: vec!["shadowsocks".into()],
        ..Filters::default()
    };
    let kept = apply_filters(parse(&body), &filters).unwrap();
    assert_eq!(names(&kept.servers), ["B"]);
    assert_eq!(kept.filtered, 1);
}

// ---- the rules of a profile -----------------------------------------------------------------

fn routes_of(rules: Vec<Value>) -> Option<rift_core::routing::ServerRoutes> {
    parse(&json!([profile(
        "P",
        vec![tcp_reality("a.example", 443)],
        rules
    )]))
    .servers
    .remove(0)
    .routes
}

#[test]
fn the_rules_of_a_profile_become_the_rules_of_its_servers() {
    let routes = routes_of(provider_rules()).unwrap();
    assert_eq!(routes.block, ["udp:443"]);
    assert_eq!(
        routes.direct,
        [
            "domain:ru",
            "geosite:category-ru",
            "domain:vk.example",
            "protocol:bittorrent"
        ]
    );
    // And nothing of them is left to read wrongly: every entry is one the user could type.
    routes.validate().unwrap();
}

#[test]
fn without_the_switch_a_server_comes_without_rules() {
    let body = json!([profile(
        "P",
        vec![tcp_reality("a.example", 443)],
        provider_rules()
    )])
    .to_string();
    assert!(
        parse_subscription_with(&body, "s", false).unwrap().servers[0]
            .routes
            .is_none()
    );
    assert!(
        parse_subscription_with(&body, "s", true).unwrap().servers[0]
            .routes
            .is_some()
    );
    // The default is on.
    assert!(parse_subscription(&body, "s").unwrap().servers[0]
        .routes
        .is_some());
}

#[test]
fn a_profile_without_rules_gives_none() {
    assert_eq!(routes_of(vec![]), None);
    // A rule that sends to the proxy is not a rule of the server's.
    assert_eq!(
        routes_of(vec![
            json!({"type": "field", "network": "tcp,udp", "outboundTag": "proxy"})
        ]),
        None
    );
}

#[test]
fn ports_and_transports_are_written_the_way_the_user_would() {
    let routes = routes_of(vec![
        json!({"type": "field", "network": "tcp,udp", "port": "53", "outboundTag": "block"}),
        json!({"type": "field", "network": "tcp", "port": "80,1000-2000", "outboundTag": "block"}),
        json!({"type": "field", "port": 25, "outboundTag": "block"}),
        json!({"type": "field", "network": "udp", "port": "443", "outboundTag": "block"}),
        // Twice is once; so is the same in other letters.
        json!({"type": "field", "network": "UDP", "port": "443", "outboundTag": "block"}),
    ])
    .unwrap();
    assert_eq!(
        routes.block,
        ["port:53", "tcp:80", "tcp:1000-2000", "port:25", "udp:443"]
    );
}

#[test]
fn domains_are_read_like_xray_reads_them() {
    let routes = routes_of(vec![json!({"type": "field", "outboundTag": "direct", "domain": [
        "domain:example.com", "full:www.example.com", "keyword:ads", "regexp:^ad[0-9]+\\.",
        "geosite:google", "bare-word", "ext:file.dat:tag", "geosite:google@cn", "domain:not a domain"
    ]})])
    .unwrap();
    assert_eq!(
        routes.direct,
        [
            "domain:example.com",
            "full:www.example.com",
            "keyword:ads",
            "regexp:^ad[0-9]+\\.",
            "geosite:google",
            // A word with no prefix is a part of a name in Xray.
            "keyword:bare-word"
        ]
    );
    let ips = routes_of(vec![json!({"type": "field", "outboundTag": "direct",
        "ip": ["geoip:private", "10.0.0.0/8", "fc00::/7", "geoip:!ru", "ext:x.dat:y"]})])
    .unwrap();
    assert_eq!(ips.direct, ["geoip:private", "10.0.0.0/8", "fc00::/7"]);
}

#[test]
fn rules_that_say_more_than_the_entries_can_are_left_out() {
    let routes = routes_of(vec![
        // Two kinds of condition in one rule: a domain and a port.
        json!({"type": "field", "outboundTag": "direct", "domain": ["domain:a.example"], "port": "443"}),
        json!({"type": "field", "outboundTag": "direct", "inboundTag": ["socks"], "domain": ["domain:b.example"]}),
        json!({"type": "field", "outboundTag": "direct", "source": ["10.0.0.0/8"]}),
        json!({"type": "field", "outboundTag": "direct", "user": ["x@y"]}),
        json!({"type": "field", "outboundTag": "direct", "attrs": {"a": "b"}, "domain": ["domain:c.example"]}),
        // A tag that is no outbound of the profile.
        json!({"type": "field", "outboundTag": "nowhere", "domain": ["domain:d.example"]}),
        // Another kind of rule.
        json!({"type": "chain", "outboundTag": "direct", "domain": ["domain:e.example"]}),
        // The one that stays.
        json!({"type": "field", "outboundTag": "direct", "domain": ["domain:kept.example"]}),
    ])
    .unwrap();
    assert_eq!(routes.direct, ["domain:kept.example"]);
    assert!(routes.block.is_empty());
}

#[test]
fn the_outbound_a_rule_names_decides_not_the_name_of_its_tag() {
    let mut body = profile(
        "P",
        vec![tcp_reality("a.example", 443)],
        vec![
            json!({"type": "field", "outboundTag": "out-free", "domain": ["domain:free.example"]}),
            json!({"type": "field", "outboundTag": "out-hole", "domain": ["domain:hole.example"]}),
            json!({"type": "field", "outboundTag": "direct", "domain": ["domain:renamed.example"]}),
        ],
    );
    body["outbounds"] = json!([
        tcp_reality("a.example", 443),
        {"tag": "out-free", "protocol": "freedom"},
        {"tag": "out-hole", "protocol": "blackhole"},
        // The proxy that is called "direct": it is no direct outbound.
        {"tag": "direct", "protocol": "dns"},
    ]);
    let routes = parse(&json!([body])).servers.remove(0).routes.unwrap();
    assert_eq!(routes.direct, ["domain:free.example"]);
    assert_eq!(routes.block, ["domain:hole.example"]);
}

#[test]
fn a_list_longer_than_a_list_may_be_is_no_rules_but_the_server_stays() {
    let domains: Vec<String> = (0..2001).map(|n| format!("domain:d{n}.example")).collect();
    let body = json!([profile(
        "P",
        vec![tcp_reality("a.example", 443)],
        vec![json!({"type": "field", "outboundTag": "direct", "domain": domains})]
    )]);
    let parsed = parse(&body);
    assert_eq!(parsed.servers.len(), 1);
    assert!(parsed.servers[0].routes.is_none());
    let fits: Vec<String> = (0..2000).map(|n| format!("domain:d{n}.example")).collect();
    let body = json!([profile(
        "P",
        vec![tcp_reality("a.example", 443)],
        vec![json!({"type": "field", "outboundTag": "direct", "domain": fits})]
    )]);
    assert_eq!(
        parse(&body).servers[0]
            .routes
            .as_ref()
            .unwrap()
            .direct
            .len(),
        2000
    );
}

#[test]
fn a_program_is_never_a_rule_of_a_server() {
    // There is no `process` condition to read; and an entry by that name would not be kept.
    let routes = routes_of(vec![
        json!({"type": "field", "outboundTag": "direct", "process": ["chrome.exe"]}),
        json!({"type": "field", "outboundTag": "direct", "domain": ["process:chrome.exe", "domain:ok.example"]}),
    ])
    .unwrap();
    assert_eq!(routes.direct, ["domain:ok.example"]);
}

#[test]
fn a_transport_written_as_a_list_is_read_and_one_that_cannot_be_is_not_any() {
    // Xray reads `"network": ["udp"]` as it reads "udp"; it must not turn into a rule for both.
    let routes = routes_of(vec![
        json!({"type": "field", "network": ["udp"], "port": "443", "outboundTag": "block"}),
        json!({"type": "field", "network": ["tcp", "udp"], "port": "53", "outboundTag": "block"}),
        // Not readable: the rule is left out, not widened to TCP and UDP.
        json!({"type": "field", "network": null, "port": "8443", "outboundTag": "block"}),
        json!({"type": "field", "network": 6, "port": "9443", "outboundTag": "block"}),
        json!({"type": "field", "network": ["udp", 6], "port": "7443", "outboundTag": "block"}),
        json!({"type": "field", "outboundTag": "direct", "domain": ["domain:ok.example"]}),
    ])
    .unwrap();
    assert_eq!(routes.block, ["udp:443", "port:53"]);
}

#[test]
fn an_entry_is_read_only_as_the_kind_of_its_condition() {
    // `domain` takes names and `ip` takes addresses; Rift's own kinds are not Xray's there.
    let routes = routes_of(vec![
        json!({"type": "field", "outboundTag": "direct", "domain": [
            "udp:443", "port:1-65535", "tcp:80", "protocol:bittorrent", "geoip:ru",
            "domain:ok.example", "full:www.ok.example", "keyword:ads", "regexp:^a", "geosite:ru"
        ]}),
        json!({"type": "field", "outboundTag": "direct", "ip": [
            "udp:443", "domain:no.example", "keyword:ads", "10.0.0.0/8", "geoip:ru"
        ]}),
    ])
    .unwrap();
    assert_eq!(
        routes.direct,
        [
            "domain:ok.example",
            "full:www.ok.example",
            "keyword:ads",
            "regexp:^a",
            "geosite:ru",
            "10.0.0.0/8",
            "geoip:ru"
        ]
    );
}

#[test]
fn a_product_of_transports_and_ports_that_a_list_cannot_hold_is_left_out_unread() {
    // The same transport again and again against a long list of ports: the product, not either of
    // them, is what comes out. It is found out before the entries are made.
    let started = std::time::Instant::now();
    let networks = vec!["tcp"; 5000].join(",");
    let ports = (1..=5000)
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let routes = routes_of(vec![
        json!({"type": "field", "network": networks, "port": ports, "outboundTag": "block"}),
        json!({"type": "field", "outboundTag": "direct", "domain": ["domain:ok.example"]}),
    ])
    .unwrap();
    assert!(routes.block.is_empty());
    assert_eq!(routes.direct, ["domain:ok.example"]);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
    // The same transport written twice is once.
    let routes = routes_of(vec![
        json!({"type": "field", "network": "tcp,tcp,udp", "port": "53", "outboundTag": "block"}),
    ])
    .unwrap();
    assert_eq!(routes.block, ["port:53"]);
}

#[test]
fn reading_a_long_list_of_rules_does_not_take_longer_than_the_list() {
    // A body can hold far more entries than a profile may keep: the reading stops at the limit
    // instead of comparing each entry with all that came before.
    let started = std::time::Instant::now();
    let domains: Vec<String> = (0..60_000)
        .map(|n| format!("domain:d{n}.example"))
        .collect();
    let body = json!([profile(
        "P",
        vec![tcp_reality("a.example", 443)],
        vec![json!({"type": "field", "outboundTag": "direct", "domain": domains})]
    )]);
    let parsed = parse(&body);
    assert!(parsed.servers[0].routes.is_none());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
}

// ---- the whole of a real body -----------------------------------------------------------------

/// The shape of the answer that made this format necessary, with the numbers it had: 41 profiles,
/// of them 26 plain servers, three Shadowsocks, four over Hysteria (no block of UDP 443), seven
/// "whitelist" pairs (five over xhttp, two over WebSocket) and one "auto" that lists the plain
/// servers again.
#[test]
fn a_subscription_like_the_one_that_was_refused() {
    let mut profiles: Vec<Value> = Vec::new();
    let plain: Vec<Value> = (0..26)
        .map(|n| tcp_reality(&format!("n{n}.example"), 8443))
        .collect();
    profiles.push(profile("Auto", plain[..13].to_vec(), provider_rules()));
    for (n, server) in plain.iter().enumerate() {
        profiles.push(profile(
            &format!("Plain {n}"),
            vec![server.clone()],
            provider_rules(),
        ));
    }
    for n in 0..3 {
        profiles.push(profile(
            &format!("SS {n}"),
            vec![shadowsocks(&format!("ss{n}.example"))],
            provider_rules(),
        ));
    }
    for n in 0..4 {
        profiles.push(profile(
            &format!("Game {n}"),
            vec![hysteria(&format!("hy{n}.example"))],
            provider_rules()[1..].to_vec(),
        ));
    }
    // Seven pairs: a decoy (one of the plain servers) and the server the profile is for.
    for (n, decoy) in plain.iter().enumerate().take(7) {
        let special = if n < 2 {
            ws_tls(&format!("wl{n}.example"))
        } else {
            xhttp(&format!("wl{}.example", n.min(5)))
        };
        profiles.push(profile(
            &format!("WL {n}"),
            vec![decoy.clone(), special],
            provider_rules(),
        ));
    }
    assert_eq!(profiles.len(), 1 + 26 + 3 + 4 + 7);
    let parsed = parse(&Value::Array(profiles));
    // 26 plain, 3 Shadowsocks, 2 WebSocket and 4 distinct XHTTP; Hysteria is refused.
    assert_eq!(parsed.servers.len(), 26 + 3 + 2 + 4);
    assert_eq!(parsed.rejected, 4, "four Hysteria remain unsupported");
    assert_eq!(by_name(&parsed.servers, "Plain 3").host, "n3.example");
    assert_eq!(by_name(&parsed.servers, "WL 0").host, "wl0.example");
    // The Hysteria profiles do not block UDP 443, the others do, and every server has its profile's.
    let with_block = parsed
        .servers
        .iter()
        .filter(|s| s.routes.as_ref().is_some_and(|r| !r.block.is_empty()))
        .count();
    assert_eq!(with_block, parsed.servers.len());
    // One set of rules for all of them.
    let sets: std::collections::HashSet<_> = parsed
        .servers
        .iter()
        .map(|s| s.routes.clone().unwrap().mode())
        .collect();
    assert_eq!(sets.len(), 1);
}

// ---- anything at all --------------------------------------------------------------------------

proptest! {
    #[test]
    fn any_text_that_begins_like_json_is_a_subscription_or_an_error(tail in "(?s).{0,400}") {
        let _ = parse_subscription(&format!("[{tail}"), "s");
        let _ = parse_subscription(&format!("{{{tail}"), "s");
    }

    #[test]
    fn any_value_in_the_places_a_profile_reads_is_refused_without_a_panic(
        remark in any::<String>(),
        number in any::<i64>(),
        text in ".{0,40}",
    ) {
        for odd in [json!(number), json!(text), json!(null), json!([number]), json!({"a": text})] {
            let mut outbound = tcp_reality("a.example", 443);
            for place in ["protocol", "settings", "streamSettings", "tag", "mux"] {
                let mut broken = outbound.clone();
                broken[place] = odd.clone();
                let body = json!([{"remarks": remark, "outbounds": [broken, {"tag": "direct", "protocol": "freedom"}],
                    "routing": {"rules": [odd.clone(), {"outboundTag": odd.clone(), "domain": odd.clone()}]}}]);
                let _ = parse_subscription(&body.to_string(), "s");
            }
            outbound["settings"]["vnext"][0]["port"] = odd.clone();
            let body = json!([{"remarks": remark, "outbounds": [outbound]}]);
            let _ = parse_subscription(&body.to_string(), "s");
        }
    }
}
