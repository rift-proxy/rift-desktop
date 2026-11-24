//! Routing: which traffic goes straight out, through the server, or nowhere.
//!
//! Three lists of rules (Block, Direct, Proxy) are checked in the order the user chose; the
//! last list of that order is "everything else", so its own rules are not needed and are not
//! applied. A rule is one line of text in the syntax of v2fly and Xray, read here into a typed
//! [`Rule`] (nothing the user types reaches the sing-box config as text).
//!
//! | line | meaning |
//! | --- | --- |
//! | `domain:example.com` | the domain and its subdomains (a leading dot is ignored) |
//! | `full:www.example.com` | exactly this domain |
//! | `keyword:ads` | a domain containing this |
//! | `regexp:^ad[0-9]+\.` | a domain matching this expression (RE2 syntax) |
//! | `geosite:category-ru` | a list of domains from the downloaded geosite database |
//! | `10.0.0.0/8`, `192.0.2.1`, `fc00::/7` | an address range or one address |
//! | `geoip:ru` | the address ranges of a country from the downloaded geoip database |
//! | `port:443`, `port:1000-2000` | a destination port or a range, TCP and UDP |
//! | `udp:443`, `tcp:1000-2000` | the same for one transport only |
//! | `protocol:bittorrent` | traffic sing-box recognises as this protocol (sniffing) |
//! | `process:chrome.exe` | the program that opens the connection |
use crate::text::t;
use crate::tr;
use crate::{Error, Result};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::IpAddr;

/// Most entries in one list.
pub const MAX_ENTRIES: usize = 2000;
/// Longest single entry.
pub const MAX_ENTRY_BYTES: usize = 256;

/// What happens to traffic that matches a list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    Block,
    Direct,
    Proxy,
}
impl Route {
    pub const ALL: [Route; 3] = [Route::Block, Route::Direct, Route::Proxy];
    pub const fn title(self) -> &'static str {
        match self {
            Route::Block => "Block",
            Route::Direct => "Direct",
            Route::Proxy => "Proxy",
        }
    }
}

/// The lists a new installation starts with, and what an existing one gets: the domains of the
/// Russian zones and the ranges that never leave the local network or the machine's own.
pub const DEFAULT_DIRECT: [&str; 12] = [
    "domain:.ru",
    "domain:.xn--p1ai",
    "geosite:category-ru",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "224.0.0.0/4",
    "255.255.255.255",
    "fc00::/7",
    "fe80::/10",
    "ff00::/8",
];

/// The rules a provider sends along with a server. They act while the server is the selected
/// one (a Clash mode of sing-box, see [`ServerRoutes::mode`]) and come after the user's own lists.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerRoutes {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub block: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub direct: Vec<String>,
}
/// Most distinct sets of server rules a store may hold: every one is a copy of its rules in the
/// config, and a hostile subscription could send a different set with each server.
pub const MAX_ROUTE_SETS: usize = 32;
/// Most entries of server rules a store may hold in all: every server keeps its own copy of its
/// set in the store, in `state.json`, in the profile and in what the TUN helper is sent.
pub const MAX_ROUTE_ENTRIES: usize = 100_000;
/// The mode of the sing-box Clash API while the selected server has no rules of its own.
pub const NO_ROUTES_MODE: &str = "none";
impl ServerRoutes {
    pub fn is_empty(&self) -> bool {
        self.block.is_empty() && self.direct.is_empty()
    }
    /// How many entries there are in both lists.
    pub fn len(&self) -> usize {
        self.block.len() + self.direct.len()
    }
    /// The entries of one list. Only Block and Direct are lists of a server.
    fn list(&self, route: Route) -> &[String] {
        match route {
            Route::Block => &self.block,
            Route::Direct => &self.direct,
            Route::Proxy => &[],
        }
    }
    /// Each list is within its limit, and every entry reads. A provider does not know the
    /// programs of this machine, so `process:` is refused here.
    pub fn validate(&self) -> Result<()> {
        for route in [Route::Block, Route::Direct] {
            let list = self.list(route);
            if list.len() > MAX_ENTRIES {
                return Err(invalid(tr!(
                    "core.routing.server_list_too_long",
                    route = route.title(),
                    max = MAX_ENTRIES
                )));
            }
            // The text of an entry that was refused is not in what is said: it came from a file or
            // a provider, and what is said goes to the journal.
            for entry in list {
                match parse_rule(entry) {
                    Ok(Rule::Process(_)) => {
                        return Err(invalid(tr!(
                            "core.routing.server_rule_program",
                            route = route.title()
                        )))
                    }
                    Ok(_) => {}
                    Err(_) => {
                        return Err(invalid(tr!(
                            "core.routing.server_rule_unreadable",
                            route = route.title()
                        )))
                    }
                }
            }
        }
        Ok(())
    }
    /// The rules of a list; unreadable entries are a hard error (use after `validate`).
    pub fn rules(&self, route: Route) -> Result<Vec<Rule>> {
        self.list(route)
            .iter()
            .map(|entry| {
                parse_rule(entry)
                    .map_err(|_| invalid(t("core.routing.server_entry_unreadable").into()))
            })
            .collect()
    }
    /// The geosite names and geoip codes the lists refer to.
    pub fn geo_names(&self) -> (Vec<String>, Vec<String>) {
        let (mut sites, mut ips) = (Vec::new(), Vec::new());
        for route in [Route::Block, Route::Direct] {
            for rule in self.rules(route).unwrap_or_default() {
                match rule {
                    Rule::Geosite(name) if !sites.contains(&name) => sites.push(name),
                    Rule::Geoip(code) if !ips.contains(&code) => ips.push(code),
                    _ => {}
                }
            }
        }
        (sites, ips)
    }
    /// The Clash mode these rules act in: the same for the same rules, whatever server brings
    /// them. A hash, so that a name never tells what the rules are.
    pub fn mode(&self) -> String {
        let mut hash = Sha256::new();
        for (name, list) in [("block", &self.block), ("direct", &self.direct)] {
            hash.update(name);
            for entry in list {
                hash.update([0]);
                hash.update(entry.as_bytes());
            }
            hash.update([1]);
        }
        let digest = format!("{:x}", hash.finalize());
        format!("routes-{}", &digest[..12])
    }
}

