use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use rift_core::{
    model::{Server, Store, Subscription},
    outbound,
    parser::parse_uri,
    persistence, profile,
};
use serde_json::{json, Value};

const MANUAL_URI: &str = "trojan://manual-secret@example.com:443#Manual";
const SUB_URI: &str = "trojan://subscription-secret@example.net:443#Imported";
const SUB_URL: &str = "https://subscriptions.example/path?token=url-secret";

fn complete_store() -> Store {
    let mut store = Store::default();
    store.subscriptions.push(Subscription {
        id: "main".into(),
        name: "Основная".into(),
        url: SUB_URL.into(),
        updated_at: Some(1_789_742_400),
        error: None,
        failure_code: None,
        ..Default::default()
    });
    store.servers.push(parse_uri(MANUAL_URI, None).unwrap());
    store
        .servers
        .push(parse_uri(SUB_URI, Some("main".into())).unwrap());
    store.selected = Some(store.servers[1].id.clone());
    store
}

#[test]
fn full_profile_round_trip_keeps_secrets_and_composition() {
    let store = complete_store();
    let bytes = profile::encode(&store).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.contains("manual-secret"));
    assert!(text.contains("subscription-secret"));
    assert!(text.contains("url-secret"));
    assert!(!text.contains("journal"));
    assert!(!text.contains("runtime_active"));

    let restored = profile::decode(&bytes).unwrap();
    assert_eq!(
        serde_json::to_value(restored).unwrap(),
        serde_json::to_value(store).unwrap()
    );
    assert_eq!(
        profile::summary(&complete_store()),
        profile::Summary {
            version: 2,
            servers: 2,
            manual_servers: 1,
            subscriptions: 1,
            has_selected_server: true,
        }
    );
}

#[test]
fn legacy_disabled_sources_retain_servers_and_selection_without_reexporting_enabled() {
    for version in [1, 2] {
        let original = complete_store();
        let mut state = serde_json::to_value(&original).unwrap();
        state["version"] = json!(version);
        state["subscriptions"][0]["enabled"] = json!(false);
        let bytes = serde_json::to_vec(&state).unwrap();
        let restored = persistence::decode(&bytes).unwrap();
        assert_eq!(restored.servers.len(), original.servers.len());
        assert_eq!(restored.selected, original.selected);
        assert_eq!(restored.available_servers().len(), 2);
        assert_eq!(restored.auto_candidates(0).len(), 2);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        persistence::save(&path, &restored).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(saved["subscriptions"][0].get("enabled").is_none());
        assert_eq!(
            persistence::load(&path).unwrap().selected,
            original.selected
        );

        let envelope = json!({"format": profile::FORMAT, "version": version, "profile": state});
        let imported = profile::decode(&serde_json::to_vec(&envelope).unwrap()).unwrap();
        assert_eq!(imported.servers.len(), original.servers.len());
        assert_eq!(imported.selected, original.selected);
        assert_eq!(imported.available_servers().len(), 2);
        assert_eq!(imported.auto_candidates(0).len(), 2);
        let exported: Value = serde_json::from_slice(&profile::encode(&imported).unwrap()).unwrap();
        assert!(exported["profile"]["subscriptions"][0]
            .get("enabled")
            .is_none());
    }
}

