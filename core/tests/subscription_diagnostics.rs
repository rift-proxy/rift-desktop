//! Subscription consumers retain concrete, cleaned local causes and never discard working servers.
use async_trait::async_trait;
use rift_core::{
    application::{Action, Application},
    diagnostics::{Cause, ContentClass, Stage, SubscriptionInfo},
    failure::FailureCode,
    model::{Server, Store, Subscription},
    runtime::Runtime,
    selection::Probe,
    subscription::{parse_subscription, FetchRequest, Fetched, SubscriptionSource},
    Error, Result,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

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

struct Body(String);
#[async_trait]
impl SubscriptionSource for Body {
    async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
        Ok(self.0.clone())
    }
    async fn fetch_with(&self, _: FetchRequest<'_>) -> Result<Fetched> {
        Ok(Fetched {
            body: self.0.clone(),
            info: SubscriptionInfo {
                content_type: Some(ContentClass::TextPlain),
                redirects: Some(2),
                ..Default::default()
            },
            ..Default::default()
        })
    }
}

struct HttpForbidden;
#[async_trait]
impl SubscriptionSource for HttpForbidden {
    async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
        Err(Error::failure(
            FailureCode::SubscriptionHttpStatus,
            "Сервер подписки вернул HTTP 403",
        )
        .at_stage(Stage::SendRequest)
        .with_status(403))
    }
}

struct NetworkFailure;
#[async_trait]
impl SubscriptionSource for NetworkFailure {
    async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
        Err(Error::failure(
            FailureCode::SubscriptionTransport,
            "Сервер подписки не ответил",
        )
        .at_stage(Stage::SendRequest)
        .with_cause(Cause::Connect))
    }
}

fn store_with_a_subscription() -> Store {
    let mut store = Store::default();
    store.subscriptions.push(Subscription {
        id: "a".into(),
        name: "Рабочая подписка".into(),
        url: "https://provider-canary.example/subscription?token=url-canary".into(),
        ..Default::default()
    });
    store.servers = parse_subscription("trojan://password-canary@old.example:443", "a")
        .unwrap()
        .servers;
    store
}

fn application(
    store: Store,
    dir: &std::path::Path,
    source: Arc<dyn SubscriptionSource>,
) -> Arc<Application> {
    Application::with_services(
        store,
        dir.into(),
        Box::new(NoRuntime),
        Arc::new(NoProbe),
        "gateway-secret-canary-0123456789012".into(),
        source,
    )
}

fn refresh() -> Action {
    Action::Refresh {
        id: Some("a".into()),
    }
}

async fn exported(app: &Application, dir: &std::path::Path) -> serde_json::Value {
    app.execute(Action::ExportDiagnostics).await.unwrap();
    serde_json::from_slice(&std::fs::read(dir.join("diagnostics.json")).unwrap()).unwrap()
}

async fn failed_refresh(
    source: Arc<dyn SubscriptionSource>,
    expected_code: FailureCode,
    expected: &[&str],
    forbidden: &[&str],
) {
    let dir = tempfile::tempdir().unwrap();
    let store = store_with_a_subscription();
    let previous = store.servers[0].id.clone();
    let app = application(store, dir.path(), source);
    app.execute(refresh()).await.unwrap();
    let snapshot = app.snapshot();
    assert_eq!(snapshot.servers.len(), 1, "working servers were preserved");
    assert_eq!(snapshot.servers[0].id, previous);
    assert!(snapshot.subscriptions[0].error.is_some());
    assert_eq!(snapshot.subscriptions[0].failure_code, Some(expected_code));
    let entry = snapshot
        .journal
        .iter()
        .rev()
        .find(|entry| entry.level == "error")
        .expect("refresh journal entry");
    assert_eq!(entry.failure_code, Some(expected_code));
    let details = entry.details.as_deref().expect("local technical cause");
    let export = exported(&app, dir.path()).await;
    let failure = export["subscription_failures"][0]["failure"]
        .as_str()
        .expect("local exported cause");
    for text in expected {
        assert!(
            details.contains(text),
            "{text} missing from journal details: {details}"
        );
        assert!(
            failure.contains(text),
            "{text} missing from exported failure: {failure}"
        );
    }
    let journal = std::fs::read_to_string(dir.path().join("journal.json")).unwrap();
    let wire = export.to_string();
    let snapshot_wire = serde_json::to_string(&snapshot).unwrap();
    for secret in [
        "url-canary",
        "provider-canary",
        "password-canary",
        "gateway-secret-canary",
    ]
    .into_iter()
    .chain(forbidden.iter().copied())
    {
        for (place, text) in [
            ("journal", journal.as_str()),
            ("export", wire.as_str()),
            ("snapshot", snapshot_wire.as_str()),
        ] {
            assert!(!text.contains(secret), "{secret} in {place}: {text}");
        }
    }
    let saved = rift_core::persistence::load(&dir.path().join("state.json")).unwrap();
    assert_eq!(saved.servers.len(), 1);
    assert_eq!(saved.servers[0].id, previous);
}

