//! Coordinator failures reach local toasts, journal, and explicit diagnostics exports.
use async_trait::async_trait;
use rift_core::{
    application::{Action, Application},
    failure::FailureCode,
    model::{Server, Store, Subscription},
    notifications,
    parser::parse_uri,
    runtime::Runtime,
    selection::Probe,
    Error, Result,
};
use std::{sync::Arc, time::Duration};

/// Toast subscriptions are process-wide; tests must not receive one another's errors.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
async fn receive(errors: &mut tokio::sync::broadcast::Receiver<String>, expected: &str) -> String {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(message) = errors.recv().await {
                if message == expected {
                    return message;
                }
            }
        }
    })
    .await
    .expect("error toast was not notified")
}

struct NoRuntime;
#[async_trait]
impl Runtime for NoRuntime {
    async fn validate(&mut self, _: &Store, _: &str) -> Result<()> {
        Ok(())
    }
    async fn start(&mut self, _: &Store, _: &str) -> Result<()> {
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(false)
    }
}

struct FailingRuntime;
#[async_trait]
impl Runtime for FailingRuntime {
    async fn validate(&mut self, _: &Store, _: &str) -> Result<()> {
        Ok(())
    }
    async fn start(&mut self, _: &Store, _: &str) -> Result<()> {
        Err(Error::Invalid("credential xy rejected".into()))
    }
    async fn stop(&mut self) -> Result<()> {
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(false)
    }
}
struct NoProbe;
#[async_trait]
impl Probe for NoProbe {
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        Ok(1)
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        Ok(1)
    }
    async fn select(&self, _: &str) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn failed_action_is_shown_as_an_error_toast() {
    let _serial = SERIAL.lock().await;
    let mut errors = notifications::subscribe();
    let dir = tempfile::tempdir().unwrap();
    let app = Application::with_probe(
        Store::default(),
        dir.path().into(),
        Box::new(NoRuntime),
        Arc::new(NoProbe),
        "12345678901234567890123456789012".into(),
    );
    assert!(app.execute(Action::Connect).await.is_err());
    let shown = receive(&mut errors, "Нет доступных серверов").await;
    assert_eq!(shown, "Нет доступных серверов");
}

#[tokio::test]
async fn runtime_failure_keeps_its_cause_but_cleans_local_journal_and_export() {
    let _serial = SERIAL.lock().await;
    let mut errors = notifications::subscribe();
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::default();
    store
        .servers
        .push(parse_uri("trojan://xy@203.0.113.7:443", None).unwrap());
    let app = Application::with_probe(
        store,
        dir.path().into(),
        Box::new(FailingRuntime),
        Arc::new(NoProbe),
        "12345678901234567890123456789012".into(),
    );
    let expected = app.execute(Action::Connect).await.unwrap_err().to_string();
    assert!(expected.contains("credential xy rejected"));
    let failure = app
        .snapshot()
        .journal
        .into_iter()
        .find(|entry| entry.level == "error")
        .unwrap();
    let shown = receive(&mut errors, &failure.message).await;
    assert!(shown.contains("credential") && shown.contains("rejected"));
    assert!(!shown.split_whitespace().any(|word| word == "xy"));
    assert!(failure.details.as_deref().unwrap().contains("credential"));
    app.execute(Action::ExportDiagnostics).await.unwrap();
    for path in ["journal.json", "diagnostics.json"] {
        let text = std::fs::read_to_string(dir.path().join(path)).unwrap();
        assert!(!text.contains("credential xy rejected"), "{text}");
        assert!(!text.contains("trojan://"), "{text}");
    }
}

