//! Text given to Rift (a paste, a QR code, a `rift://` link): what it is taken for, what is
//! shown before anything is added, and the atomic addition of several servers.
use async_trait::async_trait;
use proptest::prelude::*;
use rift_core::{
    application::{Action, Application},
    link::{classify, preview_input, Input, InputPreview, MAX_INPUT_BYTES, MAX_LINKS},
    model::{Server, Store},
    runtime::Runtime,
    selection::Probe,
    Error, Result,
};
use std::sync::Arc;

const UUID: &str = "11111111-1111-4111-8111-111111111111";
fn vless(host: &str) -> String {
    format!("vless://{UUID}@{host}:443?security=tls#Server%20{host}")
}
fn trojan(host: &str) -> String {
    format!("trojan://secret@{host}:443#T-{host}")
}
fn servers(links: &[&str]) -> Input {
    Input::Servers(links.iter().map(|link| link.to_string()).collect())
}
fn subscription(url: &str) -> Input {
    Input::Subscription(url.into())
}
fn encoded(link: &str) -> String {
    url::form_urlencoded::byte_serialize(link.as_bytes()).collect()
}

#[test]
fn a_subscription_link_is_found_in_prose_and_punctuation() {
    for text in [
        "https://panel.example/sub?token=abc",
        "  https://panel.example/sub?token=abc\n",
        "Вот ссылка: https://panel.example/sub?token=abc, спасибо",
        "(https://panel.example/sub?token=abc).",
        "<https://panel.example/sub?token=abc>",
        "\"https://panel.example/sub?token=abc\"",
        "see 'https://panel.example/sub?token=abc'!",
    ] {
        assert_eq!(
            classify(text).unwrap(),
            subscription("https://panel.example/sub?token=abc"),
            "{text:?}"
        );
    }
    // The scheme is not case-sensitive; the rest of the link is left as it is.
    assert_eq!(
        classify("HTTPS://Panel.Example/Sub").unwrap(),
        subscription("https://Panel.Example/Sub")
    );
}

#[test]
fn the_first_web_link_wins_among_several() {
    assert_eq!(
        classify("https://one.example/a https://two.example/b").unwrap(),
        subscription("https://one.example/a")
    );
}

#[test]
fn every_server_scheme_is_a_server() {
    let links = [
        vless("a.example"),
        trojan("b.example"),
        "ss://YWVzLTI1Ni1nY206cGFzcw@c.example:8388#S".to_owned(),
        "vmess://eyJhZGQiOiJkLmV4YW1wbGUifQ==".to_owned(),
    ];
    for link in &links {
        assert_eq!(
            classify(&format!("take this: {link} please")).unwrap(),
            servers(&[link]),
            "{link}"
        );
    }
}

#[test]
fn several_servers_come_in_order_without_repeats_and_beat_a_web_link() {
    let (a, b) = (vless("a.example"), trojan("b.example"));
    let text = format!("{a}\n{b}, {a}\nhttps://panel.example/sub\n");
    assert_eq!(classify(&text).unwrap(), servers(&[&a, &b]));
}

#[test]
fn names_with_spaces_are_cut_at_the_space() {
    // The link ends at whitespace: the name is cosmetic and the server is the same.
    let text = format!("vless://{UUID}@a.example:443?security=tls#My Server");
    let Input::Servers(links) = classify(&text).unwrap() else {
        panic!("not servers");
    };
    assert!(links[0].ends_with("#My"));
}

#[test]
fn words_that_only_end_like_a_scheme_are_not_links() {
    for text in [
        "bypass://x",
        "css://x.example",
        "oss://x",
        "https:/x",
        "hello world",
        "",
    ] {
        assert!(classify(text).is_err(), "{text:?}");
    }
}

#[test]
fn a_deep_link_carries_a_subscription_or_one_server() {
    let sub = "https://panel.example/sub?token=a&b=c";
    assert_eq!(
        classify(&format!("rift://add?url={}", encoded(sub))).unwrap(),
        subscription(sub)
    );
    let one = vless("a.example");
    assert_eq!(
        classify(&format!("rift://ADD/?uri={}", encoded(&one))).unwrap(),
        servers(&[&one])
    );
    // A browser may wrap it in text, and the scheme is not case-sensitive.
    assert_eq!(
        classify(&format!("Open RIFT://add?url={} now", encoded(sub))).unwrap(),
        subscription(sub)
    );
}

