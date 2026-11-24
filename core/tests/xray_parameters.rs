use rift_core::subscription::parse_subscription;
use serde_json::json;
const ID: &str = "11111111-1111-4111-8111-111111111111";
fn link(network: &str, security: &str, params: &[(&str, String)]) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("security", security)
        .append_pair("type", network)
        .extend_pairs(params.iter().map(|(k, v)| (*k, v.as_str())))
        .finish();
    format!("vless://{ID}@server.example:443?{query}")
}
fn compiled(uri: &str) -> serde_json::Value {
    let s = rift_core::parser::parse_uri(uri, None).unwrap();
    rift_core::connection::Connection::from_legacy(&s.outbound)
        .unwrap()
        .xray_outbound("synthetic")
        .unwrap()
}

#[test]
fn all_frozen_targets_import_compile_and_have_identical_uri_json_identity() {
    use rift_core::{connection::Connection, parser::parse_uri};
    let corpus: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/xray-migration-cases.json")).unwrap();
    let mut identities = std::collections::HashMap::new();
    let mut count = 0;
    for c in corpus["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["group"] == "target")
    {
        let server = if c["kind"] == "uri" {
            parse_uri(c["input"].as_str().unwrap(), None).unwrap()
        } else {
            parse_subscription(&c["input"].to_string(), "synthetic")
                .unwrap()
                .servers
                .remove(0)
        };
        let outbound = Connection::from_legacy(&server.outbound)
            .unwrap()
            .xray_outbound("synthetic")
            .unwrap();
        let stream = &outbound["streamSettings"];
        let expected = &c["expected"];
        assert_eq!(stream["network"], expected["transport"]);
        assert_eq!(stream["security"], expected["security"]);
        if let Some(mask) = expected.get("finalmask") {
            assert_eq!(&stream["finalmask"], mask);
        }
        if let Some(mode) = expected.get("xhttp_mode") {
            assert_eq!(&stream["xhttpSettings"]["mode"], mode);
            assert_eq!(stream["xhttpSettings"]["path"], expected["path"]);
        }
        if let Some(service) = expected.get("service") {
            assert_eq!(&stream["grpcSettings"]["serviceName"], service);
            assert_eq!(stream["grpcSettings"]["multiMode"], false);
        }
        let base = c["id"].as_str().unwrap().rsplit_once('-').unwrap().0;
        if let Some(id) = identities.insert(base, server.id.clone()) {
            assert_eq!(id, server.id, "URI and JSON for {base}");
        }
        count += 1;
    }
    assert_eq!(count, 16);
    assert_eq!(identities.len(), 8);
}

#[test]
fn accepted_extra_survives_compiler_store_and_profile_without_rewriting_ranges() {
    let extra = json!({"xPaddingBytes":"17-31","noGRPCHeader":true,"noSSEHeader":false,"scMaxEachPostBytes":1048576,
        "scMinPostsIntervalMs":"1-5","scMaxBufferedPosts":10,"scStreamUpServerSecs":"20-30",
        "xmux":{"maxConnections":"1-3","cMaxReuseTimes":100,"hMaxRequestTimes":"50-100","hMaxReusableSecs":300,"hKeepAlivePeriod":10}});
    let uri = link(
        "xhttp",
        "tls",
        &[
            ("mode", "packet-up".into()),
            ("extra", extra.to_string()),
            ("host", "front.example".into()),
            ("path", "/synthetic".into()),
        ],
    );
    assert_eq!(
        compiled(&uri)["streamSettings"]["xhttpSettings"]["extra"],
        extra
    );
    let mut store = rift_core::model::Store::default();
    store
        .servers
        .push(rift_core::parser::parse_uri(&uri, None).unwrap());
    store.selected = Some(store.servers[0].id.clone());
    let restored =
        rift_core::profile::decode(&rift_core::profile::encode(&store).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&store).unwrap(),
        serde_json::to_value(&restored).unwrap()
    );
    let changed = link(
        "xhttp",
        "tls",
        &[("extra", json!({"xPaddingBytes":"18-32"}).to_string())],
    );
    assert_ne!(
        store.servers[0].id,
        rift_core::parser::parse_uri(&changed, None).unwrap().id
    );
}

