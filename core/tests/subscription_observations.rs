use base64::{engine::general_purpose::STANDARD, Engine};
use rift_core::{
    diagnostics::{Diagnostic, Rejection},
    subscription::parse_subscription,
};
use serde_json::{json, Value};

fn event(body: &str) -> Value {
    let error = parse_subscription(body, "synthetic")
        .err()
        .expect("refused input");
    Diagnostic::from_error(&error).to_json()
}

#[test]
fn overlapping_presence_survives_the_first_failure_and_counts_keys_once() {
    let body = "vless://synthetic-secret@hidden.example:443?type=unsupported-secret&fm=secret&fm=another-secret&mode=secret&extra=secret";
    let event = event(body);
    let info = &event["subscription"];
    assert_eq!(info["rows"], json!({"total":1,"accepted":0,"rejected":1}));
    assert_eq!(info["rejected_by"], json!({"bad_uri":1}));
    assert_eq!(
        info["rejected_parameters"],
        json!({"fm":1,"mode":1,"extra":1})
    );
    assert_eq!(info["rejected_transports"], json!({"other":1}));
    for secret in [
        "synthetic-secret",
        "hidden.example",
        "unsupported-secret",
        "another-secret",
    ] {
        assert!(!event.to_string().contains(secret));
    }
    let no_duplicate = body
        .replace("&fm=another-secret", "")
        .replace("synthetic-secret@", "11111111-1111-4111-8111-111111111111@")
        .replace("extra=secret", "extra=%7B%7D");
    let original = parse_subscription(&no_duplicate, "synthetic")
        .err()
        .unwrap();
    let rows = Diagnostic::from_error(&original)
        .subscription
        .unwrap()
        .rows
        .unwrap();
    assert_eq!(rows.rejected(), 1);
    assert_eq!(
        rows.rejected_by[Rejection::UnsupportedTransport as usize],
        1
    );
    assert_eq!(rows.rejected_parameters, [1, 1, 1]);
}

#[test]
fn vmess_and_json_presence_are_independent_of_inactive_transport_blocks() {
    let vmess = json!({"v":"2","add":"hidden.example","port":"443","id":"synthetic-secret",
        "net":"future-secret","fm":null,"mode":"future-secret","extra":false});
    let uri = format!("vmess://{}", STANDARD.encode(vmess.to_string()));
    let encoded_body = STANDARD.encode(&uri);
    for body in [&uri, &encoded_body] {
        let e = event(body);
        assert_eq!(
            e["subscription"]["rejected_parameters"],
            json!({"fm":1,"mode":1,"extra":1})
        );
        assert_eq!(e["subscription"]["rejected_transports"], json!({"other":1}));
    }
    let raw = json!({"protocol":"vless","tag":"one","settings":{"vnext":[{"address":"hidden.example","port":443,"users":[{"id":"synthetic-secret"}]}]},
        "streamSettings":{"network":"future-secret","finalmask":null,"xhttpSettings":{"mode":false,"extra":"synthetic-secret"}}});
    let mut duplicate = raw.clone();
    duplicate["tag"] = "another-tag".into();
    let body = json!([{"outbounds":[raw]},{"outbounds":[duplicate]}]).to_string();
    let e = event(&body);
    let info = &e["subscription"];
    assert_eq!(info["rows"]["rejected"], 1);
    assert_eq!(
        info["rejected_parameters"],
        json!({"fm":1,"mode":1,"extra":1})
    );
    assert_eq!(info["rejected_transports"], json!({"other":1}));
}

#[test]
fn observations_are_optional_bounded_and_transport_names_are_closed() {
    for malformed in [Value::Null, json!(7), json!({})] {
        let raw = json!({"outbounds":[{"protocol":"vless","settings":{},
            "streamSettings":{"network":malformed,"finalmask":null}}]});
        let e = event(&raw.to_string());
        assert_eq!(e["subscription"]["rejected_transports"], json!({"other":1}));
        let vmess = json!({"net":malformed,"fm":null});
        let uri = format!("vmess://{}", STANDARD.encode(vmess.to_string()));
        let e = event(&uri);
        assert_eq!(e["subscription"]["rejected_transports"], json!({"other":1}));
    }
    for (kind, expected) in [
        ("tcp", "tcp"),
        ("ws", "ws"),
        ("grpc", "grpc"),
        ("xhttp", "xhttp"),
        ("future-secret", "other"),
    ] {
        let body = format!("vless://secret@hidden.example:443?type={kind}&mode=secret&security=tls&extra=secret&fm=secret");
        let e = event(&body);
        assert_eq!(
            e["subscription"]["rejected_transports"],
            json!({expected:1})
        );
    }
    for body in [
        "hello".to_owned(),
        format!("vless://{}?fm=secret", "x".repeat(16_384)),
        "vmess://invalid%%%".into(),
    ] {
        let e = event(&body);
        let info = &e["subscription"];
        assert!(info.get("rejected_parameters").is_none());
        assert!(info.get("rejected_transports").is_none());
    }
}

#[test]
fn partial_xray_import_reports_validation_causes_instead_of_other() {
    let valid = json!({
        "protocol": "vless",
        "settings": {"vnext": [{
            "address": "server-canary.example",
            "port": 443,
            "users": [{"id": "11111111-1111-4111-8111-111111111111"}]
        }]}
    });
    let mut unknown = valid.clone();
    unknown["unknown-canary"] = "value-canary".into();
    let mut credentials = valid.clone();
    credentials["settings"]["vnext"][0]["users"][0]["id"] = "uuid-canary".into();
    let mut endpoint = valid.clone();
    endpoint["settings"]["vnext"][0]["port"] = 0.into();
    let mut missing = valid.clone();
    missing["settings"]["vnext"][0]["users"][0] = json!({});
    let mut invalid = valid.clone();
    invalid["settings"]["vnext"][0]["users"][0]["id"] = false.into();
    let mut security = valid.clone();
    security["streamSettings"] = json!({"security":"security-canary"});
    let mut transport = valid.clone();
    transport["streamSettings"] = json!({"network":"transport-canary"});
    let body = json!({"outbounds": [
        valid, unknown, credentials, endpoint, missing, invalid, security, transport
    ]});
    let parsed = parse_subscription(&body.to_string(), "synthetic").unwrap();
    assert_eq!(parsed.servers.len(), 1);
    assert_eq!(parsed.rejected, 7);
    let mut report = Diagnostic::of(None);
    report.subscription = Some(rift_core::diagnostics::SubscriptionInfo {
        rows: Some(parsed.rows),
        ..Default::default()
    });
    let info = report.to_json()["subscription"].clone();
    assert_eq!(info["rows"], json!({"total":8,"accepted":1,"rejected":7}));
    assert_eq!(
        info["rejected_by"],
        json!({
            "unknown_param":1,
            "invalid_credentials":1,
            "invalid_endpoint":1,
            "missing_field":1,
            "invalid_field":1,
            "unsupported_security":1,
            "unsupported_transport":1
        })
    );
    assert!(!info.to_string().contains("canary"));
}
