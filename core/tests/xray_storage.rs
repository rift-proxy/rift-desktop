use rift_core::{
    elevation::helper,
    model::{Mode, Store, Subscription},
    parser::parse_uri,
    persistence, profile,
};
use serde_json::{json, Value};

fn store() -> Store {
    let mut store = Store::default();
    store
        .servers
        .push(parse_uri("trojan://synthetic@server.example:443", None).unwrap());
    store.selected = Some(store.servers[0].id.clone());
    store
}
fn wire() -> Value {
    serde_json::from_slice(&profile::encode(&store()).unwrap()).unwrap()
}

#[test]
fn neutral_connection_is_closed_at_every_nested_boundary() {
    let valid = wire();
    assert_eq!(valid["version"], 2);
    let node = &valid["profile"]["servers"][0]["connection"]["node"];
    assert_eq!(node["protocol"]["kind"], "trojan");
    assert_eq!(node["security"]["kind"], "tls");
    for path in [
        vec!["profile", "servers", "0", "connection", "command"],
        vec![
            "profile",
            "servers",
            "0",
            "connection",
            "node",
            "executable_path",
        ],
        vec![
            "profile",
            "servers",
            "0",
            "connection",
            "node",
            "protocol",
            "inbounds",
        ],
        vec![
            "profile",
            "servers",
            "0",
            "connection",
            "node",
            "transport",
            "path",
        ],
        vec![
            "profile",
            "servers",
            "0",
            "connection",
            "node",
            "security",
            "allowInsecure",
        ],
    ] {
        let mut malicious = valid.clone();
        let mut at = &mut malicious;
        for key in &path[..path.len() - 1] {
            at = if *key == "0" {
                &mut at[0]
            } else {
                &mut at[*key]
            };
        }
        at[path[path.len() - 1]] = json!("synthetic-unknown");
        let bytes = serde_json::to_vec(&malicious).unwrap();
        assert!(profile::decode(&bytes).is_err(), "unknown path {path:?}");
        assert!(persistence::decode(&serde_json::to_vec(&malicious["profile"]).unwrap()).is_err());
    }
    let mut plain = store();
    plain.servers =
        vec![parse_uri("trojan://synthetic@server.example:443?security=none", None).unwrap()];
    plain.selected = Some(plain.servers[0].id.clone());
    let mut wire: Value = serde_json::from_slice(&profile::encode(&plain).unwrap()).unwrap();
    assert!(profile::decode(&serde_json::to_vec(&wire).unwrap()).is_ok());
    wire["profile"]["servers"][0]["connection"]["node"]["security"]["allowInsecure"] = json!(true);
    assert!(profile::decode(&serde_json::to_vec(&wire).unwrap()).is_err());
}

#[test]
fn invalid_future_duplicate_or_forged_neutral_data_cannot_reach_profile_or_helper() {
    let valid = wire();
    for (path, value) in [
        (vec!["version"], json!(3)),
        (vec!["profile", "version"], json!(3)),
        (
            vec!["profile", "servers", "0", "connection", "version"],
            json!(2),
        ),
        (
            vec!["profile", "servers", "0", "connection", "node", "port"],
            json!(0),
        ),
        (
            vec!["profile", "servers", "0", "connection", "node", "host"],
            json!(""),
        ),
        (
            vec![
                "profile",
                "servers",
                "0",
                "connection",
                "node",
                "protocol",
                "kind",
            ],
            json!("exec"),
        ),
        (
            vec![
                "profile",
                "servers",
                "0",
                "connection",
                "node",
                "security",
                "fingerprint",
            ],
            json!("unknown"),
        ),
        (
            vec!["profile", "servers", "0", "host"],
            json!("forged.example"),
        ),
        (vec!["profile", "servers", "0", "id"], json!("forged")),
    ] {
        let mut malicious = valid.clone();
        let mut at = &mut malicious;
        for key in &path[..path.len() - 1] {
            at = if *key == "0" {
                &mut at[0]
            } else {
                &mut at[*key]
            };
        }
        at[path[path.len() - 1]] = value;
        assert!(
            profile::decode(&serde_json::to_vec(&malicious).unwrap()).is_err(),
            "path {path:?}"
        );
        if path[0] == "profile" {
            if let Ok(mut store) = serde_json::from_value::<Store>(malicious["profile"].clone()) {
                store.settings.mode = Mode::Tun;
                assert!(helper::vet(&store, "0123456789abcdef0123456789abcdef").is_err());
            }
        }
    }
    let mut duplicate = valid;
    duplicate["profile"]["servers"][0]["outbound"] = store().servers[0].outbound.clone();
    assert!(profile::decode(&serde_json::to_vec(&duplicate).unwrap()).is_err());
    duplicate["profile"]["servers"][0]["outbound"] = Value::Null;
    assert!(profile::decode(&serde_json::to_vec(&duplicate).unwrap()).is_err());
}

