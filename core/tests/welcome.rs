//! The welcome window: due for an empty store that never answered it, gone for good once
//! something was added or the user skipped it.
use async_trait::async_trait;
use rift_core::{
    application::{Action, Application},
    model::{Filters, Server, Store, Subscription},
    parser::parse_uri,
    persistence,
    runtime::Runtime,
    selection::Probe,
    Result,
};
use std::{path::Path, sync::Arc};

struct Idle;
#[async_trait]
impl Runtime for Idle {
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
struct Fine;
#[async_trait]
impl Probe for Fine {
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
fn start(store: Store, dir: &Path) -> Arc<Application> {
    Application::with_probe(
        store,
        dir.into(),
        Box::new(Idle),
        Arc::new(Fine),
        "12345678901234567890123456789012".into(),
    )
}
fn stored(dir: &Path) -> Store {
    persistence::load(&dir.join("state.json")).unwrap()
}
const LINK: &str = "trojan://secret@a.example:443#A";
fn add_server() -> Action {
    Action::AddServers {
        uris: vec![LINK.into()],
    }
}

#[tokio::test]
async fn a_new_installation_is_welcomed() {
    let dir = tempfile::tempdir().unwrap();
    let app = start(Store::default(), dir.path());
    assert!(app.snapshot().welcome);
}

#[tokio::test]
async fn an_installation_with_servers_or_subscriptions_is_never_welcomed() {
    let dir = tempfile::tempdir().unwrap();
    let with_server = Store {
        servers: vec![parse_uri(LINK, None).unwrap()],
        ..Store::default()
    };
    assert!(!start(with_server, dir.path()).snapshot().welcome);
    let with_subscription = Store {
        subscriptions: vec![Subscription {
            id: "a".into(),
            name: "a".into(),
            url: "https://a.example/s".into(),
            ..Default::default()
        }],
        ..Store::default()
    };
    assert!(!start(with_subscription, dir.path()).snapshot().welcome);
}

#[tokio::test]
async fn skipping_is_remembered_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let app = start(Store::default(), dir.path());
    app.execute(Action::DismissWelcome).await.unwrap();
    assert!(!app.snapshot().welcome);
    assert!(
        stored(dir.path()).welcome_dismissed,
        "persisted, not only shown"
    );
    let restarted = start(stored(dir.path()), dir.path());
    assert!(!restarted.snapshot().welcome);
}

#[tokio::test]
async fn closing_rift_without_answering_shows_the_window_again() {
    let dir = tempfile::tempdir().unwrap();
    let app = start(Store::default(), dir.path());
    assert!(app.snapshot().welcome);
    drop(app);
    let restarted = start(
        persistence::load(&dir.path().join("state.json")).unwrap(),
        dir.path(),
    );
    assert!(
        restarted.snapshot().welcome,
        "nothing was added and nothing skipped"
    );
}

#[tokio::test]
async fn adding_a_server_ends_the_welcome_for_good() {
    let dir = tempfile::tempdir().unwrap();
    let app = start(Store::default(), dir.path());
    app.execute(add_server()).await.unwrap();
    assert!(!app.snapshot().welcome);
    assert!(stored(dir.path()).welcome_dismissed);
    // Emptied again: the window does not come back.
    let id = app.snapshot().servers[0].id.clone();
    app.execute(Action::RemoveServer { id }).await.unwrap();
    assert!(app.snapshot().servers.is_empty());
    assert!(!app.snapshot().welcome);
    assert!(!start(stored(dir.path()), dir.path()).snapshot().welcome);
}

#[tokio::test]
async fn adding_a_subscription_ends_the_welcome_for_good() {
    let dir = tempfile::tempdir().unwrap();
    let app = start(Store::default(), dir.path());
    app.execute(Action::SaveSubscription {
        id: None,
        url: "https://panel.example/sub".into(),
        interval_mins: None,
        user_agent: None,
        filters: Filters::default(),
        provider_routes: true,
    })
    .await
    .unwrap();
    assert!(!app.snapshot().welcome);
    assert!(stored(dir.path()).welcome_dismissed);
}

#[tokio::test]
async fn a_store_from_before_the_window_that_is_emptied_does_not_bring_it_back() {
    let dir = tempfile::tempdir().unwrap();
    // `welcome_dismissed` is absent from the old file.
    let old = Store {
        servers: vec![parse_uri(LINK, None).unwrap()],
        ..Store::default()
    };
    let app = start(old, dir.path());
    assert!(!app.snapshot().welcome);
    let id = app.snapshot().servers[0].id.clone();
    app.execute(Action::RemoveServer { id }).await.unwrap();
    assert!(
        !app.snapshot().welcome,
        "the user had servers: this is not a first run"
    );
}

#[test]
fn an_old_state_file_without_the_flag_loads_as_not_answered() {
    let json = r#"{"version":1,"settings":{},"servers":[],"subscriptions":[],"selected":null}"#;
    let store = persistence::decode(json.as_bytes()).unwrap();
    assert!(!store.welcome_dismissed);
    assert!(store.welcome_due());
}

#[tokio::test]
async fn a_failed_save_does_not_pretend_the_window_was_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let app = start(Store::default(), dir.path());
    // A directory where the state file should be: the atomic replace fails.
    std::fs::create_dir(dir.path().join("state.json")).unwrap();
    assert!(app.execute(Action::DismissWelcome).await.is_err());
    assert!(app.snapshot().welcome, "still due: the window stays");
    std::fs::remove_dir(dir.path().join("state.json")).unwrap();
    app.execute(Action::DismissWelcome).await.unwrap();
    assert!(!app.snapshot().welcome);
}

#[tokio::test]
async fn skipping_twice_is_harmless() {
    let dir = tempfile::tempdir().unwrap();
    let app = start(Store::default(), dir.path());
    app.execute(Action::DismissWelcome).await.unwrap();
    app.execute(Action::DismissWelcome).await.unwrap();
    assert!(!app.snapshot().welcome);
}

#[tokio::test]
async fn importing_a_profile_with_content_ends_the_welcome() {
    let dir = tempfile::tempdir().unwrap();
    let app = start(Store::default(), dir.path());
    let profile = Store {
        servers: vec![parse_uri(LINK, None).unwrap()],
        ..Store::default()
    };
    app.import_profile(profile).await.unwrap();
    assert!(!app.snapshot().welcome);
    assert!(stored(dir.path()).welcome_dismissed);
}
