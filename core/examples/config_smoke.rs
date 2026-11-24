use rift_core::{
    config,
    model::{Mode, Store},
    parser::{parse_uri, UTLS_FINGERPRINTS},
};
use std::path::Path;
fn check(binary: &str, path: &Path, store: &Store) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::write(
        path,
        serde_json::to_vec(&config::generate(
            store,
            "test-secret-012345678901234567890123",
        )?)?,
    )?;
    let result = std::process::Command::new(binary)
        .args(["check", "-c"])
        .arg(path)
        .output()?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).to_string().into());
    }
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Dev smoke tool: the argument is the developer-supplied sing-box path.
    let binary = std::env::args().nth(1).ok_or("pass sing-box binary path")?; // nosemgrep: rust.lang.security.args.args
    let uris=[
        "vless://11111111-1111-4111-8111-111111111111@example.com:443?security=tls&type=ws&path=%2Fws",
        "vless://11111111-1111-4111-8111-111111111111@example.com:443?security=reality&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&sid=abcd&sni=example.com&flow=xtls-rprx-vision",
        "trojan://test-password@example.com:443?type=grpc&serviceName=test",
        "ss://aes-256-gcm:test-password@example.com:8388",
    ];
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("smoke.json");
    for uri in uris {
        for mode in [Mode::Proxy, Mode::Tun, Mode::TunProxy] {
            let mut store = Store::default();
            store.settings.mode = mode;
            store.servers.push(parse_uri(uri, None)?);
            check(&binary, &path, &store)?;
        }
    }
    // The parser allowlist must match what this sing-box accepts.
    for fp in UTLS_FINGERPRINTS {
        let mut store = Store::default();
        store.servers.push(parse_uri(
            &format!(
                "vless://11111111-1111-4111-8111-111111111111@example.com:443?security=tls&fp={fp}"
            ),
            None,
        )?);
        check(&binary, &path, &store)?;
    }
    println!(
        "12 protocol/mode configurations and {} uTLS fingerprints validated",
        UTLS_FINGERPRINTS.len()
    );
    Ok(())
}