#[tokio::test]
async fn http_403_is_an_explicit_refusal_not_a_guessed_expiry() {
    failed_refresh(
        Arc::new(HttpForbidden),
        FailureCode::SubscriptionHttpStatus,
        &["403", "send_request"],
        &["истек", "expired"],
    )
    .await;
}

#[tokio::test]
async fn explicit_provider_rejection_reaches_the_consumer_without_raw_provider_text() {
    failed_refresh(
        Arc::new(Body(
            r#"{"success":false,"code":403,"message":"provider-body-canary expired token"}"#.into(),
        )),
        FailureCode::SubscriptionInvalidBody,
        &["success=false"],
        &["provider-body-canary", "expired token"],
    )
    .await;
}

#[tokio::test]
async fn malformed_json_keeps_parser_position_without_recording_the_subscription_body() {
    failed_refresh(
        Arc::new(Body(
            "{\n\"outbounds\": [\n{\"protocol\":\"trojan\",\"body-canary\":".into(),
        )),
        FailureCode::SubscriptionInvalidBody,
        &["category=Eof", "line=3", "column="],
        &["body-canary"],
    )
    .await;
}

#[tokio::test]
async fn missing_xray_field_keeps_the_concrete_path_and_preserves_previous_servers() {
    failed_refresh(
        Arc::new(Body(r#"{"outbounds":[{"protocol":"trojan","settings":{"servers":[{"address":"missing-field-canary.example","port":443}]}}]}"#.into())),
        FailureCode::SubscriptionInvalidBody,
        &["password"], &["missing-field-canary"],
    ).await;
}

#[tokio::test]
async fn flat_shadowsocks_missing_method_keeps_its_schema_path_in_journal_and_export() {
    failed_refresh(
        Arc::new(Body(
            r#"{"outbounds":[{"protocol":"shadowsocks","settings":{"address":"flat-field-canary.example","port":8388,"password":"flat-password-canary"}}]}"#.into(),
        )),
        FailureCode::SubscriptionInvalidBody,
        &["settings.method"],
        &["flat-field-canary", "flat-password-canary"],
    )
    .await;
}

#[tokio::test]
async fn network_failure_keeps_its_local_cause_without_a_business_guess() {
    failed_refresh(
        Arc::new(NetworkFailure),
        FailureCode::SubscriptionTransport,
        &["connect", "send_request"],
        &["истек", "expired"],
    )
    .await;
}

#[tokio::test]
async fn html_is_classified_without_including_raw_subscription_content() {
    failed_refresh(
        Arc::new(Body(
            "<html><body>html-body-canary access denied</body></html>".into(),
        )),
        FailureCode::SubscriptionInvalidBody,
        &["html"],
        &["html-body-canary", "<html>", "access denied"],
    )
    .await;
}

#[tokio::test]
async fn unknown_business_reason_is_not_labeled_as_expired() {
    failed_refresh(
        Arc::new(Body(r#"{"message":"unknown-business-body-canary"}"#.into())),
        FailureCode::SubscriptionInvalidBody,
        &[
            "outbounds",
            rift_core::text::t("core.diagnostics.no_outbounds"),
        ],
        &["unknown-business-body-canary", "истек", "expired"],
    )
    .await;
}

#[tokio::test]
async fn oversized_body_keeps_the_failure_but_not_the_body() {
    let text = format!(
        "oversized-body-canary {}",
        "x".repeat(rift_core::subscription::MAX_SUBSCRIPTION_BYTES)
    );
    failed_refresh(
        Arc::new(Body(text)),
        FailureCode::SubscriptionInvalidBody,
        &[],
        &["oversized-body-canary"],
    )
    .await;
}

struct Switch(AtomicBool);
#[async_trait]
impl SubscriptionSource for Switch {
    async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
        if self.0.load(Ordering::SeqCst) {
            Ok("trojan://new-password-canary@new.example:443".into())
        } else {
            Err(Error::Network)
        }
    }
}

#[tokio::test]
async fn failed_persistence_keeps_previous_servers_and_failure_until_success_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let source = Arc::new(Switch(AtomicBool::new(false)));
    let store = store_with_a_subscription();
    let previous = store.servers[0].id.clone();
    let app = application(store, dir.path(), source.clone());
    app.execute(refresh()).await.unwrap();
    let before = exported(&app, dir.path()).await["subscription_failures"].clone();
    assert_eq!(before.as_array().unwrap().len(), 1);
    source.0.store(true, Ordering::SeqCst);
    let state = dir.path().join("state.json");
    std::fs::remove_file(&state).unwrap();
    std::fs::create_dir(&state).unwrap();
    let error = app.execute(refresh()).await.unwrap_err();
    assert_eq!(error.failure_code(), Some(FailureCode::Persistence));
    assert!(error.diagnostic_details().contains("persist"));
    assert_eq!(app.snapshot().servers[0].id, previous);
    assert_eq!(
        exported(&app, dir.path()).await["subscription_failures"],
        before
    );
    assert!(app.snapshot().subscriptions[0].error.is_some());
    std::fs::remove_dir(&state).unwrap();
    app.execute(refresh()).await.unwrap();
    assert_ne!(app.snapshot().servers[0].id, previous);
    assert!(app.snapshot().subscriptions[0].error.is_none());
    assert!(exported(&app, dir.path()).await["subscription_failures"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_failed_refresh_through_both_routes_keeps_both_causes_locally() {
    struct TwoRoutes;
    #[async_trait]
    impl SubscriptionSource for TwoRoutes {
        async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
            Err(Error::Network)
        }
        async fn fetch_with(&self, request: FetchRequest<'_>) -> Result<Fetched> {
            Err(match request.proxy_port {
                None => Error::failure(FailureCode::SubscriptionTransport, "direct failed")
                    .at_stage(Stage::SendRequest)
                    .with_cause(Cause::Connect),
                Some(_) => Error::failure(FailureCode::SubscriptionTimeout, "proxy failed")
                    .at_stage(Stage::SendRequest)
                    .with_cause(Cause::Timeout),
            })
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let app = application(store_with_a_subscription(), dir.path(), Arc::new(TwoRoutes));
    app.execute(Action::Connect).await.unwrap();
    app.execute(refresh()).await.unwrap();
    let export = exported(&app, dir.path()).await;
    let failure = export["subscription_failures"][0]["failure"]
        .as_str()
        .unwrap();
    for expected in ["connect", "timeout", "first_attempt", "local_proxy"] {
        assert!(failure.contains(expected), "{expected} missing: {failure}");
    }
}

#[tokio::test]
async fn successful_parameter_counts_reach_journal_and_export_without_values() {
    let dir = tempfile::tempdir().unwrap();
    let app = application(store_with_a_subscription(), dir.path(), Arc::new(Body(
        "vless://11111111-1111-4111-8111-111111111111@private-canary.example:443?security=tls&type=xhttp&mode=stream-one".into(),
    )));
    app.execute(refresh()).await.unwrap();
    assert!(app
        .snapshot()
        .journal
        .iter()
        .any(|entry| entry.message.contains("mode=1")
            && entry.message.contains("проигнорировано: 0")));
    let value = exported(&app, dir.path()).await;
    assert_eq!(
        value["subscription_parameters"][0]["observations"]["accepted_parameters"]["mode"],
        1
    );
    for secret in ["private-canary", "stream-one", "11111111-1111"] {
        assert!(!value.to_string().contains(secret));
    }
    app.shutdown().await.unwrap();
}

#[tokio::test]
async fn bodies_that_give_no_server_are_told_apart_and_counted_locally() {
    for (body, expected) in [
        ("", vec!["body_kind: empty", "rows.total: 0"]),
        (
            "aGVsbG8gd29ybGQ=",
            vec!["body_kind: text_other", "rows.total: 1", "rows.rejected: 1"],
        ),
        (
            r#"{"error":"unknown-body-canary"}"#,
            vec!["body_kind: json_other"],
        ),
        (
            r#"[{"outbounds":[{"protocol":"freedom"}]}]"#,
            vec!["body_kind: json_xray", "rows.total: 0"],
        ),
    ] {
        failed_refresh(
            Arc::new(Body(body.into())),
            FailureCode::SubscriptionInvalidBody,
            &expected,
            &["unknown-body-canary"],
        )
        .await;
    }
}

#[tokio::test]
async fn a_refresh_the_worker_makes_on_its_own_is_scheduled_not_manual() {
    let dir = tempfile::tempdir().unwrap();
    let app = application(
        store_with_a_subscription(),
        dir.path(),
        Arc::new(NetworkFailure),
    );
    let mut snapshots = app.snapshots.subscribe();
    app.start();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        snapshots.wait_for(|snapshot| {
            snapshot.journal.iter().any(|entry| {
                entry
                    .details
                    .as_deref()
                    .is_some_and(|detail| detail.contains("trigger: scheduled"))
            })
        }),
    )
    .await
    .expect("scheduled subscription failure")
    .unwrap();
    let value = exported(&app, dir.path()).await;
    let failure = value["subscription_failures"][0]["failure"]
        .as_str()
        .unwrap();
    assert!(failure.contains("trigger: scheduled"), "{failure}");
    assert!(!failure.contains("trigger: manual"), "{failure}");
    app.shutdown().await.unwrap();
}
