use crate::text::t;
use crate::{
    geodata::GeoSet,
    model::{Mode, Settings, Store},
    routing::{Route, Routing, Rule, NO_ROUTES_MODE},
    Error, Result,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
/// The settings that reach the generated config. A settings change interrupts running work only
/// if they differ; `Application` compares them without the coordinator lock, which a long
/// selection cycle holds, so the cancellation is never queued behind the work it must stop.
/// A test checks that no other setting changes the config.
pub fn config_inputs(settings: &Settings) -> (Mode, u16, u16, bool) {
    (
        settings.mode.clone(),
        settings.proxy_port,
        settings.api_port,
        settings.find_process,
    )
}
/// Where sing-box gets the answers for the names it resolves itself: the servers with a domain
/// address and every site that leaves directly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Resolver {
    /// Its own `local` resolver, which on Windows sends packets to port 53 itself.
    System,
    /// The loopback TCP port of [`crate::dns::DnsBridge`], which asks a chain of resolvers.
    Bridge(u16),
}
/// A protocol name no sniffer reports: a rule that asks for it matches nothing.
const NO_MATCH_PROTOCOL: &str = "rift-no-match";
/// How long sing-box waits for the bridge: the whole chain, every resolver at its limit
/// ([`crate::dns::STEP_TIMEOUT`] each), with a margin. The default of sing-box is shorter.
const BRIDGE_TIMEOUT: &str = "15s";
/// The config without the geosite and geoip databases: rules that need them are left out.
pub fn generate(store: &Store, secret: &str) -> Result<Value> {
    generate_with(store, secret, &GeoSet::none())
}
/// What the routing profile adds to the config.
#[derive(Default)]
struct Routed {
    /// After the two fixed rules (sniff, hijack-dns).
    rules: Vec<Value>,
    /// The inline lists the rules refer to.
    rule_sets: Vec<Value>,
    /// Direct domains are resolved by the local resolver, not through the server.
    dns_rules: Vec<Value>,
    find_process: bool,
    /// The catch-all rule when everything else is blocked.
    tail: Option<Value>,
    final_tag: &'static str,
}
/// The rules of one list, as sing-box rule objects grouped by what they match. Every object
/// has one kind of condition, so the entries of a list are alternatives of each other. With a
/// `mode`, the rules (and their DNS rules) act only while the Clash API is in that mode: the rules
/// a provider sends with a server.
fn list_rules(route: Route, rules: &[Rule], geo: &GeoSet, routed: &mut Routed, mode: Option<&str>) {
    let mut fields: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
    let mut sets: Vec<String> = Vec::new();
    let mut dns_sets: Vec<String> = Vec::new();
    // Entries that need two conditions at once (a transport and its port) are a rule each.
    let mut pairs: Vec<Value> = Vec::new();
    for rule in rules {
        let (field, value) = match rule {
            Rule::Suffix(domain) => ("domain_suffix", json!(domain)),
            Rule::Full(domain) => ("domain", json!(domain)),
            Rule::Keyword(word) => ("domain_keyword", json!(word)),
            Rule::Regex(expression) => ("domain_regex", json!(expression)),
            Rule::Cidr(range) => ("ip_cidr", json!(range)),
            Rule::Port(port) => ("port", json!(port)),
            Rule::PortRange(from, to) => ("port_range", json!(format!("{from}:{to}"))),
            Rule::NetPort(net, port) => {
                pairs.push(json!({"network": net.name(), "port": [port]}));
                continue;
            }
            Rule::NetPortRange(net, from, to) => {
                pairs.push(json!({"network": net.name(), "port_range": [format!("{from}:{to}")]}));
                continue;
            }
            Rule::Protocol(name) => ("protocol", json!(name)),
            Rule::Process(program) => {
                routed.find_process = true;
                ("process_name", json!(program))
            }
            Rule::Geosite(name) => {
                if let Some(list) = geo.sites.get(name).filter(|list| !list.is_empty()) {
                    let tag = format!("geosite-{name}");
                    if !routed
                        .rule_sets
                        .iter()
                        .any(|set| set["tag"] == tag.as_str())
                    {
                        routed.rule_sets.push(site_rule_set(&tag, list));
                    }
                    if !sets.contains(&tag) {
                        sets.push(tag.clone());
                    }
                    dns_sets.push(tag);
                }
                continue;
            }
            Rule::Geoip(code) => {
                if let Some(ranges) = geo.ips.get(code).filter(|ranges| !ranges.is_empty()) {
                    let tag = format!("geoip-{code}");
                    if !routed
                        .rule_sets
                        .iter()
                        .any(|set| set["tag"] == tag.as_str())
                    {
                        routed.rule_sets.push(json!({
                            "type": "inline", "tag": tag, "rules": [{"ip_cidr": ranges}]
                        }));
                    }
                    if !sets.contains(&tag) {
                        sets.push(tag);
                    }
                }
                continue;
            }
        };
        fields.entry(field).or_default().push(value);
    }
    let action = |mut rule: Value| {
        match route {
            Route::Block => rule["action"] = json!("reject"),
            Route::Direct => {
                rule["action"] = json!("route");
                rule["outbound"] = json!("direct");
            }
            Route::Proxy => {
                rule["action"] = json!("route");
                rule["outbound"] = json!("main");
            }
        }
        if let Some(mode) = mode {
            rule["clash_mode"] = json!(mode);
        }
        rule
    };
    for (field, values) in &fields {
        routed.rules.push(action(json!({ *field: values })));
    }
    if !sets.is_empty() {
        routed.rules.push(action(json!({ "rule_set": sets })));
    }
    for pair in pairs {
        routed.rules.push(action(pair));
    }
    {
        let dns_action = |mut rule: Value| {
            match route {
                Route::Direct => rule["server"] = json!("local"),
                Route::Proxy => rule["server"] = json!("remote"),
                Route::Block => {
                    rule["action"] = json!("reject");
                }
            }
            if let Some(mode) = mode {
                rule["clash_mode"] = json!(mode);
            }
            rule
        };
        for field in ["domain", "domain_suffix", "domain_keyword", "domain_regex"] {
            if let Some(values) = fields.get(field) {
                routed.dns_rules.push(dns_action(json!({ field: values })));
            }
        }
        if !dns_sets.is_empty() {
            routed
                .dns_rules
                .push(dns_action(json!({ "rule_set": dns_sets })));
        }
    }
}
fn site_rule_set(tag: &str, list: &crate::geodata::SiteList) -> Value {
    let mut rule = serde_json::Map::new();
    for (field, values) in [
        ("domain", &list.full),
        ("domain_suffix", &list.suffix),
        ("domain_keyword", &list.keyword),
        ("domain_regex", &list.regex),
    ] {
        if !values.is_empty() {
            rule.insert(field.into(), json!(values));
        }
    }
    json!({ "type": "inline", "tag": tag, "rules": [Value::Object(rule)] })
}
/// The routing profile as config parts: the first two lists of its order as rules, the third
/// as what is left over.
fn routed(routing: &Routing, geo: &GeoSet) -> Result<Routed> {
    routing.validate()?;
    let mut routed = Routed::default();
    for route in &routing.order[..2] {
        list_rules(*route, &routing.rules(*route)?, geo, &mut routed, None);
    }
    match routing.rest() {
        Route::Proxy => routed.final_tag = "main",
        Route::Direct => {
            routed.final_tag = "direct";
        }
        Route::Block => {
            routed.final_tag = "main";
            routed.tail = Some(json!({ "action": "reject" }));
        }
    }
    Ok(routed)
}
/// The config; `geo` holds the geosite and geoip lists the routing refers to (by name, those
/// that are not there are not applied).
pub fn generate_with(store: &Store, secret: &str, geo: &GeoSet) -> Result<Value> {
    generate_resolving(store, secret, geo, Resolver::System)
}
/// The config, with the names that sing-box resolves itself answered by `resolver`.
pub fn generate_resolving(
    store: &Store,
    secret: &str,
    geo: &GeoSet,
    resolver: Resolver,
) -> Result<Value> {
    store.settings.validate()?;
    if secret.len() < 32 {
        return Err(Error::Invalid(t("core.config.short_api_token").into()));
    }
    let mut seen = HashSet::new();
    let servers: Vec<_> = store
        .available_servers()
        .into_iter()
        .filter(|s| seen.insert(s.id.clone()))
        .collect();
    if servers.is_empty() {
        return Err(Error::Unavailable);
    }
    let tags: Vec<_> = servers.iter().map(|s| s.id.clone()).collect();
    let selected = store
        .selected
        .as_ref()
        .filter(|id| tags.contains(id))
        .unwrap_or(&tags[0]);
    let mut outbounds: Vec<Value> = Vec::with_capacity(servers.len() + 1);
    for s in &servers {
        let mut o = s.outbound.clone();
        // Indexing a non-object Value with a key panics; a damaged outbound is an error.
        let Some(object) = o.as_object_mut() else {
            return Err(Error::Invalid(t("core.common.bad_server_data").into()));
        };
        object.insert("tag".into(), json!(s.id));
        outbounds.push(o);
    }
    outbounds.push(json!({"type":"selector","tag":"main","outbounds":tags,"default":selected,"interrupt_exist_connections":true}));
    let mut routed = routed(&store.routing, geo)?;
    // The rules the providers send with their servers come after the user's own, each set in
    // the Clash mode of the servers that bring it.
    let sets = store.route_sets();
    let mut brought = Routed::default();
    for (mode, set) in &sets {
        let before = brought.rules.len();
        for route in [Route::Block, Route::Direct] {
            list_rules(route, &set.rules(route)?, geo, &mut brought, Some(mode));
        }
        // sing-box learns a mode from the rules that carry it. A set whose entries all name
        // lists the databases do not hold (yet) makes none, and its mode would be refused when
        // its server is chosen: a rule that matches no connection keeps the mode known.
        if brought.rules.len() == before {
            brought.rules.push(json!({
                "clash_mode": mode,
                "protocol": [NO_MATCH_PROTOCOL],
                "action": "route",
                "outbound": "direct",
            }));
        }
    }
    routed.rules.append(&mut brought.rules);
    routed.dns_rules.append(&mut brought.dns_rules);
    for set in brought.rule_sets {
        if !routed
            .rule_sets
            .iter()
            .any(|known| known["tag"] == set["tag"])
        {
            routed.rule_sets.push(set);
        }
    }
    if matches!(resolver, Resolver::Bridge(_)) {
        for rule in &mut routed.dns_rules {
            if rule["server"] == "local" {
                rule["timeout"] = json!(BRIDGE_TIMEOUT);
            }
        }
    }
    // Always there, whatever the rules say: the check for a network at all goes through it, and
    // it leaves the machine by the real interface, never into the tunnel.
    outbounds.push(json!({"type":"direct","tag":"direct"}));
    let mut inbounds = vec![];
    if store.settings.mode != Mode::Tun {
        inbounds.push(json!({"type":"mixed","tag":"mixed","listen":"127.0.0.1","listen_port":store.settings.proxy_port,"set_system_proxy":false}));
    }
    if store.settings.mode != Mode::Proxy {
        let bootstrap: Vec<_> = crate::dns::DOH_IPS
            .iter()
            .map(|ip| format!("{ip}/32"))
            .collect();
        inbounds.push(json!({"type":"tun","tag":"tun","interface_name":"Rift","address":["172.19.0.1/30","fdfe:dcba:9876::1/126"],"auto_route":true,"strict_route":true,"stack":"mixed","route_exclude_address":bootstrap}));
    }
    let mut rules = vec![
        json!({"action":"sniff"}),
        json!({"protocol":"dns","action":"hijack-dns"}),
    ];
    rules.extend(routed.rules);
    rules.extend(routed.tail);
    // `local` is the resolver of the names that leave by the real interface; `remote` is the one
    // through the tunnel.
    let local = match resolver {
        Resolver::System => json!({"type":"local","tag":"local"}),
        Resolver::Bridge(port) => {
            json!({"type":"tcp","tag":"local","server":"127.0.0.1","server_port":port})
        }
    };
    let dns_servers = vec![
        local,
        json!({"type":"https","tag":"remote","server":"1.1.1.1","path":"/dns-query","detour":"main"}),
    ];
    let mut dns = json!({"servers":dns_servers,"final":"remote"});
    if !routed.dns_rules.is_empty() {
        dns["rules"] = json!(routed.dns_rules);
    }
    let domain_resolver = match resolver {
        Resolver::System => json!("local"),
        Resolver::Bridge(_) => json!({"server":"local","timeout":BRIDGE_TIMEOUT}),
    };
    let mut route = json!({"auto_detect_interface":true,"default_domain_resolver":domain_resolver,"rules":rules,"final":routed.final_tag});
    if !routed.rule_sets.is_empty() {
        route["rule_set"] = json!(routed.rule_sets);
    }
    if routed.find_process || store.settings.find_process {
        route["find_process"] = json!(true);
    }
    let mut clash_api = json!({"external_controller":format!("127.0.0.1:{}",store.settings.api_port),"secret":secret,"access_control_allow_origin":["http://rift.invalid"],"access_control_allow_private_network":false});
    // sing-box starts in the mode with no rules of a server (the one every config has); the
    // process that starts it then sets the mode of the server it starts on, and every switch of
    // the server sets the mode of the new one.
    clash_api["default_mode"] = json!(NO_ROUTES_MODE);
    Ok(json!({"log":{"level":"warn","timestamp":true},
        "dns":dns,
        "inbounds":inbounds,"outbounds":outbounds,
        "route":route,
        "experimental":{"clash_api":clash_api}
    }))
}
/// Allowlist diagnostics: avoid accidentally leaking future credential fields. The routing
/// rules are the user's own browsing patterns: only what kind of rule there is and how many.
pub fn sanitized(config: &Value) -> Value {
    let rules: Vec<Value> = config["route"]["rules"]
        .as_array()
        .map(|rules| {
            rules
                .iter()
                .map(|rule| {
                    let conditions: serde_json::Map<String, Value> = rule
                        .as_object()
                        .into_iter()
                        .flatten()
                        .filter(|(key, _)| !["action", "outbound"].contains(&key.as_str()))
                        .map(|(key, value)| {
                            let count = value.as_array().map_or(1, Vec::len);
                            (key.clone(), json!(count))
                        })
                        .collect();
                    json!({"conditions": conditions, "action": rule["action"], "outbound": rule["outbound"]})
                })
                .collect()
        })
        .unwrap_or_default();
    let rule_sets: Vec<Value> = config["route"]["rule_set"]
        .as_array()
        .map(|sets| sets.iter().map(|_| json!({"type": "inline"})).collect())
        .unwrap_or_default();
    json!({"inbounds":config.get("inbounds"),
        "route":{"rules":rules,"rule_set":rule_sets,"final":config["route"]["final"],"find_process":config["route"]["find_process"]},
        "dns_rules":config["dns"]["rules"].as_array().map(Vec::len).unwrap_or(0),
        "outbounds":config["outbounds"].as_array().map(|arr|arr.iter().map(|o|json!({"type":o["type"],"tag":o["tag"]})).collect::<Vec<_>>()),
        "experimental":{"clash_api":{"secret":"[REDACTED]"}}})
}