#[test]
fn official_fragment_forms_and_empty_headers_survive_import_storage_and_compilation() {
    let masks = [
        json!({"tcp":[{"type":"fragment","settings":{"packets":"tlshello","length":"11-19","delay":"0-2","maxSplit":"3-5"}}]}),
        json!({"tcp":[{"type":"fragment","settings":{"packets":"tlshello","lengths":["11-19","21-29"],"delays":["0-2","3-4"]}}]}),
        json!({"tcp":[{"type":"fragment","settings":{"packets":"tlshello","length":17,"delay":0,"lengths":["11-19"],"delays":[0],"maxSplit":0}}]}),
    ];
    for mask in masks {
        let extra = json!({"headers":{},"noGRPCHeader":true,"xmux":{"maxConcurrency":"2-4","maxConnections":0}});
        let uri = link(
            "xhttp",
            "tls",
            &[("fm", mask.to_string()), ("extra", extra.to_string())],
        );
        let server = rift_core::parser::parse_uri(&uri, None).expect("official bounded fields");
        let raw = json!({"outbounds":[{"protocol":"vless","settings":{"vnext":[{"address":"server.example","port":443,"users":[{"id":ID,"encryption":"none"}]}]},"streamSettings":{"network":"xhttp","security":"tls","tlsSettings":{"serverName":"server.example"},"xhttpSettings":{"path":"/","mode":"auto","extra":extra},"finalmask":mask}}]});
        let from_json = parse_subscription(&raw.to_string(), "synthetic")
            .unwrap()
            .servers
            .remove(0);
        assert_eq!(server.id, from_json.id);
        let mut store = rift_core::model::Store::default();
        store.servers.push(server);
        let restored =
            rift_core::profile::decode(&rift_core::profile::encode(&store).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&store).unwrap(),
            serde_json::to_value(&restored).unwrap()
        );
        let outbound =
            rift_core::connection::Connection::from_legacy(&restored.servers[0].outbound)
                .unwrap()
                .xray_outbound("synthetic")
                .unwrap();
        assert_eq!(outbound["streamSettings"]["finalmask"], mask);
        assert_eq!(outbound["streamSettings"]["xhttpSettings"]["extra"], extra);
    }
}

#[test]
fn new_fragment_fields_remain_bounded_and_nonempty_headers_remain_refused() {
    for settings in [
        json!({"length":10,"delay":0,"maxSplit":65537}),
        json!({"length":10,"delay":0,"maxSplit":null}),
        json!({"lengths":[],"delays":[0]}),
        json!({"lengths":[0],"delays":[0]}),
        json!({"lengths":[10],"delays":[1001]}),
        json!({"lengths":[10],"delays":[]}),
        json!({"lengths":null,"delays":[0]}),
        json!({"lengths":[10],"delays":[0],"unknown":"synthetic-canary"}),
        json!({"lengths":vec![10;33],"delays":[0]}),
    ] {
        let mut settings = settings;
        settings["packets"] = json!("tlshello");
        let mask = json!({"tcp":[{"type":"fragment","settings":settings}]});
        let uri = link("tcp", "tls", &[("fm", mask.to_string())]);
        assert!(rift_core::parser::parse_uri(&uri, None).is_err());
    }
    for headers in [
        json!({"X-Secret":"synthetic-canary"}),
        json!(null),
        json!([]),
    ] {
        let uri = link(
            "xhttp",
            "tls",
            &[("extra", json!({"headers":headers}).to_string())],
        );
        assert!(rift_core::parser::parse_uri(&uri, None).is_err());
    }
}

#[test]
fn vmess_uri_and_xray_json_share_xhttp_extra_and_finalmask_normalization() {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let extra = json!({"xPaddingBytes":"100-200"});
    let mask = json!({"tcp":[{"type":"fragment","settings":{"packets":"tlshello","length":"10-20","delay":"1-2"}}]});
    let uri=format!("vmess://{}",URL_SAFE_NO_PAD.encode(json!({"add":"server.example","port":443,"id":ID,"aid":0,"scy":"auto","net":"xhttp","path":"/synthetic","host":"front.example","tls":"tls","sni":"localhost","mode":"stream-up","extra":extra,"fm":mask}).to_string()));
    let from_uri = rift_core::parser::parse_uri(&uri, None).unwrap();
    let raw = json!({"outbounds":[{"protocol":"vmess","settings":{"vnext":[{"address":"server.example","port":443,"users":[{"id":ID,"security":"auto","alterId":0}]}]},
        "streamSettings":{"network":"xhttp","security":"tls","tlsSettings":{"serverName":"localhost"},"xhttpSettings":{"path":"/synthetic","host":"front.example","mode":"stream-up","extra":extra},"finalmask":mask}}]});
    let from_json = parse_subscription(&raw.to_string(), "synthetic")
        .unwrap()
        .servers
        .remove(0);
    assert_eq!(from_uri.id, from_json.id);
    assert_eq!(from_uri.outbound, from_json.outbound);
    assert_eq!(compiled(&uri)["streamSettings"]["finalmask"], mask);
}