/// The routing profile: the order lists are checked in and their entries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Routing {
    /// A permutation of Block, Direct and Proxy. The last one takes all that is left.
    pub order: [Route; 3],
    pub block: Vec<String>,
    pub direct: Vec<String>,
    pub proxy: Vec<String>,
}
impl Default for Routing {
    fn default() -> Self {
        Self {
            order: [Route::Block, Route::Direct, Route::Proxy],
            block: Vec::new(),
            direct: DEFAULT_DIRECT
                .iter()
                .map(|entry| (*entry).to_owned())
                .collect(),
            proxy: Vec::new(),
        }
    }
}

/// A rule in a form the config generator can use.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Rule {
    /// The domain and its subdomains.
    Suffix(String),
    /// Exactly this domain.
    Full(String),
    Keyword(String),
    Regex(String),
    Geosite(String),
    /// An address or a range, `a.b.c.d/n` or IPv6.
    Cidr(String),
    Geoip(String),
    Port(u16),
    PortRange(u16, u16),
    /// A port of one transport only.
    NetPort(Net, u16),
    NetPortRange(Net, u16, u16),
    /// A protocol sniffing recognised, one of [`PROTOCOLS`].
    Protocol(String),
    Process(String),
}

/// A transport a port rule can be limited to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Net {
    Tcp,
    Udp,
}
impl Net {
    /// The `network` of a sing-box rule.
    pub const fn name(self) -> &'static str {
        match self {
            Net::Tcp => "tcp",
            Net::Udp => "udp",
        }
    }
}

/// The protocols a `protocol:` rule can name: what sing-box sniffs, without DNS (Rift answers
/// DNS itself).
pub const PROTOCOLS: [&str; 9] = [
    "bittorrent",
    "dtls",
    "http",
    "ntp",
    "quic",
    "rdp",
    "ssh",
    "stun",
    "tls",
];

fn invalid(message: String) -> Error {
    Error::Invalid(message)
}

/// A domain as sing-box wants it: lowercase ASCII (punycode for international names).
fn domain(value: &str) -> Option<String> {
    let value = value.trim().trim_start_matches('.').trim_end_matches('.');
    if value.is_empty() || value.len() > 253 {
        return None;
    }
    match url::Host::parse(value).ok()? {
        // Every label is there: `a..b` is not a domain.
        url::Host::Domain(name)
            if name
                .split('.')
                .all(|label| !label.is_empty() && label.len() <= 63) =>
        {
            Some(name)
        }
        // An address is not a domain: it belongs to the address rules.
        _ => None,
    }
}

/// `[a-z0-9]` plus the separators v2fly uses in the names of its lists.
fn list_name(value: &str, allowed: &str, longest: usize) -> Option<String> {
    let name = value.trim().to_ascii_lowercase();
    let fine = !name.is_empty()
        && name.len() <= longest
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || allowed.contains(c));
    fine.then_some(name)
}