#[tokio::test]
async fn failed_profile_commands_reach_the_error_toast_journal_and_snapshot() {
    let _serial = SERIAL.lock().await;
    let mut errors = notifications::subscribe();
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::default();
    store.subscriptions.push(Subscription {
        id: "sub".into(),
        name: "Test".into(),
        url: "http://insecure.example/path?token=url-secret".into(),
        updated_at: None,
        error: None,
        failure_code: None,
        ..Default::default()
    });
    let app = Application::with_probe(
        store,
        dir.path().into(),
        Box::new(NoRuntime),
        Arc::new(NoProbe),
        "12345678901234567890123456789012".into(),
    );

    // Export of a store the profile rules refuse.
    let export = app.export_profile().await.unwrap_err();
    receive(&mut errors, &export.to_string()).await;

    // Inspect: a damaged file (content) and a missing file (environment).
    let damaged = dir.path().join("damaged-profile.json");
    std::fs::write(&damaged, b"{").unwrap();
    let Err(content) = app.inspect_profile(&damaged).await else {
        panic!("a damaged profile was accepted");
    };
    receive(&mut errors, &content.to_string()).await;
    let missing = dir.path().join("missing-profile.json");
    let Err(io) = app.inspect_profile(&missing).await else {
        panic!("a missing profile was accepted");
    };
    assert_eq!(io.failure_code(), Some(FailureCode::Persistence));
    receive(&mut errors, &io.to_string()).await;

    // Write: the destination cannot be replaced.
    let blocked = dir.path().join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    let write = app.write_profile(&blocked, b"{}").await.unwrap_err();
    assert_eq!(write.failure_code(), Some(FailureCode::Persistence));
    receive(&mut errors, &write.to_string()).await;

    let snapshot = app.snapshot();
    let errors: Vec<_> = snapshot
        .journal
        .iter()
        .filter(|entry| entry.level == "error")
        .collect();
    assert_eq!(errors.len(), 4);
    assert_eq!(errors[2].failure_code, Some(FailureCode::Persistence));
    assert_eq!(errors[3].failure_code, Some(FailureCode::Persistence));
    assert_eq!(snapshot.error.as_deref(), Some(write.to_string().as_str()));
    // Static core texts only: neither the file names nor the subscription URL leak.
    for entry in &snapshot.journal {
        for secret in [
            "damaged-profile",
            "missing-profile",
            "blocked",
            "url-secret",
        ] {
            assert!(!entry.message.contains(secret), "{}", entry.message);
        }
    }
}

#[tokio::test]
async fn inspected_profile_file_round_trips_through_write_and_inspect() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::default();
    store
        .servers
        .push(parse_uri("trojan://xy@203.0.113.7:443", None).unwrap());
    let app = Application::with_probe(
        store,
        dir.path().into(),
        Box::new(NoRuntime),
        Arc::new(NoProbe),
        "12345678901234567890123456789012".into(),
    );
    let path = dir.path().join("exported-profile.json");

    app.write_profile(&path, &app.export_profile().await.unwrap())
        .await
        .unwrap();
    let inspected = app.inspect_profile(&path).await.unwrap();

    assert_eq!(inspected.servers.len(), 1);
    assert!(app
        .snapshot()
        .journal
        .iter()
        .all(|entry| entry.level != "error"));
}

#[tokio::test]
async fn concurrent_external_errors_keep_every_cleaned_entry_on_disk() {
    let _serial = SERIAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::default();
    store.servers.push(
        parse_uri(
            "trojan://concurrent-password-canary@server.example:443",
            None,
        )
        .unwrap(),
    );
    let app = Application::with_probe(
        store,
        dir.path().into(),
        Box::new(NoRuntime),
        Arc::new(NoProbe),
        "concurrent-gateway-canary-0123456789012".into(),
    );
    let before = app.snapshot().journal.len();
    std::thread::scope(|scope| {
        for worker in 0..8 {
            let app = app.clone();
            scope.spawn(move || {
                for sequence in 0..8 {
                    app.log_external_error(
                        &format!("Concurrent local error {worker} {sequence}"),
                        Some("stage=frontend; concurrent-password-canary concurrent-gateway-canary-0123456789012 https://provider.example/sub?token=concurrent-token-canary"),
                    );
                }
            });
        }
    });
    let snapshot = app.snapshot();
    assert_eq!(snapshot.journal.len(), before + 64);
    let text = std::fs::read_to_string(dir.path().join("journal.json")).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved, serde_json::to_value(snapshot.journal).unwrap());
    for secret in [
        "concurrent-password-canary",
        "concurrent-gateway-canary",
        "concurrent-token-canary",
        "https://",
    ] {
        assert!(!text.contains(secret), "{secret} in journal");
    }
}
