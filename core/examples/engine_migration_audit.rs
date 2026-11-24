//! Synthetic, offline observations from the production parser and config generator.
//! This is not a second compiler. When Runtime gains a launch plan, call that same plan here.
//! Stdout contains synthetic configs for the parent auditor; reports retain only classifications.
use rift_core::{
    config::Resolver,
    geodata::GeoSet,
    launch::LaunchPlan,
    model::{Mode, Store, Subscription},
    parser::parse_uri,
    xray, Error,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct Corpus {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    id: String,
    kind: String,
    input: Value,
}

fn failure(case: &Case, mode: &str, stage: &str, error: &Error) -> Value {
    json!({"case_id":case.id,"mode":mode,"error":{
        "stage":stage,"code":error.failure_code().map_or("untyped", |code| code.as_str())}})
}

fn main() {
    let corpus: Corpus =
        serde_json::from_str(include_str!("../tests/fixtures/xray-migration-cases.json"))
            .expect("checked-in synthetic corpus must be valid");
    let mut rows = Vec::new();
    for case in corpus.cases {
        for (name, mode) in [
            ("proxy", Mode::Proxy),
            ("tun", Mode::Tun),
            ("tun_proxy", Mode::TunProxy),
        ] {
            let server = match case.kind.as_str() {
                "uri" => parse_uri(case.input.as_str().expect("synthetic URI"), None),
                "xray_json" => xray::parse(&case.input.to_string(), "synthetic", false).and_then(
                    |mut parsed| {
                        if parsed.servers.len() == 1 && parsed.rejected == 0 {
                            Ok(parsed.servers.remove(0))
                        } else {
                            Err(Error::Unavailable)
                        }
                    },
                ),
                _ => panic!("unknown synthetic input kind"),
            };
            let server = match server {
                Ok(server) => server,
                Err(error) => {
                    rows.push(failure(&case, name, "parse", &error));
                    continue;
                }
            };
            let mut store = Store::default();
            store.settings.mode = mode;
            store.selected = Some(server.id.clone());
            // JSON import has source-owned rows. Match the subscription present in a
            // real imported Store, rather than making the audit row an orphan.
            if let Some(source) = &server.source {
                store.subscriptions.push(Subscription {
                    id: source.clone(),
                    name: "Synthetic".into(),
                    url: "https://subscriptions.example/synthetic".into(),
                    ..Default::default()
                });
            }
            store.servers.push(server);
            // Same trusted launch plan used by Sidecar::validate/start. The frozen Node
            // checker and corpus are unchanged; this adapter now observes both processes.
            match LaunchPlan::build(
                &store,
                "synthetic-audit-secret-01234567890123456789",
                &GeoSet::none(),
                Resolver::Bridge(1),
                31337,
                "synthetic-audit-handoff-01234567890123456789",
            ) {
                Ok(plan) => rows.push(json!({"case_id":case.id,"mode":name,
                    "plans":[{"engine":"sing-box","config":plan.sing_box()},
                        {"engine":"xray","config":plan.xray()}]})),
                Err(error) => rows.push(failure(&case, name, "compile", &error)),
            }
        }
    }
    println!("{}", json!({"schema":1,"rows":rows}));
}
