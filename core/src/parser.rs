use crate::text::t;
use crate::{
    diagnostics::Rejection,
    model::{display_name, Server},
    outbound, Error, Result,
};
use base64::{
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD},
    Engine,
};
use percent_encoding::percent_decode_str;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use url::Url;

fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}

/// The reasons a line of a subscription is refused, by the words of the refusal. The words are
/// fixed here and nowhere else, so the class a refusal falls into cannot drift from its text
/// without a test of this file failing.
mod why {
    use crate::text::t;
    pub fn scheme() -> &'static str {
        t("core.parser.scheme")
    }
    pub fn unknown_params() -> &'static str {
        t("core.parser.unknown_params")
    }
    pub fn bad_uri() -> [&'static str; 7] {
        [
            t("core.parser.bad_uri"),
            t("core.parser.uri_too_long"),
            t("core.parser.bad_utf8"),
            t("core.parser.bad_base64"),
            t("core.parser.unexpected_path"),
            t("core.parser.repeated_param"),
            t("core.parser.bad_vmess_json"),
        ]
    }
    pub fn missing() -> [&'static str; 3] {
        [
            t("core.parser.no_host"),
            t("core.parser.no_port"),
            t("core.parser.no_credentials"),
        ]
    }
    pub fn reality() -> [&'static str; 4] {
        [
            t("core.parser.reality_needs_vless"),
            t("core.parser.bad_reality_public_key"),
            t("core.parser.bad_reality_short_id"),
            t("core.parser.reality_without_reality"),
        ]
    }
    pub fn transport() -> [&'static str; 4] {
        [
            t("core.parser.unsupported_transport"),
            t("core.parser.transport_with_tcp"),
            t("core.parser.service_name_grpc_only"),
            t("core.parser.ws_with_grpc"),
        ]
    }
}

/// The class a refusal of one line falls into. A line with no `://` is not a link at all.
pub fn rejection(line: &str, error: &Error) -> Rejection {
    if !line.contains("://") {
        return Rejection::NotALink;
    }
    rejection_of(error)
}

