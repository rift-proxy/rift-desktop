use crate::text::t;
use crate::{
    failure::FailureCode,
    routing::{Routing, ServerRoutes, MAX_ROUTE_ENTRIES, MAX_ROUTE_SETS, NO_ROUTES_MODE},
    Error, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Longest stored server name; ingestion and the portable profile share it.
pub const MAX_NAME_BYTES: usize = 512;
/// Longest stored connectivity check URL. The setting is copied into every state snapshot, so a
/// damaged state file or a hostile profile must not be able to carry a multi-megabyte string.
pub const MAX_CONNECTIVITY_URL_BYTES: usize = 2048;
/// The name every ingestion path stores: never blank (the host stands in) and never longer
/// than `MAX_NAME_BYTES`, cut at a character boundary. Provider remarks are display text, so
/// shortening one must not make the whole store unexportable.
pub fn display_name(name: &str, host: &str) -> String {
    let shown = if name.trim().is_empty() { host } else { name };
    let mut end = shown.len().min(MAX_NAME_BYTES);
    while !shown.is_char_boundary(end) {
        end -= 1;
    }
    shown[..end].to_owned()
}

// Deliberately no Debug: this object holds credentials.
#[derive(Clone)]
pub struct Server {
    pub id: String,
    pub name: String,
    /// Legacy Store/profile v1 metadata; never resolved or exposed to the UI.
    pub country: String,
    pub protocol: String,
    pub host: String,
    pub port: u16,
    /// In-process compatibility projection. Store/profile v2 uses a neutral Connection wire;
    /// v1 `outbound` remains a read adapter. Production LaunchPlan never executes this raw JSON.
    pub outbound: Value,
    pub source: Option<String>,
    /// The rules the provider sent along with this server; not part of its identity.
    pub routes: Option<ServerRoutes>,
}
impl Server {
    /// The server's own rules, if it has any.
    pub fn own_routes(&self) -> Option<&ServerRoutes> {
        self.routes.as_ref().filter(|routes| !routes.is_empty())
    }
    /// The Clash mode in which this server's rules act (the one with no rules when it has none).
    pub fn routes_mode(&self) -> String {
        self.own_routes()
            .map_or_else(|| NO_ROUTES_MODE.to_owned(), ServerRoutes::mode)
    }
    /// What names this server across a refresh of its subscription: the source and the address, not
    /// the credentials. A provider that rotates the credentials of a server (its `id` changes with
    /// them) keeps the address, so a selection, a pool or an exclusion kept by this key survives the
    /// rotation. Rows of one source that share an address share the key. A server added by hand has
    /// the empty source; a subscription id is never empty and never holds `|`.
    pub fn stable_key(&self) -> String {
        format!(
            "{}|{}:{}",
            self.source.as_deref().unwrap_or(""),
            self.host.to_lowercase(),
            self.port
        )
    }
    pub fn fingerprint(outbound: &Value) -> String {
        let mut canonical = outbound.clone();
        if let Some(o) = canonical.as_object_mut() {
            o.remove("tag");
        }
        format!("{:x}", Sha256::digest(canonical.to_string().as_bytes()))
    }
    /// Capability refusal is distinct from storage validity: do not discard old records or
    /// treat a malformed opaque object as a known retired combination.
    pub fn legacy_refusal(&self) -> Option<Error> {
        crate::connection::Connection::from_legacy(&self.outbound)
            .and_then(|c| c.check_xray())
            .err()
            .filter(|e| e.failure_code() == Some(FailureCode::UnsupportedLegacyParameters))
    }
    pub fn is_retired(&self) -> bool {
        self.legacy_refusal().is_some()
    }
    pub fn summary(&self) -> ServerView {
        let retired = self.is_retired();
        ServerView {
            id: self.id.clone(),
            name: self.name.clone(),
            protocol: self.protocol.clone(),
            source: self.source.clone(),
            endpoint_ms: None,
            proxy_ms: None,
            status: if retired { "failed" } else { "unchecked" }.into(),
            checked_at: None,
            failure_code: retired.then_some(FailureCode::UnsupportedLegacyParameters),
            retry_at: None,
            excluded: false,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerView {
    pub id: String,
    pub name: String,
    pub protocol: String,
    pub source: Option<String>,
    pub endpoint_ms: Option<u64>,
    pub proxy_ms: Option<u64>,
    pub status: String,
    pub checked_at: Option<u64>,
    pub failure_code: Option<FailureCode>,
    /// The server did not answer and is out of the cycles of selection: when it is asked again
    /// (Unix seconds). None for a server that is measured with the others.
    #[serde(default)]
    pub retry_at: Option<u64>,
    /// The user keeps this server out of Auto Select: it is shown last and grey, and no check of
    /// the servers asks it unless it is the one in use.
    #[serde(default)]
    pub excluded: bool,
}
/// Longest subscription name. The name comes from the provider, so it is cut, not rejected.
pub const MAX_SUBSCRIPTION_NAME_BYTES: usize = 120;
/// Longest `User-Agent` a subscription may send.
pub const MAX_USER_AGENT_BYTES: usize = 256;
/// Most entries in each of the include and exclude lists of a subscription.
pub const MAX_FILTER_ENTRIES: usize = 50;
/// Longest single include or exclude entry.
pub const MAX_FILTER_ENTRY_BYTES: usize = 64;
/// Protocols a subscription filter can name: the `type` of the sing-box outbound.
pub const FILTER_PROTOCOLS: [&str; 4] = ["vless", "vmess", "trojan", "shadowsocks"];
/// Subscription refresh interval, in minutes: the bounds of the global and the per-source value.
pub const SUBSCRIPTION_INTERVAL_MINS: std::ops::RangeInclusive<u64> = 1..=10080;

/// Which servers of a subscription are kept at import. Filtered servers are not stored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filters {
    /// Keep only servers whose name contains one of these (case-insensitive). Empty: all.
    pub include: Vec<String>,
    /// Drop servers whose name contains one of these (case-insensitive).
    pub exclude: Vec<String>,
    /// Keep only these protocols (`FILTER_PROTOCOLS`). Empty: all.
    pub protocols: Vec<String>,
}
impl Filters {
    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty() && self.protocols.is_empty()
    }
    /// What a user typed, tidied: entries trimmed, blanks and repeats (ignoring case) dropped.
    pub fn normalized(mut self) -> Self {
        fn tidy(list: &mut Vec<String>) {
            let mut seen = std::collections::HashSet::new();
            *list = std::mem::take(list)
                .into_iter()
                .map(|entry| entry.trim().to_owned())
                .filter(|entry| !entry.is_empty() && seen.insert(entry.to_lowercase()))
                .collect();
        }
        tidy(&mut self.include);
        tidy(&mut self.exclude);
        self.protocols = std::mem::take(&mut self.protocols)
            .into_iter()
            .map(|protocol| protocol.trim().to_lowercase())
            .collect();
        let mut seen = std::collections::HashSet::new();
        self.protocols
            .retain(|protocol| seen.insert(protocol.clone()));
        self
    }
    pub fn validate(&self) -> Result<()> {
        let bad = |message: &str| Err(Error::Invalid(message.into()));
        for list in [&self.include, &self.exclude] {
            if list.len() > MAX_FILTER_ENTRIES {
                return bad(t("core.model.filter_too_many"));
            }
            if list.iter().any(|entry| {
                entry.trim().is_empty()
                    || entry.len() > MAX_FILTER_ENTRY_BYTES
                    || entry.chars().any(char::is_control)
            }) {
                return bad(t("core.model.filter_bad_value"));
            }
        }
        if self.protocols.len() > FILTER_PROTOCOLS.len()
            || self
                .protocols
                .iter()
                .any(|protocol| !FILTER_PROTOCOLS.contains(&protocol.as_str()))
        {
            return bad(t("core.model.filter_unknown_protocol"));
        }
        Ok(())
    }
    /// Whether the filters keep this server.
    pub fn keeps(&self, server: &Server) -> bool {
        let name = server.name.to_lowercase();
        let named = |entry: &String| name.contains(&entry.to_lowercase());
        if !self.include.is_empty() && !self.include.iter().any(named) {
            return false;
        }
        if self.exclude.iter().any(named) {
            return false;
        }
        self.protocols.is_empty()
            || server.outbound["type"]
                .as_str()
                .is_some_and(|kind| self.protocols.iter().any(|protocol| protocol == kind))
    }
}

/// Traffic and validity reported by the provider (`subscription-userinfo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Upload plus download, bytes.
    pub used: u64,
    /// Quota in bytes; `0`: the provider states none.
    pub total: u64,
    /// Unix seconds; `None`: no expiry stated.
    pub expire: Option<u64>,
}
/// Whether the servers of a subscription may be chosen automatically.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Ok,
    Exhausted,
    Expired,
}
impl Usage {
    pub fn availability(&self, now: u64) -> Availability {
        if self.expire.is_some_and(|expire| expire <= now) {
            Availability::Expired
        } else if self.total > 0 && self.used >= self.total {
            Availability::Exhausted
        } else {
            Availability::Ok
        }
    }
}

