//! The connections page: sing-box's `/connections` read into a few typed fields, shown while the
//! page is open and kept nowhere else.
mod common;

use common::{
    clash::{serve, Api, Reply},
    fake::start_on,
};
use proptest::prelude::*;
use rift_core::{
    application::Action,
    clash::{unix_from_rfc3339, view, Clash, MAX_CONNECTIONS},
    failure::FailureCode,
    jitter::Jitter,
};
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const SECRET: &str = "connections-secret-0123456789abcdef";

fn item(host: &str, start: &str, chains: &[&str]) -> Value {
    json!({
        "id": "c3a9c4a8-0000-0000-0000-000000000001",
        "metadata": {
            "network": "tcp", "type": "http",
            "sourceIP": "192.168.1.55", "sourcePort": "50321",
            "destinationIP": "93.184.216.34", "destinationPort": "443",
            "host": host, "dnsMode": "normal",
            "processPath": "C:\\Program Files\\Some Vendor\\App\\app.exe",
            "specialProxy": ""
        },
        "upload": 1200, "download": 34000, "start": start,
        "chains": chains, "rule": "final", "rulePayload": "secret-rule-payload"
    })
}
fn body(items: &[Value]) -> String {
    json!({"downloadTotal": 1, "uploadTotal": 2, "memory": 3, "connections": items}).to_string()
}
async fn client_for(reply: Reply) -> (Arc<Api>, Clash) {
    let api = Arc::new(Api::default());
    api.route("/connections", reply);
    let port = serve(api.clone()).await;
    (api, Clash::new(port, SECRET).unwrap())
}
fn ok(body: String) -> Reply {
    Reply::Body("200 OK", body)
}
fn at(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}
const NOW: u64 = 1_792_238_460; // 2026-10-17T12:01:00Z
fn names() -> Vec<(String, String)> {
    vec![
        ("srv-1".into(), "Frankfurt 01".into()),
        ("srv-2".into(), "Helsinki 02".into()),
    ]
}

#[test]
fn rfc3339_times_become_unix_seconds() {
    for (text, expected) in [
        ("2026-10-17T12:00:00Z", Some(1_792_238_400)),
        ("2026-10-17T12:00:00.123456789Z", Some(1_792_238_400)),
        ("2026-10-17T12:00:00+03:00", Some(1_792_227_600)),
        ("2026-10-17T12:00:00.5-05:30", Some(1_792_258_200)),
        ("2024-02-29T23:59:59Z", Some(1_709_251_199)),
        ("1970-01-01T00:00:00Z", Some(0)),
        ("2038-01-19T03:14:08Z", Some(2_147_483_648)),
        ("2026-10-17t12:00:00z", Some(1_792_238_400)),
        // Not a time, or not one this reader accepts.
        ("", None),
        ("yesterday", None),
        ("2026-10-17", None),
        ("2026-10-17T12:00:00", None),
        ("2026-13-17T12:00:00Z", None),
        ("2026-10-32T12:00:00Z", None),
        ("2026-10-17T24:00:00Z", None),
        ("2026-10-17T12:60:00Z", None),
        ("2026-10-17T12:00:00+3:00", None),
        ("2026-10-17T12:00:00+25:00", None),
        ("2026-10-17T12:00:00ZZ", None),
        ("1969-12-31T23:59:59Z", None),
        ("2026-1a-17T12:00:00Z", None),
        ("２０２６-10-17T12:00:00Z", None),
    ] {
        assert_eq!(unix_from_rfc3339(text), expected, "{text:?}");
    }
}

proptest! {
    #[test]
    fn no_text_makes_the_time_reader_panic(text in ".{0,48}") {
        let _ = unix_from_rfc3339(&text);
    }
    #[test]
    fn a_valid_time_round_trips_through_its_own_digits(
        year in 1971u32..2200, month in 1u32..=12, day in 1u32..=28,
        hour in 0u32..24, minute in 0u32..60, second in 0u32..60,
    ) {
        let text = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z");
        let later = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}+01:00");
        let a = unix_from_rfc3339(&text).unwrap();
        let b = unix_from_rfc3339(&later).unwrap();
        prop_assert_eq!(a, b + 3600, "an hour ahead of UTC is an hour earlier");
    }
}

