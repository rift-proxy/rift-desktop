//! Subscriptions in the Xray JSON format: an array of client profiles, each with its own
//! `outbounds` and `routing` (a panel answers so a client it takes for an Xray one). A profile is
//! not a server: its proxy outbounds are, and the rules it routes by become the rules of those
//! servers ([`ServerRoutes`]).
//!
//! Only what a link of the same server could say is read: the same protocols, transports and TLS
//! the URI parser knows, and every key not understood refuses the outbound (it is counted as
//! rejected, the rest of the subscription is kept). Nothing here reaches the sing-box config
//! except through [`crate::outbound::inspect`] (in `parser::finish`) and [`parse_rule`].
use crate::text::t;
use crate::{
    diagnostics::{Rejection, Rows, Scheme, SubscriptionInfo},
    failure::FailureCode,
    model::Server,
    parser::{self, UTLS_FINGERPRINTS},
    routing::{parse_rule, Rule, ServerRoutes, MAX_ENTRIES},
    subscription::ParsedSubscription,
    Error, Result,
};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

/// Most servers a subscription may hold, as for a list of links.
const MAX_SERVERS: usize = 2000;
/// Outbounds that carry no server of the provider: the direct and the blocking ones, the DNS one.
const NOT_SERVERS: [&str; 4] = ["freedom", "direct", "blackhole", "dns"];

fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn refused() -> Error {
    Error::Rejected(Rejection::InvalidField)
}
fn missing(path: impl Into<String>) -> Error {
    Error::Rejected(Rejection::MissingField)
        .with_code(FailureCode::SubscriptionInvalidBody)
        .at_stage(crate::diagnostics::Stage::Parse)
        .with_field_path(path)
}

/// The body without the byte order mark some panels put in front of it.
fn bare(text: &str) -> &str {
    text.trim_start_matches('\u{feff}')
}

/// Whether a body is Xray JSON: after the white space comes an array or an object. A list of links
/// never starts so, and neither does Base64.
pub fn looks_like(text: &str) -> bool {
    bare(text).trim_start().starts_with(['[', '{'])
}

/// Whether a body that looks like JSON is a list of Xray profiles: an object, or an array with an
/// object, that has `outbounds`. Any other JSON is not for this parser.
pub fn is_profile_list(text: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(bare(text)) else {
        return false;
    };
    let has_outbounds = |item: &Value| item.get("outbounds").is_some();
    match &value {
        Value::Array(items) => items.iter().any(has_outbounds),
        Value::Object(_) => has_outbounds(&value),
        _ => false,
    }
}

/// One profile of the array.
struct Profile<'a> {
    remark: String,
    proxies: Vec<&'a Value>,
    // Malformed rows are retained for diagnostics, not counted for profile precedence.
    single: bool,
    routes: Option<ServerRoutes>,
}

