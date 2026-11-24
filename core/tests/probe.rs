//! NetworkProbe against a loopback fake of the sing-box Clash API: request shape, auth and
//! answer parsing, without real sing-box or external network.
use rift_core::{
    failure::FailureCode,
    model::Settings,
    parser::parse_uri,
    probe::NetworkProbe,
    selection::{Network, Probe},
};
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const SECRET: &str = "probe-secret-0123456789012345678901";

/// Serves every connection with `status` and `body`; returns the port and the requests seen.
async fn fake_api(status: &'static str, body: &'static str) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buffer = Vec::with_capacity(16384);
            loop {
                let mut chunk = [0; 4096];
                let n = socket
                    .read(&mut chunk)
                    .await
                    .expect("fake API request read failed");
                buffer.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buffer).to_string();
                let complete = text.find("\r\n\r\n").is_some_and(|head| {
                    let length = text[..head]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    buffer.len() >= head + 4 + length
                });
                if n == 0 || complete {
                    log.lock().unwrap().push(text);
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (port, seen)
}
/// Answers every connection with `prefix` and then goes silent without closing it, so the
/// client's own timeout has to end the request (a stalled response or a stalled body).
async fn stalling_api(prefix: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = [0; 4096];
                let _ = socket.read(&mut request).await;
                let _ = socket.write_all(prefix.as_bytes()).await;
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            });
        }
    });
    port
}
/// The smallest allowed proxy timeout: the client gives up 1 s after the API would.
fn quick_probe(port: u16) -> NetworkProbe {
    let settings = Settings {
        api_port: port,
        proxy_port: port.wrapping_add(1).max(1),
        proxy_timeout_ms: 100,
        ..Settings::default()
    };
    NetworkProbe::new(settings, SECRET.into()).unwrap()
}
fn probe(port: u16) -> NetworkProbe {
    probe_with_endpoint_timeout(port, Settings::default().endpoint_timeout_ms)
}
fn probe_with_endpoint_timeout(port: u16, endpoint_timeout_ms: u64) -> NetworkProbe {
    let settings = Settings {
        api_port: port,
        proxy_port: port.wrapping_add(1).max(1),
        endpoint_timeout_ms,
        ..Settings::default()
    };
    NetworkProbe::new(settings, SECRET.into()).unwrap()
}
fn server() -> rift_core::model::Server {
    parse_uri("trojan://credential@127.0.0.1:1", None).unwrap()
}

#[tokio::test]
async fn proxy_delay_asks_the_api_for_this_outbound() {
    let (port, seen) = fake_api("200 OK", r#"{"delay":123}"#).await;
    let s = server();
    assert_eq!(probe(port).proxy(&s).await.unwrap(), 123);
    let request = seen.lock().unwrap()[0].clone();
    let first = request.lines().next().unwrap();
    assert!(
        first.starts_with(&format!("GET /proxies/{}/delay?url=", s.id)),
        "{first}"
    );
    assert!(first.contains("timeout=5000"), "{first}");
    assert!(request
        .to_ascii_lowercase()
        .contains(&format!("authorization: bearer {SECRET}")));
}

#[tokio::test]
async fn zero_or_failed_delay_is_not_a_measurement() {
    let (port, _) = fake_api("200 OK", r#"{"delay":0}"#).await;
    assert_eq!(
        probe(port)
            .proxy(&server())
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::ProxyInvalidResponse)
    );
    let (port, _) = fake_api("503 Service Unavailable", r#"{"message":"timeout"}"#).await;
    assert_eq!(
        probe(port)
            .proxy(&server())
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::ProxyTestFailed)
    );
}

/// What sing-box's delay API says, by status, and what Rift calls it. The 504 is the one that
/// used to be shown as a server that "refused": it is sing-box's own deadline, a timeout.
#[tokio::test]
async fn every_answer_of_the_delay_api_is_named_for_what_it_is() {
    for (status, body, expected) in [
        (
            "504 Gateway Timeout",
            r#"{"message":"Timeout"}"#,
            FailureCode::ProxyTimeout,
        ),
        (
            "503 Service Unavailable",
            r#"{"message":"An error occurred in the delay test"}"#,
            FailureCode::ProxyTestFailed,
        ),
        (
            "404 Not Found",
            r#"{"message":"Resource not found"}"#,
            FailureCode::ProxyMissing,
        ),
        (
            "400 Bad Request",
            r#"{"message":"Body invalid"}"#,
            FailureCode::ProxyHttpStatus,
        ),
        ("401 Unauthorized", "", FailureCode::ProxyHttpStatus),
        (
            "500 Internal Server Error",
            "",
            FailureCode::ProxyHttpStatus,
        ),
    ] {
        let (port, _) = fake_api(status, body).await;
        let error = probe(port).proxy(&server()).await.unwrap_err();
        assert_eq!(error.failure_code(), Some(expected), "{status}");
        // The text for the journal says what happened, not that something was refused.
        assert!(
            !error.to_string().to_lowercase().contains("отклон"),
            "{status}: {error}"
        );
    }
}

#[tokio::test]
async fn non_json_answer_is_an_invalid_response() {
    let (port, _) = fake_api("200 OK", "not json").await;
    assert_eq!(
        probe(port)
            .proxy(&server())
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::ProxyInvalidResponse)
    );
}

