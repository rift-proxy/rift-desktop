use rift_core::{
    config::Resolver,
    failure::FailureCode,
    geodata::GeoSet,
    launch::LaunchPlan,
    model::{Mode, Store},
    parser::parse_uri,
};
use serde_json::json;
const SECRET: &str = "synthetic-launch-secret-01234567890123456789";
fn store() -> Store {
    let mut s = Store::default();
    s.servers
        .push(parse_uri("trojan://synthetic@server.example:443", None).unwrap());
    s.selected = Some(s.servers[0].id.clone());
    s
}
fn plan(s: &Store) -> rift_core::Result<LaunchPlan> {
    LaunchPlan::build(
        s,
        SECRET,
        &GeoSet::none(),
        Resolver::Bridge(12344),
        12345,
        SECRET,
    )
}

#[test]
fn production_plan_removes_remote_protocols_from_sing_box_without_editing_store() {
    for mode in [Mode::Proxy, Mode::Tun, Mode::TunProxy] {
        let mut s = store();
        s.settings.mode = mode.clone();
        let original = serde_json::to_value(&s).unwrap();
        let p = plan(&s).unwrap();
        assert!(p.sing_box()["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .all(|o| ["socks", "selector", "direct"].contains(&o["type"].as_str().unwrap())));
        assert_eq!(p.sing_box()["outbounds"][0]["server"], "127.0.0.1");
        assert_eq!(p.sing_box()["outbounds"][0]["server_port"], 12345);
        assert_eq!(p.sing_box()["outbounds"][0]["tag"], s.servers[0].id);
        assert_eq!(p.xray()["outbounds"][0]["protocol"], "blackhole");
        assert_eq!(
            p.xray()["outbounds"][1]["settings"]["servers"][0]["address"],
            "server.example"
        );
        assert_eq!(
            p.xray()["outbounds"][1]["streamSettings"]["sockopt"]["domainStrategy"],
            "ForceIP"
        );
        assert_eq!(
            p.xray()["dns"]["servers"][0]["address"],
            "tcp+local://127.0.0.1:12344"
        );
        assert_eq!(p.xray()["dns"]["servers"][0]["timeoutMs"], 15000);
        assert_eq!(p.requires_interface(), mode != Mode::Proxy);
        if mode != Mode::Proxy {
            let p = p.with_interface("Ethernet").unwrap();
            assert!(!p.requires_interface());
            assert_eq!(
                p.xray()["outbounds"][1]["streamSettings"]["sockopt"]["interface"],
                "Ethernet"
            );
        }
        assert_eq!(serde_json::to_value(&s).unwrap(), original);
    }
}

#[test]
fn tun_binding_cannot_use_our_own_adapter_and_loopback_does_not_leave_the_machine() {
    let mut s = store();
    s.settings.mode = Mode::Tun;
    assert!(plan(&s).unwrap().with_interface("Rift").is_err());
    s.servers = vec![parse_uri("trojan://synthetic@127.0.0.1:443", None).unwrap()];
    s.selected = Some(s.servers[0].id.clone());
    let p = plan(&s).unwrap();
    assert!(!p.requires_interface());
    assert!(p.xray()["outbounds"][1]["streamSettings"]["sockopt"]
        .get("interface")
        .is_none());
}

#[test]
fn every_candidate_has_its_own_authenticated_route_and_no_shared_mutable_selector() {
    let mut s = store();
    s.servers
        .push(parse_uri("trojan://different@other.example:443", None).unwrap());
    let p = plan(&s).unwrap();
    let accounts = p.xray()["inbounds"][0]["settings"]["accounts"]
        .as_array()
        .unwrap();
    let rules = p.xray()["routing"]["rules"].as_array().unwrap();
    assert_eq!(accounts.len(), 2);
    assert_eq!(rules.len(), 2);
    assert_ne!(accounts[0]["user"], accounts[1]["user"]);
    for (index, account) in accounts.iter().enumerate() {
        assert_eq!(account["pass"], SECRET);
        assert_eq!(rules[index]["user"], json!([account["user"]]));
        assert_eq!(
            rules[index]["outboundTag"],
            p.xray()["outbounds"][index + 1]["tag"]
        );
        assert_eq!(
            account["user"],
            p.sing_box()["outbounds"][index]["username"]
        );
    }
}

#[test]
fn retired_unselected_record_is_preserved_but_cannot_become_a_remote_outbound() {
    let mut s = store();
    s.servers
        .push(parse_uri("ss://aes-128-cfb:synthetic@legacy.example:443", None).unwrap());
    let original = serde_json::to_value(&s).unwrap();
    let p = plan(&s).unwrap();
    assert_eq!(p.retired_count(), 1);
    assert_eq!(p.xray()["outbounds"].as_array().unwrap().len(), 2);
    assert_eq!(serde_json::to_value(&s).unwrap(), original);
    s.selected = Some(s.servers[1].id.clone());
    let error = plan(&s).err().unwrap();
    assert_eq!(
        error.failure_code(),
        Some(FailureCode::UnsupportedLegacyParameters)
    );
}

#[test]
fn invalid_or_unauthenticated_channels_are_rejected_before_start() {
    let s = store();
    for (resolver, port, pass) in [
        (Resolver::System, 12345, SECRET),
        (Resolver::Bridge(1), 0, SECRET),
        (Resolver::Bridge(12345), 12345, SECRET),
        (Resolver::Bridge(1), 2081, SECRET),
        (Resolver::Bridge(1), 12345, "short"),
        (
            Resolver::Bridge(1),
            12345,
            "invalid-handoff-password-0123456789:secret",
        ),
    ] {
        assert!(LaunchPlan::build(&s, SECRET, &GeoSet::none(), resolver, port, pass).is_err());
    }
}

#[test]
fn source_owned_rows_require_their_actual_subscription() {
    let mut s = store();
    s.servers[0].source = Some("synthetic".into());
    assert!(plan(&s).is_err());
    s.subscriptions.push(rift_core::model::Subscription {
        id: "synthetic".into(),
        name: "Synthetic".into(),
        url: "https://subscriptions.example/synthetic".into(),
        ..Default::default()
    });
    assert_eq!(
        plan(&s).unwrap().xray()["outbounds"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