/// The servers of an Xray JSON body, and with `provider_routes` the rules each profile brings.
///
/// Profiles with a single proxy outbound go first: the outbound is named by the profile and the
/// same outbound in a bigger profile (an "auto" profile that lists them all, a decoy next to a
/// whitelist server) is only a copy of it. A bigger profile names the servers it adds by its own
/// remark, numbered when there are several.
pub fn parse(text: &str, source: &str, provider_routes: bool) -> Result<ParsedSubscription> {
    let value = crate::parameters::parse_xray_json(bare(text))?;
    if let Some(observation) = provider_refusal(&value) {
        return Err(Error::failure(
            FailureCode::SubscriptionInvalidBody,
            crate::subscription::no_servers(),
        )
        .at_stage(crate::diagnostics::Stage::Parse)
        .with_local_details(observation));
    }
    let items: Vec<&Value> = match &value {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![&value],
        _ => return Err(invalid(t("core.parameters.not_xray_json"))),
    };
    let mut first_error = items.iter().find_map(|item| {
        let outbounds = item.as_object()?.get("outbounds")?;
        (!outbounds.is_array()).then(|| {
            refused()
                .with_code(FailureCode::SubscriptionInvalidBody)
                .with_field_path("outbounds")
        })
    });
    let profiles: Vec<Profile> = items
        .into_iter()
        .filter_map(|item| profile(item, provider_routes))
        .collect();

    let mut seen = HashSet::new();
    // Counted once however many profiles repeat the outbound.
    let mut rejected = HashMap::new();
    let mut found: Vec<(usize, usize, Server)> = Vec::new();
    let mut rows = Rows::default();
    for single in [true, false] {
        for (at, profile) in profiles.iter().enumerate() {
            if profile.single != single {
                continue;
            }
            let mut fresh: Vec<(usize, Value)> = Vec::new();
            for (index, raw) in profile.proxies.iter().enumerate() {
                match convert(raw) {
                    Ok(outbound) => {
                        if seen.insert(Server::fingerprint(&outbound)) {
                            fresh.push((index, outbound));
                        }
                    }
                    Err(error) => {
                        rejected.insert(
                            canonical(raw),
                            (
                                parser::rejection_of(&error),
                                crate::subscription_observation::Observation::xray(raw),
                            ),
                        );
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                }
            }
            for (number, (index, outbound)) in fresh.into_iter().enumerate() {
                // Without a remark the server is named by its address, whatever its number.
                let name = if profile.remark.trim().is_empty() {
                    String::new()
                } else if number > 0 {
                    format!("{} {}", profile.remark, number + 1)
                } else {
                    profile.remark.clone()
                };
                match parser::finish(outbound, &name, Some(source.into())) {
                    Ok(mut server) => {
                        rows.accept();
                        crate::subscription_observation::Observation::xray(profile.proxies[index])
                            .count_accepted_into(&mut rows);
                        server.routes = profile.routes.clone();
                        found.push((at, index, server));
                    }
                    Err(error) => {
                        let raw = profile.proxies[index];
                        rejected.insert(
                            canonical(raw),
                            (
                                parser::rejection_of(&error),
                                crate::subscription_observation::Observation::xray(raw),
                            ),
                        );
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                }
            }
        }
    }
    found.sort_by_key(|(at, index, _)| (*at, *index));
    if found.len() > MAX_SERVERS {
        return Err(invalid(t("core.subscription.too_many_servers")));
    }
    for (why, observation) in rejected.values() {
        rows.reject(*why, Scheme::Other);
        observation.count_into(&mut rows);
    }
    if found.is_empty() {
        return Err(first_error
            .unwrap_or_else(|| {
                if profiles.is_empty() {
                    missing("outbounds").with_local_details(t("core.diagnostics.no_outbounds"))
                } else {
                    Error::failure(
                        FailureCode::SubscriptionInvalidBody,
                        crate::subscription::no_servers(),
                    )
                    .with_local_details(t("core.diagnostics.no_outbounds"))
                }
            })
            .with_code(FailureCode::SubscriptionInvalidBody)
            .at_stage(crate::diagnostics::Stage::Parse)
            .with_subscription(SubscriptionInfo {
                rows: Some(rows),
                ..SubscriptionInfo::default()
            }));
    }
    Ok(ParsedSubscription {
        servers: found.into_iter().map(|(_, _, server)| server).collect(),
        rejected: rejected.len(),
        filtered: 0,
        rows,
    })
}

/// Only explicit refusal indicators are retained, never provider-controlled message text.
fn provider_refusal(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    if object.contains_key("outbounds") {
        return None;
    }
    for key in ["success", "status"] {
        if object.get(key) == Some(&Value::Bool(false)) {
            return Some(crate::tr!(
                "core.diagnostics.provider_refusal",
                observation = format!("{key}=false")
            ));
        }
    }
    if object.get("error") == Some(&Value::Bool(true)) {
        return Some(crate::tr!(
            "core.diagnostics.provider_refusal",
            observation = "error=true"
        ));
    }
    if object.get("error").is_some_and(|error| match error {
        Value::String(text) => !text.is_empty(),
        Value::Object(_) | Value::Array(_) => true,
        _ => false,
    }) {
        return Some(t("core.diagnostics.provider_error").into());
    }
    if let Some(status) = object
        .get("status")
        .and_then(Value::as_u64)
        .filter(|status| (400..=599).contains(status))
    {
        return Some(crate::tr!(
            "core.diagnostics.provider_status",
            status = status
        ));
    }
    None
}

/// The part of an outbound that tells it from another, as text (the tag is only a label).
fn canonical(raw: &Value) -> String {
    let mut raw = raw.clone();
    if let Some(object) = raw.as_object_mut() {
        object.remove("tag");
    }
    raw.to_string()
}

fn profile(item: &Value, provider_routes: bool) -> Option<Profile<'_>> {
    let object = item.as_object()?;
    let outbounds = object.get("outbounds")?.as_array()?;
    let proxies: Vec<&Value> = outbounds
        .iter()
        .filter(|outbound| {
            outbound["protocol"]
                .as_str()
                .is_none_or(|protocol| !NOT_SERVERS.contains(&protocol))
        })
        .collect();
    let single = proxies
        .iter()
        .filter(|outbound| outbound["protocol"].is_string())
        .count()
        == 1;
    let routes = provider_routes
        .then(|| object.get("routing").and_then(|r| routes(r, outbounds)))
        .flatten();
    Some(Profile {
        remark: object
            .get("remarks")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        proxies,
        single,
        routes,
    })
}

// ---- servers ----------------------------------------------------------------------------------

fn only(map: &Map<String, Value>, known: &[&str]) -> Result<()> {
    map.keys()
        .all(|key| known.contains(&key.as_str()))
        .then_some(())
        .ok_or(Error::Rejected(Rejection::UnknownParam))
}
fn text<'a>(map: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => Err(refused()),
    }
}
fn required<'a>(
    map: &'a Map<String, Value>,
    key: &str,
    path: impl FnOnce() -> String,
) -> Result<&'a str> {
    text(map, key)?
        .filter(|text| !text.is_empty())
        .ok_or_else(|| missing(path()))
}
fn object<'a>(map: &'a Map<String, Value>, key: &str) -> Result<Option<&'a Map<String, Value>>> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(object)) => Ok(Some(object)),
        Some(_) => Err(refused()),
    }
}
fn port(map: &Map<String, Value>, parent_path: &str) -> Result<u16> {
    if !map.contains_key("port") || map.get("port") == Some(&Value::Null) {
        return Err(missing(format!("{parent_path}.port")));
    }
    let port = match map.get("port") {
        Some(Value::Number(number)) => number.as_u64().and_then(|n| u16::try_from(n).ok()),
        Some(Value::String(text)) => text.trim().parse::<u16>().ok(),
        _ => None,
    };
    port.filter(|port| *port != 0)
        .ok_or(Error::Rejected(Rejection::InvalidEndpoint))
}
fn flag(map: &Map<String, Value>, key: &str) -> Result<bool> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(refused()),
    }
}