#[test]
fn a_deep_link_is_taken_only_in_the_exact_shape() {
    let sub = encoded("https://panel.example/sub");
    let server = encoded(&vless("a.example"));
    for bad in [
        "rift://add".to_owned(),
        "rift://add?".to_owned(),
        "rift://other?url=x".to_owned(),
        format!("rift://add/more?url={sub}"),
        format!("rift://user@add?url={sub}"),
        format!("rift://add?url={sub}&uri={server}"),
        format!("rift://add?url={sub}&extra=1"),
        "rift://add?foo=1".to_owned(),
        // The parameter has to match what it names.
        format!("rift://add?url={server}"),
        format!("rift://add?uri={sub}"),
        format!("rift://add?url={}", encoded("http://insecure.example/s")),
        format!("rift://add?url={}", encoded("javascript:alert(1)")),
        format!(
            "rift://add?url={}",
            encoded(&"https://x.example/".repeat(1000))
        ),
    ] {
        assert!(classify(&bad).is_err(), "{bad}");
    }
}

#[test]
fn a_bad_deep_link_does_not_hide_a_good_link_next_to_it() {
    assert_eq!(
        classify("rift://add?foo=1 https://panel.example/sub").unwrap(),
        subscription("https://panel.example/sub")
    );
}

#[test]
fn the_number_of_links_and_the_size_of_the_text_are_limited() {
    let links: Vec<String> = (0..=MAX_LINKS)
        .map(|i| trojan(&format!("h{i}.example")))
        .collect();
    assert!(
        classify(&links[..MAX_LINKS].join("\n")).is_ok(),
        "exactly 100"
    );
    assert!(classify(&links.join("\n")).is_err(), "101");
    let huge = format!("{} https://a.example/s", " ".repeat(MAX_INPUT_BYTES));
    assert!(classify(&huge).is_err());
}

#[test]
fn the_preview_of_a_subscription_is_its_valid_https_link() {
    let preview = preview_input("get https://panel.example/sub?token=abc").unwrap();
    assert!(
        matches!(&preview, InputPreview::Subscription { url } if url == "https://panel.example/sub?token=abc")
    );
    let json = serde_json::to_value(&preview).unwrap();
    assert_eq!(json["kind"], "subscription");
    // Credentials in the address are refused like in the real import.
    assert!(preview_input("https://user:pass@panel.example/sub").is_err());
}

#[test]
fn the_preview_of_servers_shows_what_will_be_added_and_counts_the_rest() {
    let good = vless("a.example");
    let text = format!("{good}\nvless://not-a-valid-link\n{}", trojan("b.example"));
    let InputPreview::Servers { items, rejected } = preview_input(&text).unwrap() else {
        panic!("not servers");
    };
    assert_eq!(rejected, 1);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].uri, good);
    assert_eq!(items[0].server.name, "Server a.example");
    assert_eq!(items[0].server.protocol, "vless");
    assert_eq!(items[1].server.protocol, "trojan");
    let json = serde_json::to_string(&items[0].server).unwrap();
    assert!(
        !json.contains(UUID),
        "the preview of a server carries no credentials"
    );

    assert!(preview_input("vless://not-a-valid-link vmess://also-bad").is_err());
}

proptest! {
    #[test]
    fn any_text_is_classified_without_panicking(text in "(?s).{0,600}") {
        match classify(&text) {
            Ok(Input::Subscription(url)) => prop_assert!(url.starts_with("https://")),
            Ok(Input::Servers(links)) => {
                prop_assert!(!links.is_empty() && links.len() <= MAX_LINKS);
                for link in links {
                    prop_assert!(["vless://", "vmess://", "trojan://", "ss://"]
                        .iter()
                        .any(|scheme| link.starts_with(scheme)));
                }
            }
            Err(_) => {}
        }
        let _ = preview_input(&text);
    }

    #[test]
    fn a_link_inside_any_prose_is_found(before in "[a-zа-я ,.:]{0,40}", after in "[a-zа-я ,.:]{0,40}") {
        let link = "https://panel.example/sub?token=abc";
        prop_assert_eq!(
            classify(&format!("{before} {link} {after}")).unwrap(),
            Input::Subscription(link.into())
        );
    }
}

// ---- adding several servers --------------------------------------------------------------

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
fn app(store: Store) -> (Arc<Application>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let app = Application::with_probe(
        store,
        dir.path().into(),
        Box::new(Idle),
        Arc::new(Fine),
        "12345678901234567890123456789012".into(),
    );
    (app, dir)
}
fn add(uris: &[String]) -> Action {
    Action::AddServers {
        uris: uris.to_vec(),
    }
}

