//! Offline config check of the production compiler against a previously verified binary.
use rift_core::{connection::Connection, failure::FailureCode, parser::parse_uri};
use serde_json::{json, Value};
use std::{
    io::Write,
    process::{Command, Stdio},
};

fn main() {
    // nosemgrep: rust.lang.security.args-os.args-os -- Explicit argv[1] tool input; argv[0] is never used as executable identity.
    let binary = std::env::args_os()
        .nth(1)
        .expect("path to verified Xray 26.10.21 required");
    let version = Command::new(&binary)
        .arg("version")
        .output()
        .expect("Xray version");
    assert!(version.status.success() && version.stdout.starts_with(b"Xray 26.10.21 "));
    let corpus: Value =
        serde_json::from_str(include_str!("../tests/fixtures/xray-migration-cases.json")).unwrap();
    let (mut checked, mut retired) = (0, 0);
    let mut rejected = Vec::new();
    for case in corpus["cases"].as_array().unwrap().iter() {
        let server = if case["kind"] == "uri" {
            parse_uri(case["input"].as_str().unwrap(), None).unwrap()
        } else {
            rift_core::subscription::parse_subscription(&case["input"].to_string(), "synthetic")
                .unwrap()
                .servers
                .remove(0)
        };
        let node = Connection::from_legacy(&server.outbound).unwrap();
        let outbound = match node.xray_outbound("candidate") {
            Ok(outbound) => {
                assert_eq!(case["retired"], false);
                outbound
            }
            Err(error) => {
                assert!(case["retired"] == true || case["id"] == "vless-reality-ws");
                assert_eq!(
                    error.failure_code(),
                    Some(FailureCode::UnsupportedLegacyParameters)
                );
                retired += 1;
                continue;
            }
        };
        let mut child = Command::new(&binary)
            .args(["run", "-test", "-c", "stdin:", "-format", "json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let config =
            serde_json::to_vec(&json!({"log":{"loglevel":"none"},"outbounds":[outbound]})).unwrap();
        child.stdin.take().unwrap().write_all(&config).unwrap();
        if !child.wait().unwrap().success() {
            rejected.push(case["id"].clone());
        }
        checked += 1;
    }
    assert_eq!((checked, retired), (78, 52));
    println!(
        "Production compiler: checked {checked}, accepted {}, retired {retired}",
        checked - rejected.len()
    );
    assert!(
        rejected.is_empty(),
        "Upstream rejected synthetic cases: {rejected:?}"
    );
}