#[tokio::test]
async fn only_the_allowed_fields_reach_the_screen() {
    let (api, clash) = client_for(ok(body(&[item(
        "example.com",
        "2026-10-17T12:00:30.5Z",
        &["srv-1", "main"],
    )])))
    .await;
    let fetched = clash.connections().await.unwrap();
    let shown = view(fetched, &names(), at(NOW));
    assert_eq!(shown.total, 1);
    let text = serde_json::to_string(&shown).unwrap();
    let row = &shown.items[0];
    assert_eq!(
        (row.host.as_str(), row.port, row.network, row.route),
        ("example.com", 443, "tcp", "proxy")
    );
    assert_eq!(row.server.as_deref(), Some("Frankfurt 01"));
    assert_eq!(
        row.process.as_deref(),
        Some("app.exe"),
        "the name, not the path"
    );
    assert_eq!(
        (row.upload, row.download, row.seconds),
        (1200, 34000, Some(30))
    );
    // Everything else sing-box said stays behind.
    for private in [
        "192.168.1.55",
        "50321",
        "93.184.216.34",
        "Program Files",
        "Some Vendor",
        "secret-rule-payload",
        "c3a9c4a8",
        "srv-1",
    ] {
        assert!(
            !text.contains(private),
            "{private} reached the screen: {text}"
        );
    }
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap()["items"][0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        [
            "host", "port", "network", "route", "server", "process", "upload", "download",
            "seconds"
        ]
        .into_iter()
        .collect()
    );
    // The secret goes as a bearer token only.
    let head = api.heads().remove(0).to_lowercase();
    assert!(head.starts_with("get /connections "), "{head}");
    assert!(head.contains(&format!("authorization: bearer {}", SECRET.to_lowercase())));
}

#[tokio::test]
async fn the_route_and_the_server_come_from_the_chain_in_either_order() {
    let (_api, clash) = client_for(ok(body(&[
        item("a", "2026-10-17T12:00:01Z", &["main", "srv-2"]),
        item("b", "2026-10-17T12:00:02Z", &["srv-1", "main"]),
        item("c", "2026-10-17T12:00:03Z", &["direct"]),
        item("d", "2026-10-17T12:00:04Z", &["main", "who-knows"]),
        item("e", "2026-10-17T12:00:05Z", &[]),
    ])))
    .await;
    let shown = view(clash.connections().await.unwrap(), &names(), at(NOW));
    let rows: Vec<_> = shown
        .items
        .iter()
        .map(|r| (r.host.as_str(), r.route, r.server.as_deref()))
        .collect();
    // Newest first.
    assert_eq!(
        rows,
        [
            ("e", "other", None),
            ("d", "other", None),
            ("c", "direct", None),
            ("b", "proxy", Some("Frankfurt 01")),
            ("a", "proxy", Some("Helsinki 02")),
        ]
    );
}