/// The entry of `settings.<list>`: the one server a profile outbound names, or the settings
/// themselves in the flat form newer Xray writes.
fn entry<'a>(settings: &'a Map<String, Value>, list: &str) -> Result<&'a Map<String, Value>> {
    match settings.get(list) {
        Some(Value::Array(items)) if items.len() == 1 => items[0].as_object().ok_or_else(refused),
        Some(Value::Array(items)) if items.is_empty() => {
            Err(missing(format!("settings.{list}[0]")))
        }
        Some(Value::Array(_)) => Err(Error::Rejected(Rejection::UnsupportedParam)),
        Some(_) => Err(refused()),
        None if settings.contains_key("address") => Ok(settings),
        None => Err(missing(format!("settings.{list}"))),
    }
}

/// An outbound of a profile as the sing-box outbound a link of the same server would give.
fn convert(raw: &Value) -> Result<Value> {
    let outbound = raw.as_object().ok_or_else(refused)?;
    only(
        outbound,
        &["tag", "protocol", "settings", "streamSettings", "mux"],
    )?;
    let settings = object(outbound, "settings")?.ok_or_else(|| missing("settings"))?;
    let stream = object(outbound, "streamSettings")?;
    let protocol = required(outbound, "protocol", || "protocol".into())?;
    let mut out = match protocol {
        "vless" | "vmess" => {
            let server = entry(settings, "vnext")?;
            let server_path = if settings.contains_key("vnext") {
                "settings.vnext[0]"
            } else {
                "settings"
            };
            let user_path = || {
                if server.contains_key("users") {
                    format!("{server_path}.users[0].id")
                } else {
                    format!("{server_path}.id")
                }
            };
            // In the flat form the server and its user are one object.
            only(
                server,
                &[
                    "address",
                    "port",
                    "users",
                    "level",
                    "email",
                    "id",
                    "encryption",
                    "flow",
                    "alterId",
                    "security",
                ],
            )?;
            let user = match server.get("users") {
                Some(Value::Array(users)) if users.len() == 1 => {
                    users[0].as_object().ok_or_else(refused)?
                }
                Some(Value::Array(_)) => {
                    return Err(Error::Rejected(Rejection::UnsupportedParam));
                }
                Some(_) => return Err(refused()),
                None => server,
            };
            let mut out = json!({
                "type": protocol,
                "server": required(server, "address", || format!("{server_path}.address"))?.trim_matches(['[', ']']).to_lowercase(),
                "server_port": port(server, server_path)?,
            });
            let id = uuid::Uuid::parse_str(required(user, "id", user_path)?)
                .map_err(|_| Error::Rejected(Rejection::InvalidCredentials))?;
            out["uuid"] = json!(id.to_string());
            if protocol == "vless" {
                only(
                    user,
                    &[
                        "id",
                        "encryption",
                        "flow",
                        "level",
                        "email",
                        "address",
                        "port",
                    ],
                )?;
                if !matches!(text(user, "encryption")?, None | Some("" | "none")) {
                    return Err(Error::Rejected(Rejection::UnsupportedSecurity));
                }
                match text(user, "flow")? {
                    None | Some("") => {}
                    Some(flow) => out["flow"] = json!(flow),
                }
            } else {
                only(
                    user,
                    &[
                        "id", "alterId", "security", "level", "email", "address", "port",
                    ],
                )?;
                out["alter_id"] = json!(match user.get("alterId") {
                    None | Some(Value::Null) => 0,
                    Some(Value::Number(n)) => n.as_u64().ok_or_else(refused)?,
                    Some(_) => return Err(refused()),
                });
                out["security"] = json!(text(user, "security")?
                    .filter(|security| !security.is_empty())
                    .unwrap_or("auto"));
            }
            out
        }
        "trojan" | "shadowsocks" => {
            let server = entry(settings, "servers")?;
            let server_path = if settings.contains_key("servers") {
                "settings.servers[0]"
            } else {
                "settings"
            };
            let mut out = json!({
                "type": protocol,
                "server": required(server, "address", || format!("{server_path}.address"))?.trim_matches(['[', ']']).to_lowercase(),
                "server_port": port(server, server_path)?,
                "password": required(server, "password", || format!("{server_path}.password"))?,
            });
            if protocol == "trojan" {
                only(server, &["address", "port", "password", "level", "email"])?;
            } else {
                only(
                    server,
                    &[
                        "address",
                        "port",
                        "password",
                        "method",
                        "uot",
                        "UoTVersion",
                        "ivCheck",
                        "level",
                        "email",
                    ],
                )?;
                // Shadowsocks over a TCP tunnel for UDP is another protocol on the wire.
                if flag(server, "uot")? {
                    return Err(Error::Rejected(Rejection::UnsupportedParam));
                }
                out["method"] = json!(required(server, "method", || format!(
                    "{server_path}.method"
                ))?);
            }
            out
        }
        _ => return Err(Error::Rejected(Rejection::UnsupportedScheme)),
    };
    transport(&mut out, stream)?;
    Ok(out)
}

