use rift_core::{
    application::Entry,
    diagnostics::{Diagnostic, Origin, Stage},
    failure::FailureCode,
    journal::{
        clean_details, clean_message, Secrets, MAX_DETAIL_BYTES, MAX_INPUT_BYTES, MAX_MESSAGE_BYTES,
    },
    model::{Store, Subscription},
    parser::parse_uri,
    Error,
};
use std::{error::Error as _, path::Path};

fn secrets() -> Secrets {
    let mut store = Store::default();
    store
        .servers
        .push(parse_uri("trojan://password-canary@server.example:443", None).unwrap());
    store.subscriptions.push(Subscription {
        url: "https://provider.example/sub?token=token-canary".into(),
        ..Default::default()
    });
    Secrets::from_store(&store, Path::new("/synthetic-install"))
}

#[test]
fn assigning_a_failure_code_preserves_the_original_parser_cause() {
    let original = serde_json::from_str::<serde_json::Value>("{\n\"outbounds\": [}").unwrap_err();
    let line = original.line();
    let column = original.column();
    let error = Error::from(original)
        .with_code(FailureCode::SubscriptionInvalidBody)
        .at_stage(Stage::Parse)
        .with_field_path("$");
    assert_eq!(
        error.failure_code(),
        Some(FailureCode::SubscriptionInvalidBody)
    );
    let source = error.source().expect("coded error retains the core source");
    assert!(source.downcast_ref::<Error>().is_some());
    assert!(source
        .source()
        .unwrap()
        .downcast_ref::<serde_json::Error>()
        .is_some());
    let details = error.diagnostic_details();
    assert!(details.contains(&format!("line={line}")), "{details}");
    assert!(details.contains(&format!("column={column}")), "{details}");
    assert!(details.contains("parse"), "{details}");
}

#[test]
fn local_cleaning_keeps_field_and_position_but_removes_secrets_and_raw_bodies() {
    let details = "stage=parse; field=settings.vnext[0].users[0].id; line 3 column 17; password-canary token-canary https://provider.example/sub?token=unknown-token trojan://unknown-password@example.net:443 {\"password\":\"body-only-canary\"}";
    let cleaned = clean_details(Some(details), &secrets()).unwrap();
    for expected in ["parse", "settings.vnext[0].users[0].id", "line 3 column 17"] {
        assert!(cleaned.contains(expected), "{expected}: {cleaned}");
    }
    for forbidden in [
        "password-canary",
        "token-canary",
        "unknown-token",
        "unknown-password",
        "body-only-canary",
        "https://",
        "trojan://",
    ] {
        assert!(!cleaned.contains(forbidden), "{forbidden}: {cleaned}");
    }
}

#[test]
fn unicode_details_messages_and_work_boundary_credentials_are_bounded() {
    let secrets = secrets();
    let unicode = "Ж🙂".repeat(MAX_INPUT_BYTES);
    let details = clean_details(Some(&unicode), &secrets).unwrap();
    assert!(details.len() <= MAX_DETAIL_BYTES);
    assert!(clean_message(&unicode, &secrets).len() <= MAX_MESSAGE_BYTES);
    let mut crossing = " ".repeat(MAX_INPUT_BYTES - 5);
    crossing.push_str("password-canary");
    let cleaned = clean_details(Some(&crossing), &secrets);
    assert!(cleaned
        .as_deref()
        .is_none_or(|value| !value.contains("passw")));
    assert_eq!(clean_details(None, &secrets), None);
    assert_eq!(clean_details(Some(" \n\t"), &secrets), None);
}

#[test]
fn old_journal_entries_without_details_still_deserialize() {
    let entry: Entry =
        serde_json::from_str(r#"{"at":1,"level":"error","message":"old error"}"#).unwrap();
    assert_eq!(entry.details, None);
    assert_eq!(entry.crash_id, None);
    let value = serde_json::to_value(entry).unwrap();
    assert!(value.get("details").is_none());
    assert!(value.get("crash_id").is_none());
}

#[test]
fn diagnostic_origins_strip_nested_build_directories_in_exported_metadata() {
    for (input, expected) in [
        ("core/src/application.rs", "core/src/application.rs"),
        (
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/application.rs"),
            "core/src/application.rs",
        ),
        (
            "/build/project/core/src/application.rs",
            "core/src/application.rs",
        ),
        (
            "/build/worktree/project/desktop/src-tauri/src/lib.rs",
            "desktop/src-tauri/src/lib.rs",
        ),
        (
            r"C:\build\worktree\project\core\src\application.rs",
            "core/src/application.rs",
        ),
        (
            r"C:\build\project\desktop\src-tauri\src\lib.rs",
            "desktop/src-tauri/src/lib.rs",
        ),
    ] {
        let mut diagnostic = Diagnostic::of(Some(FailureCode::Unknown));
        diagnostic.origin = Origin::new(input, 712);
        assert_eq!(
            diagnostic.to_json()["origin"],
            serde_json::json!({ "file": expected, "line": 712 }),
            "{input}",
        );
    }
}

#[test]
fn diagnostic_origins_reject_unsafe_or_unrelated_locations() {
    for input in [
        "core/src/../application.rs",
        "core/src/./application.rs",
        "core/src//application.rs",
        "core/src/",
        r"C:\build\project\desktop\src-tauri\src\..\lib.rs",
        "/build/project/src/application.rs",
    ] {
        assert_eq!(Origin::new(input, 712), None, "{input}");
    }
    assert_eq!(Origin::new("core/src/application.rs", 0), None);
}