#[test]
fn xray_body_fragment_strings_are_typed_refusals_beside_valid_objects() {
    use rift_core::diagnostics::Rejection;
    let extra = json!({"xPaddingBytes":"100-200"});
    let mask = json!({"tcp":[{"type":"fragment","settings":{"packets":"tlshello","length":"10-20","delay":"1-2"}}]});
    let good = json!({"protocol":"trojan","settings":{"servers":[{"address":"good.example","port":443,"password":"synthetic"}]},
        "streamSettings":{"network":"xhttp","security":"tls","xhttpSettings":{"extra":extra},"finalmask":mask}});
    let expected = parse_subscription(&json!({"outbounds":[good]}).to_string(), "synthetic")
        .unwrap()
        .servers
        .remove(0);
    let mut bad_extra = good.clone();
    bad_extra["settings"]["servers"][0]["address"] = json!("extra.example");
    bad_extra["streamSettings"]["xhttpSettings"]["extra"] = json!(extra.to_string());
    let mut bad_mask = good.clone();
    bad_mask["settings"]["servers"][0]["address"] = json!("mask.example");
    bad_mask["streamSettings"]["finalmask"] = json!(mask.to_string());
    // Check both supported Xray body layouts, including profile arrays.
    for body in [
        json!({"outbounds":[bad_extra.clone(), good, bad_mask.clone()]}),
        json!([{"outbounds":[bad_extra, good, bad_mask]}]),
    ] {
        let parsed = parse_subscription(&body.to_string(), "synthetic").unwrap();
        assert_eq!(parsed.servers.len(), 1);
        assert_eq!(parsed.servers[0].id, expected.id);
        assert_eq!(parsed.servers[0].outbound, expected.outbound);
        assert_eq!(parsed.rows.accepted, 1);
        assert_eq!(parsed.rejected, 2);
        assert_eq!(parsed.rows.rejected_by[Rejection::InvalidExtra as usize], 1);
        assert_eq!(
            parsed.rows.rejected_by[Rejection::InvalidFinalmask as usize],
            1
        );
        assert_eq!(parsed.rows.rejected(), 2);
    }
}

#[test]
fn vmess_fragment_objects_and_strings_preserve_identical_connections() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let extra = json!({"xPaddingBytes":"100-200"});
    let mask = json!({"tcp":[{"type":"fragment","settings":{"packets":"tlshello","length":"10-20","delay":"1-2"}}]});
    let mut expected = None;
    for extra_as_string in [false, true] {
        for mask_as_string in [false, true] {
            let uri = format!(
                "vmess://{}",
                STANDARD.encode(json!({"add":"server.example","port":443,"id":ID,"aid":0,"scy":"auto","net":"xhttp","tls":"tls",
                    "extra":if extra_as_string { json!(extra.to_string()) } else { extra.clone() },
                    "fm":if mask_as_string { json!(mask.to_string()) } else { mask.clone() }}).to_string())
            );
            let server = rift_core::parser::parse_uri(&uri, None).unwrap();
            if let Some((id, outbound)) = &expected {
                assert_eq!(&server.id, id);
                assert_eq!(&server.outbound, outbound);
            } else {
                expected = Some((server.id.clone(), server.outbound.clone()));
            }
            let outbound = compiled(&uri);
            assert_eq!(outbound["streamSettings"]["xhttpSettings"]["extra"], extra);
            assert_eq!(outbound["streamSettings"]["finalmask"], mask);
        }
    }
}

#[test]
fn modes_and_foreign_transport_settings_never_disappear() {
    use rift_core::{
        diagnostics::Rejection,
        parser::{parse_uri, rejection},
    };
    for m in [None, Some("gun"), Some("")] {
        let pairs = m.map(|m| vec![("mode", m.into())]).unwrap_or_default();
        assert!(parse_uri(&link("grpc", "tls", &pairs), None).is_ok());
    }
    for mode in ["multi", "guna", "stream-one", "unknown"] {
        let uri = link("grpc", "tls", &[("mode", mode.into())]);
        let e = parse_uri(&uri, None).err().unwrap();
        assert_eq!(rejection(&uri, &e), Rejection::UnsupportedMode);
    }
    for network in ["tcp", "ws"] {
        for pair in [("mode", String::new()), ("extra", "{}".into())] {
            let uri = link(network, "tls", &[pair]);
            let e = parse_uri(&uri, None).err().unwrap();
            assert_eq!(rejection(&uri, &e), Rejection::IncompatibleParam);
        }
    }
}