fn address(entry: &str) -> Option<String> {
    let (ip, prefix) = match entry.split_once('/') {
        Some((ip, prefix)) => (ip, Some(prefix)),
        None => (entry, None),
    };
    let ip: IpAddr = ip.trim().parse().ok()?;
    let longest = if ip.is_ipv4() { 32 } else { 128 };
    let prefix = match prefix {
        Some(text) => text.trim().parse::<u8>().ok().filter(|p| *p <= longest)?,
        None => longest,
    };
    Some(format!("{ip}/{prefix}"))
}

fn port(text: &str) -> Option<u16> {
    text.trim().parse::<u16>().ok().filter(|p| *p != 0)
}

// Validate a conservative common subset before saving: Rust accepts extensions that Go's
// regexp (sing-box) rejects. Rust is used for syntax/size checking, never for DNS matching.
fn sing_box_regex(expression: &str) -> bool {
    use regex_syntax::ast::*;
    fn flags(flags: &Flags) -> bool {
        flags.items.iter().all(|item| {
            matches!(
                item.kind,
                FlagsItemKind::Negation
                    | FlagsItemKind::Flag(
                        Flag::CaseInsensitive
                            | Flag::MultiLine
                            | Flag::DotMatchesNewLine
                            | Flag::SwapGreed
                    )
            )
        })
    }
    fn literal(literal: &Literal) -> bool {
        !matches!(
            literal.kind,
            LiteralKind::HexFixed(HexLiteralKind::UnicodeShort | HexLiteralKind::UnicodeLong)
                | LiteralKind::HexBrace(HexLiteralKind::UnicodeShort | HexLiteralKind::UnicodeLong)
        )
    }
    fn unicode(class: &ClassUnicode) -> bool {
        match &class.kind {
            ClassUnicodeKind::OneLetter(_) => true,
            // General categories and the scripts supported by Rift's documented subset.
            ClassUnicodeKind::Named(name) => matches!(
                name.as_str(),
                "L" | "Ll"
                    | "Lm"
                    | "Lo"
                    | "Lt"
                    | "Lu"
                    | "M"
                    | "Mc"
                    | "Me"
                    | "Mn"
                    | "N"
                    | "Nd"
                    | "Nl"
                    | "No"
                    | "P"
                    | "Pc"
                    | "Pd"
                    | "Pe"
                    | "Pf"
                    | "Pi"
                    | "Po"
                    | "Ps"
                    | "S"
                    | "Sc"
                    | "Sk"
                    | "Sm"
                    | "So"
                    | "Z"
                    | "Zl"
                    | "Zp"
                    | "Zs"
                    | "C"
                    | "Cc"
                    | "Cf"
                    | "Co"
                    | "Cs"
                    | "Any"
                    | "Latin"
                    | "Greek"
                    | "Cyrillic"
                    | "Han"
                    | "Hiragana"
                    | "Katakana"
                    | "Arabic"
                    | "Hebrew"
            ),
            _ => false,
        }
    }
    fn class_item(item: &ClassSetItem) -> bool {
        match item {
            ClassSetItem::Literal(value) => literal(value),
            ClassSetItem::Range(range) => literal(&range.start) && literal(&range.end),
            ClassSetItem::Unicode(value) => unicode(value),
            ClassSetItem::Union(union) => union.items.iter().all(class_item),
            ClassSetItem::Bracketed(_) | ClassSetItem::Empty(_) => false,
            _ => true,
        }
    }
    let Ok(ast) = regex_syntax::ast::parse::Parser::new().parse(expression) else {
        return false;
    };
    let mut pending = vec![(&ast, 1u32)];
    while let Some((node, repeated)) = pending.pop() {
        match node {
            Ast::Flags(set) if !flags(&set.flags) => return false,
            Ast::Literal(value) if !literal(value) => return false,
            Ast::ClassUnicode(value) if !unicode(value) => return false,
            Ast::ClassBracketed(value) => match &value.kind {
                ClassSet::Item(item) if class_item(item) => {}
                _ => return false,
            },
            Ast::Assertion(value)
                if !matches!(
                    value.kind,
                    AssertionKind::StartLine
                        | AssertionKind::EndLine
                        | AssertionKind::StartText
                        | AssertionKind::EndText
                        | AssertionKind::WordBoundary
                        | AssertionKind::NotWordBoundary
                ) =>
            {
                return false
            }
            Ast::Group(group) => {
                if group.flags().is_some_and(|f| !flags(f)) {
                    return false;
                }
                if let GroupKind::CaptureName { name, .. } = &group.kind {
                    if !name
                        .name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    {
                        return false;
                    }
                }
                pending.push((&group.ast, repeated));
            }
            Ast::Repetition(rep) => {
                if matches!(*rep.ast, Ast::Repetition(_)) {
                    return false;
                }
                let count = match rep.op.kind {
                    RepetitionKind::Range(
                        RepetitionRange::Exactly(n)
                        | RepetitionRange::AtLeast(n)
                        | RepetitionRange::Bounded(_, n),
                    ) => n,
                    _ => 1,
                };
                let count = repeated.saturating_mul(count.max(1));
                if count > 1000 {
                    return false;
                }
                pending.push((&rep.ast, count));
            }
            Ast::Alternation(value) => pending.extend(value.asts.iter().map(|ast| (ast, repeated))),
            Ast::Concat(value) => pending.extend(value.asts.iter().map(|ast| (ast, repeated))),
            _ => {}
        }
    }
    true
}

