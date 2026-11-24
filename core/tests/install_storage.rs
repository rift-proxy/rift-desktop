use rift_core::{model::Store, parser::parse_uri, persistence, storage};
use std::{fs, path::PathBuf};

struct Fixture {
    _root: tempfile::TempDir,
    current: PathBuf,
    legacy: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("Installed Rift/данные/data");
        let legacy = root.path().join("legacy-app-data");
        storage::prepare(&current).unwrap();
        fs::create_dir_all(&legacy).unwrap();
        Self {
            _root: root,
            current,
            legacy,
        }
    }

    fn old_store(&self) -> Store {
        let mut store = Store::default();
        store.settings.theme = "dark".into();
        store
            .servers
            .push(parse_uri("trojan://synthetic@example.com:443#Legacy%20name", None).unwrap());
        store.selected = Some(store.servers[0].id.clone());
        persistence::save(&self.legacy.join("state.json"), &store).unwrap();
        store
    }

    fn load(&self) -> rift_core::Result<Store> {
        storage::load_or_migrate(&self.current, &self.legacy)
    }
}

#[test]
fn fresh_start_commits_defaults_and_never_imports_legacy_on_a_later_run() {
    let f = Fixture::new();
    let first = f.load().unwrap();
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(Store::default()).unwrap()
    );
    assert!(
        f.current.join("state.json").is_file(),
        "first-run completion must survive restart"
    );
    f.old_store();
    let second = f.load().unwrap();
    assert!(
        second.servers.is_empty(),
        "a later legacy state must not replace the first-run local state"
    );
    assert!(f.current.join("tmp").is_dir());
}

#[test]
fn migrates_state_and_companion_files_without_touching_originals_or_webview_cache() {
    let f = Fixture::new();
    let expected = f.old_store();
    fs::write(
        f.legacy.join("journal.json"),
        b"[{\"message\":\"old journal\"}]",
    )
    .unwrap();
    fs::write(f.legacy.join("diagnostics.json"), b"{\"servers\":1}").unwrap();
    fs::create_dir(f.legacy.join("EBWebView")).unwrap();
    fs::write(f.legacy.join("EBWebView/Cookies"), b"old browser cache").unwrap();
    let originals: Vec<_> = ["state.json", "journal.json", "diagnostics.json"]
        .into_iter()
        .map(|name| (name, fs::read(f.legacy.join(name)).unwrap()))
        .collect();
    let actual = f.load().unwrap();
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    for (name, bytes) in originals {
        assert_eq!(
            fs::read(f.legacy.join(name)).unwrap(),
            bytes,
            "legacy {name} changed"
        );
        assert_eq!(
            fs::read(f.current.join(name)).unwrap(),
            bytes,
            "local {name} missing or different"
        );
    }
    assert!(!f.current.join("EBWebView").exists());
    assert_eq!(
        fs::read(f.legacy.join("EBWebView/Cookies")).unwrap(),
        b"old browser cache"
    );
}

#[test]
fn missing_optional_files_do_not_block_a_valid_migration() {
    let f = Fixture::new();
    f.old_store();
    assert_eq!(f.load().unwrap().servers.len(), 1);
    assert!(f.current.join("state.json").is_file());
    assert!(!f.current.join("journal.json").exists());
    assert!(!f.current.join("diagnostics.json").exists());
}

#[test]
fn local_state_wins_and_no_legacy_files_are_read_on_subsequent_runs() {
    let f = Fixture::new();
    f.old_store();
    f.load().unwrap();
    let original = fs::read(f.current.join("state.json")).unwrap();
    fs::write(f.legacy.join("state.json"), b"invalid legacy state").unwrap();
    fs::create_dir(f.legacy.join("journal.json")).unwrap();
    fs::create_dir(f.legacy.join("diagnostics.json")).unwrap();
    assert_eq!(f.load().unwrap().servers.len(), 1);
    assert_eq!(fs::read(f.current.join("state.json")).unwrap(), original);
}

