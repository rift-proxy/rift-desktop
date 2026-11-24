#![no_main]

use libfuzzer_sys::fuzz_target;
use rift_core::{
    journal::{clean_details, clean_message, Secrets, MAX_DETAIL_BYTES, MAX_MESSAGE_BYTES},
    model::{Store, Subscription},
    parser::parse_uri,
};
use std::path::Path;

const CANARY: &str = "journal-secret-canary-9f3a1c7e";

fuzz_target!(|data: &[u8]| {
    let mut store = Store::default();
    store.servers.push(parse_uri(&format!("trojan://{CANARY}@example.invalid:443"), None).unwrap());
    store.subscriptions.push(Subscription {
        url: format!("https://provider.invalid/sub?token={CANARY}"),
        ..Default::default()
    });
    let secrets = Secrets::from_store(&store, Path::new("/synthetic-install"));
    let input = String::from_utf8_lossy(data);
    let middle = input.char_indices().map(|(at, _)| at).find(|at| *at >= input.len() / 2).unwrap_or(input.len());
    let text = format!("{}{CANARY}{}", &input[..middle], &input[middle..]);
    if let Some(cleaned) = clean_details(Some(&text), &secrets) {
        assert!(cleaned.len() <= MAX_DETAIL_BYTES);
        assert!(!cleaned.contains(CANARY));
        assert!(cleaned.chars().all(|c| !c.is_control() || matches!(c, '\n' | '\t')));
    }
    let message = clean_message(&text, &secrets);
    assert!(message.len() <= MAX_MESSAGE_BYTES);
    assert!(!message.contains(CANARY));

    // Subscription bodies and complete URIs are not journal details, even if their secret
    // values were not present in the saved Store.
    let raw = format!(r#"{{"outbounds":[{{"password":"body-only-canary","extra":{}}}]}}"#, serde_json::json!(input));
    let cleaned = clean_details(Some(&raw), &secrets).unwrap();
    assert!(!cleaned.contains("body-only-canary"));
    let uri = format!("trojan://uri-only-canary@example.invalid:443?extra={}", data.len());
    assert!(!clean_details(Some(&uri), &secrets).unwrap().contains("uri-only-canary"));
});