#[test]
fn malformed_unsafe_and_unbounded_masks_and_extra_are_typed_refusals() {
    use rift_core::{
        diagnostics::Rejection,
        parser::{parse_uri, rejection},
    };
    for extra in [
        json!({"downloadSettings":{"address":"other.example","port":443}}),
        json!({"headers":{"X-Secret":"synthetic-canary"}}),
        json!({"sockopt":{"dialerProxy":"direct"}}),
        json!({"extra":{}}),
        json!({"xPaddingBytes":0}),
        json!({"xPaddingBytes":"20-10"}),
        json!({"xPaddingBytes":65537}),
        json!({"scMaxBufferedPosts":1025}),
        json!({"scMaxEachPostBytes":16777217}),
        json!({"xmux":{"maxConnections":1,"maxConcurrency":1}}),
        json!({"xmux":{"maxConnections":-1}}),
        json!(null),
        json!([1]),
    ] {
        let uri = link("xhttp", "tls", &[("extra", extra.to_string())]);
        let e = parse_uri(&uri, None).err().unwrap();
        assert_eq!(rejection(&uri, &e), Rejection::InvalidExtra);
        assert!(!e.to_string().contains("synthetic-canary"));
    }
    for raw in [
        "{\"xPaddingBytes\":1,\"xPaddingBytes\":2}".to_owned(),
        "[".repeat(10) + &"]".repeat(10),
        " ".repeat(8193),
    ] {
        let uri = link("xhttp", "tls", &[("extra", raw)]);
        assert!(parse_uri(&uri, None).is_err());
    }
    for mask in [
        json!({}),
        json!({"tcp":[]}),
        json!({"udp":[{"type":"noise"}]}),
        json!({"tcp":[{"type":"exec","settings":{"command":"synthetic-canary"}}]}),
        json!({"tcp":[{"type":"fragment","settings":{"packets":"tlshello","length":0,"delay":1}}]}),
        json!({"tcp":[{"type":"fragment","settings":{"packets":"tlshello","length":10,"delay":1001}}]}),
        json!({"tcp":[{"type":"fragment","settings":{"packets":"tlshello","length":10,"delay":1,"file":"synthetic-canary"}}]}),
    ] {
        let uri = link("tcp", "tls", &[("fm", mask.to_string())]);
        let e = parse_uri(&uri, None).err().unwrap();
        assert_eq!(rejection(&uri, &e), Rejection::InvalidFinalmask);
        assert!(!e.to_string().contains("synthetic-canary"));
    }
    let plain=link("tcp","none",&[("fm",json!({"tcp":[{"type":"fragment","settings":{"packets":"tlshello","length":10,"delay":1}}]}).to_string())]);
    assert_eq!(
        rejection(&plain, &parse_uri(&plain, None).err().unwrap()),
        Rejection::IncompatibleParam
    );
    let duplicate = r#"{"outbounds":[{"protocol":"trojan","settings":{"servers":[{"address":"server.example","port":443,"password":"synthetic"}]},"streamSettings":{"network":"xhttp","xhttpSettings":{"extra":{"xPaddingBytes":1,"xPaddingBytes":2}}}}]}"#.to_owned();
    assert!(parse_subscription(&duplicate, "synthetic").is_err());
}
#[test]
fn grpc_multi_mode_is_not_silently_replaced_by_gun() {
    let good = json!({"protocol":"trojan","settings":{"servers":[{"address":"good.example","port":443,"password":"synthetic"}]}});
    let bad = json!({"protocol":"vless","settings":{"vnext":[{"address":"grpc.example","port":443,"users":[{"id":ID,"encryption":"none"}]}]},
        "streamSettings":{"network":"grpc","grpcSettings":{"serviceName":"synthetic","multiMode":true}}});
    let parsed =
        parse_subscription(&json!({"outbounds":[good,bad]}).to_string(), "synthetic").unwrap();
    assert_eq!(
        parsed.servers.len(),
        1,
        "multiMode must not disappear during conversion"
    );
    assert_eq!(parsed.rejected, 1);
}