/// The network and the TLS of `streamSettings`, as the URI parser would put them.
fn transport(out: &mut Value, stream: Option<&Map<String, Value>>) -> Result<()> {
    let empty = Map::new();
    let stream = stream.unwrap_or(&empty);
    only(
        stream,
        &[
            "network",
            "security",
            "tcpSettings",
            "rawSettings",
            "tlsSettings",
            "realitySettings",
            "wsSettings",
            "grpcSettings",
            "xhttpSettings",
            "finalmask",
        ],
    )?;
    let host = out["server"].as_str().unwrap_or_default().to_owned();
    let network = text(stream, "network")?.unwrap_or("");
    for (key, matches) in [
        ("tcpSettings", ["", "tcp", "raw"].contains(&network)),
        ("rawSettings", ["", "tcp", "raw"].contains(&network)),
        ("wsSettings", network == "ws"),
        ("grpcSettings", network == "grpc"),
        ("xhttpSettings", network == "xhttp"),
    ] {
        if stream.get(key).is_some_and(|v| !v.is_null()) && !matches {
            return Err(crate::parameters::ParameterProblem::Incompatible.into());
        }
    }
    match network {
        "" | "tcp" | "raw" => {
            // Only plain TCP: an HTTP header in front of the stream is another transport.
            for key in ["tcpSettings", "rawSettings"] {
                if let Some(settings) = object(stream, key)? {
                    only(settings, &["header"])?;
                    if let Some(header) = object(settings, "header")? {
                        if !matches!(text(header, "type")?, None | Some("" | "none")) {
                            return Err(Error::Rejected(Rejection::UnsupportedTransport));
                        }
                        only(header, &["type"])?;
                    }
                }
            }
        }
        "ws" => {
            let settings = object(stream, "wsSettings")?.unwrap_or(&empty);
            only(settings, &["path", "headers", "host"])?;
            let mut ws = json!({
                "type": "ws",
                "path": text(settings, "path")?.filter(|path| !path.is_empty()).unwrap_or("/"),
            });
            let mut named = text(settings, "host")?.map(str::to_owned);
            if let Some(headers) = object(settings, "headers")? {
                for (key, value) in headers {
                    if !key.eq_ignore_ascii_case("host") {
                        return Err(Error::Rejected(Rejection::UnsupportedParam));
                    }
                    named = Some(value.as_str().ok_or_else(refused)?.to_owned());
                }
            }
            if let Some(name) = named.filter(|name| !name.is_empty()) {
                ws["headers"] = json!({ "Host": name });
            }
            out["transport"] = ws;
        }
        "grpc" => {
            let settings = object(stream, "grpcSettings")?.unwrap_or(&empty);
            only(settings, &["serviceName", "multiMode"])?;
            if flag(settings, "multiMode")? {
                return Err(crate::parameters::ParameterProblem::Mode.into());
            }
            out["transport"] = json!({
                "type": "grpc",
                "service_name": text(settings, "serviceName")?.unwrap_or(""),
            });
        }
        "xhttp" => {
            let settings = object(stream, "xhttpSettings")?.unwrap_or(&empty);
            let checked = crate::parameters::Xhttp::read(Value::Object(settings.clone()))?;
            out["transport"] = json!(checked);
            out["transport"]["type"] = json!("xhttp");
        }
        _ => return Err(Error::Rejected(Rejection::UnsupportedTransport)),
    }
    let protocol = out["type"].as_str().unwrap_or_default().to_owned();
    let security = text(stream, "security")?.unwrap_or("");
    for (key, expected) in [("tlsSettings", "tls"), ("realitySettings", "reality")] {
        if stream.get(key).is_some_and(|v| !v.is_null()) && security != expected {
            return Err(crate::parameters::ParameterProblem::Incompatible.into());
        }
    }
    match security {
        "" | "none" => {}
        security @ ("tls" | "reality") => {
            let settings = object(
                stream,
                if security == "tls" {
                    "tlsSettings"
                } else {
                    "realitySettings"
                },
            )?
            .unwrap_or(&empty);
            if security == "tls" {
                only(
                    settings,
                    &["serverName", "fingerprint", "alpn", "allowInsecure"],
                )?;
            } else {
                only(
                    settings,
                    &[
                        "serverName",
                        "fingerprint",
                        "publicKey",
                        "shortId",
                        "spiderX",
                        "show",
                    ],
                )?;
            }
            if flag(settings, "allowInsecure")? {
                return Err(Error::Rejected(Rejection::UnsupportedSecurity));
            }
            let name = text(settings, "serverName")?.filter(|name| !name.is_empty());
            let mut tls = json!({"enabled": true, "server_name": name.unwrap_or(&host)});
            match text(settings, "fingerprint")?.filter(|fp| !fp.is_empty()) {
                Some(fp) if UTLS_FINGERPRINTS.contains(&fp) => {
                    tls["utls"] = json!({"enabled": true, "fingerprint": fp});
                }
                Some(_) => return Err(Error::Rejected(Rejection::UnsupportedSecurity)),
                None => {}
            }
            if let Some(alpn) = settings.get("alpn") {
                let protocols: Option<Vec<&str>> = alpn
                    .as_array()
                    .ok_or_else(refused)?
                    .iter()
                    .map(Value::as_str)
                    .collect();
                tls["alpn"] = json!(protocols.ok_or_else(refused)?);
            }
            if security == "reality" {
                if flag(settings, "show")?
                    || text(settings, "spiderX")?.is_some_and(|s| !["", "/"].contains(&s))
                {
                    return Err(crate::parameters::ParameterProblem::Unsupported.into());
                }
                if protocol != "vless" {
                    return Err(Error::Rejected(Rejection::BadReality));
                }
                tls["reality"] = json!({
                    "enabled": true,
                    "public_key": required(settings, "publicKey", || "streamSettings.realitySettings.publicKey".into())?,
                    "short_id": text(settings, "shortId")?.unwrap_or(""),
                });
                if tls.get("utls").is_none() {
                    tls["utls"] = json!({"enabled": true, "fingerprint": "chrome"});
                }
            }
            out["tls"] = tls;
        }
        _ => return Err(Error::Rejected(Rejection::UnsupportedSecurity)),
    }
    // As for a link: Shadowsocks has neither a transport nor TLS of its own here.
    if let Some(mask) = stream.get("finalmask") {
        out["finalmask"] = json!(crate::parameters::FinalMask::read(mask.clone())?);
    }
    if protocol == "shadowsocks" && (out.get("transport").is_some() || out.get("tls").is_some()) {
        return Err(Error::Rejected(Rejection::IncompatibleParam));
    }
    Ok(())
}