/// A fixed validation reason shared by URI and Xray JSON subscription rows.
pub(crate) fn rejection_of(error: &Error) -> Rejection {
    if let Error::Failure {
        source: Some(source),
        ..
    } = error
    {
        if let Some(source) = source.downcast_ref::<Error>() {
            return rejection_of(source);
        }
    }
    if let Error::Rejected(reason) = error {
        return *reason;
    }
    if let Error::Parameter(problem) = error {
        return problem.rejection();
    }
    let Error::Invalid(message) = error else {
        return Rejection::Other;
    };
    let message = message.as_str();
    if message == why::scheme() {
        Rejection::UnsupportedScheme
    } else if message == why::unknown_params() {
        Rejection::UnknownParam
    } else if why::bad_uri().contains(&message) {
        Rejection::BadUri
    } else if why::missing().contains(&message) {
        Rejection::MissingField
    } else if why::reality().contains(&message) {
        Rejection::BadReality
    } else if why::transport().contains(&message) {
        Rejection::UnsupportedTransport
    } else if [
        t("core.parser.bad_uuid"),
        t("core.parser.vless_password"),
        t("core.parser.password_url_encoded"),
        t("core.parser.bad_shadowsocks_credentials"),
        t("core.parser.empty_shadowsocks_credentials"),
        t("core.parser.bad_shadowsocks_2022_key"),
    ]
    .contains(&message)
    {
        Rejection::InvalidCredentials
    } else if [
        t("core.parser.unsupported_fingerprint"),
        t("core.parser.unsupported_vless_encryption"),
        t("core.parser.unsupported_vless_flow"),
        t("core.parser.unsupported_shadowsocks_method"),
        t("core.parser.insecure_tls"),
        t("core.parser.bad_tls_params"),
        t("core.parser.unsupported_vmess_cipher"),
        t("core.parser.bad_vmess_tls_params"),
    ]
    .contains(&message)
    {
        Rejection::UnsupportedSecurity
    } else if message == t("core.parser.bad_port") {
        Rejection::InvalidEndpoint
    } else if [
        t("core.parser.bad_server_params"),
        t("core.parser.bad_alter_id"),
    ]
    .contains(&message)
    {
        Rejection::InvalidField
    } else if [
        t("core.parser.vmess_header_type"),
        t("core.parser.shadowsocks_transport_tls"),
    ]
    .contains(&message)
    {
        Rejection::UnsupportedTransport
    } else {
        Rejection::Other
    }
}
/// uTLS fingerprints accepted by bundled sing-box 1.14.1 (`sing-box check`). Rejecting
/// others per row keeps one bad subscription line from failing the whole config.
pub const UTLS_FINGERPRINTS: [&str; 10] = [
    "chrome",
    "firefox",
    "edge",
    "safari",
    "360",
    "qq",
    "ios",
    "android",
    "random",
    "randomized",
];
fn utls(fingerprint: &str) -> Result<Value> {
    if !UTLS_FINGERPRINTS.contains(&fingerprint) {
        return Err(invalid(t("core.parser.unsupported_fingerprint")));
    }
    Ok(json!({"enabled":true,"fingerprint":fingerprint}))
}
pub fn decode64(text: &str) -> Result<String> {
    let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    [STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD]
        .iter()
        .find_map(|e| e.decode(&clean).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(|| invalid(why::bad_uri()[3]))
}
fn decode(s: &str) -> Result<String> {
    percent_decode_str(s)
        .decode_utf8()
        .map(|s| s.into_owned())
        .map_err(|_| invalid(why::bad_uri()[2]))
}
fn take(q: &mut BTreeMap<String, String>, key: &str) -> String {
    q.remove(key).unwrap_or_default()
}
fn transport(out: &mut Value, kind: &str, path: &str, host: &str, service: &str) -> Result<()> {
    match kind {
        "" | "tcp" => {
            if !path.is_empty() || !host.is_empty() || !service.is_empty() {
                return Err(invalid(why::transport()[1]));
            }
        }
        "ws" => {
            if !service.is_empty() {
                return Err(invalid(why::transport()[2]));
            }
            out["transport"] = json!({"type":"ws", "path":if path.is_empty(){"/"}else{path}});
            if !host.is_empty() {
                out["transport"]["headers"] = json!({"Host":host});
            }
        }
        "grpc" => {
            if !path.is_empty() || !host.is_empty() {
                return Err(invalid(why::transport()[3]));
            }
            out["transport"] = json!({"type":"grpc", "service_name":service});
        }
        _ => return Err(invalid(why::transport()[0])),
    }
    Ok(())
}
fn advanced_transport(
    out: &mut Value,
    kind: &str,
    path: &str,
    host: &str,
    service: &str,
    mode: Option<String>,
    extra: Option<Value>,
) -> Result<()> {
    use crate::parameters::{ParameterProblem as Problem, Xhttp};
    if kind == "xhttp" {
        if !service.is_empty() {
            return Err(Problem::Incompatible.into());
        }
        let mut value = json!({"type":"xhttp","path":path});
        if !host.is_empty() {
            value["host"] = json!(host);
        }
        if let Some(mode) = mode {
            value["mode"] = json!(mode);
        }
        if let Some(extra) = extra {
            value["extra"] = extra;
        }
        let checked = Xhttp::legacy(value)?;
        out["transport"] = json!(checked);
        out["transport"]["type"] = json!("xhttp");
    } else {
        transport(out, kind, path, host, service)?;
        if extra.is_some() {
            return Err(Problem::Incompatible.into());
        }
        if kind == "grpc" {
            if mode.as_deref().is_some_and(|m| !["", "gun"].contains(&m)) {
                return Err(Problem::Mode.into());
            }
        } else if mode.is_some() {
            return Err(Problem::Incompatible.into());
        }
    }
    Ok(())
}
pub fn parse_uri(input: &str, source: Option<String>) -> Result<Server> {
    let input = input.trim();
    if input.len() > 16384 {
        return Err(invalid(why::bad_uri()[1]));
    }
    if input
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("vmess://"))
    {
        return vmess(input, source);
    }
    let mut normalized = input.to_owned();
    if let Some(raw) = input.strip_prefix("ss://") {
        if !raw.split('#').next().unwrap_or("").contains('@') {
            let (encoded, name) = raw.split_once('#').unwrap_or((raw, ""));
            normalized = format!("ss://{}#{}", decode64(encoded)?, name);
        }
    }
    let uri = Url::parse(&normalized).map_err(|_| invalid(why::bad_uri()[0]))?;
    let kind = uri.scheme();
    if !["vless", "trojan", "ss"].contains(&kind) {
        return Err(invalid(why::scheme()));
    }
    let host = uri
        .host_str()
        .ok_or_else(|| invalid(why::missing()[0]))?
        .trim_matches(['[', ']'])
        .to_lowercase();
    // Port 0 and an empty host are rejected once, in `finish`, for every protocol.
    let port = uri.port().ok_or_else(|| invalid(why::missing()[1]))?;
    if !["", "/"].contains(&uri.path()) {
        return Err(invalid(why::bad_uri()[4]));
    }
    let mut q = BTreeMap::new();
    for (k, v) in uri.query_pairs() {
        if q.insert(k.into_owned(), v.into_owned()).is_some() {
            return Err(invalid(why::bad_uri()[5]));
        }
    }
    let mut out =
        json!({"type":if kind=="ss"{"shadowsocks"}else{kind},"server":host,"server_port":port});
    let user = decode(uri.username())?;
    if user.is_empty() {
        return Err(invalid(why::missing()[2]));
    }
    match kind {
        "vless" => {
            uuid::Uuid::parse_str(&user).map_err(|_| invalid(t("core.parser.bad_uuid")))?;
            if uri.password().is_some() {
                return Err(invalid(t("core.parser.vless_password")));
            }
            out["uuid"] = json!(user.to_lowercase());
            let encryption = take(&mut q, "encryption");
            if !["", "none"].contains(&encryption.as_str()) {
                return Err(invalid(t("core.parser.unsupported_vless_encryption")));
            }
            let flow = take(&mut q, "flow");
            if !["", "xtls-rprx-vision"].contains(&flow.as_str()) {
                return Err(invalid(t("core.parser.unsupported_vless_flow")));
            }
            if !flow.is_empty() {
                out["flow"] = json!(flow);
            }
        }
        "trojan" => {
            if uri.password().is_some() {
                return Err(invalid(t("core.parser.password_url_encoded")));
            }
            out["password"] = json!(user);
        }
        "ss" => {
            let credentials = if let Some(p) = uri.password() {
                format!("{}:{}", user, decode(p)?)
            } else {
                decode64(&user)?
            };
            let (method, password) = credentials
                .split_once(':')
                .ok_or_else(|| invalid(t("core.parser.bad_shadowsocks_credentials")))?;
            if method.is_empty() || password.is_empty() {
                return Err(invalid(t("core.parser.empty_shadowsocks_credentials")));
            }
            if !outbound::SHADOWSOCKS_METHODS.contains(&method) {
                return Err(invalid(t("core.parser.unsupported_shadowsocks_method")));
            }
            if !outbound::shadowsocks_password_valid(method, password) {
                return Err(invalid(t("core.parser.bad_shadowsocks_2022_key")));
            }
            out["method"] = json!(method);
            out["password"] = json!(password);
        }
        _ => unreachable!(),
    }
    let mut security = take(&mut q, "security");
    if kind == "trojan" && security.is_empty() {
        security = "tls".into();
    }
    let sni = take(&mut q, "sni");
    let fp = take(&mut q, "fp");
    let pbk = take(&mut q, "pbk");
    let sid = take(&mut q, "sid");
    let alpn = take(&mut q, "alpn");
    let insecure = take(&mut q, "allowInsecure");
    if !["", "0", "false"].contains(&insecure.as_str()) {
        return Err(invalid(t("core.parser.insecure_tls")));
    }
    if security == "tls" || security == "reality" {
        out["tls"] = json!({"enabled":true,"server_name":if sni.is_empty(){&host}else{&sni}});
        if !fp.is_empty() {
            out["tls"]["utls"] = utls(&fp)?;
        }
        if !alpn.is_empty() {
            out["tls"]["alpn"] = json!(alpn.split(',').collect::<Vec<_>>());
        }
        if security == "reality" {
            if kind != "vless" || pbk.is_empty() {
                return Err(invalid(why::reality()[0]));
            }
            if !outbound::reality_key_valid(&pbk) {
                return Err(invalid(why::reality()[1]));
            }
            if !outbound::reality_short_id_valid(&sid) {
                return Err(invalid(why::reality()[2]));
            }
            out["tls"]["reality"] = json!({"enabled":true,"public_key":pbk,"short_id":sid});
            if fp.is_empty() {
                out["tls"]["utls"] = json!({"enabled":true,"fingerprint":"chrome"});
            }
        } else if !pbk.is_empty() || !sid.is_empty() {
            return Err(invalid(why::reality()[3]));
        }
    } else if !["", "none"].contains(&security.as_str())
        || !sni.is_empty()
        || !fp.is_empty()
        || !pbk.is_empty()
        || !sid.is_empty()
        || !alpn.is_empty()
    {
        return Err(invalid(t("core.parser.bad_tls_params")));
    }
    let network = take(&mut q, "type");
    let path = take(&mut q, "path");
    let ws_host = take(&mut q, "host");
    let service = take(&mut q, "serviceName");
    let mode = q.remove("mode");
    let extra = q
        .remove("extra")
        .map(|s| crate::parameters::parse_json(&s, crate::parameters::ParameterProblem::Extra))
        .transpose()?;
    advanced_transport(&mut out, &network, &path, &ws_host, &service, mode, extra)?;
    if let Some(mask) = q.remove("fm") {
        let mask =
            crate::parameters::parse_json(&mask, crate::parameters::ParameterProblem::FinalMask)?;
        out["finalmask"] = json!(crate::parameters::FinalMask::read(mask)?);
    }
    if kind == "ss" && (out.get("transport").is_some() || out.get("tls").is_some()) {
        return Err(invalid(t("core.parser.shadowsocks_transport_tls")));
    }
    if !q.is_empty() {
        return Err(invalid(why::unknown_params()));
    }
    let name = decode(uri.fragment().unwrap_or(""))?;
    finish(out, &name, source)
}
pub(crate) fn finish(outbound: Value, remark: &str, source: Option<String>) -> Result<Server> {
    // The same typed allowlist guards profile import, so a row built here always survives an
    // export/import round trip and its derived fields (host, port, protocol) cannot disagree
    // with the outbound. It also rejects an empty host and port 0 for every protocol.
    let endpoint = outbound::inspect(&outbound)?;
    Ok(Server {
        id: Server::fingerprint(&outbound),
        name: display_name(remark, &endpoint.host),
        country: String::new(),
        protocol: endpoint.protocol,
        host: endpoint.host,
        port: endpoint.port,
        outbound,
        source,
        routes: None,
    })
}
fn vmess(input: &str, source: Option<String>) -> Result<Server> {
    // Keep strict fragment validation against the original JSON spans: ordinary
    // serde parsing would silently discard duplicate extra/fm keys.
    let v = crate::parameters::parse_xray_json(&decode64(&input[8..])?).map_err(
        |error| match error {
            Error::Failure {
                code: crate::failure::FailureCode::SubscriptionInvalidBody,
                detail,
                ..
            } => Error::Failure {
                code: crate::failure::FailureCode::Unknown,
                message: t("core.parser.bad_vmess_json").into(),
                detail,
                source: Some(Box::new(invalid(t("core.parser.bad_vmess_json")))),
            },
            other => other,
        },
    )?;
    let obj = v
        .as_object()
        .ok_or_else(|| invalid(t("core.parser.vmess_not_object")))?;
    let allowed = [
        "v", "ps", "add", "port", "id", "aid", "scy", "net", "type", "host", "path", "tls", "sni",
        "alpn", "fp", "mode", "fm", "extra",
    ];
    if obj.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(invalid(t("core.parser.unknown_vmess_params")));
    }
    let get = |k: &str| {
        v.get(k)
            .map(|x| {
                x.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| x.to_string())
            })
            .unwrap_or_default()
    };
    let host = get("add").trim_matches(['[', ']']).to_lowercase();
    let port = get("port")
        .parse::<u16>()
        .map_err(|_| invalid(t("core.parser.bad_port")))?;
    let id = uuid::Uuid::parse_str(&get("id"))
        .map_err(|_| invalid(t("core.parser.bad_uuid")))?
        .to_string();
    let aid = if get("aid").is_empty() {
        0
    } else {
        get("aid")
            .parse::<u32>()
            .map_err(|_| invalid(t("core.parser.bad_alter_id")))?
    };
    if !get("scy").is_empty() && !outbound::VMESS_SECURITIES.contains(&get("scy").as_str()) {
        return Err(invalid(t("core.parser.unsupported_vmess_cipher")));
    }
    let mut out = json!({"type":"vmess","server":host,"server_port":port,"uuid":id,"alter_id":aid,"security":if get("scy").is_empty(){"auto".into()}else{get("scy")}});
    if !["", "none"].contains(&get("type").as_str()) {
        return Err(invalid(t("core.parser.vmess_header_type")));
    }
    let net = get("net");
    let path = get("path");
    let (ws_path, service) = if net == "grpc" {
        ("", path.as_str())
    } else {
        (path.as_str(), "")
    };
    let mode = obj
        .get("mode")
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| crate::Error::from(crate::parameters::ParameterProblem::Mode))
        })
        .transpose()?;
    let json_parameter =
        |key: &str, problem: crate::parameters::ParameterProblem| -> Result<Option<Value>> {
            obj.get(key)
                .map(|v| match v {
                    Value::String(s) => crate::parameters::parse_json(s, problem),
                    Value::Object(_) => Ok(v.clone()),
                    _ => Err(problem.into()),
                })
                .transpose()
        };
    let extra = json_parameter("extra", crate::parameters::ParameterProblem::Extra)?;
    advanced_transport(&mut out, &net, ws_path, &get("host"), service, mode, extra)?;
    if get("tls") == "tls" {
        let sni = get("sni");
        out["tls"] = json!({"enabled":true,"server_name":if sni.is_empty(){&host}else{&sni}});
        if !get("fp").is_empty() {
            out["tls"]["utls"] = utls(&get("fp"))?;
        }
        if !get("alpn").is_empty() {
            out["tls"]["alpn"] = json!(get("alpn").split(',').collect::<Vec<_>>());
        }
    } else if !["", "none"].contains(&get("tls").as_str())
        || !get("sni").is_empty()
        || !get("fp").is_empty()
        || !get("alpn").is_empty()
    {
        return Err(invalid(t("core.parser.bad_vmess_tls_params")));
    }
    if let Some(mask) = json_parameter("fm", crate::parameters::ParameterProblem::FinalMask)? {
        out["finalmask"] = json!(crate::parameters::FinalMask::read(mask)?);
    }
    finish(out, &get("ps"), source)
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "00000000-0000-4000-8000-000000000000";

    /// What the line of a subscription is refused for, from the refusal of the parser itself.
    fn why_refused(line: &str) -> Rejection {
        match parse_uri(line, None) {
            Err(error) => rejection(line, &error),
            Ok(_) => panic!("{line} is a server"),
        }
    }

    #[test]
    fn a_refusal_falls_into_the_class_of_its_cause() {
        let cases = [
            ("hysteria2://p@h.example:443", Rejection::UnsupportedScheme),
            ("tuic://a:b@h.example:443", Rejection::UnsupportedScheme),
            ("just some words", Rejection::NotALink),
            (
                &format!("vless://{UUID}@h.example:443?encryption=none&riftUnknown=1") as &str,
                Rejection::UnknownParam,
            ),
            (
                &format!("vless://{UUID}@h.example"),
                Rejection::MissingField,
            ),
            ("trojan://@h.example:443", Rejection::MissingField),
            (
                &format!("vless://{UUID}@h.example:443?security=reality"),
                Rejection::BadReality,
            ),
            (
                &format!("vless://{UUID}@h.example:443?type=kcp"),
                Rejection::UnsupportedTransport,
            ),
            (
                &format!("vless://{UUID}@h.example:443?type=grpc&path=/x"),
                Rejection::UnsupportedTransport,
            ),
            (
                &format!("vless://{UUID}@h.example:443/extra"),
                Rejection::BadUri,
            ),
            (
                &format!("vless://{UUID}@h.example:443?type=ws&type=ws"),
                Rejection::BadUri,
            ),
            (
                "vless://not-a-uuid@h.example:443",
                Rejection::InvalidCredentials,
            ),
        ];
        for (line, expected) in cases {
            assert_eq!(why_refused(line), expected, "{line}");
        }
    }

    #[test]
    fn an_error_that_is_not_a_refusal_of_the_parser_is_other() {
        assert_eq!(rejection("vless://x", &Error::Network), Rejection::Other);
        assert_eq!(rejection("no scheme", &Error::Network), Rejection::NotALink);
    }
}
