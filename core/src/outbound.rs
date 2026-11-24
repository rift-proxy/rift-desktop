//! Typed allowlist for `Server.outbound`.
//!
//! `parser::parse_uri` is the only producer of outbounds, and it emits four protocols with a
//! fixed set of keys. A profile file is an untrusted second producer: the id fingerprint only
//! proves the file is self-consistent, and `sing-box check` accepts options that start
//! programs (`tor` with `executable_path`/`extra_args`) or bypass the tunnel (`direct`). So
//! every outbound read from a profile must be exactly something `parse_uri` could have built:
//! known type, known keys, bounded values. Anything else is rejected before it can reach the
//! generated config, `sing-box check` or the sidecar.
use crate::text::t;
use crate::{parser::UTLS_FINGERPRINTS, Error, Result};
use base64::{
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};
use serde_json::{Map, Value};

/// Longest host name (RFC 1035) and therefore the longest `server`.
pub const MAX_HOST_BYTES: usize = 253;
/// Every free-form string of an outbound comes from a URI capped at 16 KiB.
const MAX_TEXT_BYTES: usize = 16 * 1024;

pub const SHADOWSOCKS_METHODS: [&str; 18] = [
    "2022-blake3-aes-128-gcm",
    "2022-blake3-aes-256-gcm",
    "2022-blake3-chacha20-poly1305",
    "none",
    "aes-128-gcm",
    "aes-192-gcm",
    "aes-256-gcm",
    "chacha20-ietf-poly1305",
    "xchacha20-ietf-poly1305",
    "aes-128-ctr",
    "aes-192-ctr",
    "aes-256-ctr",
    "aes-128-cfb",
    "aes-192-cfb",
    "aes-256-cfb",
    "rc4-md5",
    "chacha20-ietf",
    "xchacha20",
];
/// VMess `security` values that bundled sing-box accepts.
pub const VMESS_SECURITIES: [&str; 6] = [
    "auto",
    "none",
    "zero",
    "aes-128-gcm",
    "chacha20-poly1305",
    "aes-128-ctr",
];

/// What the rest of a `Server` row is derived from.
#[derive(Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub protocol: String,
    pub host: String,
    pub port: u16,
}

fn invalid() -> Error {
    Error::Invalid(t("core.parser.bad_server_params").into())
}

/// 2022 ciphers need base64 keys of the cipher's length, joined by `:` for relays.
pub fn shadowsocks_password_valid(method: &str, password: &str) -> bool {
    if !SHADOWSOCKS_METHODS.contains(&method) || password.is_empty() {
        return false;
    }
    if !method.starts_with("2022-") {
        return true;
    }
    let required = if method == "2022-blake3-aes-128-gcm" {
        16
    } else {
        32
    };
    password.split(':').all(|key| {
        STANDARD
            .decode(key)
            .or_else(|_| STANDARD_NO_PAD.decode(key))
            .is_ok_and(|bytes| bytes.len() == required)
    })
}
pub fn reality_key_valid(key: &str) -> bool {
    URL_SAFE_NO_PAD
        .decode(key)
        .or_else(|_| URL_SAFE.decode(key))
        .is_ok_and(|bytes| bytes.len() == 32)
}
pub fn reality_short_id_valid(id: &str) -> bool {
    id.len() <= 16 && id.len().is_multiple_of(2) && id.bytes().all(|c| c.is_ascii_hexdigit())
}

/// The keys of `value` must be exactly `required` plus any subset of `optional`.
fn object<'a>(
    value: &'a Value,
    required: &[&str],
    optional: &[&str],
) -> Result<&'a Map<String, Value>> {
    let map = value.as_object().ok_or_else(invalid)?;
    let known = |key: &str| required.contains(&key) || optional.contains(&key);
    if required.iter().any(|key| !map.contains_key(*key)) || !map.keys().all(|key| known(key)) {
        return Err(invalid());
    }
    Ok(map)
}
fn text<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    map.get(key)
        .and_then(Value::as_str)
        .filter(|text| text.len() <= MAX_TEXT_BYTES)
        .ok_or_else(invalid)
}
fn required_text<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    Some(text(map, key)?)
        .filter(|text| !text.is_empty())
        .ok_or_else(invalid)
}
fn check_uuid(map: &Map<String, Value>) -> Result<()> {
    uuid::Uuid::parse_str(required_text(map, "uuid")?)
        .map(|_| ())
        .map_err(|_| invalid())
}
fn enabled(map: &Map<String, Value>) -> Result<()> {
    (map.get("enabled") == Some(&Value::Bool(true)))
        .then_some(())
        .ok_or_else(invalid)
}