/// The ports of an entry: one, or a range `from-to`.
enum Ports {
    One(u16),
    Range(u16, u16),
}
fn ports(value: &str, entry: &str) -> Result<Ports> {
    let value = value.trim();
    match value
        .split_once('-')
        .map(|(from, to)| (port(from), port(to)))
    {
        Some((Some(from), Some(to))) if from <= to => Ok(Ports::Range(from, to)),
        Some(_) => Err(invalid(tr!("core.routing.bad_port_range", entry = entry))),
        None => port(value)
            .map(Ports::One)
            .ok_or_else(|| invalid(tr!("core.routing.bad_port", entry = entry))),
    }
}

/// One entry, read. The error names what is wrong with it in words a user can act on.
pub fn parse_rule(entry: &str) -> Result<Rule> {
    let entry = entry.trim();
    if entry.len() > MAX_ENTRY_BYTES {
        return Err(invalid(tr!(
            "core.routing.entry_too_long",
            max = MAX_ENTRY_BYTES
        )));
    }
    let unknown = || invalid(tr!("core.routing.unreadable", entry = entry));
    if let Some((prefix, value)) = entry.split_once(':') {
        let name = prefix.to_ascii_lowercase();
        match name.as_str() {
            "domain" => {
                return domain(value)
                    .map(Rule::Suffix)
                    .ok_or_else(|| invalid(tr!("core.routing.bad_domain", entry = entry)))
            }
            "full" => {
                return domain(value)
                    .map(Rule::Full)
                    .ok_or_else(|| invalid(tr!("core.routing.bad_domain", entry = entry)))
            }
            "keyword" => {
                let word = value.trim().to_lowercase();
                return if !word.is_empty()
                    && word.len() <= 64
                    && !word.chars().any(|c| c.is_whitespace() || c.is_control())
                {
                    Ok(Rule::Keyword(word))
                } else {
                    Err(invalid(tr!("core.routing.bad_word", entry = entry)))
                };
            }
            "regexp" => {
                let expression = value.trim();
                // The same limits sing-box's own RE2 has no trouble with: no unbounded state.
                return RegexBuilder::new(expression)
                    .size_limit(1 << 20)
                    .build()
                    .ok()
                    .filter(|_| !expression.is_empty() && sing_box_regex(expression))
                    .map(|_| Rule::Regex(expression.to_owned()))
                    .ok_or_else(|| invalid(tr!("core.routing.bad_regex", entry = entry)));
            }
            "geosite" => {
                if value.trim_start().starts_with('!') || value.contains('@') {
                    return Err(invalid(tr!(
                        "core.routing.negation_and_attributes",
                        entry = entry
                    )));
                }
                return list_name(value, "!_-", 64)
                    .map(Rule::Geosite)
                    .ok_or_else(|| invalid(tr!("core.routing.bad_list_name", entry = entry)));
            }
            "geoip" => {
                if value.trim_start().starts_with('!') {
                    return Err(invalid(tr!("core.routing.negation", entry = entry)));
                }
                return list_name(value, "_-", 32)
                    .map(Rule::Geoip)
                    .ok_or_else(|| invalid(tr!("core.routing.bad_list_code", entry = entry)));
            }
            "port" => {
                return ports(value, entry).map(|ports| match ports {
                    Ports::One(port) => Rule::Port(port),
                    Ports::Range(from, to) => Rule::PortRange(from, to),
                })
            }
            "tcp" | "udp" => {
                let net = if name == "tcp" { Net::Tcp } else { Net::Udp };
                return ports(value, entry).map(|ports| match ports {
                    Ports::One(port) => Rule::NetPort(net, port),
                    Ports::Range(from, to) => Rule::NetPortRange(net, from, to),
                });
            }
            "protocol" => {
                let protocol = value.trim().to_ascii_lowercase();
                return PROTOCOLS
                    .contains(&protocol.as_str())
                    .then_some(Rule::Protocol(protocol))
                    .ok_or_else(|| invalid(tr!("core.routing.unknown_protocol", entry = entry)));
            }
            "process" => {
                let program = value.trim();
                let fine = !program.is_empty()
                    && program.len() <= 128
                    && !program
                        .chars()
                        .any(|c| c.is_control() || "\\/:*?\"<>|,".contains(c));
                return if fine {
                    Ok(Rule::Process(program.to_owned()))
                } else {
                    Err(invalid(tr!("core.routing.bad_program_name", entry = entry)))
                };
            }
            _ => {}
        }
    }
    address(entry).map(Rule::Cidr).ok_or_else(unknown)
}