// ---- rules ------------------------------------------------------------------------------------

/// The rules of a profile that the user's syntax can say: what goes straight out or nowhere, by
/// domain, address, port (with its transport) or protocol. A rule with more than one kind of
/// condition (a domain *and* a port), one that routes by balancer or to the proxy, and every
/// condition Rift has no entry for (the inbound, the user, the source) are left out, so a
/// profile never gets more reach than the entries say.
fn routes(routing: &Value, outbounds: &[Value]) -> Option<ServerRoutes> {
    let rules = routing.get("rules")?.as_array()?;
    let mut found = ServerRoutes::default();
    let (mut seen_direct, mut seen_block) = (HashSet::new(), HashSet::new());
    for rule in rules {
        let Some(rule) = rule.as_object() else {
            continue;
        };
        if !matches!(
            rule.get("type").and_then(Value::as_str),
            None | Some("field")
        ) {
            continue;
        }
        let Some(tag) = rule.get("outboundTag").and_then(Value::as_str) else {
            continue;
        };
        let protocol = outbounds
            .iter()
            .find(|outbound| outbound["tag"] == tag)
            .and_then(|outbound| outbound["protocol"].as_str());
        let (list, seen) = match protocol {
            Some("freedom" | "direct") => (&mut found.direct, &mut seen_direct),
            Some("blackhole") => (&mut found.block, &mut seen_block),
            _ => continue,
        };
        let mut conditions: Vec<&str> = rule
            .keys()
            .map(String::as_str)
            .filter(|key| !["type", "outboundTag", "balancerTag", "ruleTag"].contains(key))
            .collect();
        conditions.sort_unstable();
        // What each condition may say: a `domain` is a name, an `ip` an address; an entry of
        // another kind of Rift's own (`udp:443`, `port:1-65535`) is not what Xray means there.
        let (entries, allowed): (Vec<String>, fn(&Rule) -> bool) = match conditions.as_slice() {
            ["domain"] => (
                strings(&rule["domain"]).into_iter().map(domain).collect(),
                is_name,
            ),
            ["ip"] => (strings(&rule["ip"]), is_address),
            ["protocol"] => (
                strings(&rule["protocol"])
                    .into_iter()
                    .map(|name| format!("protocol:{}", name.to_ascii_lowercase()))
                    .collect(),
                is_kept,
            ),
            ["port"] => (ports(&rule["port"], None), is_kept),
            // A transport that cannot be read is not "any": the rule is left out.
            ["network", "port"] => match network(&rule["network"]) {
                Some(network) => (ports(&rule["port"], Some(&network)), is_kept),
                None => continue,
            },
            _ => continue,
        };
        for entry in entries {
            if !parse_rule(&entry).is_ok_and(|rule| allowed(&rule)) {
                continue;
            }
            if seen.insert(entry.to_ascii_lowercase()) {
                list.push(entry);
                // Longer than a list may be is more than a profile of rules for a server: none of
                // it, and no reason to read on.
                if list.len() > MAX_ENTRIES {
                    return None;
                }
            }
        }
    }
    (!found.is_empty()).then_some(found)
}