#[test]
fn legacy_reporting_fields_are_read_but_never_written_to_state_or_profiles() {
    for enabled in [true, false] {
        let original = complete_store();
        let mut state = serde_json::to_value(&original).unwrap();
        state["settings"]["send_error_reports"] = json!(enabled);
        state["error_reporting"] = json!(enabled);
        let decoded = persistence::decode(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(decoded.servers.len(), original.servers.len());
        assert_eq!(decoded.selected, original.selected);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        persistence::save(&path, &decoded).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(saved["settings"].get("send_error_reports").is_none());
        assert!(saved.get("error_reporting").is_none());

        let envelope = json!({"format": profile::FORMAT, "version": 2, "profile": state});
        let imported = profile::decode(&serde_json::to_vec(&envelope).unwrap()).unwrap();
        assert_eq!(imported.servers.len(), original.servers.len());
        assert_eq!(imported.selected, original.selected);
        let exported: Value = serde_json::from_slice(&profile::encode(&imported).unwrap()).unwrap();
        assert!(exported["profile"]["settings"]
            .get("send_error_reports")
            .is_none());
        assert!(exported["profile"].get("error_reporting").is_none());
    }
}

#[test]
fn legacy_activation_preserves_usage_and_individual_exclusions() {
    let mut state = serde_json::to_value(complete_store()).unwrap();
    state["subscriptions"][0]["enabled"] = json!(false);
    state["subscriptions"][0]["usage"] = json!({"used": 10, "total": 10, "expire": null});
    let restored = persistence::decode(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(
        restored.available_servers().len(),
        2,
        "manual selection ignores quota"
    );
    assert_eq!(
        restored.auto_candidates(0).len(),
        1,
        "quota still limits Auto Select"
    );
    state["subscriptions"][0]["usage"] = json!({"used": 0, "total": 0, "expire": 10});
    let mut restored = persistence::decode(&serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(restored.auto_candidates(9).len(), 2);
    assert_eq!(
        restored.auto_candidates(10).len(),
        1,
        "expiry is still enforced"
    );
    restored.excluded.push(restored.servers[1].stable_key());
    assert_eq!(
        restored.auto_candidates(9).len(),
        1,
        "source migration retains exclusions"
    );
    let exported = profile::encode(&restored).unwrap();
    let imported = profile::decode(&exported).unwrap();
    assert_eq!(imported.excluded, restored.excluded);
    assert_eq!(imported.auto_candidates(9).len(), 1);
}

#[test]
fn damaged_future_and_oversized_profiles_are_rejected() {
    let bytes = profile::encode(&complete_store()).unwrap();
    let mut future: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    future["version"] = 3.into();
    assert!(profile::decode(&serde_json::to_vec(&future).unwrap()).is_err());
    assert!(profile::decode(b"{").is_err());
    assert!(profile::decode(&vec![b' '; profile::MAX_BYTES + 1]).is_err());
}

#[test]
fn inconsistent_profile_is_rejected() {
    let mut missing_source = complete_store();
    missing_source.servers[1].source = Some("missing".into());
    assert!(profile::encode(&missing_source).is_err());

    let mut bad_fingerprint = complete_store();
    bad_fingerprint.servers[0].id = "changed".into();
    assert!(profile::encode(&bad_fingerprint).is_err());

    let mut stale_selection = complete_store();
    stale_selection.selected = Some("missing".into());
    assert!(profile::encode(&stale_selection).is_err());
}

/// One representative URI per protocol shape `parser` can emit, with every optional key.
fn every_protocol_shape() -> Vec<String> {
    let reality = "vless://11111111-1111-4111-8111-111111111111@[::1]:443?security=reality&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&sid=ab12&sni=example.com&fp=firefox&flow=xtls-rprx-vision&type=grpc&serviceName=svc#Reality";
    let vless_ws = "vless://11111111-1111-4111-8111-111111111111@example.com:443?security=tls&alpn=h2,http/1.1&type=ws&path=%2Fproxy&host=cdn.example.com&fp=chrome#VlessWs";
    let vless_plain =
        "vless://11111111-1111-4111-8111-111111111111@198.51.100.7:8443?encryption=none#VlessPlain";
    let trojan_tls = "trojan://p%40ss%3Aword@example.com:443?type=grpc&serviceName=svc#TrojanGrpc";
    let trojan_plain = "trojan://pw@example.com:443?security=none#TrojanPlain";
    let ss = "ss://aes-256-gcm:secret@example.com:8388#Ss";
    let key = STANDARD.encode([7u8; 32]);
    let ss_2022 = format!(
        "ss://2022-blake3-aes-256-gcm:{}@example.com:8388#Ss2022",
        key.replace('=', "%3D")
            .replace('+', "%2B")
            .replace('/', "%2F")
    );
    let vmess = format!(
        "vmess://{}",
        URL_SAFE_NO_PAD.encode(
            r#"{"v":"2","ps":"Vmess","add":"Example.com","port":"443","id":"11111111-1111-4111-8111-111111111111","aid":"4","scy":"aes-128-gcm","net":"ws","path":"/ws","host":"cdn.example.com","tls":"tls","sni":"example.com","alpn":"h2","fp":"ios"}"#
        )
    );
    [
        reality,
        vless_ws,
        vless_plain,
        trojan_tls,
        trojan_plain,
        ss,
        &ss_2022,
        &vmess,
    ]
    .map(str::to_owned)
    .to_vec()
}

#[test]
fn every_server_the_parser_builds_survives_export_and_import() {
    let mut store = Store::default();
    for uri in every_protocol_shape() {
        let server = parse_uri(&uri, None).unwrap_or_else(|_| panic!("parser rejected a fixture"));
        assert_eq!(
            outbound::inspect(&server.outbound).unwrap().protocol,
            server.protocol
        );
        store.servers.push(server);
    }
    store.selected = Some(store.servers[2].id.clone());

    let restored = profile::decode(&profile::encode(&store).unwrap()).unwrap();

    assert_eq!(restored.servers.len(), 8);
    assert_eq!(
        serde_json::to_value(restored).unwrap(),
        serde_json::to_value(store).unwrap()
    );
}

/// A profile file whose only server carries `outbound`, with every derived field and the id
/// computed the way a careful attacker would: the file is fully self-consistent.
fn profile_with_outbound(outbound: Value) -> Vec<u8> {
    let mut file: Value =
        serde_json::from_slice(&profile::encode(&complete_store()).unwrap()).unwrap();
    // Keep the manual server only, so no subscription or selection refers to a removed row.
    file["profile"]["selected"] = Value::Null;
    file["profile"]["subscriptions"] = json!([]);
    // Attack the v1 input adapter deliberately; do not produce a duplicate alias and a false PASS.
    file["version"] = json!(1);
    file["profile"]["version"] = json!(1);
    let servers = file["profile"]["servers"].as_array_mut().unwrap();
    servers.truncate(1);
    servers[0]["id"] = json!(Server::fingerprint(&outbound));
    servers[0]["protocol"] = outbound["type"].clone();
    servers[0]["host"] = outbound["server"].clone();
    servers[0]["port"] = outbound["server_port"].clone();
    servers[0].as_object_mut().unwrap().remove("connection");
    servers[0]["outbound"] = outbound;
    serde_json::to_vec(&file).unwrap()
}

#[test]
fn consistent_file_with_a_program_launching_outbound_is_rejected() {
    // sing-box 1.14.1 accepts this in `check` and starts the executable on `run`.
    let tor = json!({
        "type": "tor",
        "server": "example.com",
        "server_port": 443,
        "executable_path": "C:\\Windows\\System32\\calc.exe",
        "extra_args": ["--flag"],
    });
    let bytes = profile_with_outbound(tor);
    assert!(
        matches!(profile::decode(&bytes), Err(rift_core::Error::Invalid(_))),
        "a tor outbound must never reach the sidecar config"
    );
}

#[test]
fn outbounds_outside_the_parser_allowlist_are_rejected() {
    let uuid = "11111111-1111-4111-8111-111111111111";
    let base = |extra: Value| {
        let mut outbound =
            json!({"type":"vless","server":"example.com","server_port":443,"uuid":uuid});
        for (key, value) in extra.as_object().unwrap() {
            outbound[key] = value.clone();
        }
        outbound
    };
    // The control: the same shape without the extra key is accepted.
    assert!(profile::decode(&profile_with_outbound(base(json!({})))).is_ok());
    for outbound in [
        json!({"type":"direct","server":"example.com","server_port":443}),
        json!({"type":"selector","server":"example.com","server_port":443,"outbounds":["x"]}),
        json!({"type":"ssh","server":"example.com","server_port":22,"user":"root","private_key_path":"/etc/shadow"}),
        json!({"type":"socks","server":"example.com","server_port":1080}),
        base(json!({"detour": "direct"})),
        base(json!({"tag": "main"})),
        base(json!({"executable_path": "/bin/sh"})),
        base(json!({"extra_args": ["-c"]})),
        base(json!({"uuid": "not-a-uuid"})),
        base(json!({"flow": "xtls-rprx-direct"})),
        base(json!({"server_port": 70000})),
        base(json!({"server_port": 443.5})),
        base(json!({"server": "bad host"})),
        base(json!({"server": "a".repeat(254)})),
        base(json!({"tls": {"enabled": true, "server_name": "example.com", "insecure": true}})),
        base(
            json!({"tls": {"enabled": true, "server_name": "example.com", "certificate_path": "/etc/ssl/x"}}),
        ),
        base(
            json!({"tls": {"enabled": true, "server_name": "example.com", "utls": {"enabled": true, "fingerprint": "unknown"}}}),
        ),
        base(
            json!({"tls": {"enabled": true, "server_name": "example.com", "reality": {"enabled": true, "public_key": "short", "short_id": ""}}}),
        ),
        base(json!({"transport": {"type": "http", "host": ["example.com"]}})),
        base(json!({"transport": {"type": "ws", "path": "/", "early_data_header_name": "x"}})),
        base(json!({"transport": {"type": "ws", "path": "/", "headers": {"Host": "a", "X": "b"}}})),
        json!({"type":"trojan","server":"example.com","server_port":443,"password":"","extra":1}),
        json!({"type":"shadowsocks","server":"example.com","server_port":443,"method":"rot13","password":"x"}),
        json!({"type":"shadowsocks","server":"example.com","server_port":443,"method":"aes-256-gcm","password":"x","tls":{"enabled":true,"server_name":"example.com"}}),
        json!({"type":"vmess","server":"example.com","server_port":443,"uuid":uuid,"alter_id":-1,"security":"auto"}),
        json!({"type":"vmess","server":"example.com","server_port":443,"uuid":uuid,"alter_id":0,"security":"rot13"}),
        json!("tor"),
    ] {
        let bytes = profile_with_outbound(outbound.clone());
        assert!(
            profile::decode(&bytes).is_err(),
            "accepted outbound {outbound}"
        );
    }
}

#[test]
fn derived_row_fields_must_match_the_outbound() {
    let mutations: [(&str, Value); 6] = [
        ("protocol", json!("vless reality")),
        ("protocol", json!("direct")),
        ("host", json!("beacon.example.net")),
        ("port", json!(444)),
        ("host", json!("a".repeat(2 * 1024 * 1024))),
        ("protocol", json!("p".repeat(2 * 1024 * 1024))),
    ];
    for (field, value) in mutations {
        let mut file: Value =
            serde_json::from_slice(&profile::encode(&complete_store()).unwrap()).unwrap();
        file["profile"]["servers"][0][field] = value;
        assert!(
            profile::decode(&serde_json::to_vec(&file).unwrap()).is_err(),
            "accepted a mismatched {field}"
        );
    }
}

#[test]
fn country_and_subscription_error_are_bounded() {
    for country in ["XX1", "de", "D", &"A".repeat(1024 * 1024)] {
        let mut file: Value =
            serde_json::from_slice(&profile::encode(&complete_store()).unwrap()).unwrap();
        file["profile"]["servers"][0]["country"] = json!(country);
        assert!(
            profile::decode(&serde_json::to_vec(&file).unwrap()).is_err(),
            "accepted country of {} bytes",
            country.len()
        );
    }
    let mut file: Value =
        serde_json::from_slice(&profile::encode(&complete_store()).unwrap()).unwrap();
    file["profile"]["servers"][0]["country"] = json!("DE");
    file["profile"]["subscriptions"][0]["error"] = json!("Подписка недоступна");
    assert!(profile::decode(&serde_json::to_vec(&file).unwrap()).is_ok());
    file["profile"]["subscriptions"][0]["error"] = json!("e".repeat(1024 * 1024));
    assert!(profile::decode(&serde_json::to_vec(&file).unwrap()).is_err());
}

fn profile_with_connectivity_url(url: &str) -> Vec<u8> {
    let mut file: Value =
        serde_json::from_slice(&profile::encode(&complete_store()).unwrap()).unwrap();
    file["profile"]["settings"]["connectivity_url"] = json!(url);
    serde_json::to_vec(&file).unwrap()
}

#[test]
fn connectivity_url_is_bounded() {
    let base = "https://check.example/";
    let longest = format!("{base}{}", "a".repeat(2048 - base.len()));
    assert_eq!(longest.len(), 2048);
    let accepted = profile::decode(&profile_with_connectivity_url(&longest)).unwrap();
    assert_eq!(accepted.settings.connectivity_url, longest);

    for oversized in [
        format!("{longest}a"),
        format!("{base}{}", "a".repeat(1024 * 1024)),
    ] {
        assert!(
            profile::decode(&profile_with_connectivity_url(&oversized)).is_err(),
            "accepted a connectivity URL of {} bytes",
            oversized.len()
        );
    }

    // The persisted state is validated by the same settings rule.
    let mut store = Store::default();
    store.settings.connectivity_url = format!("{longest}a");
    assert!(persistence::decode(&serde_json::to_vec(&store).unwrap()).is_err());
    assert!(store.settings.validate().is_err());
}

#[test]
fn subscription_id_is_bounded() {
    let with_id = |id: &str| {
        let mut file: Value =
            serde_json::from_slice(&profile::encode(&complete_store()).unwrap()).unwrap();
        file["profile"]["subscriptions"][0]["id"] = json!(id);
        file["profile"]["servers"][1]["source"] = json!(id);
        serde_json::to_vec(&file).unwrap()
    };
    // The ids the application generates (`SaveSubscription`) are UUIDs.
    let uuid = uuid::Uuid::new_v4().to_string();
    assert_eq!(
        profile::decode(&with_id(&uuid)).unwrap().subscriptions[0].id,
        uuid
    );
    assert!(profile::decode(&with_id(&"i".repeat(128))).is_ok());
    for oversized in ["i".repeat(129), "i".repeat(1024 * 1024)] {
        assert!(
            profile::decode(&with_id(&oversized)).is_err(),
            "accepted a subscription id of {} bytes",
            oversized.len()
        );
    }
}

fn store_with_rows(count: usize, remark: &str) -> Store {
    let mut store = Store::default();
    for i in 0..count {
        let uri = format!(
            "trojan://pw{i}@host{i}.example.com:{}#{remark}",
            1000 + i % 60000
        );
        store.servers.push(parse_uri(&uri, None).unwrap());
    }
    store
}
fn envelope_len(store: &Store, pretty: bool) -> usize {
    let file = json!({"format": "rift-profile", "version": 1, "profile": store});
    if pretty {
        serde_json::to_vec_pretty(&file).unwrap().len()
    } else {
        serde_json::to_vec(&file).unwrap().len()
    }
}

#[test]
fn accepted_remarks_never_make_the_store_unexportable() {
    let long_ascii = "x".repeat(600);
    // 511 ASCII bytes plus one two-byte letter straddle the 512-byte limit.
    let straddling = format!("{}я", "a".repeat(511));
    let cyrillic = "я".repeat(400);
    for remark in [
        "%20".to_owned(),
        "%09%20".to_owned(),
        long_ascii.clone(),
        straddling,
        cyrillic,
    ] {
        let uri = format!("trojan://pw@remark.example.com:443#{remark}");
        let server = parse_uri(&uri, None).unwrap();
        assert!(!server.name.trim().is_empty());
        assert!(server.name.len() <= 512);
        let mut store = Store::default();
        store.servers.push(server);
        let restored = profile::decode(&profile::encode(&store).unwrap()).unwrap();
        assert_eq!(restored.servers[0].name, store.servers[0].name);
    }
    // A blank remark falls back to the host; a long one keeps its first 512 bytes.
    assert_eq!(
        parse_uri("trojan://pw@blank.example.com:443#%20", None)
            .unwrap()
            .name,
        "blank.example.com"
    );
    assert_eq!(
        parse_uri(
            &format!("trojan://pw@long.example.com:443#{long_ascii}"),
            None
        )
        .unwrap()
        .name,
        "x".repeat(512)
    );
}

#[test]
fn rows_saved_by_earlier_versions_with_blank_or_long_names_still_export() {
    let mut store = complete_store();
    store.servers[0].name = "  ".into();
    store.servers[1].name = "я".repeat(300);

    let restored = profile::decode(&profile::encode(&store).unwrap()).unwrap();

    assert_eq!(restored.servers[0].name, store.servers[0].host);
    assert_eq!(restored.servers[1].name.len(), 512);
    assert_eq!(restored.servers[1].name, "я".repeat(256));
}

#[test]
fn export_switches_to_compact_json_before_it_exceeds_the_import_limit() {
    let remark = "r".repeat(400);
    let probe = store_with_rows(200, &remark);
    let (compact_row, pretty_row) = (
        (envelope_len(&probe, false) - envelope_len(&Store::default(), false)) / 200,
        (envelope_len(&probe, true) - envelope_len(&Store::default(), true)) / 200,
    );
    // Midway between the two sizes: pretty form is too big, compact form fits.
    let rows = profile::MAX_BYTES * 2 / (compact_row + pretty_row);
    let store = store_with_rows(rows, &remark);
    assert!(
        envelope_len(&store, true) > profile::MAX_BYTES,
        "premise: pretty too big"
    );
    assert!(
        envelope_len(&store, false) <= profile::MAX_BYTES,
        "premise: compact fits"
    );

    let bytes = profile::encode(&store).unwrap();

    assert!(bytes.len() <= profile::MAX_BYTES);
    assert_eq!(profile::decode(&bytes).unwrap().servers.len(), rows);
}

#[test]
fn export_refuses_a_store_the_import_would_refuse() {
    let remark = "r".repeat(400);
    let probe = store_with_rows(200, &remark);
    let compact_row = (envelope_len(&probe, false) - envelope_len(&Store::default(), false)) / 200;
    let store = store_with_rows(profile::MAX_BYTES / compact_row + 100, &remark);
    assert!(
        envelope_len(&store, false) > profile::MAX_BYTES,
        "premise: compact too big"
    );

    match profile::encode(&store) {
        Err(rift_core::Error::Invalid(message)) => assert_eq!(message, profile::too_large()),
        Err(other) => panic!("unexpected error: {other}"),
        Ok(bytes) => panic!("encoded {} bytes that import refuses", bytes.len()),
    }
}

#[test]
fn fuzz_seed_with_a_server_is_a_valid_profile() {
    // Keeps the persisted_state corpus reaching the outbound allowlist instead of failing early.
    let store = profile::decode(include_bytes!(
        "../../fuzz/seeds/persisted_state/profile-v1-server.json"
    ))
    .unwrap();
    assert_eq!(store.servers.len(), 1);
    assert_eq!(
        store.selected.as_deref(),
        Some(store.servers[0].id.as_str())
    );
}

#[test]
fn error_texts_blame_the_file_only_for_file_problems() {
    use rift_core::{failure::FailureCode, Error};
    let rejected = profile::inspect_error_text(&Error::failure(
        FailureCode::SidecarConfigRejected,
        "sing-box отклонил конфигурацию",
    ));
    assert!(rejected.contains("неподдерживаемую конфигурацию"));
    for environment in [
        FailureCode::SidecarConfigTimeout,
        FailureCode::SidecarStartExit,
    ] {
        let text = profile::inspect_error_text(&Error::failure(environment, "x"));
        assert!(
            !text.contains("неподдерживаемую конфигурацию"),
            "{environment:?} blamed the config: {text}"
        );
    }
    assert_ne!(
        profile::inspect_error_text(&Error::failure(FailureCode::SidecarConfigTimeout, "x")),
        profile::inspect_error_text(&Error::failure(FailureCode::SidecarStartExit, "x")),
    );
    // A read failure and an unusable data directory are both `Persistence`: only the first
    // is the file's problem.
    let unreadable = profile::inspect_error_text(&Error::failure(
        FailureCode::Persistence,
        "Ошибка чтения или сохранения данных",
    ));
    assert_eq!(unreadable, "Не удалось прочитать профиль");
    let unavailable = profile::inspect_error_text(&Error::failure(
        FailureCode::Persistence,
        profile::check_unavailable(),
    ));
    assert_eq!(unavailable, profile::check_unavailable());
    assert!(!unavailable.contains("прочитать профиль"));
    // Content problems keep their own specific text.
    assert_eq!(
        profile::inspect_error_text(&Error::Invalid("Файл профиля больше 8 MiB".into())),
        "Файл профиля больше 8 MiB"
    );
    assert_eq!(
        profile::export_error_text(&Error::Invalid(profile::too_large().into())),
        profile::too_large()
    );
    assert_eq!(
        profile::export_error_text(&Error::failure(FailureCode::Persistence, "x")),
        "Не удалось сохранить профиль"
    );
}

#[test]
fn xray_profile_validation_errors_keep_distinct_safe_rejection_timeout_and_spawn_text() {
    use rift_core::{failure::FailureCode, Error};
    for (code, expected) in [
        (
            FailureCode::XrayConfigRejected,
            "Профиль содержит неподдерживаемую конфигурацию Xray",
        ),
        (
            FailureCode::XrayConfigTimeout,
            "Проверка профиля Xray не завершилась вовремя; повторите попытку",
        ),
        (
            FailureCode::XrayStartExit,
            "Не удалось запустить Xray для проверки профиля",
        ),
    ] {
        let text =
            profile::inspect_error_text(&Error::failure(code, "synthetic-secret-child-output"));
        assert_eq!(text, expected);
        assert!(!text.contains("synthetic-secret-child-output"));
    }
}
