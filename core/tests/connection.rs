use rift_core::{connection::Connection, failure::FailureCode, parser::parse_uri};
use serde_json::{json, Value};

fn baseline() -> Vec<Value> {
    let corpus: Value =
        serde_json::from_str(include_str!("fixtures/xray-migration-cases.json")).unwrap();
    corpus["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["group"] == "baseline")
        .cloned()
        .collect()
}

#[test]
fn all_frozen_legacy_inputs_remain_readable_and_retirement_is_explicit() {
    let mut supported = 0;
    let mut retired = 0;

    for case in baseline() {
        let server = parse_uri(case["input"].as_str().unwrap(), None).unwrap();
        let original = serde_json::to_value(&server).unwrap();
        let node = Connection::from_legacy(&server.outbound).unwrap();
        assert_eq!(node.host(), server.host);
        assert_eq!(node.port(), server.port);
        let compiled = node.xray_outbound("candidate");
        if case["retired"] == true || case["id"] == "vless-reality-ws" {
            assert_eq!(
                compiled.unwrap_err().failure_code(),
                Some(FailureCode::UnsupportedLegacyParameters)
            );
            retired += 1;
        } else {
            let config = compiled.unwrap();
            assert_eq!(config["protocol"], case["expected"]["protocol"]);
            let target = config["settings"]["vnext"]
                .get(0)
                .or_else(|| config["settings"]["servers"].get(0))
                .unwrap();
            assert_eq!(target["address"], server.host);
            assert_eq!(target["port"], server.port);
            supported += 1;
        }
        assert_eq!(
            serde_json::to_value(&server).unwrap(),
            original,
            "projection never edits stored data"
        );
    }
    assert_eq!((supported, retired), (62, 52));
}

#[test]
fn unsupported_vision_transports_are_retained_but_never_silently_executed() {
    for params in [
        "security=none&type=tcp",
        "security=tls&type=ws",
        "security=tls&type=grpc",
        "security=tls&type=xhttp",
    ] {
        let server=parse_uri(&format!("vless://11111111-1111-4111-8111-111111111111@server.example:443?flow=xtls-rprx-vision&{params}"),None).unwrap();
        let original = serde_json::to_value(&server).unwrap();
        assert_eq!(
            Connection::from_legacy(&server.outbound)
                .unwrap()
                .check_xray()
                .unwrap_err()
                .failure_code(),
            Some(FailureCode::UnsupportedLegacyParameters)
        );
        assert_eq!(serde_json::to_value(&server).unwrap(), original);
    }
}

#[test]
fn compiler_preserves_credentials_transport_and_tls_instead_of_defaults() {
    for case in baseline()
        .into_iter()
        .filter(|case| case["retired"] == false)
    {
        let s = parse_uri(case["input"].as_str().unwrap(), None).unwrap();
        if case["id"] == "vless-reality-ws" {
            assert_eq!(
                Connection::from_legacy(&s.outbound)
                    .unwrap()
                    .check_xray()
                    .unwrap_err()
                    .failure_code(),
                Some(FailureCode::UnsupportedLegacyParameters)
            );
            continue;
        }
        let output = Connection::from_legacy(&s.outbound)
            .unwrap()
            .xray_outbound("candidate")
            .unwrap();
        let target = output["settings"]["vnext"]
            .get(0)
            .or_else(|| output["settings"]["servers"].get(0))
            .unwrap();
        let user = target["users"].get(0).unwrap_or(target);
        let stream = &output["streamSettings"];
        let expected = &case["expected"];
        assert_eq!(stream["network"], expected["transport"]);
        assert_eq!(stream["security"], expected["security"]);
        if let Some(id) = expected.get("uuid") {
            assert_eq!(&user["id"], id);
        }
        if let Some(password) = expected.get("password") {
            assert_eq!(&target["password"], password);
        }
        if let Some(method) = expected.get("method") {
            assert_eq!(&target["method"], method);
        }
        if let Some(security) = expected.get("vmess_security") {
            assert_eq!(&user["security"], security);
        }
        if let Some(flow) = expected.get("flow") {
            assert_eq!(flow, "xtls-rprx-vision");
            assert_eq!(
                user["flow"], "xtls-rprx-vision-udp443",
                "local Xray flag preserves UDP443; wire flow stays unchanged"
            );
        }
        if let Some(path) = expected.get("path") {
            assert_eq!(&stream["wsSettings"]["path"], path);
        }
        if let Some(host) = expected.get("ws_host") {
            assert_eq!(&stream["wsSettings"]["headers"]["Host"], host);
        }
        if let Some(service) = expected.get("service") {
            assert_eq!(&stream["grpcSettings"]["serviceName"], service);
            assert_eq!(stream["grpcSettings"]["multiMode"], false);
        }
        let tls = if expected["security"] == "reality" {
            &stream["realitySettings"]
        } else {
            &stream["tlsSettings"]
        };
        if let Some(name) = expected.get("sni") {
            assert_eq!(&tls["serverName"], name);
        }
        if let Some(fp) = expected.get("fingerprint") {
            assert_eq!(&tls["fingerprint"], fp);
        }
        if let Some(key) = expected.get("public_key") {
            assert_eq!(&tls["publicKey"], key);
        }
        if let Some(sid) = expected.get("short_id") {
            assert_eq!(&tls["shortId"], sid);
        }
    }
}

#[test]
fn untrusted_programs_unknown_fields_and_wrong_types_cannot_reach_compiler() {
    let good = parse_uri("trojan://synthetic@server.example:443", None)
        .unwrap()
        .outbound;
    let mut bad = good.clone();
    bad["executable_path"] = json!("program");
    assert!(Connection::from_legacy(&bad).is_err());
    let mut bad = good.clone();
    bad["tls"]["insecure"] = json!(true);
    assert!(Connection::from_legacy(&bad).is_err());
    let mut bad = good.clone();
    bad["tls"]["alpn"] = json!([42]);
    assert!(Connection::from_legacy(&bad).is_err());
    for value in [
        Value::Null,
        json!({"type":"direct"}),
        json!({"type":"tor","executable_path":"program"}),
    ] {
        assert!(Connection::from_legacy(&value).is_err());
    }
    let node = Connection::from_legacy(&good).unwrap();
    for tag in ["", "../../path", "candidate\nsecret", "with space"] {
        assert!(node.xray_outbound(tag).is_err());
    }
}

#[test]
fn nonzero_vmess_alter_id_is_not_silently_lost() {
    let mut value = json!({"type":"vmess","server":"server.example","server_port":443,
        "uuid":"11111111-1111-4111-8111-111111111111","alter_id":1,"security":"auto"});
    let node = Connection::from_legacy(&value).unwrap();
    assert!(node.check_xray().is_err());
    value["alter_id"] = json!(0);
    assert!(Connection::from_legacy(&value)
        .unwrap()
        .check_xray()
        .is_ok());
}

#[test]
fn tls_alpn_is_preserved_and_unrepresentable_reality_alpn_is_not_silently_ignored() {
    let tls=Connection::from_legacy(&parse_uri("vless://11111111-1111-4111-8111-111111111111@server.example:443?security=tls&fp=firefox&alpn=h2,http/1.1",None).unwrap().outbound).unwrap();
    let output = tls.xray_outbound("candidate").unwrap();
    assert_eq!(
        output["streamSettings"]["tlsSettings"]["alpn"],
        json!(["h2", "http/1.1"])
    );
    let node = Connection::from_legacy(&parse_uri("vless://11111111-1111-4111-8111-111111111111@server.example:443?security=reality&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&sid=ab12&fp=firefox&sni=front.example&alpn=h2,http/1.1", None).unwrap().outbound).unwrap();
    assert_eq!(
        node.xray_outbound("candidate").unwrap_err().failure_code(),
        Some(FailureCode::UnsupportedLegacyParameters)
    );
}