/// Whether an entry is a name of a `domain` condition.
fn is_name(rule: &Rule) -> bool {
    matches!(
        rule,
        Rule::Suffix(_) | Rule::Full(_) | Rule::Keyword(_) | Rule::Regex(_) | Rule::Geosite(_)
    )
}
/// Whether an entry is an address of an `ip` condition.
fn is_address(rule: &Rule) -> bool {
    matches!(rule, Rule::Cidr(_) | Rule::Geoip(_))
}
/// Entries Rift made itself from a condition that says what they are; only a program is never kept.
fn is_kept(rule: &Rule) -> bool {
    !matches!(rule, Rule::Process(_))
}
/// The transports a rule names: written as text (`tcp,udp`) or as a list of such texts, as Xray
/// reads either.
fn network(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => items
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()
            .map(|names| names.join(",")),
        _ => None,
    }
}

/// The strings of a condition: a list, or a single one written as text.
fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => text.split(',').map(|part| part.trim().to_owned()).collect(),
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(|text| text.trim().to_owned())
            .collect(),
        _ => Vec::new(),
    }
}
/// A domain of a rule: with a prefix as it stands (`ext:` and the like then do not read), a bare
/// word as Xray reads it, anywhere in the name.
fn domain(entry: String) -> String {
    if entry.contains(':') {
        entry
    } else {
        format!("keyword:{}", entry.to_lowercase())
    }
}
/// The entries for ports `65,80-90` and the transports `network` names. More entries than a list
/// may hold is no rule at all, and is found out before a single one is made.
fn ports(value: &Value, network: Option<&str>) -> Vec<String> {
    let ports = match value {
        Value::Number(number) => vec![number.to_string()],
        other => strings(other),
    };
    let nets: Vec<String> = match network {
        None => vec!["port".to_owned()],
        Some(list) => {
            let mut nets: Vec<String> = list
                .split(',')
                .map(|net| net.trim().to_ascii_lowercase())
                .collect();
            nets.sort_unstable();
            nets.dedup();
            // Both transports are what a rule without a transport says.
            if nets == ["tcp", "udp"] {
                vec!["port".to_owned()]
            } else if nets.iter().all(|net| net == "tcp" || net == "udp") {
                nets
            } else {
                return Vec::new();
            }
        }
    };
    if nets.len().saturating_mul(ports.len()) > MAX_ENTRIES {
        return Vec::new();
    }
    nets.iter()
        .flat_map(|net| ports.iter().map(move |ports| format!("{net}:{ports}")))
        .collect()
}

