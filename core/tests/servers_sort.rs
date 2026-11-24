//! The order of the list of servers the user chose: saved with the state, read back from files
//! of every age, and changed only by its own action.
use async_trait::async_trait;
use rift_core::{
    application::{Action, Application},
    model::{Server, ServersSort, SortColumn, Store},
    parser::parse_uri,
    persistence,
    runtime::Runtime,
    selection::Probe,
    Result,
};
use serde_json::json;
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
        "x".repeat(32),
    )
}
fn sort(column: SortColumn, descending: bool) -> ServersSort {
    ServersSort { column, descending }
}
fn by(column: SortColumn, descending: bool) -> Action {
    Action::SetServersSort { column, descending }
}
fn state_with(sort: serde_json::Value) -> Vec<u8> {
    let mut state = serde_json::to_value(Store::default()).unwrap();
    match sort {
        serde_json::Value::Null => {
            state.as_object_mut().unwrap().remove("servers_sort");
        }
        other => {
            state["servers_sort"] = other;
        }
    }
    serde_json::to_vec(&state).unwrap()
}

#[test]
fn a_new_installation_orders_by_name_from_a_to_z() {
    let default = Store::default().servers_sort;
    assert_eq!(default, sort(SortColumn::Name, false));
    let app = start(Store::default(), tempfile::tempdir().unwrap().path());
    assert_eq!(app.snapshot().servers_sort, default);
}

#[test]
fn a_state_file_from_before_the_choice_existed_orders_by_name() {
    let store = persistence::decode(&state_with(serde_json::Value::Null)).unwrap();
    assert_eq!(store.servers_sort, sort(SortColumn::Name, false));
}

#[test]
fn every_column_survives_the_state_file_in_both_directions() {
    let names = [
        (SortColumn::Name, "name"),
        (SortColumn::Protocol, "protocol"),
        (SortColumn::EndpointMs, "endpoint_ms"),
        (SortColumn::ProxyMs, "proxy_ms"),
        (SortColumn::Status, "status"),
        (SortColumn::Source, "source"),
        (SortColumn::CheckedAt, "checked_at"),
    ];
    for (column, name) in names {
        for descending in [false, true] {
            let wire = json!({"column": name, "descending": descending});
            assert_eq!(
                serde_json::to_value(sort(column, descending)).unwrap(),
                wire
            );
            let store = persistence::decode(&state_with(wire)).unwrap();
            assert_eq!(store.servers_sort, sort(column, descending), "{name}");
        }
    }
}

#[test]
fn a_choice_this_version_does_not_know_reads_as_the_default_and_breaks_nothing() {
    for unknown in [
        json!({"column": "traffic", "descending": true}),
        json!({"column": 7, "descending": true}),
        json!({"column": null, "descending": true}),
        json!({"column": "name", "descending": "yes"}),
        json!({"column": "proxy_ms"}),
        json!({"descending": true}),
        json!({}),
        json!("proxy_ms"),
        json!([1, 2]),
        json!(5),
    ] {
        let store = persistence::decode(&state_with(unknown.clone()))
            .unwrap_or_else(|error| panic!("{unknown}: {error}"));
        assert_eq!(store.servers_sort, ServersSort::default(), "{unknown}");
    }
}

#[test]
fn the_action_names_a_column_from_the_list_only() {
    let action: Action = serde_json::from_value(
        json!({"type": "set_servers_sort", "column": "proxy_ms", "descending": true}),
    )
    .unwrap();
    assert!(matches!(
        action,
        Action::SetServersSort {
            column: SortColumn::ProxyMs,
            descending: true
        }
    ));
    for bad in [
        json!({"type": "set_servers_sort", "column": "traffic", "descending": false}),
        json!({"type": "set_servers_sort", "column": "name"}),
        json!({"type": "set_servers_sort", "descending": false}),
    ] {
        assert!(
            serde_json::from_value::<Action>(bad.clone()).is_err(),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn the_choice_is_saved_and_the_next_start_has_it() {
    let dir = tempfile::tempdir().unwrap();
    let app = start(Store::default(), dir.path());
    let snapshot = app.execute(by(SortColumn::ProxyMs, true)).await.unwrap();
    assert_eq!(snapshot.servers_sort, sort(SortColumn::ProxyMs, true));
    assert_eq!(app.snapshot().servers_sort, sort(SortColumn::ProxyMs, true));

    let saved = persistence::load(&dir.path().join("state.json")).unwrap();
    assert_eq!(saved.servers_sort, sort(SortColumn::ProxyMs, true));
    let again = start(saved, dir.path());
    assert_eq!(
        again.snapshot().servers_sort,
        sort(SortColumn::ProxyMs, true)
    );

    // And back to the default is a choice like another.
    again.execute(by(SortColumn::Name, false)).await.unwrap();
    let saved = persistence::load(&dir.path().join("state.json")).unwrap();
    assert_eq!(saved.servers_sort, ServersSort::default());
}

#[tokio::test]
async fn ordering_the_list_changes_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let server = parse_uri("trojan://secret@a.example:443#A", None).unwrap();
    let store = Store {
        selected: Some(server.id.clone()),
        servers: vec![server],
        ..Store::default()
    };
    let app = start(store, dir.path());
    let before = app.snapshot();
    let after = app.execute(by(SortColumn::Status, false)).await.unwrap();
    assert_eq!(after.selected, before.selected);
    assert_eq!(after.servers.len(), 1);
    assert_eq!(after.status, before.status);
    assert!(!after.checking && !after.pending);
    assert_eq!(after.settings.mode, before.settings.mode);
    // No entry in the journal: choosing an order is not an event.
    assert_eq!(after.journal.len(), before.journal.len());
    let saved = persistence::load(&dir.path().join("state.json")).unwrap();
    assert_eq!(saved.selected, before.selected);
    assert_eq!(saved.servers.len(), 1);
}

#[tokio::test]
async fn the_same_choice_again_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let app = start(Store::default(), dir.path());
    app.execute(by(SortColumn::Source, false)).await.unwrap();
    let state = dir.path().join("state.json");
    std::fs::remove_file(&state).unwrap();
    app.execute(by(SortColumn::Source, false)).await.unwrap();
    assert!(!state.exists());
}

#[tokio::test]
async fn a_choice_that_cannot_be_saved_is_not_shown_as_made() {
    let dir = tempfile::tempdir().unwrap();
    let app = start(Store::default(), dir.path());
    // The state file's place is taken by a directory: the write fails.
    std::fs::create_dir(dir.path().join("state.json")).unwrap();
    assert!(app.execute(by(SortColumn::Protocol, true)).await.is_err());
    assert_eq!(app.snapshot().servers_sort, ServersSort::default());
}