#[tokio::test]
async fn closed_connection_is_a_proxy_transport_failure() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            drop(socket);
        }
    });
    assert_eq!(
        probe(port)
            .proxy(&server())
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::ProxyTransport)
    );
}

#[tokio::test]
async fn silent_api_is_a_proxy_timeout() {
    let port = stalling_api("").await;
    assert_eq!(
        quick_probe(port)
            .proxy(&server())
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::ProxyTimeout)
    );
}

#[tokio::test]
async fn stalled_response_body_is_a_proxy_timeout_not_an_invalid_response() {
    // Headers and the start of the JSON arrive, the rest never does: the client timeout hits
    // while the body is read.
    let port = stalling_api(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 40\r\n\r\n{\"del",
    )
    .await;
    assert_eq!(
        quick_probe(port)
            .proxy(&server())
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::ProxyTimeout)
    );
}

#[tokio::test]
async fn select_puts_the_server_into_main() {
    let (port, seen) = fake_api("204 No Content", "").await;
    probe(port).select("server-id").await.unwrap();
    let request = seen.lock().unwrap()[0].clone();
    assert!(request.starts_with("PUT /proxies/main "), "{request}");
    assert!(request.ends_with(r#"{"name":"server-id"}"#), "{request}");
    let (port, _) = fake_api("404 Not Found", "").await;
    assert_eq!(
        probe(port)
            .select("missing")
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::SelectorSwitch)
    );
}

#[tokio::test]
async fn routes_set_the_mode_and_ask_what_it_is_now() {
    let (port, seen) = fake_api("200 OK", r#"{"mode":"routes-0a1b2c3d4e5f"}"#).await;
    probe(port).routes("routes-0a1b2c3d4e5f").await.unwrap();
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(
        requests[0].starts_with("PATCH /configs "),
        "{}",
        requests[0]
    );
    assert!(
        requests[0].ends_with(r#"{"mode":"routes-0a1b2c3d4e5f"}"#),
        "{}",
        requests[0]
    );
    assert!(requests[1].starts_with("GET /configs "), "{}", requests[1]);
    for request in &requests {
        assert!(
            request.to_ascii_lowercase().contains(&format!(
                "authorization: bearer {}",
                SECRET.to_ascii_lowercase()
            )),
            "{request}"
        );
    }
}

#[tokio::test]
async fn a_mode_sing_box_did_not_take_is_a_selector_failure() {
    // sing-box answers 204 to a mode it does not know and stays where it was.
    let (port, _) = fake_api("200 OK", r#"{"mode":"none"}"#).await;
    assert_eq!(
        probe(port)
            .routes("routes-0a1b2c3d4e5f")
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::SelectorSwitch)
    );
    // An answer that says nothing of a mode is no confirmation either.
    let (port, _) = fake_api("200 OK", "{}").await;
    assert!(probe(port).routes("none").await.is_err());
    let (port, _) = fake_api("200 OK", "not json").await;
    assert!(probe(port).routes("none").await.is_err());
}

#[tokio::test]
async fn a_refused_mode_change_is_a_selector_failure() {
    let (port, seen) = fake_api("401 Unauthorized", "").await;
    assert_eq!(
        probe(port).routes("none").await.unwrap_err().failure_code(),
        Some(FailureCode::SelectorSwitch)
    );
    // Not asked what the mode is after a refusal.
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn ready_requires_an_authorised_api() {
    let (port, seen) = fake_api("200 OK", r#"{"version":"1.14.1"}"#).await;
    probe(port).ready().await.unwrap();
    assert!(seen.lock().unwrap()[0].starts_with("GET /version "));
    let (port, _) = fake_api("401 Unauthorized", "").await;
    assert!(probe(port).ready().await.is_err());
}

#[tokio::test]
async fn endpoint_measures_a_real_tcp_connect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let open = listener.local_addr().unwrap().port();
    let mut s = server();
    s.port = open;
    // Linux refuses a closed loopback port at once, Windows retries the SYN and reports the
    // refusal only after about 2 s: with the 1.5 s default it would be EndpointTimeout there.
    let p = probe_with_endpoint_timeout(1, 10_000);
    assert!(p.endpoint(&s).await.unwrap() >= 1);
    let closed_listener = TcpListener::bind("127.0.0.2:0").await.unwrap();
    s.host = "127.0.0.2".into();
    s.port = closed_listener.local_addr().unwrap().port();
    drop(closed_listener);
    assert_eq!(
        p.endpoint(&s).await.unwrap_err().failure_code(),
        Some(FailureCode::EndpointConnect),
        "closed port is unreachable"
    );
}

#[tokio::test]
async fn the_network_is_asked_through_the_direct_outbound_and_one_answer_is_enough() {
    let (port, seen) = fake_api("200 OK", r#"{"delay":12}"#).await;
    assert_eq!(probe(port).network().await, Network::Up);
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "the first site answered");
    let first = requests[0].lines().next().unwrap();
    assert!(
        first.starts_with(
            "GET /proxies/direct/delay?url=https%3A%2F%2Fwww.google.com%2Fgenerate_204"
        ),
        "{first}"
    );
    assert!(requests[0]
        .to_ascii_lowercase()
        .contains(&format!("authorization: bearer {SECRET}")));
}

/// A loopback address nobody listens on: asked, it refuses at once.
async fn closed_site() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}/")
}

#[tokio::test]
async fn without_an_answer_from_any_of_the_three_sites_there_is_no_network() {
    let (port, seen) = fake_api("504 Gateway Timeout", r#"{"message":"Timeout"}"#).await;
    let asked = probe(port).with_machine_sites(vec![closed_site().await]);
    let Network::Down(why) = asked.network().await else {
        panic!("no answer from anywhere is no network");
    };
    let requests = seen.lock().unwrap().clone();
    let sites: Vec<&str> = requests.iter().map(|r| r.lines().next().unwrap()).collect();
    assert_eq!(sites.len(), 3, "{sites:?}");
    assert!(sites[0].contains("google.com"), "{}", sites[0]);
    assert!(sites[1].contains("ya.ru"), "{}", sites[1]);
    assert!(sites[2].contains("vk.ru"), "{}", sites[2]);
    assert!(sites.iter().all(|r| r.contains("/proxies/direct/delay")));
    assert_eq!(
        why, "через sing-box: таймаут; из Rift: нет соединения",
        "what each way saw"
    );
}

#[tokio::test]
async fn a_sidecar_that_cannot_get_out_does_not_make_the_machine_offline() {
    // sing-box hears nothing from any site, but the machine's own request is answered (even
    // with an error status: it is an answer).
    let (port, _) = fake_api("503 Service Unavailable", r#"{"message":"failed"}"#).await;
    let (site, asked) = fake_api("404 Not Found", "{}").await;
    let probe = probe(port).with_machine_sites(vec![
        closed_site().await,
        format!("http://127.0.0.1:{site}/"),
    ]);
    assert_eq!(
        probe.network().await,
        Network::SidecarDeaf("через sing-box: проверка не удалась".into()),
        "in the network, but the sidecar is deaf"
    );
    assert_eq!(asked.lock().unwrap().len(), 1, "the second site answered");
}

#[tokio::test]
async fn the_machine_is_not_asked_while_sing_box_hears_an_answer() {
    let (port, _) = fake_api("200 OK", r#"{"delay":12}"#).await;
    let (site, asked) = fake_api("200 OK", "{}").await;
    let probe = probe(port).with_machine_sites(vec![format!("http://127.0.0.1:{site}/")]);
    assert_eq!(probe.network().await, Network::Up);
    assert!(asked.lock().unwrap().is_empty(), "no extra traffic");
}

#[tokio::test]
async fn revoked_or_replaced_gateway_is_cancelled_but_unchanged_dead_gateway_is_a_crash() {
    use rift_core::{model::Store, proxy_channel::ProxyHandoff};
    let mut store = Store::default();
    store.servers.push(server());
    let channel = Arc::new(std::sync::RwLock::new(None));
    let probe = quick_probe(12345).with_channel(Some(channel.clone()));
    assert_eq!(
        probe
            .proxy(&store.servers[0])
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::Cancelled)
    );
    for replace in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        *channel.write().unwrap() = Some(ProxyHandoff::new(port, SECRET.into(), &store).unwrap());
        let task = tokio::spawn({
            let channel = channel.clone();
            let store = store.clone();
            async move {
                let (socket, _) = listener.accept().await.unwrap();
                *channel.write().unwrap() = if replace {
                    Some(ProxyHandoff::new(port, SECRET.into(), &store).unwrap())
                } else {
                    None
                };
                drop(listener);
                drop(socket);
            }
        });
        assert_eq!(
            probe
                .proxy(&store.servers[0])
                .await
                .unwrap_err()
                .failure_code(),
            Some(FailureCode::Cancelled)
        );
        task.await.unwrap();
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    *channel.write().unwrap() = Some(ProxyHandoff::new(port, SECRET.into(), &store).unwrap());
    assert_eq!(
        probe
            .proxy(&store.servers[0])
            .await
            .unwrap_err()
            .failure_code(),
        Some(FailureCode::XrayCrashed)
    );
}