#[cfg(test)]
mod local_cause_tests {
    use super::*;

    #[test]
    fn malformed_json_retains_position_but_not_input() {
        let input = "{\n\"secret\":\"unique-provider-credential\",\n\"outbounds\": [}";
        let error = parse(input, "source", true).err().expect("malformed JSON");
        let details = error.diagnostic_details();
        assert!(details.contains("line=3"));
        assert!(details.contains("column="));
        assert_eq!(error.detail().unwrap().field_path.as_deref(), Some("$"));
        assert!(!details.contains("unique-provider-credential"));
        assert!(!details.contains("outbounds\":"));
    }

    #[test]
    fn missing_xray_field_retains_its_path_without_other_credentials() {
        let input = json!({
            "outbounds": [{
                "protocol": "trojan",
                "settings": {"servers": [{
                    "port": 443,
                    "password": "unique-provider-credential"
                }]}
            }]
        });
        let error = parse(&input.to_string(), "source", true)
            .err()
            .expect("missing address");
        assert_eq!(
            error.detail().unwrap().field_path.as_deref(),
            Some("settings.servers[0].address")
        );
        assert!(!error
            .diagnostic_details()
            .contains("unique-provider-credential"));
    }

    #[test]
    fn explicit_provider_refusal_does_not_invent_expiry_or_echo_messages() {
        let input = json!({
            "success": false,
            "message": "expired unique-provider-credential https://private.invalid/token"
        });
        let error = parse(&input.to_string(), "source", true)
            .err()
            .expect("provider refusal");
        let details = error.diagnostic_details();
        assert!(details.contains("success=false"));
        assert!(!details.contains("expired"));
        assert!(!details.contains("unique-provider-credential"));
        assert!(!details.contains("private.invalid"));
    }
}