#[tokio::test]
async fn the_host_is_the_address_when_the_name_is_unknown_and_the_port_reads_either_way() {
    let mut named = item("", "2026-10-17T12:00:01Z", &["direct"]);
    named["metadata"]["destinationPort"] = json!(8443);
    let mut text_port = item("", "2026-10-17T12:00:02Z", &["direct"]);
    text_port["metadata"]["destinationPort"] = json!("not a number");
    let mut no_port = item("host", "2026-10-17T12:00:03Z", &["direct"]);
    no_port["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("destinationPort");
    let (_api, clash) = client_for(ok(body(&[named, text_port, no_port]))).await;
    let shown = view(clash.connections().await.unwrap(), &[], at(NOW));
    let rows: Vec<_> = shown
        .items
        .iter()
        .map(|r| (r.host.as_str(), r.port))
        .collect();
    assert_eq!(
        rows,
        [("host", 0), ("93.184.216.34", 0), ("93.184.216.34", 8443)]
    );
}

#[tokio::test]
async fn text_from_the_network_is_cleaned_and_cut() {
    let long_host = format!("{}.example", "h".repeat(400));
    let mut hostile = item(&long_host, "2026-10-17T12:00:01Z", &["srv-1"]);
    hostile["metadata"]["processPath"] = json!(format!(
        "/opt/{}/bin\u{7}ary\n{}",
        "d".repeat(50),
        "p".repeat(300)
    ));
    hostile["metadata"]["network"] = json!("QUIC");
    let mut named = item("x", "2026-10-17T12:00:02Z", &["srv-long"]);
    named["metadata"]["network"] = json!("UDP");
    let (_api, clash) = client_for(ok(body(&[hostile, named]))).await;
    let long_name = format!("{}\u{1b}[31m", "n".repeat(300));
    let shown = view(
        clash.connections().await.unwrap(),
        &[
            ("srv-1".into(), "ok".into()),
            ("srv-long".into(), long_name),
        ],
        at(NOW),
    );
    let (udp, other) = (&shown.items[0], &shown.items[1]);
    assert_eq!(udp.network, "udp", "case folded");
    assert_eq!(udp.server.as_ref().unwrap().chars().count(), 120);
    assert!(!udp.server.as_ref().unwrap().contains('\u{1b}'));
    assert_eq!(
        other.network, "other",
        "a protocol the page has no word for"
    );
    assert_eq!(other.host.chars().count(), 253);
    let process = other.process.as_ref().unwrap();
    assert_eq!(process.chars().count(), 128);
    assert!(process.chars().all(|c| !c.is_control()));
}

#[tokio::test]
async fn a_busy_machine_gives_the_newest_five_hundred_and_the_count_of_all() {
    let items: Vec<Value> = (0..800)
        .map(|i| {
            let start = format!("2026-10-17T12:{:02}:{:02}Z", i / 60 % 60, i % 60);
            item(&format!("host-{i}"), &start, &["direct"])
        })
        .collect();
    let (_api, clash) = client_for(ok(body(&items))).await;
    let shown = view(clash.connections().await.unwrap(), &[], at(NOW + 3600));
    assert_eq!(shown.total, 800);
    assert_eq!(shown.items.len(), MAX_CONNECTIONS);
    assert_eq!(shown.items[0].host, "host-799");
    assert_eq!(shown.items[MAX_CONNECTIONS - 1].host, "host-300");
}

#[tokio::test]
async fn a_connection_without_a_known_start_has_no_duration_and_goes_last() {
    let (_api, clash) = client_for(ok(body(&[
        item("unknown", "not a time", &["direct"]),
        item("known", "2026-10-17T12:00:30Z", &["direct"]),
    ])))
    .await;
    let shown = view(clash.connections().await.unwrap(), &[], at(NOW));
    assert_eq!(shown.items[0].host, "known");
    assert_eq!(shown.items[0].seconds, Some(30));
    assert_eq!(shown.items[1].host, "unknown");
    assert_eq!(shown.items[1].seconds, None);
    // A start in the future (clocks differ) is a zero, not a wrapped number.
    let future = view(clash.connections().await.unwrap(), &[], at(NOW - 3600));
    assert_eq!(future.items[0].seconds, Some(0));
}

#[tokio::test]
async fn an_empty_or_minimal_answer_is_an_empty_list() {
    for text in [
        r#"{"connections":[]}"#,
        r#"{}"#,
        r#"{"connections":null,"x":1}"#,
    ] {
        let (_api, clash) = client_for(ok(text.into())).await;
        let shown = view(clash.connections().await.unwrap(), &[], at(NOW));
        assert_eq!((shown.total, shown.items.len()), (0, 0), "{text}");
    }
}

#[tokio::test]
async fn answers_that_are_not_the_list_are_errors_with_one_code() {
    let oversized = format!(
        r#"{{"connections":[],"pad":"{}"}}"#,
        "x".repeat(17 * 1024 * 1024)
    );
    for (name, reply) in [
        ("refused", Reply::Body("401 Unauthorized", "{}".into())),
        ("not json", ok("<html>".into())),
        ("wrong shape", ok(r#"{"connections":"many"}"#.into())),
        (
            "negative size",
            ok(r#"{"connections":[{"upload":-1}]}"#.into()),
        ),
        ("too large", ok(oversized)),
    ] {
        let (_api, clash) = client_for(reply).await;
        let error = clash.connections().await.unwrap_err();
        assert_eq!(error.failure_code(), Some(FailureCode::ClashApi), "{name}");
    }
}

fn connections_item(host: &str) -> Value {
    item(host, "2026-10-17T12:00:30Z", &["direct"])
}
async fn api_for_app(items: &[Value]) -> (Arc<Api>, u16) {
    let api = Arc::new(Api::default());
    api.route("/connections", ok(body(items)));
    let port = serve(api.clone()).await;
    (api, port)
}
async fn settle(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

#[tokio::test]
async fn the_page_asks_every_two_seconds_only_while_it_is_open() {
    let (api, port) = api_for_app(&[connections_item("secret-host.example")]).await;
    let h = start_on(false, Jitter::none(), |s| s.settings.api_port = port);
    h.app.execute(Action::Connect).await.unwrap();
    settle(500).await;
    assert_eq!(api.count("/connections"), 0, "closed page, no request");

    h.app.watch_connections(true);
    let mut rx = h.app.connections();
    let first = tokio::time::timeout(Duration::from_secs(3), rx.wait_for(|v| v.is_some()))
        .await
        .expect("no list")
        .unwrap()
        .clone()
        .unwrap();
    assert_eq!(first.items[0].host, "secret-host.example");
    settle(2300).await;
    assert!(
        api.count("/connections") >= 2,
        "asked again after about two seconds"
    );
    assert!(api.count("/connections") <= 3, "and not much oftener");

    h.app.watch_connections(false);
    tokio::time::timeout(Duration::from_secs(3), rx.wait_for(|v| v.is_none()))
        .await
        .expect("the list stayed after the page closed")
        .unwrap();
    let asked = api.count("/connections");
    settle(2500).await;
    assert_eq!(
        api.count("/connections"),
        asked,
        "closed page, no more requests"
    );
}

#[tokio::test]
async fn disconnecting_clears_the_list_and_stops_the_asking() {
    let (api, port) = api_for_app(&[connections_item("h.example")]).await;
    let h = start_on(false, Jitter::none(), |s| s.settings.api_port = port);
    h.app.watch_connections(true);
    settle(300).await;
    assert_eq!(
        api.count("/connections"),
        0,
        "not connected, nothing to ask"
    );
    h.app.execute(Action::Connect).await.unwrap();
    let mut rx = h.app.connections();
    tokio::time::timeout(Duration::from_secs(3), rx.wait_for(|v| v.is_some()))
        .await
        .expect("no list")
        .unwrap();
    h.app.execute(Action::Disconnect).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), rx.wait_for(|v| v.is_none()))
        .await
        .expect("the list stayed after Disconnect")
        .unwrap();
    let asked = api.count("/connections");
    settle(2500).await;
    assert_eq!(api.count("/connections"), asked);
}

#[tokio::test]
async fn the_connections_leave_no_trace_in_the_snapshot_the_journal_the_files_or_the_report() {
    let (_api, port) = api_for_app(&[connections_item("traces-of-me.example")]).await;
    let h = start_on(false, Jitter::none(), |s| s.settings.api_port = port);
    h.app.execute(Action::Connect).await.unwrap();
    h.app.watch_connections(true);
    let mut rx = h.app.connections();
    tokio::time::timeout(Duration::from_secs(3), rx.wait_for(|v| v.is_some()))
        .await
        .expect("no list")
        .unwrap();
    h.app.execute(Action::ExportDiagnostics).await.unwrap();
    h.app.watch_connections(false);
    settle(200).await;

    let mut everything = serde_json::to_string(&h.app.snapshot()).unwrap();
    for entry in std::fs::read_dir(h._dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            everything.push_str(&String::from_utf8_lossy(&std::fs::read(path).unwrap()));
        }
    }
    for private in ["traces-of-me", "app.exe", "Some Vendor", "93.184.216.34"] {
        assert!(!everything.contains(private), "{private} was kept");
    }
}

#[tokio::test]
async fn three_failures_in_a_row_are_one_error() {
    let api = Arc::new(Api::default());
    api.route(
        "/connections",
        Reply::Body("500 Internal Server Error", "{}".into()),
    );
    let port = serve(api.clone()).await;
    let h = start_on(false, Jitter::fixed(-10_000), |s| {
        s.settings.api_port = port
    });
    h.app.execute(Action::Connect).await.unwrap();
    h.app.watch_connections(true);
    tokio::time::timeout(Duration::from_secs(8), async {
        while api.count("/connections") < 4 {
            settle(50).await;
        }
    })
    .await
    .expect("fewer than four attempts");
    let errors: Vec<_> = h
        .app
        .snapshot()
        .journal
        .into_iter()
        .filter(|e| e.failure_code == Some(FailureCode::ClashApi))
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].message, "Не удалось получить список соединений");
}