/// Returns whether TLS carries Reality.
fn tls(value: &Value) -> Result<bool> {
    let map = object(
        value,
        &["enabled", "server_name"],
        &["utls", "alpn", "reality"],
    )?;
    enabled(map)?;
    required_text(map, "server_name")?;
    if let Some(utls) = map.get("utls") {
        let utls = object(utls, &["enabled", "fingerprint"], &[])?;
        enabled(utls)?;
        if !UTLS_FINGERPRINTS.contains(&text(utls, "fingerprint")?) {
            return Err(invalid());
        }
    }
    if let Some(alpn) = map.get("alpn") {
        let protocols = alpn.as_array().ok_or_else(invalid)?;
        if protocols.len() > MAX_TEXT_BYTES
            || !protocols
                .iter()
                .all(|p| p.as_str().is_some_and(|p| p.len() <= MAX_TEXT_BYTES))
        {
            return Err(invalid());
        }
    }
    let Some(reality) = map.get("reality") else {
        return Ok(false);
    };
    let reality = object(reality, &["enabled", "public_key", "short_id"], &[])?;
    enabled(reality)?;
    // Reality always pairs with a uTLS fingerprint (the parser defaults it to chrome).
    if !map.contains_key("utls")
        || !reality_key_valid(text(reality, "public_key")?)
        || !reality_short_id_valid(text(reality, "short_id")?)
    {
        return Err(invalid());
    }
    Ok(true)
}
fn transport(value: &Value) -> Result<()> {
    let kind = value.get("type").and_then(Value::as_str);
    match kind {
        Some("ws") => {
            let map = object(value, &["type", "path"], &["headers"])?;
            text(map, "path")?;
            if let Some(headers) = map.get("headers") {
                text(object(headers, &["Host"], &[])?, "Host")?;
            }
        }
        Some("grpc") => {
            let map = object(value, &["type", "service_name"], &[])?;
            text(map, "service_name")?;
        }
        Some("xhttp") => {
            crate::parameters::Xhttp::legacy(value.clone())?;
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

/// Accepts only an outbound `parser` can build and returns the row fields derived from it.
pub fn inspect(outbound: &Value) -> Result<Endpoint> {
    let kind = outbound
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let (required, optional): (&[&str], &[&str]) = match kind {
        "vless" => (
            &["type", "server", "server_port", "uuid"],
            &["flow", "tls", "transport", "finalmask"],
        ),
        "vmess" => (
            &[
                "type",
                "server",
                "server_port",
                "uuid",
                "alter_id",
                "security",
            ],
            &["tls", "transport", "finalmask"],
        ),
        "trojan" => (
            &["type", "server", "server_port", "password"],
            &["tls", "transport", "finalmask"],
        ),
        "shadowsocks" => (
            &["type", "server", "server_port", "method", "password"],
            &[],
        ),
        _ => {
            return Err(Error::Invalid(
                t("core.common.unsupported_server_type").into(),
            ))
        }
    };
    let map = object(outbound, required, optional)?;
    let host = required_text(map, "server")?;
    let port = map
        .get("server_port")
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port != 0)
        .ok_or_else(invalid)?;
    if host.len() > MAX_HOST_BYTES || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid());
    }
    match kind {
        "vless" => {
            check_uuid(map)?;
            if let Some(flow) = map.get("flow") {
                if flow.as_str() != Some("xtls-rprx-vision") {
                    return Err(invalid());
                }
            }
        }
        "vmess" => {
            check_uuid(map)?;
            if map
                .get("alter_id")
                .and_then(Value::as_u64)
                .is_none_or(|id| u32::try_from(id).is_err())
                || !VMESS_SECURITIES.contains(&text(map, "security")?)
            {
                return Err(invalid());
            }
        }
        "trojan" => {
            required_text(map, "password")?;
        }
        _ => {
            if !shadowsocks_password_valid(text(map, "method")?, text(map, "password")?) {
                return Err(invalid());
            }
        }
    }
    let reality = map.get("tls").map(tls).transpose()?.unwrap_or(false);
    if let Some(mask) = map.get("finalmask") {
        if !map.contains_key("tls") {
            return Err(crate::parameters::ParameterProblem::Incompatible.into());
        }
        crate::parameters::FinalMask::read(mask.clone())?;
    }
    if let Some(transport_value) = map.get("transport") {
        transport(transport_value)?;
    }
    if reality && kind != "vless" {
        return Err(invalid());
    }
    Ok(Endpoint {
        protocol: if reality {
            "vless reality".into()
        } else {
            kind.into()
        },
        host: host.into(),
        port,
    })
}
