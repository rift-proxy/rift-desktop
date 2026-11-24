//! Deterministic, synthetic reference for the frontend's Storybook double.
use rift_core::{
    model::{Store, Subscription},
    parser, profile,
};
use serde_json::json;
use std::io::{self, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let uri = "vless://00000000-0000-4000-8000-000000000001@manual.example:443?security=tls&sni=manual.example#Contract";
    let manual = parser::parse_uri(uri, None)?;
    let subscribed = parser::parse_uri(
        "trojan://synthetic-password@subscription.example:443?security=tls#Subscription",
        Some("contract-source".into()),
    )?;
    let unchecked = manual.summary();
    let sample = Store {
        selected: Some(manual.id.clone()),
        servers: vec![manual, subscribed],
        subscriptions: vec![Subscription {
            id: "contract-source".into(),
            name: "Contract source".into(),
            url: "https://subscription.example/list".into(),
            ..Subscription::default()
        }],
        ..Store::default()
    };
    // Exercise the public profile encoder as well as the summary's serialization.
    let encoded: serde_json::Value = serde_json::from_slice(&profile::encode(&sample)?)?;
    let reference = json!({
        "app_version": env!("CARGO_PKG_VERSION"),
        "profile_format": encoded["format"],
        "profile_version": encoded["version"],
        "store_defaults": Store::default(),
        "profile_summary": profile::summary(&sample),
        "server_uri": uri,
        "unchecked_server": unchecked,
    });
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    serde_json::to_writer_pretty(&mut stdout, &reference)?;
    writeln!(stdout)?;
    Ok(())
}
