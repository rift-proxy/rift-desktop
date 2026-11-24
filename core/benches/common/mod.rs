//! Shared performance fixtures: Criterion, Gungraun and the `perf_workload` example
//! (Samply/Heaptrack/cargo-bloat) measure the same inputs. Synthetic, no credentials.
#![allow(dead_code)]
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD, Engine};
use rift_core::{
    model::{Server, Store, Subscription},
    selection::Probe,
    subscription::{parse_subscription, SubscriptionSource},
    Result,
};

/// Subscription limit enforced by `parse_subscription`: the worst accepted case.
pub const SERVERS: usize = 2000;

pub const VLESS_REALITY: &str = "vless://11111111-1111-4111-8111-111111111111@example.com:443?security=reality&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&sid=abcd&sni=example.com&flow=xtls-rprx-vision&fp=chrome#Reality";

pub fn vmess() -> String {
    let raw = r#"{"v":"2","ps":"VMess","add":"example.com","port":"443","id":"11111111-1111-4111-8111-111111111111","net":"ws","path":"/ws","tls":"tls","sni":"example.com","fp":"firefox"}"#;
    format!("vmess://{}", STANDARD.encode(raw))
}

/// One line per protocol family, distinct ports so every server has its own id.
pub fn uri(i: usize) -> String {
    let port = 10_000 + i;
    match i % 4 {
        0 => format!("vless://11111111-1111-4111-8111-111111111111@example.com:{port}?security=tls&type=ws&path=%2Fws&fp=chrome#VLESS-{i}"),
        1 => format!("trojan://test-password@example.com:{port}?type=grpc&serviceName=svc#Trojan-{i}"),
        2 => format!("ss://aes-256-gcm:test-password@example.com:{port}#SS-{i}"),
        _ => format!("vless://11111111-1111-4111-8111-111111111111@example.com:{port}?security=reality&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&sid=abcd&sni=example.com#Reality-{i}"),
    }
}

/// Base64 subscription body, as providers serve it.
pub fn subscription(n: usize) -> String {
    let body: Vec<String> = (0..n).map(uri).collect();
    STANDARD.encode(body.join("\n"))
}

pub fn servers(n: usize) -> Vec<Server> {
    parse_subscription(&subscription(n), "sub").unwrap().servers
}

/// Store whose servers come from one subscription `sub`, as after a refresh.
pub fn store(n: usize) -> Store {
    Store {
        servers: if n == 0 { vec![] } else { servers(n) },
        subscriptions: vec![Subscription {
            id: "sub".into(),
            name: "Bench".into(),
            url: "https://example.com/sub".into(),
            updated_at: None,
            error: None,
            failure_code: None,
            ..Default::default()
        }],
        ..Store::default()
    }
}

/// Answers immediately; latency depends on the server so selection has real work to do.
pub struct InstantProbe;
#[async_trait]
impl Probe for InstantProbe {
    async fn endpoint(&self, server: &Server) -> Result<u64> {
        Ok(u64::from(server.id.as_bytes()[0]))
    }
    async fn proxy(&self, server: &Server) -> Result<u64> {
        Ok(u64::from(server.id.as_bytes()[1]))
    }
    async fn select(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

/// Serves [`subscription`] of `SERVERS` lines instead of downloading it.
pub struct FixtureSource;
#[async_trait]
impl SubscriptionSource for FixtureSource {
    async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
        Ok(subscription(SERVERS))
    }
}