// Deliberately no `Eq`: nothing compares subscriptions as a whole.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub name: String,
    pub url: String,
    pub updated_at: Option<u64>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<FailureCode>,
    /// Refresh interval of this source, minutes. `None`: the global one from the settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_mins: Option<u64>,
    /// `User-Agent` of this source. `None`: `Rift/<version>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Filters::is_empty")]
    pub filters: Filters,
    /// What the provider reported at the last refresh. `None`: nothing reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Whether the rules a provider sends with its servers (Xray JSON) are kept with them.
    #[serde(
        default = "provider_routes_by_default",
        skip_serializing_if = "is_true"
    )]
    pub provider_routes: bool,
}
/// A subscription brings the rules of its servers unless it says otherwise.
pub(crate) fn provider_routes_by_default() -> bool {
    true
}
// serde hands a field to `skip_serializing_if` by reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_true(value: &bool) -> bool {
    *value
}
// A subscription starts with the provider's rules kept.
impl Default for Subscription {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            url: String::new(),
            updated_at: None,
            error: None,
            failure_code: None,
            interval_mins: None,
            user_agent: None,
            filters: Filters::default(),
            usage: None,
            provider_routes: true,
        }
    }
}
impl Subscription {
    /// The options a user can set, checked: also the validation of stored and imported data.
    pub fn validate_options(&self) -> Result<()> {
        if self
            .interval_mins
            .is_some_and(|mins| !SUBSCRIPTION_INTERVAL_MINS.contains(&mins))
        {
            return Err(Error::Invalid(t("core.model.subscription_interval").into()));
        }
        if let Some(agent) = &self.user_agent {
            if agent.is_empty()
                || agent.len() > MAX_USER_AGENT_BYTES
                || !agent.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
            {
                return Err(Error::Invalid(t("core.model.user_agent").into()));
            }
        }
        self.filters.validate()
    }
    /// The subscription name from its provider: control characters removed, cut at a character
    /// boundary to `MAX_SUBSCRIPTION_NAME_BYTES`. `None` if nothing readable is left.
    pub fn clean_name(raw: &str) -> Option<String> {
        let text: String = raw.chars().filter(|c| !c.is_control()).collect();
        let text = text.trim();
        let mut end = text.len().min(MAX_SUBSCRIPTION_NAME_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let text = text[..end].trim_end();
        (!text.is_empty()).then(|| text.to_owned())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Tun,
    Proxy,
    TunProxy,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub mode: Mode,
    pub theme: String,
    /// The language of the interface: a code of a catalog (`text::languages`, the files of
    /// `desktop/src/locales`). It sets the language of the core's own texts and of the interface;
    /// it does not touch the sing-box or Xray configuration.
    pub language: String,
    pub auto_select: bool,
    pub proxy_port: u16,
    pub api_port: u16,
    /// How often the fast check asks the servers of the pool whether they still answer as they did
    /// (seconds). Files of earlier versions allowed up to a day: when a file is read this is brought
    /// to the nearest value now allowed (`bring_into_range`); from the interface it is refused.
    pub interval_secs: u64,
    /// How many of the best servers the last full check keeps ready (the pool), at most one per host.
    pub pool_size: usize,
    /// How often every server is checked and the pool is built again (minutes, from the end of the
    /// previous full check).
    pub full_interval_mins: u64,
    /// A server of the pool is worse when its delay grew by this share (percent) of the delay it had
    /// when the pool was built...
    pub degrade_percent: u64,
    /// ...but the share never lifts the limit above this many milliseconds in total (a ceiling on
    /// the delay, not on the growth): a delay above it is allowed only the growth of
    /// `switch_threshold_ms`, the least growth worth noticing. Together they are the tolerance.
    pub degrade_cap_ms: u64,
    pub endpoint_timeout_ms: u64,
    pub proxy_timeout_ms: u64,
    /// How many servers are connected to at the same moment while they are measured. A burst of
    /// connections to many servers at once is what makes the list of servers easy to recognise
    /// from outside, so it stays small. A saved larger value is read as the largest
    /// (`bring_into_range`); from the interface it is refused.
    pub concurrency: usize,
    pub connectivity_url: String,
    pub subscription_interval_mins: u64,
    /// A working server is left only for one faster by this share of its delay (percent)...
    pub switch_threshold_percent: u64,
    /// ...and by this many milliseconds. Both must hold.
    pub switch_threshold_ms: u64,
    /// Start Rift with Windows (a `Run` entry of the current user; the window stays in the tray).
    /// The registration is local to this computer: a profile import does not change it.
    pub autostart: bool,
    /// Connect when Rift starts, with the saved mode and the last selected server.
    pub auto_connect: bool,
    /// Look for new routing databases (geosite, geoip) every twelve hours and download a new
    /// version when there is one. Off: the databases are only fetched when one that the rules
    /// need is missing, and by the button.
    pub geodata_auto_update: bool,
    /// Ask sing-box which program opened each connection (`route.find_process`), to show it on
    /// the connections page. It costs a lookup per new connection, so it is off by default; a
    /// routing rule by program turns the lookup on by itself.
    pub find_process: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Proxy,
            theme: "system".into(),
            language: crate::text::DEFAULT_LANGUAGE.into(),
            auto_select: true,
            proxy_port: 2080,
            api_port: 2081,
            interval_secs: 60,
            pool_size: 3,
            full_interval_mins: 10,
            degrade_percent: 50,
            degrade_cap_ms: 1500,
            endpoint_timeout_ms: 1500,
            proxy_timeout_ms: 5000,
            concurrency: Settings::MAX_CONCURRENCY,
            connectivity_url: "https://www.gstatic.com/generate_204".into(),
            subscription_interval_mins: 60,
            switch_threshold_percent: 30,
            switch_threshold_ms: 50,
            autostart: false,
            auto_connect: false,
            find_process: false,
            geodata_auto_update: true,
        }
    }
}
impl Settings {
    /// The most servers measured at the same moment.
    pub const MAX_CONCURRENCY: usize = 3;
    /// Seconds between fast checks.
    pub const MIN_INTERVAL_SECS: u64 = 50;
    pub const MAX_INTERVAL_SECS: u64 = 300;
    /// Servers kept ready.
    pub const MAX_POOL_SIZE: usize = 5;
    /// Minutes between full checks.
    pub const MIN_FULL_INTERVAL_MINS: u64 = 10;
    pub const MAX_FULL_INTERVAL_MINS: u64 = 30;
    pub const MIN_DEGRADE_PERCENT: u64 = 10;
    pub const MAX_DEGRADE_PERCENT: u64 = 300;
    pub const MIN_DEGRADE_CAP_MS: u64 = 200;
    pub const MAX_DEGRADE_CAP_MS: u64 = 10000;
    /// What a file written by an earlier version may hold that this one refuses: up to a day between
    /// checks (now 50-300 s) and up to 64 servers measured at once (now 3). A stored state or a
    /// profile is read with these brought to the nearest value allowed instead of being refused as
    /// damaged. Zero stays zero and fails `validate`. Settings that come from the interface are not
    /// brought anywhere: `validate` refuses them.
    pub fn bring_into_range(&mut self) {
        self.interval_secs = self
            .interval_secs
            .clamp(Self::MIN_INTERVAL_SECS, Self::MAX_INTERVAL_SECS);
        self.concurrency = self.concurrency.min(Self::MAX_CONCURRENCY);
    }
    pub fn validate(&self) -> Result<()> {
        if self.connectivity_url.len() > MAX_CONNECTIVITY_URL_BYTES {
            return Err(Error::Invalid(t("core.model.check_url_too_long").into()));
        }
        let url = url::Url::parse(&self.connectivity_url)
            .map_err(|_| Error::Invalid(t("core.model.bad_check_url").into()))?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(Error::Invalid(t("core.model.check_url_https").into()));
        }
        if self.proxy_port == 0
            || self.api_port == 0
            || self.api_port == self.proxy_port
            || !(1..=Self::MAX_CONCURRENCY).contains(&self.concurrency)
            || !(Self::MIN_INTERVAL_SECS..=Self::MAX_INTERVAL_SECS).contains(&self.interval_secs)
            || !(1..=Self::MAX_POOL_SIZE).contains(&self.pool_size)
            || !(Self::MIN_FULL_INTERVAL_MINS..=Self::MAX_FULL_INTERVAL_MINS)
                .contains(&self.full_interval_mins)
            || !(Self::MIN_DEGRADE_PERCENT..=Self::MAX_DEGRADE_PERCENT)
                .contains(&self.degrade_percent)
            || !(Self::MIN_DEGRADE_CAP_MS..=Self::MAX_DEGRADE_CAP_MS).contains(&self.degrade_cap_ms)
            || !(100..=30000).contains(&self.endpoint_timeout_ms)
            || !(100..=30000).contains(&self.proxy_timeout_ms)
            || !(1..=10080).contains(&self.subscription_interval_mins)
            || self.switch_threshold_percent > 100
            || self.switch_threshold_ms > 10000
            || !["system", "light", "dark"].contains(&self.theme.as_str())
            || !crate::text::is_language(&self.language)
        {
            return Err(Error::Invalid(t("core.model.bad_settings").into()));
        }
        Ok(())
    }
}
/// The column the list of servers is ordered by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortColumn {
    #[default]
    Name,
    Protocol,
    EndpointMs,
    ProxyMs,
    Status,
    Source,
    CheckedAt,
}
/// How the user ordered the list of servers: by which column, and whether backwards. A saved
/// value this version does not know (a damaged file, a later version) reads as the default,
/// the name from A to Z, instead of failing the whole state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ServersSort {
    pub column: SortColumn,
    pub descending: bool,
}
impl<'de> Deserialize<'de> for ServersSort {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        // Whatever stands in the file, the state is not refused for it.
        let value = Value::deserialize(deserializer)?;
        let column = value
            .get("column")
            .and_then(|column| SortColumn::deserialize(column).ok());
        let descending = value.get("descending").and_then(Value::as_bool);
        Ok(column
            .zip(descending)
            .map_or_else(Self::default, |(column, descending)| Self {
                column,
                descending,
            }))
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Store {
    pub version: u32,
    pub settings: Settings,
    pub servers: Vec<Server>,
    pub subscriptions: Vec<Subscription>,
    pub selected: Option<String>,
    /// The welcome window has done its job: something was added or the user skipped it. Rift
    /// never shows it again; a window closed unanswered (Rift quit) comes back at the next start.
    #[serde(default)]
    pub welcome_dismissed: bool,
    /// Which traffic goes direct, through the server or nowhere. Old files get the defaults.
    #[serde(default)]
    pub routing: Routing,
    /// The order of the list of servers. Old files get the name from A to Z.
    #[serde(default)]
    pub servers_sort: ServersSort,
    /// Servers kept out of Auto Select by the user, by `Server::stable_key`: the source and the
    /// address, so that two servers of one name are two and a rotation of credentials changes
    /// nothing. A key with no server behind it is dropped (`prune_excluded`).
    #[serde(default)]
    pub excluded: Vec<String>,
}
/// Most keys of excluded servers a file may carry, and the longest one: a subscription holds at most
/// 2000 servers, the source id is at most 128 bytes and a host at most 253.
pub const MAX_EXCLUDED: usize = 4096;
pub const MAX_EXCLUDED_KEY_BYTES: usize = 512;
impl Default for Store {
    fn default() -> Self {
        Self {
            version: 2,
            settings: Settings::default(),
            servers: vec![],
            subscriptions: vec![],
            selected: None,
            welcome_dismissed: false,
            routing: Routing::default(),
            servers_sort: ServersSort::default(),
            excluded: vec![],
        }
    }
}
impl Store {
    /// Whether the user keeps this server out of Auto Select.
    pub fn is_excluded(&self, server: &Server) -> bool {
        !self.excluded.is_empty() && self.excluded.contains(&server.stable_key())
    }
    /// The server in use, when the user has kept it out of Auto Select: it is still asked while it
    /// is in use (a manual choice is respected), and is not a candidate for anything else.
    pub fn excluded_in_use(&self) -> Option<Server> {
        let id = self.selected.as_ref()?;
        self.available_iter()
            .find(|server| &server.id == id && self.is_excluded(server))
            .cloned()
    }
    /// Forgets the keys that no server of the list stands behind, and repeats.
    pub fn prune_excluded(&mut self) {
        if self.excluded.is_empty() {
            return;
        }
        let keys: std::collections::HashSet<String> =
            self.servers.iter().map(Server::stable_key).collect();
        let mut seen = std::collections::HashSet::new();
        self.excluded
            .retain(|key| keys.contains(key) && seen.insert(key.clone()));
    }
    /// The distinct sets of server rules among the servers that can be selected, by mode.
    pub fn route_sets(&self) -> std::collections::BTreeMap<String, ServerRoutes> {
        let mut seen = std::collections::HashSet::new();
        let mut sets = std::collections::BTreeMap::new();
        for server in self.available_iter() {
            // The config holds the first of the servers that share an id.
            if !seen.insert(server.id.as_str()) {
                continue;
            }
            if let Some(routes) = server.own_routes() {
                sets.entry(routes.mode()).or_insert_with(|| routes.clone());
            }
        }
        sets
    }
    /// The geosite names and geoip codes the applied rules refer to: the user's lists and the
    /// rules servers bring.
    pub fn geo_names(&self) -> (Vec<String>, Vec<String>) {
        let (mut sites, mut ips) = self.routing.geo_names();
        for set in self.route_sets().values() {
            let (more_sites, more_ips) = set.geo_names();
            for name in more_sites {
                if !sites.contains(&name) {
                    sites.push(name);
                }
            }
            for code in more_ips {
                if !ips.contains(&code) {
                    ips.push(code);
                }
            }
        }
        (sites, ips)
    }
    /// The server the `main` selector runs for this Store (the config `default`): the selected
    /// one when it can be used, else the first.
    pub fn live_selection(&self) -> String {
        self.selected
            .as_ref()
            .filter(|id| self.available_iter().any(|s| &s.id == *id))
            .cloned()
            .or_else(|| {
                self.available_iter()
                    .find(|s| !s.is_retired())
                    .map(|s| s.id.clone())
            })
            .unwrap_or_default()
    }
    /// The mode in which the rules of the server sing-box starts on act.
    pub fn live_routes_mode(&self) -> String {
        self.routes_mode_of(&self.live_selection())
            .unwrap_or_else(|| NO_ROUTES_MODE.to_owned())
    }
    /// The mode in which the rules of server `id` act; `None` for a server that is not there.
    pub fn routes_mode_of(&self, id: &str) -> Option<String> {
        self.available_iter()
            .find(|server| server.id == id)
            .map(Server::routes_mode)
    }
    /// How many entries the rules of all servers come to, each server counted with its own copy.
    pub fn route_entries(&self) -> usize {
        self.servers
            .iter()
            .filter_map(|server| server.own_routes().map(ServerRoutes::len))
            .sum()
    }
    /// The server rules are all readable and there are not too many different sets or entries.
    pub fn validate_routes(&self) -> Result<()> {
        for server in &self.servers {
            if let Some(routes) = &server.routes {
                routes.validate()?;
            }
        }
        let distinct: std::collections::HashSet<String> = self
            .servers
            .iter()
            .filter_map(|server| server.own_routes().map(ServerRoutes::mode))
            .collect();
        if distinct.len() > MAX_ROUTE_SETS {
            return Err(Error::Invalid(t("core.model.too_many_rule_sets").into()));
        }
        if self.route_entries() > MAX_ROUTE_ENTRIES {
            return Err(Error::Invalid(t("core.model.too_many_server_rules").into()));
        }
        Ok(())
    }
    /// Whether the welcome window is due: a store with nothing in it that never had its say.
    pub fn welcome_due(&self) -> bool {
        self.servers.is_empty() && self.subscriptions.is_empty() && !self.welcome_dismissed
    }
    /// The servers Auto Select may choose at `now` (Unix seconds): a server qualifies through
    /// any copy of it that is manual or comes from a subscription whose reported
    /// traffic is not used up and whose term has not ended. Manual selection is not limited
    /// by this. One entry per server id, in store order.
    pub fn auto_candidates(&self, now: u64) -> Vec<Server> {
        // Whichever copy of a server qualifies, the rules it brings are those of the copy the
        // config holds (the first source-present one): only that set has its mode in the config.
        let mut held: std::collections::HashMap<&str, Option<&ServerRoutes>> =
            std::collections::HashMap::new();
        for server in self.available_iter() {
            held.entry(server.id.as_str())
                .or_insert_with(|| server.own_routes());
        }
        let mut seen = std::collections::HashSet::new();
        let excluded: std::collections::HashSet<&str> =
            self.excluded.iter().map(String::as_str).collect();
        self.servers
            .iter()
            .filter(|server| !server.is_retired())
            .filter(|server| {
                excluded.is_empty() || !excluded.contains(server.stable_key().as_str())
            })
            .filter(|server| {
                server.source.as_ref().is_none_or(|id| {
                    self.subscriptions.iter().any(|source| {
                        &source.id == id
                            && source
                                .usage
                                .is_none_or(|usage| usage.availability(now) == Availability::Ok)
                    })
                })
            })
            .filter(|server| seen.insert(server.id.clone()))
            .map(|server| {
                let mut server = server.clone();
                server.routes = held.get(server.id.as_str()).copied().flatten().cloned();
                server
            })
            .collect()
    }
    pub fn available_servers(&self) -> Vec<Server> {
        self.available_iter().cloned().collect()
    }
    /// Manual servers and rows whose source still exists, without copying them.
    pub(crate) fn available_iter(&self) -> impl Iterator<Item = &Server> {
        self.servers.iter().filter(|s| {
            s.source
                .as_ref()
                .is_none_or(|id| self.subscriptions.iter().any(|sub| &sub.id == id))
        })
    }
}