#[tokio::test]
async fn several_servers_are_added_together() {
    let (app, dir) = app(Store::default());
    app.execute(add(&[vless("a.example"), trojan("b.example")]))
        .await
        .unwrap();
    let snapshot = app.snapshot();
    assert_eq!(snapshot.servers.len(), 2);
    assert!(snapshot
        .servers
        .iter()
        .all(|server| server.source.is_none()));
    assert!(snapshot
        .journal
        .iter()
        .any(|entry| entry.message == "Серверов добавлено: 2"));
    // Stored, not only shown.
    let stored = rift_core::persistence::load(&dir.path().join("state.json")).unwrap();
    assert_eq!(stored.servers.len(), 2);
}

#[tokio::test]
async fn servers_that_are_already_there_are_not_added_twice() {
    let (app, _dir) = app(Store::default());
    let a = vless("a.example");
    app.execute(add(std::slice::from_ref(&a))).await.unwrap();
    app.execute(add(&[a.clone(), a.clone(), trojan("b.example")]))
        .await
        .unwrap();
    assert_eq!(app.snapshot().servers.len(), 2);
    app.execute(add(&[a])).await.unwrap();
    assert_eq!(app.snapshot().servers.len(), 2);
    assert!(app
        .snapshot()
        .journal
        .iter()
        .any(|entry| entry.message == "Серверов добавлено: 0"));
}

#[tokio::test]
async fn one_unreadable_link_adds_nothing() {
    let (app, _dir) = app(Store::default());
    let error = app
        .execute(add(&[vless("a.example"), "vless://broken".into()]))
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Invalid(_)));
    assert!(app.snapshot().servers.is_empty(), "all or none");
}

#[tokio::test]
async fn malformed_vmess_json_has_uri_context_and_safe_position_in_journal_and_export() {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    use rift_core::{
        diagnostics::{Operation, Rejection, Stage},
        failure::FailureCode,
        parser::{parse_uri, rejection},
    };

    let raw = "{\n\"provider-body-canary\":";
    let uri = format!("vmess://{}", URL_SAFE_NO_PAD.encode(raw));
    let error = parse_uri(&uri, None).err().expect("malformed VMess JSON");
    assert_eq!(
        error.to_string(),
        rift_core::text::t("core.parser.bad_vmess_json")
    );
    assert_eq!(error.failure_code(), Some(FailureCode::Unknown));
    assert_eq!(rejection(&uri, &error), Rejection::BadUri);
    assert_eq!(error.detail().unwrap().stage, Stage::Parse);
    assert_ne!(
        rift_core::diagnostics::Diagnostic::from_error(&error).operation,
        Operation::SubscriptionRefresh
    );

    let (app, dir) = app(Store::default());
    let error = app
        .execute(Action::AddServer { uri: uri.clone() })
        .await
        .unwrap_err();
    assert!(app.snapshot().servers.is_empty());
    let details = app
        .snapshot()
        .journal
        .last()
        .unwrap()
        .details
        .clone()
        .unwrap();
    app.execute(Action::ExportDiagnostics).await.unwrap();
    let export = std::fs::read_to_string(dir.path().join("diagnostics.json")).unwrap();
    for text in [&error.diagnostic_details(), &details, &export] {
        for expected in ["category=Eof", "line=2", "column=", "parse"] {
            assert!(text.contains(expected), "{expected} missing from {text}");
        }
        for forbidden in [
            "provider-body-canary",
            uri.as_str(),
            "subscription_invalid_body",
            rift_core::text::t("core.parameters.not_xray_json"),
        ] {
            assert!(!text.contains(forbidden), "{forbidden} in {text}");
        }
    }
}

#[tokio::test]
async fn the_batch_size_is_limited() {
    let (app, _dir) = app(Store::default());
    assert!(app.execute(add(&[])).await.is_err());
    let many: Vec<String> = (0..=MAX_LINKS)
        .map(|i| trojan(&format!("h{i}.example")))
        .collect();
    assert!(app.execute(add(&many)).await.is_err());
    assert!(app.snapshot().servers.is_empty());
    app.execute(add(&many[..MAX_LINKS])).await.unwrap();
    assert_eq!(app.snapshot().servers.len(), MAX_LINKS);
}

#[test]
fn wrapped_vmess_links_accept_case_insensitive_schemes() {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let raw = r#"{"v":"2","ps":"Test","add":"example.com","port":"443","id":"11111111-1111-4111-8111-111111111111","net":"tcp"}"#;
    let uri = format!("VMESS://{}", URL_SAFE_NO_PAD.encode(raw));
    let wrapper = format!("rift://add?uri={}", encoded(&uri));
    assert!(preview_input(&wrapper).is_ok());
}