/// The text a user typed or pasted for one list, split into entries: one per line, or several
/// separated by commas or semicolons. A `regexp:` line stays whole (a comma belongs to it).
pub fn split_entries(text: &str) -> Vec<String> {
    let mut entries: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.to_ascii_lowercase().starts_with("regexp:") {
            entries.push(line.to_owned());
            continue;
        }
        entries.extend(
            line.split([',', ';'])
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_owned),
        );
    }
    let mut seen = std::collections::HashSet::new();
    entries.retain(|entry| seen.insert(entry.to_lowercase()));
    entries
}

impl Routing {
    /// A profile from what the user typed, checked. Nothing is kept of a list with an
    /// entry that cannot be read.
    pub fn from_text(order: [Route; 3], block: &str, direct: &str, proxy: &str) -> Result<Self> {
        let routing = Self {
            order,
            block: split_entries(block),
            direct: split_entries(direct),
            proxy: split_entries(proxy),
        };
        routing.validate()?;
        Ok(routing)
    }

    pub fn list(&self, route: Route) -> &[String] {
        match route {
            Route::Block => &self.block,
            Route::Direct => &self.direct,
            Route::Proxy => &self.proxy,
        }
    }

    /// The route that takes everything the other two lists do not.
    pub fn rest(&self) -> Route {
        self.order[2]
    }

    /// The order is a permutation, the lists are within their limits and every entry that
    /// can matter (those of the first two lists) reads. The last list is not applied, but a
    /// damaged entry there is still refused: it would come back to life with a new order.
    pub fn validate(&self) -> Result<()> {
        let mut seen = [false; 3];
        for route in self.order {
            let at = Route::ALL.iter().position(|r| *r == route).unwrap_or(0);
            if std::mem::replace(&mut seen[at], true) {
                return Err(invalid(t("core.routing.duplicate_list_order").into()));
            }
        }
        for route in Route::ALL {
            let list = self.list(route);
            if list.len() > MAX_ENTRIES {
                return Err(invalid(tr!(
                    "core.routing.list_too_long",
                    route = route.title(),
                    max = MAX_ENTRIES
                )));
            }
            for entry in list {
                parse_rule(entry).map_err(|error| {
                    invalid(tr!(
                        "core.routing.list_error",
                        route = route.title(),
                        error = error
                    ))
                })?;
            }
        }
        Ok(())
    }

    /// The rules of a list; unreadable entries are a hard error (use after `validate`).
    pub fn rules(&self, route: Route) -> Result<Vec<Rule>> {
        self.list(route)
            .iter()
            .map(|entry| parse_rule(entry))
            .collect()
    }

    /// The geosite names and geoip codes the lists that are applied refer to.
    pub fn geo_names(&self) -> (Vec<String>, Vec<String>) {
        let (mut sites, mut ips) = (Vec::new(), Vec::new());
        for route in &self.order[..2] {
            for rule in self.rules(*route).unwrap_or_default() {
                match rule {
                    Rule::Geosite(name) if !sites.contains(&name) => sites.push(name),
                    Rule::Geoip(code) if !ips.contains(&code) => ips.push(code),
                    _ => {}
                }
            }
        }
        (sites, ips)
    }

    /// The six orders, in the order the settings list them.
    pub fn all_orders() -> [[Route; 3]; 6] {
        use Route::*;
        [
            [Block, Direct, Proxy],
            [Block, Proxy, Direct],
            [Direct, Block, Proxy],
            [Direct, Proxy, Block],
            [Proxy, Block, Direct],
            [Proxy, Direct, Block],
        ]
    }
}
