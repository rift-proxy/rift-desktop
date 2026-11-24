//! What sing-box says about its failures reaches the journal and the diagnostics as facts
//! (stage, cause, whom), never as the words of its log.
use async_trait::async_trait;
use rift_core::{
    application::{Action, Application, Category},
    dns::ResolverStats,
    model::{Server, Store},
    parser::parse_uri,
    runtime::Runtime,
    selection::Probe,
    sidecar_log::{SingBoxLog, Summary},
    subscription::SubscriptionSource,
    Error, Result,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

struct LoggedRuntime {
    alive: Arc<AtomicBool>,
    log: Arc<SingBoxLog>,
    fail_start: bool,
}
#[async_trait]
impl Runtime for LoggedRuntime {
    async fn validate(&mut self, _: &Store, _: &str) -> Result<()> {
        Ok(())
    }
    async fn start(&mut self, _: &Store, _: &str) -> Result<()> {
        if self.fail_start {
            return Err(Error::failure(
                rift_core::failure::FailureCode::SidecarStartExit,
                "Не удалось запустить sing-box",
            ));
        }
        self.alive.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn stop(&mut self) -> Result<()> {
        self.alive.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn alive(&mut self) -> Result<bool> {
        Ok(self.alive.load(Ordering::SeqCst))
    }
    fn dns_stats(&self) -> Vec<ResolverStats> {
        vec![ResolverStats {
            name: "google",
            answered: 3,
            missed: 1,
            last_miss: Some("timeout"),
            last_answer_ms: Some(120),
            left_out: false,
        }]
    }
    fn log_notes(&self, now: u64) -> Vec<String> {
        self.log.notes(now)
    }
    fn log_summary(&self) -> Option<Summary> {
        Some(self.log.summary())
    }
    fn last_direct_failure(&self) -> Option<String> {
        self.log.last_direct()
    }
}
struct Answers(bool);
#[async_trait]
impl Probe for Answers {
    async fn network(&self) -> rift_core::selection::Network {
        if self.0 {
            rift_core::selection::Network::SidecarDeaf("нет ответа".into())
        } else {
            rift_core::selection::Network::Up
        }
    }
    async fn endpoint(&self, _: &Server) -> Result<u64> {
        Ok(5)
    }
    async fn proxy(&self, _: &Server) -> Result<u64> {
        Ok(20)
    }
    async fn select(&self, _: &str) -> Result<()> {
        Ok(())
    }
}
struct NoFeed;
#[async_trait]
impl SubscriptionSource for NoFeed {
    async fn fetch(&self, _: &str, _: Option<u16>) -> Result<String> {
        Err(Error::Network)
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn start(servers: Vec<Server>) -> (Arc<Application>, Arc<SingBoxLog>, tempfile::TempDir) {
    let (app, log, _, dir) = start_with(servers, false, false);
    (app, log, dir)
}
fn start_with(
    servers: Vec<Server>,
    fail_start: bool,
    deaf: bool,
) -> (
    Arc<Application>,
    Arc<SingBoxLog>,
    Arc<AtomicBool>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(SingBoxLog::default());
    let alive = Arc::new(AtomicBool::new(false));
    let app = Application::with_services(
        Store {
            servers,
            ..Store::default()
        },
        dir.path().into(),
        Box::new(LoggedRuntime {
            alive: alive.clone(),
            fail_start,
            log: log.clone(),
        }),
        Arc::new(Answers(deaf)),
        "12345678901234567890123456789012".into(),
        Arc::new(NoFeed),
    );
    app.start();
    (app, log, alive, dir)
}

#[tokio::test]
async fn a_failure_sing_box_reports_becomes_one_core_line_in_the_journal_without_its_words() {
    let (app, log, _dir) = start(vec![parse_uri("trojan://p@198.51.100.7:443", None).unwrap()]);
    app.execute(Action::Connect).await.unwrap();
    let line = "+0300 2026-10-19 17:09:42 ERROR [1 10.1s] connection: open connection to private-site.example:443 using outbound/direct[direct]: lookup private-site.example: (dial udp 8.8.4.4:53: i/o timeout)";
    log.record(line, now());
    log.record(line, now());
    let mut rx = app.snapshots.subscribe();
    let said = tokio::time::timeout(
        Duration::from_secs(5),
        rx.wait_for(|s| {
            s.journal
                .iter()
                .any(|e| e.category == Category::Core && e.message.starts_with("sing-box не смог"))
        }),
    )
    .await
    .expect("the worker passes the note to the journal")
    .unwrap()
    .journal
    .iter()
    .filter(|e| e.category == Category::Core && e.message.starts_with("sing-box не смог"))
    .map(|e| (e.level.clone(), e.message.clone()))
    .collect::<Vec<_>>();
    assert_eq!(
        said.len(),
        1,
        "two lines of the same kind are one note: {said:?}"
    );
    assert_eq!(said[0].0, "warn");
    assert!(
        said[0].1.contains("не смог найти адрес по имени напрямую"),
        "{said:?}"
    );
    assert!(said[0].1.contains("нет ответа (таймаут)"));
    for word in ["private-site", "8.8.4.4", "dial udp"] {
        assert!(!said[0].1.contains(word), "{word} in {said:?}");
    }
}

#[tokio::test]
async fn the_diagnostics_say_how_servers_are_addressed_and_what_sing_box_said_of_each() {
    // Credentials intentionally collide with fixed DNS/cause labels. Only credential-bearing
    // text is cleaned; these runtime facts and generated server fingerprints must survive.
    let by_ip = parse_uri("trojan://google@198.51.100.7:443", None).unwrap();
    let by_name = parse_uri("trojan://timeout@secret-host.example:443", None).unwrap();
    let (app, log, dir) = start(vec![by_ip.clone(), by_name.clone()]);
    app.execute(Action::Select {
        id: by_name.id.clone(),
    })
    .await
    .unwrap();
    app.log_external_error(
        "export regression",
        Some("password=timeout; peer=secret-host.example; password=google"),
    );
    log.record(
        &format!(
            "+0300 2026-10-19 17:09:42 ERROR [1 5s] connection: open connection to x.example:443 using outbound/trojan[{}]: lookup secret-host.example: i/o timeout",
            by_name.id
        ),
        now(),
    );
    app.execute(Action::ExportDiagnostics).await.unwrap();
    let text = std::fs::read_to_string(dir.path().join("diagnostics.json")).unwrap();
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    let servers = report["servers"].as_array().unwrap();
    let find = |id: &str| servers.iter().find(|s| s["id"] == id).unwrap();
    assert_eq!(find(&by_ip.id)["address"], "ip");
    assert_eq!(find(&by_name.id)["address"], "name");
    assert_eq!(find(&by_ip.id)["sing_box_error"], serde_json::Value::Null);
    let error = &find(&by_name.id)["sing_box_error"];
    assert_eq!(
        (error["stage"].as_str(), error["cause"].as_str()),
        (Some("dns"), Some("timeout"))
    );
    assert_eq!(report["sing_box_log"]["lines"], 1);
    assert_eq!(report["sing_box_log"]["groups"][0]["whom"], "server");
    assert_eq!(report["dns"][0]["name"], "google");
    assert_eq!(report["dns"][0]["last_miss"], "timeout");
    assert_eq!(report["selected"], by_name.id);
    assert_eq!(report["sing_box_version"], "1.14.1");
    assert_eq!(report["xray_version"], "26.10.21");
    let entry = report["journal"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["message"] == "export regression")
        .unwrap();
    let details = entry["details"].as_str().unwrap();
    for credential in ["timeout", "google", "secret-host"] {
        assert!(!details.contains(credential), "{details}");
    }
    // Neither the addresses nor the words of the log are in it.
    for word in ["secret-host", "198.51.100.7", "x.example"] {
        assert!(!text.contains(word), "{word} in the diagnostics");
    }
}

#[tokio::test]
async fn a_failed_start_and_a_crash_both_flush_reduced_failure_notes() {
    for failed_start in [true, false] {
        let (app, log, alive, _dir) = start_with(
            vec![parse_uri("trojan://p@127.0.0.1:9", None).unwrap()],
            failed_start,
            false,
        );
        if !failed_start {
            app.execute(Action::Connect).await.unwrap();
        }
        log.record("ERROR connection: dial tcp site.example:443: i/o timeout using outbound/direct[direct]", now());
        if failed_start {
            assert!(app.execute(Action::Connect).await.is_err());
        } else {
            alive.store(false, Ordering::SeqCst);
        }
        let mut rx = app.snapshots.subscribe();
        tokio::time::timeout(
            Duration::from_secs(5),
            rx.wait_for(|s| {
                s.journal
                    .iter()
                    .any(|row| row.message.starts_with("sing-box не смог"))
            }),
        )
        .await
        .expect("the reduced failure survives a disconnected runtime")
        .unwrap();
        app.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn the_deaf_warning_includes_the_latest_reduced_direct_failure() {
    let (app, log, _, _dir) = start_with(
        vec![parse_uri("trojan://p@127.0.0.1:9", None).unwrap()],
        false,
        true,
    );
    log.record(
        "ERROR connection: lookup private.example: i/o timeout using outbound/direct[direct]",
        now(),
    );
    app.execute(Action::Connect).await.unwrap();
    let mut rx = app.snapshots.subscribe();
    let state = tokio::time::timeout(
        Duration::from_secs(5),
        rx.wait_for(|s| {
            s.journal
                .iter()
                .any(|row| row.message.starts_with("sing-box не выходит"))
        }),
    )
    .await
    .unwrap()
    .unwrap()
    .clone();
    let warning = &state
        .journal
        .iter()
        .find(|row| row.message.starts_with("sing-box не выходит"))
        .unwrap()
        .message;
    assert!(
        warning.contains("не смог найти адрес по имени"),
        "{warning}"
    );
    assert!(warning.contains("нет ответа (таймаут)"), "{warning}");
    assert!(!warning.contains("private.example"));
    app.shutdown().await.unwrap();
}