#[test]
fn foreign_security_settings_and_nondefault_legacy_options_are_explicit_refusals() {
    use rift_core::diagnostics::Rejection;
    let base = json!({"protocol":"vless","settings":{"vnext":[{"address":"server.example","port":443,"users":[{"id":ID,"encryption":"none"}]}]},
        "streamSettings":{"network":"tcp","security":"reality","realitySettings":{"serverName":"front.example","publicKey":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","shortId":"ab12","spiderX":"/","show":false}}});
    assert!(parse_subscription(&json!({"outbounds":[base]}).to_string(), "synthetic").is_ok());
    for (field, value, why) in [
        (
            "spiderX",
            json!("/nondefault?synthetic=canary"),
            Rejection::UnsupportedParam,
        ),
        ("show", json!(true), Rejection::UnsupportedParam),
    ] {
        let mut bad = base.clone();
        bad["streamSettings"]["realitySettings"][field] = value;
        let error = parse_subscription(&json!({"outbounds":[bad]}).to_string(), "synthetic")
            .err()
            .unwrap();
        assert_eq!(
            error
                .detail()
                .unwrap()
                .subscription
                .unwrap()
                .rows
                .unwrap()
                .rejected_by[why as usize],
            1
        );
    }
    for (field, value) in [
        ("grpcSettings", json!({"serviceName":"must-not-disappear"})),
        ("tlsSettings", json!({"serverName":"must-not-disappear"})),
    ] {
        let mut bad = base.clone();
        bad["streamSettings"][field] = value;
        let error = parse_subscription(&json!({"outbounds":[bad]}).to_string(), "synthetic")
            .err()
            .unwrap();
        assert_eq!(
            error
                .detail()
                .unwrap()
                .subscription
                .unwrap()
                .rows
                .unwrap()
                .rejected_by[Rejection::IncompatibleParam as usize],
            1
        );
    }
    let mut inactive = base;
    inactive["streamSettings"]["tlsSettings"] = serde_json::Value::Null;
    assert!(parse_subscription(&json!({"outbounds":[inactive]}).to_string(), "synthetic").is_ok());
}

#[test]
fn strict_fragments_reject_only_their_outbound_not_the_subscription() {
    use rift_core::diagnostics::Rejection;
    let good = json!({"protocol":"trojan","settings":{"servers":[{"address":"good.example","port":443,"password":"synthetic"}]}});
    for (fragment, why) in [
        (
            r#""network":"xhttp","xhttpSettings":{"extra":{"xPaddingBytes":1,"xPaddingBytes":2}}"#,
            Rejection::InvalidExtra,
        ),
        (
            r#""network":"tcp","security":"tls","finalmask":{"tcp":[],"tcp":[]}"#,
            Rejection::InvalidFinalmask,
        ),
    ] {
        let bad = format!(
            r#"{{"protocol":"trojan","settings":{{"servers":[{{"address":"bad.example","port":443,"password":"synthetic"}}]}},"streamSettings":{{{fragment}}}}}"#
        );
        let parsed =
            parse_subscription(&format!(r#"{{"outbounds":[{good},{bad}]}}"#), "synthetic").unwrap();
        assert_eq!(parsed.servers.len(), 1);
        assert_eq!(parsed.servers[0].host, "good.example");
        assert_eq!(parsed.rejected, 1);
        assert_eq!(parsed.rows.rejected_by[why as usize], 1);
    }
    let ignored = "[".repeat(40) + "0" + &"]".repeat(40);
    let parsed = parse_subscription(
        &format!(r#"{{"outbounds":[{good}],"ignored":0,"ignored":{ignored}}}"#),
        "synthetic",
    )
    .unwrap();
    assert_eq!(parsed.servers.len(), 1);
    assert_eq!(parsed.rejected, 0);
}

#[test]
fn vmess_fragment_objects_and_strings_keep_typed_per_line_rejections() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use rift_core::diagnostics::Rejection;
    for (key, raw, why) in [
        (
            "extra",
            r#"{"xPaddingBytes":1,"xPaddingBytes":2}"#,
            Rejection::InvalidExtra,
        ),
        ("fm", r#"{"tcp":[],"tcp":[]}"#, Rejection::InvalidFinalmask),
    ] {
        for as_string in [false, true] {
            let fragment = if as_string {
                serde_json::to_string(raw).unwrap()
            } else {
                raw.into()
            };
            let vmess = format!(
                r#"{{"add":"bad.example","port":443,"id":"{ID}","net":"xhttp","tls":"tls","{key}":{fragment}}}"#
            );
            let body = format!(
                "trojan://synthetic@good.example:443\nvmess://{}",
                STANDARD.encode(vmess)
            );
            let parsed = parse_subscription(&body, "synthetic").unwrap();
            assert_eq!(parsed.servers.len(), 1);
            assert_eq!(parsed.rejected, 1);
            assert_eq!(parsed.rows.rejected_by[why as usize], 1);
        }
    }
}