#[test]
fn old_noncanonical_objects_and_metadata_remain_lossless_without_becoming_trusted_nodes() {
    let fixture: Value =
        serde_json::from_slice(include_bytes!("fixtures/xray-store-v1.json")).unwrap();
    for payload in [
        fixture["state"]["servers"][0]["outbound"].clone(),
        json!({"type":"unrecognized","format":"opaque-provider-value","extra":0.9665220325508706}),
    ] {
        let mut state = fixture["state"].clone();
        state["servers"][0]["id"] = json!("old-noncanonical-id");
        state["servers"][0]["outbound"] = payload.clone();
        let migrated = persistence::decode(&serde_json::to_vec(&state).unwrap()).unwrap();
        let written = serde_json::to_value(&migrated).unwrap();
        assert_eq!(
            written["servers"][0]["connection"]["format"],
            "rift-legacy-connection"
        );
        let reread = persistence::decode(&serde_json::to_vec(&written).unwrap()).unwrap();
        assert_eq!(reread.servers[0].outbound, payload);
        assert_eq!(reread.servers[0].id, "old-noncanonical-id");
        assert_eq!(serde_json::to_value(&reread).unwrap(), written);
        assert!(
            profile::encode(&reread).is_err(),
            "opaque data cannot bypass the profile/helper boundary"
        );
    }
}

#[test]
fn source_owned_duplicates_and_selection_survive_neutral_round_trip() {
    let mut store = store();
    store.subscriptions.push(Subscription {
        id: "synthetic-source".into(),
        name: "Synthetic".into(),
        url: "https://subscriptions.example/synthetic".into(),
        ..Default::default()
    });
    let mut copy = store.servers[0].clone();
    copy.source = Some("synthetic-source".into());
    store.excluded.push(copy.stable_key());
    store.servers.push(copy);
    let restored = profile::decode(&profile::encode(&store).unwrap()).unwrap();
    assert_eq!(restored.servers.len(), 2);
    assert_eq!(restored.servers[0].id, restored.servers[1].id);
    assert_ne!(restored.servers[0].source, restored.servers[1].source);
    assert_eq!(
        serde_json::to_value(&restored).unwrap(),
        serde_json::to_value(&store).unwrap()
    );
}