#[test]
fn damaged_local_state_is_an_error_and_is_never_replaced_by_valid_legacy_state() {
    let f = Fixture::new();
    f.old_store();
    fs::write(f.current.join("state.json"), b"damaged local state").unwrap();
    assert!(f.load().is_err());
    assert_eq!(
        fs::read(f.current.join("state.json")).unwrap(),
        b"damaged local state"
    );
    assert!(!f.current.join("journal.json").exists());
}

#[test]
fn non_file_local_state_is_not_treated_as_missing() {
    let f = Fixture::new();
    f.old_store();
    fs::create_dir(f.current.join("state.json")).unwrap();
    assert!(f.load().is_err());
    assert!(f.current.join("state.json").is_dir());
}

#[test]
fn invalid_legacy_json_and_invalid_domain_data_do_not_publish_any_migration() {
    for bytes in [
        b"invalid".to_vec(),
        {
            let store = Store {
                version: 3,
                ..Store::default()
            };
            serde_json::to_vec(&store).unwrap()
        },
        {
            let mut store = Store::default();
            store.settings.proxy_port = 0;
            serde_json::to_vec(&store).unwrap()
        },
    ] {
        let f = Fixture::new();
        fs::write(f.legacy.join("state.json"), &bytes).unwrap();
        fs::write(f.legacy.join("journal.json"), b"[]").unwrap();
        assert!(f.load().is_err());
        assert!(!f.current.join("state.json").exists());
        assert!(!f.current.join("journal.json").exists());
        assert_eq!(fs::read(f.legacy.join("state.json")).unwrap(), bytes);
    }
}

#[test]
fn unreadable_legacy_state_fails_without_default_fallback() {
    let f = Fixture::new();
    fs::create_dir(f.legacy.join("state.json")).unwrap();
    assert!(f.load().is_err());
    assert!(!f.current.join("state.json").exists());
}

#[test]
fn failed_companion_read_leaves_state_unpublished_and_partial_migration_is_retryable() {
    let f = Fixture::new();
    f.old_store();
    fs::write(f.legacy.join("journal.json"), b"[1]").unwrap();
    fs::create_dir(f.legacy.join("diagnostics.json")).unwrap();
    assert!(f.load().is_err());
    assert!(
        !f.current.join("state.json").exists(),
        "state must be published last"
    );
    assert_eq!(fs::read(f.current.join("journal.json")).unwrap(), b"[1]");
    assert!(f.legacy.join("state.json").is_file());
    fs::remove_dir(f.legacy.join("diagnostics.json")).unwrap();
    fs::write(f.legacy.join("diagnostics.json"), b"{}").unwrap();
    assert_eq!(f.load().unwrap().servers.len(), 1);
    assert_eq!(fs::read(f.current.join("diagnostics.json")).unwrap(), b"{}");
    assert!(fs::read_dir(&f.current).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".tmp")));
}

#[test]
fn blocked_companion_destination_fails_before_state_is_published() {
    let f = Fixture::new();
    f.old_store();
    fs::write(f.legacy.join("diagnostics.json"), b"{}").unwrap();
    fs::create_dir(f.current.join("diagnostics.json")).unwrap();
    assert!(
        f.load().is_err(),
        "a directory cannot receive migrated diagnostics"
    );
    assert!(!f.current.join("state.json").exists());
    assert_eq!(fs::read(f.legacy.join("diagnostics.json")).unwrap(), b"{}");
}

#[test]
fn installation_storage_failure_never_falls_back_or_alters_legacy_files() {
    let f = Fixture::new();
    f.old_store();
    let blocked = f.current.join("unwritable-root");
    fs::write(&blocked, b"existing file").unwrap();
    assert!(storage::prepare(&blocked).is_err());
    assert_eq!(fs::read(&blocked).unwrap(), b"existing file");
    assert!(f.legacy.join("state.json").is_file());
    assert!(!f.current.join("state.json").exists());
}

#[test]
fn a_blocked_temporary_directory_fails_storage_preparation() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("tmp"), b"do not overwrite").unwrap();
    assert!(storage::prepare(root.path()).is_err());
    assert_eq!(
        fs::read(root.path().join("tmp")).unwrap(),
        b"do not overwrite"
    );
}
