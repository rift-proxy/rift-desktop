#![no_main]

use libfuzzer_sys::fuzz_target;
use rift_core::{
    application::Action,
    parser::parse_uri,
    journal::{clean, Secrets},
    subscription::validate_url,
};

const CANARY: &str = "RIFT-CANARY-9f3a1c7e";

fuzz_target!(|data: &[u8]| {
    // `execute` receives the frontend's JSON as an `Action`.
    if let Ok(action) = serde_json::from_slice::<Action>(data) {
        match action {
            Action::SaveSettings { settings } => {
                if settings.validate().is_ok() {
                    assert_ne!(settings.proxy_port, settings.api_port);
                    assert!((1..=64).contains(&settings.concurrency));
                }
            }
            Action::AddServer { uri } => {
                let _ = parse_uri(&uri, None);
            }
            Action::SaveSubscription { url, .. } => {
                if let Ok(parsed) = validate_url(&url) {
                    assert_eq!(parsed.scheme(), "https");
                    assert!(parsed.host_str().is_some());
                    assert!(parsed.username().is_empty() && parsed.password().is_none());
                }
            }
            _ => {}
        }
    }

    // `log_frontend_error` and `preview_server` receive free text from the frontend.
    let middle = data.len() / 2;
    let before = String::from_utf8_lossy(&data[..middle]);
    let after = String::from_utf8_lossy(&data[middle..]);
    let text = format!("{before}{CANARY}{after}");
    let mut store = rift_core::model::Store::default();
    store.subscriptions.push(rift_core::model::Subscription {
        url: format!("https://example.invalid/{CANARY}"),
        ..Default::default()
    });
    let known = Secrets::from_store(&store, std::path::Path::new("."));
    assert!(!clean(&text, &known).contains(CANARY));
    let _ = clean(&before, &known);
    let _ = parse_uri(&before, None);
});