#[test]
fn invalid_save_keeps_original_v1_and_creates_no_backup_or_partial_file() {
    let fixture: Value =
        serde_json::from_slice(include_bytes!("fixtures/xray-store-v1.json")).unwrap();
    let original = serde_json::to_vec(&fixture["state"]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, &original).unwrap();
    let mut loaded = persistence::load(&path).unwrap();
    loaded.settings.proxy_port = 0;
    assert!(persistence::save(&path, &loaded).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn retired_rows_stay_in_storage_but_leave_candidates_and_implicit_default() {
    let mut s = store();
    let healthy = s.servers[0].id.clone();
    let mut retired = parse_uri("ss://aes-128-cfb:synthetic@legacy.example:443", None).unwrap();
    retired.routes = Some(rift_core::routing::ServerRoutes {
        block: vec!["udp:443".into()],
        ..Default::default()
    });
    let retired_id = retired.id.clone();
    s.servers.insert(0, retired);
    s.selected = None;
    let before = serde_json::to_value(&s).unwrap();
    assert_eq!(
        s.route_entries(),
        1,
        "retired rows still count against storage safety bounds"
    );
    assert_eq!(
        s.auto_candidates(0)
            .iter()
            .map(|s| s.id.clone())
            .collect::<Vec<_>>(),
        vec![healthy.clone()]
    );
    assert_eq!(s.live_selection(), healthy);
    let view = s.servers[0].summary();
    assert_eq!(view.status, "failed");
    assert_eq!(
        view.failure_code,
        Some(rift_core::failure::FailureCode::UnsupportedLegacyParameters)
    );
    assert!(view.proxy_ms.is_none() && view.endpoint_ms.is_none() && view.retry_at.is_none());
    assert_eq!(serde_json::to_value(&s).unwrap(), before);
    s.selected = Some(retired_id.clone());
    assert_eq!(
        s.live_selection(),
        retired_id,
        "explicit choice is not silently replaced"
    );
    assert_eq!(
        profile::decode(&profile::encode(&s).unwrap())
            .unwrap()
            .servers
            .len(),
        2
    );
}

#[test]
fn identity_consistent_noncanonical_xhttp_survives_save_and_export() {
    for transport in [
        json!({"type":"xhttp","path":"/p"}),
        json!({"type":"xhttp","mode":"","path":"/p"}),
        json!({"type":"xhttp","mode":"auto"}),
        json!({"type":"xhttp","mode":"auto","path":""}),
        json!({"type":"xhttp","mode":"auto","path":"/p","host":""}),
        json!({"type":"xhttp","mode":"auto","path":"/p","host":null}),
        json!({"type":"xhttp","mode":"auto","path":"/p","extra":null}),
    ] {
        let mut state = store();
        let mut server = parse_uri(
            "vless://00000000-0000-0000-0000-000000000001@server.example:443?security=tls&type=xhttp",
            None,
        )
        .unwrap();
        server.outbound["transport"] = transport.clone();
        server.id = rift_core::model::Server::fingerprint(&server.outbound);
        state.selected = Some(server.id.clone());
        state.servers = vec![server.clone()];
        let exportable = profile::validate(&state).is_ok();
        assert_eq!(exportable, transport.get("extra").is_none());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        persistence::save(&path, &store()).unwrap();
        persistence::save(&path, &state).unwrap();
        let restored = persistence::load(&path).unwrap();
        assert_eq!(restored.servers[0].outbound, server.outbound, "{transport}");
        assert_eq!(restored.servers[0].id, server.id);
        if !exportable {
            assert!(profile::encode(&state).is_err());
            continue;
        }
        let exported = profile::encode(&state).unwrap();
        let restored = profile::decode(&exported).unwrap();
        assert_eq!(restored.servers[0].outbound, server.outbound);
        assert_eq!(restored.selected, state.selected);
        let wire: Value = serde_json::from_slice(&exported).unwrap();
        assert_eq!(
            wire["profile"]["servers"][0]["connection"]["format"],
            "rift-legacy-connection"
        );
    }
}

#[cfg(unix)]
#[test]
fn repeated_failed_v1_migration_keeps_only_one_exact_backup() {
    const CHILD: &str = "RIFT_V1_BACKUP_FAILURE_TEST_DIR";
    if let Some(dir) = std::env::var_os(CHILD) {
        let path = std::path::PathBuf::from(dir).join("state.json");
        let state = persistence::load(&path).unwrap();
        for _ in 0..3 {
            assert!(persistence::save(&path, &state).is_err());
        }
        return;
    }
    let fixture: Value =
        serde_json::from_slice(include_bytes!("fixtures/xray-store-v1.json")).unwrap();
    let original = serde_json::to_vec(&fixture["state"]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, &original).unwrap();
    let loaded = persistence::load(&path).unwrap();
    let migrated = serde_json::to_vec_pretty(&loaded).unwrap();
    assert!(original.len() < 3072 && migrated.len() > 3072);
    let output = std::process::Command::new("bash")
        .args(["-c", "trap '' XFSZ; ulimit -f 3; exec \"$@\"", "--"])
        // Unprivileged self-spawn of this test harness, not a trusted helper or authorization path.
        // nosemgrep: rust.lang.security.current-exe.current-exe
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "repeated_failed_v1_migration_keeps_only_one_exact_backup",
            "--nocapture",
        ])
        .env(CHILD, dir.path())
        // This child's file-size limit would truncate its coverage profile too. The parent
        // remains instrumented; discard only the intentionally resource-limited child's data.
        .env("LLVM_PROFILE_FILE", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let backups: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "bak"))
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}
