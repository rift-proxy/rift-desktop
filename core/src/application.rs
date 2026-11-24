//! Serialized application operations and a single selection worker. No Tauri dependency.
use crate::text::t;
use crate::tr;
use crate::{
    autostart::{self, auto_connect_plan, AutoConnect, Autostart},
    clash::{self, Clash, ConnectionsView, Speed},
    config, dead,
    diagnostics::{
        Attempt, CycleKind, Detail, Mark, Operation, SelectionInfo, Stage, Trigger, Via,
    },
    failure::FailureCode,
    fast::{self, Watched},
    geodata::{self, GeoCache, GeoSource},
    history::{self, History},
    jitter::Jitter,
    journal, link,
    model::{
        Availability, Filters, Mode, Server, ServerView, ServersSort, Settings, SortColumn, Store,
        Subscription, Usage,
    },
    notifications,
    parser::parse_uri,
    persistence,
    pool::{self, Tolerance},
    probe::NetworkProbe,
    profile, qr,
    routing::{Route, Routing, NO_ROUTES_MODE},
    runtime::Runtime,
    selection::{self, Cycle, Decision, Network, Plan, Probe, Progress, Reason, Schedule},
    subscription, Error, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{oneshot, watch, Mutex, Notify},
    time::{sleep, Instant},
};
use tokio_util::sync::CancellationToken;

pub fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubscriptionView {
    pub id: String,
    pub name: String,
    pub count: usize,
    pub updated_at: Option<u64>,
    pub error: Option<String>,
    pub failure_code: Option<FailureCode>,
    /// Refresh interval of this source, minutes; `None`: the global one.
    pub interval_mins: Option<u64>,
    /// `User-Agent` of this source; `None`: Rift's own.
    pub user_agent: Option<String>,
    pub filters: Filters,
    /// Whether the rules the provider sends with its servers (Xray JSON) are kept with them.
    pub provider_routes: bool,
    /// Traffic and term the provider reported at the last refresh.
    pub usage: Option<Usage>,
    /// Whether Auto Select may use the servers of this source now (term and traffic).
    pub availability: Availability,
}
/// Where a journal entry comes from. Entries written before the journal had sources, and
/// values a future version adds, read as `App`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Subscription downloads and their failures.
    Subscriptions,
    /// Server checks and the choice of a server.
    Checks,
    /// The sing-box process: start, stop, crash, the config it was given.
    Core,
    /// Rift itself: servers, settings, profile, diagnostics, start.
    #[serde(other)]
    #[default]
    App,
}
impl Category {
    /// The source an error belongs to, by the stage its code names.
    pub const fn of_failure(code: Option<FailureCode>) -> Self {
        match code {
            Some(
                FailureCode::EndpointTimeout
                | FailureCode::EndpointConnect
                | FailureCode::ProxyTimeout
                | FailureCode::ProxyTransport
                | FailureCode::ProxyHttpStatus
                | FailureCode::ProxyTestFailed
                | FailureCode::ProxyMissing
                | FailureCode::ProxyInvalidResponse
                | FailureCode::SelectorSwitch
                | FailureCode::ConnectionLost
                | FailureCode::NoAvailableServer,
            ) => Self::Checks,
            Some(
                FailureCode::SubscriptionTimeout
                | FailureCode::SubscriptionTransport
                | FailureCode::SubscriptionHttpStatus
                | FailureCode::SubscriptionInvalidBody
                | FailureCode::SubscriptionFiltered,
            ) => Self::Subscriptions,
            Some(
                FailureCode::SidecarConfigRejected
                | FailureCode::XrayConfigRejected
                | FailureCode::XrayConfigTimeout
                | FailureCode::XrayStartExit
                | FailureCode::XrayReadinessTimeout
                | FailureCode::XrayCrashed
                | FailureCode::SidecarConfigTimeout
                | FailureCode::SidecarStartExit
                | FailureCode::LocalPortInUse
                | FailureCode::LocalPortDenied
                | FailureCode::SidecarReadinessTimeout
                | FailureCode::SidecarCrashed
                | FailureCode::SidecarStop
                | FailureCode::ClashApi
                | FailureCode::ElevationDeclined
                | FailureCode::ElevationFailed
                | FailureCode::ElevationChannel
                | FailureCode::ElevationHelperCrashed,
            ) => Self::Core,
            _ => Self::App,
        }
    }
}
/// A geosite or geoip database on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DbView {
    /// Unix seconds the file was written.
    pub updated_at: u64,
    pub bytes: u64,
    /// Unix seconds the source last confirmed it is the latest version. Kept beside the file
    /// (`<database>.checked`), so a restart still knows it; `None` when never confirmed.
    #[serde(default)]
    pub checked_at: Option<u64>,
}
/// The databases behind `geosite:` and `geoip:` rules.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeodataView {
    pub geosite: Option<DbView>,
    pub geoip: Option<DbView>,
    /// A download is running.
    pub updating: bool,
    /// The database being fetched now (`geosite` or `geoip`), while one is.
    #[serde(default)]
    pub current: Option<String>,
    /// The routing profile refers to a list of this database.
    #[serde(default)]
    pub uses_geosite: bool,
    #[serde(default)]
    pub uses_geoip: bool,
    /// Why the last download failed; cleared by the next one that succeeds.
    pub failure_code: Option<FailureCode>,
    /// This build can download them.
    pub available: bool,
    /// The routing profile refers to a list of the databases.
    pub wanted: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub at: u64,
    pub level: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    /// Locally generated identity makes crash-file acknowledgement retries idempotent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crash_id: Option<uuid::Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<FailureCode>,
    #[serde(default)]
    pub category: Category,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub settings: Settings,
    pub servers: Vec<ServerView>,
    pub subscriptions: Vec<SubscriptionView>,
    pub selected: Option<String>,
    pub status: String,
    pub runtime_active: bool,
    pub checking: bool,
    /// A subscription refresh is active, including a coalesced follow-up batch.
    pub refreshing: bool,
    pub pending: bool,
    pub next_check: Option<u64>,
    pub error: Option<String>,
    pub journal: Vec<Entry>,
    /// The welcome window is due: nothing is added yet and it was never answered.
    pub welcome: bool,
    pub routing: Routing,
    /// How the list of servers is ordered (saved).
    pub servers_sort: ServersSort,
    pub geodata: GeodataView,
}
impl Snapshot {
    fn from_store(store: &Store) -> Self {
        Self {
            settings: store.settings.clone(),
            servers: store
                .servers
                .iter()
                .map(|s| {
                    let mut view = s.summary();
                    view.excluded = store.is_excluded(s);
                    view
                })
                .collect(),
            subscriptions: store
                .subscriptions
                .iter()
                .map(|s| SubscriptionView {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    count: store
                        .servers
                        .iter()
                        .filter(|v| v.source.as_ref() == Some(&s.id))
                        .count(),
                    updated_at: s.updated_at,
                    error: s.error.clone(),
                    failure_code: s.failure_code,
                    interval_mins: s.interval_mins,
                    user_agent: s.user_agent.clone(),
                    filters: s.filters.clone(),
                    provider_routes: s.provider_routes,
                    usage: s.usage,
                    availability: s
                        .usage
                        .map_or(Availability::Ok, |usage| usage.availability(timestamp())),
                })
                .collect(),
            selected: store.selected.clone(),
            status: "disconnected".into(),
            runtime_active: false,
            checking: false,
            refreshing: false,
            pending: false,
            next_check: None,
            error: None,
            journal: vec![],
            welcome: store.welcome_due(),
            routing: store.routing.clone(),
            servers_sort: store.servers_sort,
            geodata: GeodataView::default(),
        }
    }
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Connect,
    Disconnect,
    CheckNow,
    SetAuto {
        enabled: bool,
    },
    Select {
        id: String,
    },
    AddServer {
        uri: String,
    },
    /// Several servers at once, all or none (a paste, a QR code or a `rift://` link).
    AddServers {
        uris: Vec<String>,
    },
    RemoveServer {
        id: String,
    },
    /// The name comes from the subscription itself; until the first refresh it is the host.
    SaveSubscription {
        id: Option<String>,
        url: String,
        #[serde(default)]
        interval_mins: Option<u64>,
        #[serde(default)]
        user_agent: Option<String>,
        #[serde(default)]
        filters: Filters,
        /// Keep the rules the provider sends with its servers. Older callers do not say: yes.
        #[serde(default = "crate::model::provider_routes_by_default")]
        provider_routes: bool,
    },
    RemoveSubscription {
        id: String,
    },
    Refresh {
        id: Option<String>,
    },
    SaveSettings {
        settings: Settings,
    },
    ClearJournal,
    ExportDiagnostics,
    /// «Пропустить» in the welcome window: it is not shown again.
    DismissWelcome,
    /// A routing profile as typed: the order of the lists and one text per list.
    SaveRouting {
        order: [Route; 3],
        block: String,
        direct: String,
        proxy: String,
    },
    /// Back to the profile a new installation has.
    ResetRouting,
    /// Download the geosite and geoip databases now.
    UpdateGeodata,
    /// Order the list of servers by a column (the choice is saved).
    SetServersSort {
        column: SortColumn,
        descending: bool,
    },
    /// Keep a server out of Auto Select, or let it back in. The choice is saved by address and
    /// source (not by the id the credentials make) for every row that has this id: a link in two
    /// subscriptions is one server to Auto Select.
    SetServerExcluded {
        id: String,
        excluded: bool,
    },
}
struct Inner {
    store: Store,
    runtime: Box<dyn Runtime>,
    probe: Arc<dyn Probe>,
    schedule: Schedule,
    connected: bool,
    /// Per-subscription consecutive-failure count and next-due time, keyed by subscription
    /// id. Kept per source (not one shared counter) so a manual single-subscription refresh,
    /// or one permanently failing subscription, cannot perturb or over-fetch a different,
    /// healthy subscription's own schedule.
    subscription_backoff: HashMap<String, SourceBackoff>,
    /// The local account of the last failed refresh of each subscription, until one succeeds.
    subscription_failures: HashMap<String, String>,
    subscription_rows: HashMap<String, crate::diagnostics::Rows>,
    /// Safety net for the automatic cycle: when `refresh()` fails for a reason other than an
    /// individual subscription fetch (e.g. the runtime rejects the merged config), per-source
    /// backoff alone would not necessarily retry soon, so this forces one shortly after.
    subscription_retry_at: Instant,
    /// After a failed automatic refresh the worker does not start another one for a source that
    /// is merely due until then: a result that could not be stored leaves its source due, and
    /// without this the worker would repeat the download every tick. `subscription_retry_at`
    /// forces the next full attempt at the same moment.
    subscription_hold_until: Instant,
    failover: Failover,
    /// The last cycle found no network and said so; the next one that finds it says that too.
    offline: bool,
    /// The journal has said that the sidecar cannot get out by itself (once per connection).
    deaf_said: bool,
    /// The best servers of the last full check, and the delays they had.
    pool: PoolState,
    /// What the fast check found that the full check after it has to know.
    hints: Hints,
    /// The server in use did not answer a fast check: the full check that replaces it starts at once.
    urgent: Option<Reason>,
    /// The last full check found no working server and said so; the subscriptions are refreshed
    /// out of turn until one is found.
    outage: bool,
    /// Not before this are the subscriptions refreshed out of turn again.
    outage_refresh_at: Instant,
}
/// The pool: the best servers of the last full check, by the key of their address, so that a
/// subscription that gives its servers other credentials does not empty it.
#[derive(Default)]
struct PoolState {
    /// Best first.
    members: Vec<PoolMember>,
    /// How many members the last full check could find, at most `pool_size`. Fewer present now
    /// means one of them is gone.
    target: usize,
    /// Everything that answered reliably in the last full check, fastest first: where the pool is
    /// made up again from when a server of it is kept out of Auto Select, without new questions.
    ranking: Vec<KeyedRank>,
    /// The delays the fast check compares with, by key: the members', and the delay of a server in
    /// use that is not one of them, once it is known.
    bases: HashMap<String, u64>,
}
struct PoolMember {
    key: String,
}
struct KeyedRank {
    key: String,
    host: String,
    ms: u64,
}
/// What a fast check hands to the full check it asks for.
#[derive(Default)]
struct Hints {
    /// Servers that did not answer a moment ago: not asked again in that check.
    skip: HashSet<String>,
    /// The server in use is known dead: it is not asked again either.
    current_lost: bool,
}
/// Light checks of the selected server between cycles. Two answers missed in a row start a cycle
/// at once (Auto Select) or put "connection lost" in the status (manual choice).
struct Failover {
    /// When the next light check is due; `None` until planned.
    next_at: Option<Instant>,
    /// Consecutive missed answers of the selected server.
    failures: u32,
    /// The loss is reported (manual choice): the status says so until an answer returns.
    lost: bool,
    /// No failover cycle before this: the last one found nothing, the regular interval is waited.
    held_until: Option<Instant>,
}
impl Failover {
    const fn new() -> Self {
        Self {
            next_at: None,
            failures: 0,
            lost: false,
            held_until: None,
        }
    }
}
/// How often the selected server is asked, and how soon again after a missed answer.
const FAILOVER_INTERVAL: Duration = Duration::from_secs(15);
const FAILOVER_RETRY: Duration = Duration::from_secs(5);
/// Missed answers in a row that make a loss.
const FAILOVER_FAILURES: u32 = 2;
/// Backoff bookkeeping for one subscription source (see `Inner::subscription_backoff`).
#[derive(Clone)]
struct SourceBackoff {
    failures: u32,
    next_due: Instant,
}
struct Control {
    token: CancellationToken,
    refresh: CancellationToken,
    /// Ends a pass of asking the dead servers again.
    dead: CancellationToken,
    disconnecting: bool,
}
#[derive(Clone)]
enum RefreshScope {
    All,
    Sources(HashSet<String>),
}
impl RefreshScope {
    fn none() -> Self {
        Self::Sources(HashSet::new())
    }
    fn one(id: Option<String>) -> Self {
        match id {
            Some(id) => Self::Sources(HashSet::from([id])),
            None => Self::All,
        }
    }
    fn contains(&self, id: &str) -> bool {
        match self {
            Self::All => true,
            Self::Sources(ids) => ids.contains(id),
        }
    }
    fn covers(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::All, _) => true,
            (Self::Sources(_), Self::All) => false,
            (Self::Sources(ours), Self::Sources(theirs)) => theirs.is_subset(ours),
        }
    }
    fn merge(&mut self, other: Self) {
        match (&mut *self, other) {
            (Self::All, _) => {}
            (_, Self::All) => *self = Self::All,
            (Self::Sources(ours), Self::Sources(theirs)) => ours.extend(theirs),
        }
    }
}
#[derive(Clone)]
enum RefreshOutcome {
    Success,
    Cancelled,
    Failed {
        message: String,
        failure_code: Option<FailureCode>,
        /// Where and how it happened, handed on to everyone who waited for this refresh.
        detail: Option<Box<Detail>>,
        diagnostic_details: String,
    },
}
impl RefreshOutcome {
    fn from_result(result: &Result<()>) -> Self {
        match result {
            Ok(()) => Self::Success,
            Err(Error::Cancelled) => Self::Cancelled,
            Err(error) => Self::Failed {
                message: error.to_string(),
                failure_code: error.failure_code(),
                detail: error.detail().cloned().map(Box::new),
                diagnostic_details: error.diagnostic_details(),
            },
        }
    }
    fn into_result(self) -> Result<()> {
        match self {
            Self::Success => Ok(()),
            Self::Cancelled => Err(Error::Cancelled),
            Self::Failed {
                message,
                failure_code: Some(code),
                detail: Some(detail),
                diagnostic_details,
            } => Err(Error::Failure {
                code,
                message,
                detail,
                source: None,
            }
            .with_local_details(diagnostic_details)),
            Self::Failed {
                message,
                failure_code: Some(code),
                detail: None,
                diagnostic_details,
            } => Err(Error::failure(code, message).with_local_details(diagnostic_details)),
            Self::Failed {
                message,
                failure_code: None,
                ..
            } => Err(Error::Invalid(message)),
        }
    }
}
struct RefreshBatch {
    scope: RefreshScope,
    /// The sources inside `scope` that skip their backoff. Kept per source: merging a forced
    /// single-source request with an automatic refresh-all must not force the whole scope.
    forced: RefreshScope,
    /// The sources inside `scope` that somebody asked for by hand. Separate from `forced`: the
    /// automatic worker forces a full cycle too, and a failure of that is no manual one.
    manual: RefreshScope,
    waiters: Vec<oneshot::Sender<RefreshOutcome>>,
}
#[derive(Default)]
struct RefreshGate {
    active: Option<RefreshBatch>,
    pending: Option<RefreshBatch>,
}
/// Held by the refresh leader for the whole flight. A leader that is dropped (its task is
/// aborted) or panics before it hands the gate back would leave `active` set for good: every
/// later refresh and the background worker would wait for a result that never comes. The
/// guard then fails the waiting requests with `Cancelled` and reopens the gate.
struct RefreshLeader<'a> {
    app: &'a Application,
    released: bool,
}
impl Drop for RefreshLeader<'_> {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let (active, pending) = {
            let mut gate = self
                .app
                .refresh_gate
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            (gate.active.take(), gate.pending.take())
        };
        for batch in [active, pending].into_iter().flatten() {
            for waiter in batch.waiters {
                let _ = waiter.send(RefreshOutcome::Cancelled);
            }
        }
        self.app
            .snapshots
            .send_modify(|snapshot| snapshot.refreshing = false);
    }
}
pub struct Application {
    inner: Mutex<Inner>,
    pub snapshots: watch::Sender<Snapshot>,
    control: StdMutex<Control>,
    /// One subscription refresh owns the network path. Compatible requests join it;
    /// different scopes merge into one follow-up batch instead of forming an unbounded queue.
    refresh_gate: StdMutex<RefreshGate>,
    pending: AtomicU8,
    wake: Notify,
    shutdown: CancellationToken,
    directory: PathBuf,
    secret: String,
    subscriptions: Arc<dyn subscription::SubscriptionSource>,
    /// The text of `Snapshot.error` while the profile commands own it. Only a later successful
    /// profile command clears it, and only while the snapshot still shows this text: a crash or
    /// a selection failure replaces the banner and takes the ownership with it.
    profile_error: StdMutex<Option<String>>,
    /// Spread of every periodic interval of the coordinator (see `jitter`).
    jitter: Jitter,
    /// Registration of Rift in the programs that start with Windows (a no-op until the host
    /// installs the real one).
    autostart: StdMutex<Arc<dyn Autostart>>,
    /// The geosite and geoip databases in `<directory>/geodata`.
    geo: GeoCache,
    /// Where they are downloaded from; none: this build does not download them.
    geo_source: StdMutex<Option<Arc<dyn GeoSource>>>,
    geo_plan: StdMutex<GeoPlan>,
    /// The geosite names and geoip codes the stored rules (the user's and the servers') refer to.
    geo_wanted: StdMutex<(Vec<String>, Vec<String>)>,
    /// The address of each subscription by its id, as the coordinator last published them. The
    /// dialog that edits a subscription asks for it, and a full check holds the coordinator for
    /// minutes: this is read without it. Memory only; it leaves the core through
    /// `subscription_url` and nowhere else.
    subscription_urls: StdMutex<HashMap<String, String>>,
    /// Known user values used to clean journal text without taking the coordinator lock.
    journal_secrets: StdMutex<journal::Secrets>,
    /// Serializes synchronous host/core journal appends and clears through their atomic writes.
    journal_gate: StdMutex<()>,
    /// The Clash mode sing-box is in: the rules of the server it works through, as far as Rift
    /// told it. Only to journal a change.
    live_routes: StdMutex<String>,
    /// The latest measurements of each server, memory only; asked for, never published.
    history: StdMutex<History>,
    /// Servers that did not answer: out of the cycles of selection, asked again one by one at
    /// times of their own (see `dead`). Memory only; a new connection starts with none.
    dead: StdMutex<dead::Dead>,
    /// A pass of asking them again is running.
    recheck_running: AtomicBool,
    /// The next cycle was asked for by hand and looks at every server, the dead ones too.
    check_all: AtomicBool,
    /// The screen that shows the speed is open. Nothing is read from sing-box while it is not.
    traffic_wanted: watch::Sender<bool>,
    /// The latest speed; none when nothing is being read. Separate from the snapshot: it changes
    /// every second and only a screen showing it needs it.
    traffic: watch::Sender<Option<Speed>>,
    /// The connections page is open.
    connections_wanted: watch::Sender<bool>,
    /// The latest list of connections; none when nothing is being read. Memory only, apart from
    /// the snapshot, the journal, diagnostics and the profile.
    connections: watch::Sender<Option<ConnectionsView>>,
}
/// How often the open connections page asks sing-box. A fixed two seconds: the page is a live view
/// of the local API, not a schedule that reaches a network.
const CONNECTIONS_POLL: Duration = Duration::from_secs(2);
/// When the databases are looked at next, and what the last look found.
struct GeoPlan {
    next_due: Instant,
    failures: u32,
    updating: bool,
    /// The database being fetched now.
    current: Option<geodata::Kind>,
    /// When each database was last confirmed to be the latest one.
    checked: HashMap<geodata::Kind, u64>,
    failure: Option<FailureCode>,
}
impl GeoPlan {
    fn new() -> Self {
        Self {
            next_due: Instant::now(),
            failures: 0,
            updating: false,
            current: None,
            checked: HashMap::new(),
            failure: None,
        }
    }
    /// What the folder remembers of the last looks: a restart does not start the count over.
    fn remembering(geo: &GeoCache) -> Self {
        let mut plan = Self::new();
        for kind in geodata::Kind::ALL {
            if let Some(at) = geo.checked_at(kind) {
                plan.checked.insert(kind, at);
            }
        }
        plan
    }
}
/// How often the source is asked whether a database has a newer version (every twelve hours).
const GEODATA_CHECK_EVERY: Duration = Duration::from_secs(12 * 60 * 60);
impl Application {
    pub fn new(store: Store, directory: PathBuf, runtime: Box<dyn Runtime>) -> Result<Arc<Self>> {
        let secret = uuid::Uuid::new_v4().simple().to_string();
        let probe = Arc::new(
            NetworkProbe::new(store.settings.clone(), secret.clone())?
                .with_channel(runtime.proxy_channel()),
        );
        let app = Self::with_jitter(
            store,
            directory,
            runtime,
            probe,
            secret,
            Arc::new(subscription::HttpSubscriptionSource),
            Jitter::random(),
        );
        app.set_geo_source(Arc::new(geodata::HttpGeoSource::new()));
        Ok(app)
    }
    pub fn with_probe(
        store: Store,
        directory: PathBuf,
        runtime: Box<dyn Runtime>,
        probe: Arc<dyn Probe>,
        secret: String,
    ) -> Arc<Self> {
        Self::with_services(
            store,
            directory,
            runtime,
            probe,
            secret,
            Arc::new(subscription::HttpSubscriptionSource),
        )
    }
    pub fn with_services(
        store: Store,
        directory: PathBuf,
        runtime: Box<dyn Runtime>,
        probe: Arc<dyn Probe>,
        secret: String,
        subscriptions: Arc<dyn subscription::SubscriptionSource>,
    ) -> Arc<Self> {
        Self::with_jitter(
            store,
            directory,
            runtime,
            probe,
            secret,
            subscriptions,
            Jitter::none(),
        )
    }
    /// The constructor `new` uses, with the spread of periodic intervals made explicit.
    /// The other constructors (tests) use exact intervals.
    pub fn with_jitter(
        store: Store,
        directory: PathBuf,
        runtime: Box<dyn Runtime>,
        probe: Arc<dyn Probe>,
        secret: String,
        subscriptions: Arc<dyn subscription::SubscriptionSource>,
        jitter: Jitter,
    ) -> Arc<Self> {
        let mut snapshot = Snapshot::from_store(&store);
        let mut journal_secrets = journal::Secrets::from_store(&store, &directory);
        journal_secrets.extend([secret.clone()]);
        if let Ok(bytes) = std::fs::read(directory.join("journal.json")) {
            if bytes.len() <= 1_048_576 {
                if let Ok(mut entries) = serde_json::from_slice::<Vec<Entry>>(&bytes) {
                    for entry in &mut entries {
                        entry.message = journal::clean(&entry.message, &journal_secrets);
                        entry.details =
                            journal::clean_details(entry.details.as_deref(), &journal_secrets);
                    }
                    journal::trim(&mut entries, timestamp());
                    if let Ok(cleaned) = serde_json::to_vec(&entries) {
                        let _ =
                            persistence::atomic_write(&directory.join("journal.json"), &cleaned);
                    }
                    snapshot.journal = entries;
                }
            }
        }
        // Recover panic records left by a previous process before the live writer exists.
        if journal::has_pending_crashes(&directory).unwrap_or(false) {
            let _ =
                journal::persist_with_crashes(&directory, &mut snapshot.journal, &journal_secrets);
        }
        let geo = GeoCache::open(directory.join("geodata"));
        let wanted = store.geo_names();
        let urls = subscription_urls_of(&store);
        let geo_plan = GeoPlan::remembering(&geo);
        snapshot.geodata = geodata_view(&geo, &wanted, false, &geo_plan);
        let (snapshots, _) = watch::channel(snapshot);
        Arc::new(Self {
            inner: Mutex::new(Inner {
                schedule: Schedule::new(
                    Duration::from_secs(store.settings.interval_secs),
                    Duration::from_secs(store.settings.full_interval_mins * 60),
                    jitter.clone(),
                ),
                store,
                runtime,
                probe,
                connected: false,
                subscription_backoff: HashMap::new(),
                subscription_failures: HashMap::new(),
                subscription_rows: HashMap::new(),
                subscription_retry_at: Instant::now(),
                subscription_hold_until: Instant::now(),
                failover: Failover::new(),
                offline: false,
                deaf_said: false,
                pool: PoolState::default(),
                hints: Hints::default(),
                urgent: None,
                outage: false,
                outage_refresh_at: Instant::now(),
            }),
            snapshots,
            control: StdMutex::new(Control {
                token: CancellationToken::new(),
                refresh: CancellationToken::new(),
                dead: CancellationToken::new(),
                disconnecting: false,
            }),
            refresh_gate: StdMutex::new(RefreshGate::default()),
            pending: AtomicU8::new(0),
            wake: Notify::new(),
            shutdown: CancellationToken::new(),
            directory,
            secret,
            subscriptions,
            profile_error: StdMutex::new(None),
            jitter,
            autostart: StdMutex::new(Arc::new(autostart::NoAutostart)),
            geo,
            geo_source: StdMutex::new(None),
            geo_plan: StdMutex::new(geo_plan),
            geo_wanted: StdMutex::new(wanted),
            subscription_urls: StdMutex::new(urls),
            journal_secrets: StdMutex::new(journal_secrets),
            journal_gate: StdMutex::new(()),
            live_routes: StdMutex::new(NO_ROUTES_MODE.to_owned()),
            history: StdMutex::new(History::default()),
            dead: StdMutex::new(dead::Dead::new()),
            recheck_running: AtomicBool::new(false),
            check_all: AtomicBool::new(false),
            traffic_wanted: watch::channel(false).0,
            traffic: watch::channel(None).0,
            connections_wanted: watch::channel(false).0,
            connections: watch::channel(None).0,
        })
    }
    /// The screen with the speed opens (`true`) or closes (`false`). Safe to repeat.
    pub fn watch_traffic(&self, on: bool) {
        self.traffic_wanted.send_replace(on);
    }
    /// The connections page opens (`true`) or closes (`false`). Safe to repeat.
    pub fn watch_connections(&self, on: bool) {
        self.connections_wanted.send_replace(on);
    }
    /// The connections, every two seconds while the page is open and Rift is connected.
    pub fn connections(&self) -> watch::Receiver<Option<ConnectionsView>> {
        self.connections.subscribe()
    }
    /// The speed through the proxy, once a second while it is watched and Rift is connected.
    pub fn traffic(&self) -> watch::Receiver<Option<Speed>> {
        self.traffic.subscribe()
    }
    /// The latest measurements of one server, the oldest first. Memory only and not part of the
    /// snapshot, so a long list of servers does not weigh on every event.
    pub fn history(&self, id: &str) -> Vec<history::Sample> {
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .of(id)
    }
    /// The address of a subscription, for the dialog that edits it. It leaves the core only here,
    /// when the dialog asks: it is in no snapshot, journal or diagnostics export.
    ///
    /// It does not wait for the coordinator: a full check holds that for minutes, and the dialog
    /// would stay on «Читаем адрес…» all that time. The address is the one last published with the
    /// subscriptions, so it is there whenever the dialog can ask for it.
    pub async fn subscription_url(&self, id: &str) -> Result<String> {
        self.subscription_urls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| Error::Invalid(t("core.application.subscription_not_found").into()))
    }
    pub fn snapshot(&self) -> Snapshot {
        self.snapshots.borrow().clone()
    }
    /// Failures of the portable-profile commands take the path of `execute` failures: a
    /// snapshot error, a journal entry carrying the typed code and cleaned details, and the
    /// local error toast. Paths and file content are never displayed without cleaning.
    /// The banner stays until the user acts, but a later successful profile command removes it
    /// again (see `Application::profile_error`).
    pub async fn report_failure(&self, error: &Error) {
        self.record_failure(error, true).await;
    }
    async fn record_failure(&self, error: &Error, profile: bool) {
        // The profile commands never change the store, so their failures do not wait for the
        // coordinator, which a selection cycle holds for as long as its probes take.
        if !profile {
            self.publish_store(&self.inner.lock().await.store);
        }
        let text = self.clean_message(&error.to_string());
        *self.profile_error.lock().unwrap_or_else(|e| e.into_inner()) =
            profile.then(|| text.clone());
        self.snapshots.send_modify(|s| s.error = Some(text));
        self.log_error(&error.to_string(), error);
    }
    /// Another writer of `Snapshot.error` (crash, selection failure) owns the banner now.
    fn forget_profile_error(&self) {
        *self.profile_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    /// A profile command succeeded: its own earlier failure is no longer current.
    fn clear_profile_error(&self) {
        let owned = self
            .profile_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(text) = owned {
            self.snapshots.send_if_modified(|s| {
                let current = s.error.as_deref() == Some(text.as_str());
                if current {
                    s.error = None;
                }
                current
            });
        }
    }
    async fn reported<T>(&self, result: Result<T>) -> Result<T> {
        match &result {
            Ok(_) => self.clear_profile_error(),
            Err(error) => self.report_failure(error).await,
        }
        result
    }
    pub async fn export_profile(&self) -> Result<Vec<u8>> {
        let encoded = profile::encode(&self.inner.lock().await.store);
        self.reported(encoded).await
    }
    /// Writes an exported profile to the path the user chose; an existing file survives a
    /// failed write.
    pub async fn write_profile(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let written = persistence::atomic_write(path, bytes)
            .map_err(|error| error.with_code(FailureCode::Persistence));
        self.reported(written).await
    }
    /// Reads and fully checks the profile at `path` (including `sing-box check`).
    pub async fn inspect_profile(&self, path: &Path) -> Result<Store> {
        let inspected = match profile::read(path) {
            Ok(store) => self.check_profile(&store).await.map(|()| store),
            Err(error @ Error::Io(_)) => Err(error.with_code(FailureCode::Persistence)),
            Err(error) => Err(error),
        };
        self.reported(inspected).await
    }
    /// Reads the QR code of the image file the user picked and checks what it asks for (a
    /// subscription or servers): nothing is added. Failures are recorded like those of the
    /// profile commands; their texts are static, with no path and no content.
    pub async fn scan_qr_file(&self, path: &Path) -> Result<link::InputPreview> {
        let read = qr::read_file(path);
        let scanned = match read {
            Ok(bytes) => self.decode_qr(bytes).await,
            Err(error) => Err(error),
        };
        self.reported(scanned).await
    }
    /// The same for an image already in memory (a picture from the clipboard).
    pub async fn scan_qr_image(&self, bytes: Vec<u8>) -> Result<link::InputPreview> {
        let scanned = self.decode_qr(bytes).await;
        self.reported(scanned).await
    }
    /// Decoding is CPU work on untrusted pixels: off the async threads.
    async fn decode_qr(&self, bytes: Vec<u8>) -> Result<link::InputPreview> {
        tokio::task::spawn_blocking(move || {
            qr::read_image(&bytes).and_then(|text| link::preview_input(&text))
        })
        .await
        .map_err(|_| Error::Invalid(t("core.qr.image_read_failed").into()))?
    }
    pub async fn validate_profile(&self, store: &Store) -> Result<()> {
        let checked = self.check_profile(store).await;
        self.reported(checked).await
    }
    async fn check_profile(&self, store: &Store) -> Result<()> {
        profile::validate(store)?;
        let mut inner = self.inner.lock().await;
        if !store.available_servers().is_empty() {
            inner
                .runtime
                .validate(store, &self.secret)
                .await
                .map_err(|error| match error {
                    // The check could not run at all: not the file's fault, not a rejection.
                    Error::Failure {
                        code: FailureCode::Persistence,
                        ..
                    } => Error::failure(FailureCode::Persistence, profile::check_unavailable()),
                    other => other.with_code(FailureCode::SidecarConfigRejected),
                })?;
        }
        Ok(())
    }
    pub async fn import_profile(&self, store: Store) -> Result<Snapshot> {
        let applied = self.apply_profile(store).await;
        // The snapshot is taken after the earlier profile failure was cleared.
        self.reported(applied).await.map(|()| self.snapshot())
    }
    async fn apply_profile(&self, store: Store) -> Result<()> {
        profile::validate(&store)?;
        let replacement_probe = NetworkProbe::new(store.settings.clone(), self.secret.clone())?;
        self.cancel(false);
        let mut inner = self.inner.lock().await;
        // The registration belongs to this computer, not to the profile.
        let replacement_probe =
            Arc::new(replacement_probe.with_channel(inner.runtime.proxy_channel()));
        let mut store = store;
        store.settings.autostart = inner.store.settings.autostart;
        let previous = inner.store.settings.clone();
        let restarted = self.replace(&mut inner, store, true).await?;
        // Rebuilt only when a setting it reads changed, as in SaveSettings.
        if probe_inputs(&previous) != probe_inputs(&inner.store.settings) {
            inner.probe = replacement_probe;
        }
        inner.subscription_backoff.clear();
        inner.subscription_failures.clear();
        inner.subscription_rows.clear();
        // Due at once and unfiltered. Lift the pause itself as well: if this first flight is
        // cancelled, the imported sources must still be due on the next worker tick.
        inner.subscription_retry_at = Instant::now();
        inner.subscription_hold_until = Instant::now();
        let interval = Duration::from_secs(inner.store.settings.interval_secs);
        let full_interval = Duration::from_secs(inner.store.settings.full_interval_mins * 60);
        if restarted {
            // A new sidecar has a new selector: nothing was switched yet.
            inner.schedule = Schedule::new(interval, full_interval, self.jitter.clone());
            self.forget_pool(&mut inner);
        } else {
            // The running selector keeps its server, so the switch + interval cooldown stays.
            inner.schedule.set_intervals(interval, full_interval);
        }
        // The snapshot flags are reset below, so the schedule must not keep a cycle they no
        // longer show; the Initial request re-queues one when Auto Select is on.
        inner.schedule.pending = None;
        let start_initial = inner.connected && inner.store.settings.auto_select;
        self.pending.store(0, Ordering::SeqCst);
        self.snapshots.send_modify(|snapshot| {
            snapshot.pending = false;
            snapshot.next_check = None;
        });
        if start_initial {
            self.request(Reason::ListChanged);
        }
        self.publish_store(&inner.store);
        self.log("info", t("core.application.profile_imported"));
        Ok(())
    }
    /// Installs the registration of Rift among the programs that start with Windows. Call it
    /// before `start`.
    pub fn set_autostart(&self, autostart: Arc<dyn Autostart>) {
        *self.autostart.lock().unwrap_or_else(|e| e.into_inner()) = autostart;
    }
    fn autostart(&self) -> Arc<dyn Autostart> {
        self.autostart
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Makes the registration match the setting: a moved installation rewrites its entry, a
    /// disabled setting removes a stale one. A failure is reported; the setting stays, so the
    /// next launch tries again.
    fn reconcile_autostart(&self) {
        let wanted = self.snapshot().settings.autostart;
        let port = self.autostart();
        let result = if wanted {
            match port.is_enabled() {
                Ok(true) => Ok(()),
                _ => port.set(true),
            }
        } else {
            port.set(false)
        };
        if let Err(error) = result {
            self.log_error(&error.to_string(), &error);
        }
    }
    /// Connects at launch when the setting asks for it and there is something to connect to.
    /// Failures take the path of any `Connect`: local toast and journal.
    pub async fn auto_connect(&self) {
        let plan = auto_connect_plan(&self.inner.lock().await.store);
        match plan {
            AutoConnect::Off => {}
            AutoConnect::Skip(reason) => self.log("info", reason),
            AutoConnect::Connect => {
                self.log("info", t("core.application.autoconnect_on_start"));
                let _ = self.execute(Action::Connect).await;
            }
        }
    }
    pub fn start(self: &Arc<Self>) {
        self.reconcile_autostart();
        let app = self.clone();
        tokio::spawn(async move {
            app.worker().await;
        });
        let app = self.clone();
        tokio::spawn(async move {
            app.traffic_worker().await;
        });
        let app = self.clone();
        tokio::spawn(async move {
            app.connections_worker().await;
        });
    }
    /// The user refused the Windows prompt: Proxy is set instead. A notice, no error, nothing sent.
    fn declined_notice(&self) {
        let text = t("core.application.uac_declined_proxy");
        self.log_in(Category::Core, "info", text);
        notifications::inform(text);
    }
    /// Waits until the screen wants what it shows and sing-box runs. `false`: Rift is exiting.
    async fn wait_wanted(
        &self,
        wanted: &mut watch::Receiver<bool>,
        snapshots: &mut watch::Receiver<Snapshot>,
    ) -> bool {
        loop {
            if self.shutdown.is_cancelled() {
                return false;
            }
            if *wanted.borrow_and_update() && snapshots.borrow_and_update().runtime_active {
                return true;
            }
            tokio::select! {
                _=self.shutdown.cancelled()=>return false,
                r=wanted.changed()=>if r.is_err() { return false },
                r=snapshots.changed()=>if r.is_err() { return false },
            }
        }
    }
    /// Runs `work` until it ends; the moment the screen stops wanting it, sing-box stops or Rift
    /// exits, `stop` fires and `work` is expected to end.
    async fn while_wanted<T>(
        &self,
        wanted: &mut watch::Receiver<bool>,
        snapshots: &mut watch::Receiver<Snapshot>,
        stop: &CancellationToken,
        work: impl std::future::Future<Output = T>,
    ) -> T {
        tokio::pin!(work);
        loop {
            tokio::select! {biased;
                r=&mut work=>return r,
                _=self.shutdown.cancelled()=>stop.cancel(),
                r=wanted.changed()=>if r.is_err() || !*wanted.borrow() { stop.cancel() },
                r=snapshots.changed()=>
                    if r.is_err() || !snapshots.borrow().runtime_active { stop.cancel() },
            }
        }
    }
    /// What a display worker does after a round that failed: counts it, tells the user once when
    /// three in a row failed with nothing received between, and waits before the next round.
    async fn display_failed(&self, broken: &mut u32, error: &Error, what: &str) -> bool {
        *broken += 1;
        if *broken == 3 {
            self.log_error(what, error);
        }
        tokio::select! {
            _=self.shutdown.cancelled()=>false,
            _=sleep(self.jitter.spread(Duration::from_secs(2)))=>true,
        }
    }
    /// Reads `/traffic` while the screen wants the speed and sing-box runs; reads nothing
    /// otherwise. A stream that breaks is opened again after a pause; three breaks in a row, with
    /// no speed received in between, are an error shown once.
    async fn traffic_worker(self: Arc<Self>) {
        let mut wanted = self.traffic_wanted.subscribe();
        let mut snapshots = self.snapshots.subscribe();
        let mut broken = 0u32;
        while self.wait_wanted(&mut wanted, &mut snapshots).await {
            let port = snapshots.borrow().settings.api_port;
            let stop = CancellationToken::new();
            let mut received = false;
            let result = match Clash::new(port, &self.secret) {
                Ok(clash) => {
                    let stream = clash.traffic(&stop, |speed| {
                        received = true;
                        self.traffic.send_replace(Some(speed));
                    });
                    self.while_wanted(&mut wanted, &mut snapshots, &stop, stream)
                        .await
                }
                Err(error) => Err(error),
            };
            self.traffic.send_replace(None);
            if received {
                broken = 0;
            }
            if let Err(error) = result {
                if !self
                    .display_failed(&mut broken, &error, t("core.application.speed_unavailable"))
                    .await
                {
                    return;
                }
            }
        }
    }
    /// Asks `/connections` every two seconds while the page wants them and sing-box runs; asks
    /// nothing otherwise. The list is kept only for the page: three failures in a row are one
    /// error.
    async fn connections_worker(self: Arc<Self>) {
        let mut wanted = self.connections_wanted.subscribe();
        let mut snapshots = self.snapshots.subscribe();
        let mut broken = 0u32;
        while self.wait_wanted(&mut wanted, &mut snapshots).await {
            let port = snapshots.borrow().settings.api_port;
            let stop = CancellationToken::new();
            let mut received = false;
            let result = match Clash::new(port, &self.secret) {
                Ok(clash) => {
                    let poll = async {
                        loop {
                            let fetched = tokio::select! {biased;
                                _=stop.cancelled()=>return Ok(()),
                                r=clash.connections()=>r?,
                            };
                            received = true;
                            let names: Vec<(String, String)> = self
                                .snapshots
                                .borrow()
                                .servers
                                .iter()
                                .map(|server| (server.id.clone(), server.name.clone()))
                                .collect();
                            self.connections.send_replace(Some(clash::view(
                                fetched,
                                &names,
                                SystemTime::now(),
                            )));
                            tokio::select! {biased;
                                _=stop.cancelled()=>return Ok(()),
                                _=sleep(CONNECTIONS_POLL)=>{},
                            }
                        }
                    };
                    self.while_wanted(&mut wanted, &mut snapshots, &stop, poll)
                        .await
                }
                Err(error) => Err(error),
            };
            self.connections.send_replace(None);
            if received {
                broken = 0;
            }
            if let Err(error) = result {
                if !self
                    .display_failed(
                        &mut broken,
                        &error,
                        t("core.application.connections_unavailable"),
                    )
                    .await
                {
                    return;
                }
            }
        }
    }
    fn cancel(&self, disconnect: bool) {
        {
            let mut c = self.control.lock().unwrap_or_else(|e| e.into_inner());
            c.disconnecting |= disconnect;
            c.token.cancel();
            c.refresh.cancel();
            c.dead.cancel();
            c.dead = CancellationToken::new();
        }
        if disconnect {
            // The next connection starts with no list of servers that did not answer.
            self.dead.lock().unwrap_or_else(|e| e.into_inner()).clear();
            self.publish_dead();
            // Disconnect/Exit also cancel the refresh requests that only wait for the active
            // batch. Other cancellations (settings, manual choice, import) stop the download in
            // flight but leave the queued batch to run. Decided here, once, instead of from the
            // `disconnecting` flag, which stays set until the next Connect.
            let pending = self
                .refresh_gate
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .pending
                .take();
            for waiter in pending.into_iter().flat_map(|batch| batch.waiters) {
                let _ = waiter.send(RefreshOutcome::Cancelled);
            }
        }
    }
    /// Asks for a full check; the worker picks it up. The first reason asked stands.
    fn request(&self, reason: Reason) {
        let _ = self
            .pending
            .compare_exchange(0, reason.code(), Ordering::SeqCst, Ordering::SeqCst);
        self.snapshots.send_modify(|s| s.pending = true);
        self.wake.notify_one();
    }
    fn clean_message(&self, message: &str) -> String {
        let secrets = self
            .journal_secrets
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        journal::clean(message, &secrets)
    }
    /// An entry of the application itself.
    fn log(&self, level: &str, message: &str) {
        self.log_in(Category::App, level, message);
    }
    fn log_in(&self, category: Category, level: &str, message: &str) {
        self.record(category, level, message, None, None);
    }
    /// An error entry: its source is the stage its code names.
    fn log_failure(&self, level: &str, message: &str, failure_code: Option<FailureCode>) {
        self.record(
            Category::of_failure(failure_code),
            level,
            message,
            failure_code,
            None,
        );
    }
    /// Records the concrete local cause, stage and parser observations alongside the brief text.
    fn log_error(&self, message: &str, error: &Error) {
        let failure_code = error.failure_code();
        self.record(
            Category::of_failure(failure_code),
            "error",
            message,
            failure_code,
            Some(&error.diagnostic_details()),
        );
    }
    /// Records a host/frontend failure locally. Details never leave the machine automatically.
    pub fn log_external_error(&self, message: &str, detail: Option<&str>) {
        self.record(Category::App, "error", message, None, detail);
    }
    /// Noninteractive scheduled failures stay in the journal without an error toast.
    pub fn log_background_error(&self, message: &str, detail: Option<&str>) {
        self.record_with_notification(Category::App, "error", message, None, detail, false);
    }
    /// Panic-hook entry point: never waits for journal, sanitizer, or snapshot locks.
    /// The live writer recovers its separate atomic record on append/export or next startup.
    pub fn log_panic(&self, message: &str, detail: &str) -> String {
        let (message, details) = match self.journal_secrets.try_lock() {
            Ok(secrets) => (
                journal::clean_message(message, &secrets),
                journal::clean_details(Some(detail), &secrets),
            ),
            Err(std::sync::TryLockError::Poisoned(error)) => {
                let secrets = error.into_inner();
                (
                    journal::clean_message(message, &secrets),
                    journal::clean_details(Some(detail), &secrets),
                )
            }
            // The panicking thread may own this mutex. Do not expose uncleaned text or wait.
            Err(std::sync::TryLockError::WouldBlock) => (
                t("core.application.internal_error").to_owned(),
                Some("[redacted]".to_owned()),
            ),
        };
        let entry = Entry {
            at: timestamp(),
            level: "error".to_owned(),
            message,
            details,
            crash_id: Some(uuid::Uuid::new_v4()),
            failure_code: None,
            category: Category::App,
        };
        let _ = journal::record_crash(&self.directory, &entry);
        notifications::error(&entry.message);
        entry.details.unwrap_or_default()
    }
    /// Called by the host's existing error-event consumer after a panic hook returns.
    /// Recovery emits no notification of its own, so a write failure cannot recurse.
    pub fn recover_local_errors(&self) -> Result<()> {
        if !journal::has_pending_crashes(&self.directory)? {
            return Ok(());
        }
        let _journal = self
            .journal_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.persist_journal()
    }
    fn record(
        &self,
        category: Category,
        level: &str,
        message: &str,
        failure_code: Option<FailureCode>,
        detail: Option<&str>,
    ) {
        self.record_with_notification(category, level, message, failure_code, detail, true);
    }
    fn record_with_notification(
        &self,
        category: Category,
        level: &str,
        message: &str,
        failure_code: Option<FailureCode>,
        detail: Option<&str>,
        notify: bool,
    ) {
        let _journal = self
            .journal_gate
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let secrets = self
            .journal_secrets
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let message = journal::clean(message, &secrets);
        let details = journal::clean_details(detail, &secrets);
        drop(secrets);
        if notify && level == "error" {
            notifications::error(&message);
        }
        let now = timestamp();
        let entry = Entry {
            at: now,
            level: level.into(),
            message,
            details,
            crash_id: None,
            failure_code,
            category,
        };
        self.snapshots.send_modify(|s| {
            s.journal.push(entry);
            journal::trim(&mut s.journal, now);
        });
        let _ = self.persist_journal();
    }
    /// Caller owns journal_gate. Snapshot and disk move together only after atomic persistence;
    /// failed crash recovery leaves its files available to the next append/export/startup.
    fn persist_journal(&self) -> Result<()> {
        let secrets = self
            .journal_secrets
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut entries = self.snapshot().journal;
        journal::persist_with_crashes(&self.directory, &mut entries, &secrets)?;
        drop(secrets);
        self.snapshots
            .send_modify(|snapshot| snapshot.journal = entries);
        Ok(())
    }
    fn publish_store(&self, store: &Store) {
        let mut secrets = journal::Secrets::from_store(store, &self.directory);
        secrets.extend([self.secret.clone()]);
        *self
            .journal_secrets
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = secrets;
        crate::text::set_language(&store.settings.language);
        self.snapshots.send_modify(|s| {
            let fresh = Snapshot::from_store(store);
            s.settings = fresh.settings;
            s.selected = fresh.selected;
            s.subscriptions = fresh.subscriptions;
            s.welcome = fresh.welcome;
            s.routing = fresh.routing;
            s.servers_sort = fresh.servers_sort;
            s.servers = fresh
                .servers
                .into_iter()
                .map(|mut v| {
                    if let Some(old) = s.servers.iter().find(|o| o.id == v.id).filter(|_| {
                        v.failure_code != Some(FailureCode::UnsupportedLegacyParameters)
                    }) {
                        v.endpoint_ms = old.endpoint_ms;
                        v.proxy_ms = old.proxy_ms;
                        v.status = old.status.clone();
                        v.checked_at = old.checked_at;
                        v.failure_code = old.failure_code;
                        v.retry_at = old.retry_at;
                    }
                    v
                })
                .collect();
        });
        *self.geo_wanted.lock().unwrap_or_else(|e| e.into_inner()) = store.geo_names();
        *self
            .subscription_urls
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = subscription_urls_of(store);
        self.publish_geodata();
    }
    /// Installs where the geosite and geoip databases are downloaded from. Without it nothing
    /// is downloaded (tests; a build without the feature).
    pub fn set_geo_source(&self, source: Arc<dyn GeoSource>) {
        *self.geo_source.lock().unwrap_or_else(|e| e.into_inner()) = Some(source);
        self.publish_geodata();
    }
    /// Checks and applies a routing profile, or leaves everything as it was.
    async fn set_routing(&self, inner: &mut Inner, routing: Routing) -> Result<()> {
        // With the databases on disk a list they do not have is a typing mistake; without them
        // it cannot be told yet.
        let unknown = self.geo.unknown_names(&routing);
        if !unknown.is_empty() {
            return Err(Error::Invalid(tr!(
                "core.application.unknown_lists",
                lists = unknown.join(", ")
            )));
        }
        if routing != inner.store.routing {
            let mut next = inner.store.clone();
            next.routing = routing;
            self.replace(inner, next, true).await?;
            self.log("info", t("core.application.routing_saved"));
        }
        // The lists it refers to may be missing: look for them at the next tick.
        self.geo_plan
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .next_due = Instant::now();
        Ok(())
    }
    fn geo_source(&self) -> Option<Arc<dyn GeoSource>> {
        self.geo_source
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Journals a change of the rules in force: `server` is the one sing-box now works through.
    /// Counts only; the rules themselves never reach the journal.
    fn routes_in_force(&self, server: Option<&Server>) {
        let mode = server.map_or_else(|| NO_ROUTES_MODE.to_owned(), Server::routes_mode);
        {
            let mut live = self.live_routes.lock().unwrap_or_else(|e| e.into_inner());
            if *live == mode {
                return;
            }
            *live = mode;
        }
        match server.and_then(Server::own_routes) {
            Some(routes) => self.log_in(
                Category::Core,
                "info",
                &tr!(
                    "core.application.server_rules_on",
                    direct = routes.direct.len(),
                    block = routes.block.len()
                ),
            ),
            None => self.log_in(
                Category::Core,
                "info",
                t("core.application.server_rules_removed"),
            ),
        }
    }
    /// sing-box is gone: the next one starts with the rules of its server, not these.
    fn routes_forgotten(&self) {
        *self.live_routes.lock().unwrap_or_else(|e| e.into_inner()) = NO_ROUTES_MODE.to_owned();
    }
    fn geo_wanted(&self) -> (Vec<String>, Vec<String>) {
        self.geo_wanted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// The databases as the settings show them.
    fn publish_geodata(&self) {
        let available = self.geo_source().is_some();
        let wanted = self.geo_wanted();
        let view = {
            let plan = self.geo_plan.lock().unwrap_or_else(|e| e.into_inner());
            geodata_view(&self.geo, &wanted, available, &plan)
        };
        self.snapshots.send_if_modified(|snapshot| {
            let changed = snapshot.geodata != view;
            snapshot.geodata = view;
            changed
        });
    }
    /// Downloads the databases the routing profile refers to (`kinds: None` with a manual
    /// request: both) and puts them to work: a database is checked with the config it makes
    /// (`sing-box check`), the connection is restarted with it, and a database the check
    /// rejects is taken back. Without a source, nothing happens.
    pub async fn update_geodata(&self, only: Option<Vec<geodata::Kind>>) -> Result<()> {
        let manual = only.is_none();
        let Some(source) = self.geo_source() else {
            return if manual {
                Err(Error::Invalid(
                    t("core.application.geodata_unavailable").into(),
                ))
            } else {
                Ok(())
            };
        };
        {
            let mut plan = self.geo_plan.lock().unwrap_or_else(|e| e.into_inner());
            if plan.updating {
                return Ok(());
            }
            plan.updating = true;
        }
        self.publish_geodata();
        let kinds = only.unwrap_or_else(|| geodata::Kind::ALL.to_vec());
        let result = self.fetch_geodata(source.as_ref(), &kinds).await;
        {
            let mut plan = self.geo_plan.lock().unwrap_or_else(|e| e.into_inner());
            plan.updating = false;
            plan.current = None;
            let now = Instant::now();
            match &result {
                Ok(()) => {
                    plan.failures = 0;
                    plan.failure = None;
                    // The next look decides by the time of the last confirmation what is due.
                    plan.next_due = now + self.jitter.spread(Duration::from_secs(60));
                }
                Err(Error::Cancelled) => plan.next_due = now,
                Err(error) => {
                    plan.failures = plan.failures.saturating_add(1);
                    plan.failure = error.failure_code();
                    // Five minutes, doubling, at most six hours: a base that cannot be reached
                    // is not asked for every tick.
                    let wait = Duration::from_secs(300)
                        .saturating_mul(1 << (plan.failures - 1).min(6))
                        .min(Duration::from_secs(6 * 3600));
                    plan.next_due = now + self.jitter.spread(wait);
                }
            }
        }
        self.publish_geodata();
        result
    }
    async fn fetch_geodata(&self, source: &dyn GeoSource, kinds: &[geodata::Kind]) -> Result<()> {
        let (tunnel, port) = {
            let inner = self.inner.lock().await;
            (
                inner.connected && inner.store.settings.mode != Mode::Tun,
                inner.store.settings.proxy_port,
            )
        };
        let mut installed: Vec<(geodata::Kind, Option<Vec<u8>>)> = Vec::new();
        let mut failure = None;
        for kind in kinds {
            self.geo_plan
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .current = Some(*kind);
            self.publish_geodata();
            // What is installed decides whether there is anything to download: the source
            // answers `Same` from its checksum file, without the database.
            let have = self.geo.digest(*kind);
            // Direct first; through the tunnel when connected and that failed (as subscriptions).
            let mut fetched = tokio::select! { biased;
                _ = self.shutdown.cancelled() => return Err(Error::Cancelled),
                result = source.fetch(*kind, None, have.as_deref()) => result,
            };
            if fetched.is_err() && tunnel {
                fetched = tokio::select! { biased;
                    _ = self.shutdown.cancelled() => return Err(Error::Cancelled),
                    result = source.fetch(*kind, Some(port), have.as_deref()) => result,
                };
            }
            match fetched {
                Ok(geodata::Fetched::Same) => {
                    self.confirm_geodata(*kind);
                    self.log(
                        "info",
                        &tr!("core.application.geo_latest", kind = kind.name()),
                    );
                }
                Ok(geodata::Fetched::Latest(bytes)) => {
                    let previous = std::fs::read(self.geo.path(*kind)).ok();
                    match self.geo.install(*kind, &bytes) {
                        Ok(()) => {
                            self.confirm_geodata(*kind);
                            self.log(
                                "info",
                                &tr!("core.application.geo_new_version", kind = kind.name()),
                            );
                            installed.push((*kind, previous));
                        }
                        Err(error) => {
                            self.say_geodata_failed(*kind, &error);
                            failure = failure.or(Some(error));
                        }
                    }
                }
                Err(error) => {
                    self.say_geodata_failed(*kind, &error);
                    failure = failure.or(Some(error));
                }
            }
        }
        self.geo_plan
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current = None;
        if !installed.is_empty() {
            self.apply_geodata(installed).await?;
        }
        failure.map_or(Ok(()), Err)
    }
    /// The source says the database on disk is the latest one, as of now.
    fn confirm_geodata(&self, kind: geodata::Kind) {
        let now = timestamp();
        self.geo_plan
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .checked
            .insert(kind, now);
        // Kept beside the file, so that a restart does not forget it; a failed write costs one
        // more look after the next start, nothing else.
        let _ = self.geo.set_checked_at(kind, now);
    }
    /// Which database failed (the error that follows names only the stage).
    fn say_geodata_failed(&self, kind: geodata::Kind, error: &Error) {
        let code = error.failure_code().map_or("unknown", FailureCode::as_str);
        self.log(
            "warn",
            &tr!(
                "core.application.geo_not_loaded",
                kind = kind.name(),
                code = code
            ),
        );
    }
    /// Checks the config the new databases make and restarts with it; takes them back if the
    /// check refuses.
    async fn apply_geodata(&self, installed: Vec<(geodata::Kind, Option<Vec<u8>>)>) -> Result<()> {
        let mut inner = self.inner.lock().await;
        let store = inner.store.clone();
        let uses_geo = {
            let (sites, ips) = store.geo_names();
            !sites.is_empty() || !ips.is_empty()
        };
        let applied = if uses_geo {
            // The servers are the same and so is the one in use: the sidecar restarts to take the lists
            // in, and nothing about the servers is to be found out again (the screen would show them
            // all as just checked).
            self.replace_with(&mut inner, store, true, true, true)
                .await
                .map(|_| ())
        } else {
            Ok(())
        };
        match applied {
            Ok(()) => {
                self.log("info", t("core.application.geodata_updated"));
                self.publish_store(&inner.store);
                Ok(())
            }
            Err(error) => {
                for (kind, previous) in installed {
                    match previous {
                        Some(bytes) => {
                            let _ = self.geo.install(kind, &bytes);
                        }
                        None => {
                            let _ = std::fs::remove_file(self.geo.path(kind));
                        }
                    }
                }
                Err(error)
            }
        }
    }
    /// Once per tick: looks for databases that are missing or old, and downloads them.
    /// `auto_update`: the setting. Off, only a database that the rules need and that is missing is
    /// fetched; on, every database on disk is asked about every twelve hours, counted from the
    /// last time the source confirmed it (not from the age of the file: a database that does not
    /// change would be asked about at every look).
    fn geo_tick(self: &Arc<Self>, auto_update: bool) {
        if self.geo_source().is_none() {
            return;
        }
        let checked = {
            let plan = self.geo_plan.lock().unwrap_or_else(|e| e.into_inner());
            if plan.updating || Instant::now() < plan.next_due {
                return;
            }
            plan.checked.clone()
        };
        let (sites, ips) = self.geo_wanted();
        let now = timestamp();
        let every = GEODATA_CHECK_EVERY.as_secs();
        // The next look is when the first database comes due, but not later than in an hour (the
        // rules may start to need one by then) and not sooner than in a minute.
        let mut next_look = 3600;
        let mut due: Vec<geodata::Kind> = vec![];
        for (kind, wanted) in [
            (geodata::Kind::Geosite, !sites.is_empty()),
            (geodata::Kind::Geoip, !ips.is_empty()),
        ] {
            match self.geo.info(kind) {
                None if wanted => due.push(kind),
                None => {}
                Some(_) if !auto_update => {}
                Some(info) => {
                    let last = checked.get(&kind).copied().unwrap_or(info.updated_at);
                    let age = now.saturating_sub(last);
                    if age >= every {
                        due.push(kind);
                    } else {
                        next_look = next_look.min((every - age).max(60));
                    }
                }
            }
        }
        if due.is_empty() {
            self.geo_plan
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .next_due = Instant::now() + self.jitter.spread(Duration::from_secs(next_look));
            return;
        }
        let app = self.clone();
        tokio::spawn(async move {
            if let Err(error) = app.update_geodata(Some(due)).await {
                if !matches!(error, Error::Cancelled) {
                    app.log_error(&error.to_string(), &error);
                }
            }
        });
    }
    fn persist(&self, store: &Store) -> Result<()> {
        persistence::save(&self.directory.join("state.json"), store)
            .map_err(|error| error.with_code(FailureCode::Persistence))
    }
    pub async fn execute(&self, action: Action) -> Result<Snapshot> {
        if matches!(action, Action::CheckNow) {
            if !self.snapshot().runtime_active {
                return Err(Error::Invalid(t("core.application.connect_first").into()));
            }
            // Asked for by hand, so it looks at every server: the full comparison of an Initial
            // cycle, not the quiet stop at the first working one.
            self.check_all.store(true, Ordering::SeqCst);
            self.request(Reason::CheckNow);
            return Ok(self.snapshot());
        }
        if matches!(action, Action::Disconnect) {
            self.cancel(true);
        }
        if matches!(
            action,
            Action::Select { .. } | Action::SetAuto { enabled: false }
        ) {
            self.cancel(false);
        }
        // The order of the list is only how it is shown: it is seen at once, though the action
        // itself waits for a running check to let go of the coordinator before it is kept.
        if let Action::SetServersSort { column, descending } = &action {
            let sort = ServersSort {
                column: *column,
                descending: *descending,
            };
            self.snapshots.send_modify(|s| s.servers_sort = sort);
        }
        // SaveSettings interrupts running work only when the config changes (not theme/name-only).
        // The comparison must not wait for the coordinator lock: a running cycle holds it, and
        // the cancellation is what lets that cycle end.
        if let Action::SaveSettings { settings } = &action {
            if config::config_inputs(settings) != config::config_inputs(&self.snapshot().settings) {
                self.cancel(false);
            }
        }
        // A routing profile that differs from the running one interrupts what runs, like a
        // change of the config: it is about to restart the sidecar.
        if let Action::SaveRouting {
            order,
            block,
            direct,
            proxy,
        } = &action
        {
            if Routing::from_text(*order, block, direct, proxy)
                .is_ok_and(|next| next != self.snapshot().routing)
            {
                self.cancel(false);
            }
        }
        if matches!(action, Action::ResetRouting) && self.snapshot().routing != Routing::default() {
            self.cancel(false);
        }
        // Refresh enters its single-flight gate before the coordinator lock. This keeps a
        // burst of UI/worker requests from becoming serialized network jobs in `apply()`.
        let result = match action {
            Action::Refresh { id } => self.request_refresh(id, true, true).await,
            // A download takes long: it must not hold the coordinator.
            Action::UpdateGeodata => self.update_geodata(None).await,
            action => self.apply(action).await,
        };
        if let Err(e) = &result {
            self.record_failure(e, false).await;
        }
        result.map(|_| self.snapshot())
    }
    async fn apply(&self, action: Action) -> Result<()> {
        let mut inner = self.inner.lock().await;
        match action {
            Action::Connect => {
                if inner.connected {
                    return Ok(());
                }
                if inner.store.available_servers().is_empty() {
                    return Err(Error::Unavailable);
                }
                let live = inner.store.live_selection();
                if let Some(error) = inner
                    .store
                    .available_iter()
                    .find(|s| s.id == live)
                    .and_then(Server::legacy_refusal)
                    .or_else(|| {
                        live.is_empty()
                            .then(|| {
                                inner
                                    .store
                                    .available_iter()
                                    .find_map(Server::legacy_refusal)
                            })
                            .flatten()
                    })
                {
                    return Err(error);
                }
                {
                    let mut c = self.control.lock().unwrap_or_else(|e| e.into_inner());
                    c.disconnecting = false;
                    c.token = CancellationToken::new();
                }
                self.dead.lock().unwrap_or_else(|e| e.into_inner()).clear();
                self.publish_dead();
                self.snapshots.send_modify(|s| {
                    s.status = "connecting".into();
                    s.error = None;
                });
                let mut store = inner.store.clone();

                let mut started = inner
                    .runtime
                    .start(&store, &self.secret)
                    .await
                    .map_err(|error| error.with_code(FailureCode::SidecarStartExit));
                if matches!(&started, Err(e) if is_declined(e)) {
                    // The user would not allow TUN: connect the way that needs no rights.
                    let mut next = inner.store.clone();
                    next.settings.mode = Mode::Proxy;
                    self.persist(&next)?;
                    inner.store = next;
                    self.publish_store(&inner.store);
                    self.declined_notice();
                    store = inner.store.clone();
                    started = inner
                        .runtime
                        .start(&store, &self.secret)
                        .await
                        .map_err(|error| error.with_code(FailureCode::SidecarStartExit));
                }
                if let Err(e) = started {
                    self.snapshots.send_modify(|s| s.status = "error".into());
                    return Err(e);
                }
                if !store.settings.auto_select {
                    // A started child with an unselected/unsaved server is not a connection:
                    // stop it so the next Connect retries from scratch.
                    let selected = store.live_selection();
                    let mut next = store.clone();
                    next.selected = Some(selected.clone());
                    let applied = match switch_live(&*inner.probe, &store, &selected).await {
                        Ok(()) => self.persist(&next),
                        Err(e) => Err(e),
                    };
                    if let Err(e) = applied {
                        let _ = inner.runtime.stop().await;
                        self.snapshots.send_modify(|s| {
                            s.status = "error".into();
                            s.runtime_active = false;
                        });
                        return Err(e);
                    }
                    inner.store = next;
                }
                inner.connected = true;
                inner.failover = Failover::new();
                self.snapshots.send_modify(|s| s.runtime_active = true);
                self.routes_forgotten();
                self.routes_in_force(live_server(&inner.store).as_ref());
                inner.schedule = Schedule::new(
                    Duration::from_secs(store.settings.interval_secs),
                    Duration::from_secs(store.settings.full_interval_mins * 60),
                    self.jitter.clone(),
                );
                self.forget_pool(&mut inner);
                if store.settings.auto_select {
                    self.request(Reason::Connect);
                } else {
                    self.snapshots
                        .send_modify(|s| s.status = "connected".into());
                }
                self.log_in(
                    Category::Core,
                    "info",
                    t("core.application.singbox_started"),
                );
            }
            Action::Disconnect => {
                self.stop_locked(&mut inner).await?;
                self.log_in(Category::Core, "info", t("core.application.disconnected"));
            }
            Action::Select { id } => {
                if !inner.store.available_servers().iter().any(|s| s.id == id) {
                    return Err(Error::Invalid(
                        t("core.application.server_unavailable").into(),
                    ));
                }
                if let Some(error) = inner
                    .store
                    .available_iter()
                    .find(|s| s.id == id)
                    .and_then(Server::legacy_refusal)
                {
                    return Err(error);
                }
                // The choice does not turn Auto Select off: the server becomes the one in use like any
                // other, and the checks that follow compare it with the rest.
                let mut next = inner.store.clone();
                next.selected = Some(id.clone());
                if inner.connected {
                    if let Err(error) = switch_live(&*inner.probe, &next, &id).await {
                        // The selector may have moved before the rules failed: back to the saved
                        // choice, as far as that works.
                        let previous = inner.store.live_selection();
                        let _ = switch_live(&*inner.probe, &inner.store, &previous).await;
                        return Err(error);
                    }
                }
                if let Err(error) = self.persist(&next) {
                    // The live selector already moved; put traffic back on the saved choice.
                    if inner.connected {
                        let previous = inner.store.live_selection();
                        if switch_live(&*inner.probe, &inner.store, &previous)
                            .await
                            .is_err()
                        {
                            self.snapshots.send_modify(|s| s.status = "error".into());
                            return Err(Error::Invalid(
                                t("core.application.selection_not_saved").into(),
                            ));
                        }
                    }
                    return Err(error);
                }
                if inner.connected {
                    inner.schedule.switched(Instant::now());
                    self.routes_in_force(live_server(&next).as_ref());
                }
                inner.store = next;
                self.failover_reset(&mut inner);
                // What the checks found about the server that was in use is about another one now.
                inner.urgent = None;
                inner.hints = Hints::default();
                let auto = inner.store.settings.auto_select;
                if auto {
                    self.log_in(
                        Category::Checks,
                        "info",
                        t("core.application.manual_choice_compare"),
                    );
                } else {
                    self.pending.store(0, Ordering::SeqCst);
                    inner.schedule.pending = None;
                    self.snapshots.send_modify(|s| {
                        s.pending = false;
                        s.next_check = None;
                    });
                    self.log_in(
                        Category::Checks,
                        "info",
                        t("core.application.manual_choice"),
                    );
                }
            }
            Action::SetAuto { enabled } => {
                let mut next = inner.store.clone();
                next.settings.auto_select = enabled;
                self.persist(&next)?;
                inner.store = next;
                self.auto_select_changed(&mut inner, enabled);
            }
            Action::AddServer { uri } => {
                let server = parse_uri(&uri, None)?;
                let mut next = inner.store.clone();
                if !next
                    .servers
                    .iter()
                    .any(|s| s.id == server.id && s.source.is_none())
                {
                    next.servers.push(server);
                    self.replace(&mut inner, next, true).await?;
                }
                self.log("info", t("core.application.server_added"));
            }
            Action::AddServers { uris } => {
                if uris.is_empty() || uris.len() > link::MAX_LINKS {
                    return Err(Error::Invalid(t("core.application.links_limit").into()));
                }
                // All or none: one unreadable link refuses the whole batch, like a bad profile.
                let servers = uris
                    .iter()
                    .map(|uri| parse_uri(uri, None))
                    .collect::<Result<Vec<_>>>()?;
                let mut next = inner.store.clone();
                let mut added = 0;
                for server in servers {
                    if !next
                        .servers
                        .iter()
                        .any(|s| s.id == server.id && s.source.is_none())
                    {
                        next.servers.push(server);
                        added += 1;
                    }
                }
                // The portable profile carries at most this many servers.
                if next.servers.len() > profile::MAX_SERVERS {
                    return Err(Error::Invalid(
                        t("core.application.too_many_servers").into(),
                    ));
                }
                if added > 0 {
                    self.replace(&mut inner, next, true).await?;
                }
                self.log(
                    "info",
                    &tr!("core.application.servers_added", added = added),
                );
            }
            Action::RemoveServer { id } => {
                let mut next = inner.store.clone();
                next.servers.retain(|s| !(s.id == id && s.source.is_none()));
                self.replace(&mut inner, next, true).await?;
            }
            Action::SaveSubscription {
                id,
                url,
                interval_mins,
                user_agent,
                filters,
                provider_routes,
            } => {
                subscription::validate_url(&url)?;
                let user_agent = user_agent
                    .map(|agent| agent.trim().to_owned())
                    .filter(|agent| !agent.is_empty());
                let filters = filters.normalized();
                // The portable profile carries at most this many; refusing more here keeps
                // every store the application accepts exportable.
                if id.is_none() && inner.store.subscriptions.len() >= profile::MAX_SUBSCRIPTIONS {
                    return Err(Error::Invalid(
                        t("core.application.too_many_subscriptions").into(),
                    ));
                }
                let mut next = inner.store.clone();
                let touched_id = if let Some(id) = &id {
                    let s = next
                        .subscriptions
                        .iter_mut()
                        .find(|s| &s.id == id)
                        .ok_or_else(|| {
                            Error::Invalid(t("core.application.subscription_not_found").into())
                        })?;
                    s.url = url;
                    s.interval_mins = interval_mins;
                    s.user_agent = user_agent;
                    s.filters = filters;
                    s.provider_routes = provider_routes;
                    s.validate_options()?;
                    id.clone()
                } else {
                    let new_id = uuid::Uuid::new_v4().to_string();
                    let fresh = Subscription {
                        id: new_id.clone(),
                        // Replaced by the provider's title at the first refresh.
                        name: subscription::host_name(&url),
                        url,
                        interval_mins,
                        user_agent,
                        filters,
                        provider_routes,
                        ..Default::default()
                    };
                    fresh.validate_options()?;
                    next.subscriptions.push(fresh);
                    new_id
                };
                // Without the switch the servers of this subscription bring no rules, now: not only
                // after the next refresh, which may fail.
                if !provider_routes {
                    for server in next
                        .servers
                        .iter_mut()
                        .filter(|server| server.source.as_deref() == Some(touched_id.as_str()))
                    {
                        server.routes = None;
                    }
                }
                self.replace(&mut inner, next, true).await?;
                // A saved subscription is due on the very next
                // tick, not whatever backoff its previous attempts accumulated.
                inner.subscription_backoff.remove(&touched_id);
                inner.subscription_hold_until = Instant::now();
                self.wake.notify_one();
            }
            Action::RemoveSubscription { id } => {
                let mut next = inner.store.clone();
                next.subscriptions.retain(|s| s.id != id);
                next.servers.retain(|s| s.source.as_ref() != Some(&id));
                self.replace(&mut inner, next, true).await?;
            }
            Action::Refresh { .. } => unreachable!("refresh is handled before apply"),
            Action::UpdateGeodata => unreachable!("the download is handled before apply"),
            Action::SaveRouting {
                order,
                block,
                direct,
                proxy,
            } => {
                let routing = Routing::from_text(order, &block, &direct, &proxy)?;
                self.set_routing(&mut inner, routing).await?;
            }
            Action::ResetRouting => {
                self.set_routing(&mut inner, Routing::default()).await?;
            }
            Action::SaveSettings { settings } => {
                settings.validate()?;
                let was_auto = inner.store.settings.auto_select;
                let mut next = inner.store.clone();
                next.settings = settings;
                let previous = inner.store.settings.clone();
                // The registry first: a failed registration leaves the settings as they were.
                let autostart_changed = next.settings.autostart != previous.autostart;
                if autostart_changed {
                    self.autostart()
                        .set(next.settings.autostart)
                        .map_err(|error| error.with_code(FailureCode::Autostart))?;
                }
                if let Err(error) = self.replace(&mut inner, next, true).await {
                    if autostart_changed {
                        // The settings were not stored: put the registration back.
                        let _ = self.autostart().set(previous.autostart);
                    }
                    return Err(error);
                }
                let current = &inner.store.settings;
                // Rebuild the HTTP probe only when a setting it reads changed.
                if probe_inputs(current) != probe_inputs(&previous) {
                    inner.probe = Arc::new(
                        NetworkProbe::new(current.clone(), self.secret.clone())?
                            .with_channel(inner.runtime.proxy_channel()),
                    );
                }
                let interval = Duration::from_secs(inner.store.settings.interval_secs);
                let full_interval =
                    Duration::from_secs(inner.store.settings.full_interval_mins * 60);
                inner.schedule.set_intervals(interval, full_interval);
                // Same Auto Select semantics as SetAuto, even when the config is unchanged.
                let enabled = inner.store.settings.auto_select && !was_auto;
                self.auto_select_changed(&mut inner, enabled);
            }
            Action::ClearJournal => {
                let _journal = self
                    .journal_gate
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                self.persist_journal()
                    .map_err(|error| error.with_code(FailureCode::Persistence))?;
                // Persist first: a failed clear must not look successful and then reappear.
                journal::clear(&self.directory)
                    .map_err(|error| error.with_code(FailureCode::Persistence))?;
                self.snapshots.send_modify(|s| s.journal.clear());
            }
            Action::ExportDiagnostics => {
                {
                    let _journal = self
                        .journal_gate
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    self.persist_journal()
                        .map_err(|error| error.with_code(FailureCode::Persistence))?;
                }
                // With the lists of the databases in the data folder, as the sidecar runs it.
                let geo = self.geo.resolve_store(&inner.store);
                let mut config = config::generate_with(&inner.store, &self.secret, &geo)
                    .ok()
                    .map(|c| config::sanitized(&c));
                let mut state = self.snapshot();
                let secrets = self
                    .journal_secrets
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                // Clean externally supplied strings, not the closed runtime observations.
                // A password may itself be "timeout" or "google"; that must not erase facts.
                if let Some(config) = &mut config {
                    journal::clean_value(config, &secrets);
                }
                let server_ids: HashSet<&str> = inner
                    .store
                    .servers
                    .iter()
                    .filter(|server| server.id == Server::fingerprint(&server.outbound))
                    .map(|server| server.id.as_str())
                    .collect();
                let server_id = |id: &str| {
                    if server_ids.contains(id) {
                        id.to_owned()
                    } else {
                        journal::clean(id, &secrets)
                    }
                };
                let selected = state.selected.as_deref().map(&server_id);
                for entry in &mut state.journal {
                    entry.level = journal::clean(&entry.level, &secrets);
                    entry.message = journal::clean_message(&entry.message, &secrets);
                    entry.details = journal::clean_details(entry.details.as_deref(), &secrets);
                }
                let database = |db: Option<DbView>| {
                    db.map(|db| json!({"bytes":db.bytes,"updated_at":db.updated_at,"checked_at":db.checked_at}))
                };
                let geodata = json!({
                    "geosite": database(state.geodata.geosite),
                    "geoip": database(state.geodata.geoip),
                    "uses_geosite": state.geodata.uses_geosite,
                    "uses_geoip": state.geodata.uses_geoip,
                    "failure_code": state.geodata.failure_code,
                });
                let failures = json!({
                    "servers": failure_counts(state.servers.iter().map(|server| server.failure_code)),
                    "subscriptions": failure_counts(state.subscriptions.iter().map(|source| source.failure_code)),
                    "journal": failure_counts(state.journal.iter().map(|entry| entry.failure_code)),
                });
                // Allowlist: names and configurable URLs are user-controlled and might contain secrets.
                // How a server is addressed (never the address) and the last thing sing-box said
                // about it: the two facts that tell "the name could not be found" from "the server
                // did not answer".
                let hosts: HashMap<&str, &str> = inner
                    .store
                    .servers
                    .iter()
                    .map(|s| {
                        let kind = if s.host.parse::<std::net::IpAddr>().is_ok() {
                            "ip"
                        } else {
                            "name"
                        };
                        (s.id.as_str(), kind)
                    })
                    .collect();
                let log = inner.runtime.log_summary();
                let servers: Vec<_> = state
                    .servers
                    .iter()
                    .map(|s| {
                        let protocol = match s.protocol.as_str() {
                            "vless" | "vmess" | "trojan" | "shadowsocks" => s.protocol.clone(),
                            protocol => journal::clean(protocol, &secrets),
                        };
                        json!({"id":server_id(&s.id),"protocol":protocol,"address":hosts.get(s.id.as_str()),"status":s.status,"endpoint_ms":s.endpoint_ms,"proxy_ms":s.proxy_ms,"failure_code":s.failure_code,"sing_box_error":log.as_ref().and_then(|log| log.servers.get(&s.id))})
                    })
                    .collect();
                let sing_box_log = log
                    .as_ref()
                    .map(|log| json!({"lines":log.lines,"groups":log.groups}));
                // Concrete cleaned causes from the last failed refresh, never response bodies.
                let last_failures: Vec<_> = state
                    .subscriptions
                    .iter()
                    .filter_map(|source| {
                        let details = inner.subscription_failures.get(&source.id)?;
                        Some(json!({"id":journal::clean(&source.id, &secrets),"failure":journal::clean_details(Some(details), &secrets)}))
                    })
                    .collect();
                let xray_log = inner
                    .runtime
                    .xray_log_summary()
                    .map(|log| json!({"lines":log.lines,"groups":log.groups}));
                let subscription_parameters: Vec<_> = inner.subscription_rows.iter().map(|(id, rows)| {
                    json!({"id":journal::clean(id, &secrets),"observations":crate::diagnostics::subscription_json(&crate::diagnostics::SubscriptionInfo {
                        rows:Some(*rows), ..Default::default()
                    })})
                }).collect();
                let report = json!({"rift_version":env!("CARGO_PKG_VERSION"),"os":os_version(),"sing_box_version":"1.14.1","settings":{"mode":state.settings.mode,"auto_select":state.settings.auto_select,"proxy_port":state.settings.proxy_port,"interval_secs":state.settings.interval_secs},"selected":selected,"servers":servers,"failures":failures,"subscription_failures":last_failures,"geodata":geodata,"dns":inner.runtime.dns_stats(),"sing_box_log":sing_box_log,"xray_log":xray_log,"xray_version":"26.10.21","runtime":inner.runtime.diagnostics(),"subscription_parameters":subscription_parameters,"journal":state.journal,"config":config});
                drop(secrets);
                persistence::atomic_write(
                    &self.directory.join("diagnostics.json"),
                    &serde_json::to_vec_pretty(&report)?,
                )
                .map_err(|error| error.with_code(FailureCode::Persistence))?;
                self.log("info", t("core.application.diagnostics_saved"));
            }
            Action::DismissWelcome => {
                if !inner.store.welcome_dismissed {
                    let mut next = inner.store.clone();
                    next.welcome_dismissed = true;
                    self.persist(&next)?;
                    inner.store = next;
                }
            }
            Action::SetServersSort { column, descending } => {
                let sort = ServersSort { column, descending };
                if inner.store.servers_sort != sort {
                    let mut next = inner.store.clone();
                    next.servers_sort = sort;
                    self.persist(&next)?;
                    inner.store = next;
                }
            }
            Action::SetServerExcluded { id, excluded } => {
                // The id is the outbound, the key is the row: one link in two subscriptions (or by
                // hand and in a subscription) is one id and two keys. Auto Select takes one row for
                // an outbound, so the choice covers every row that shares the id, whichever of them
                // was clicked.
                let keys: Vec<String> = inner
                    .store
                    .servers
                    .iter()
                    .filter(|s| s.id == id)
                    .map(Server::stable_key)
                    .collect();
                if keys.is_empty() {
                    return Err(Error::Invalid(
                        t("core.application.server_unavailable").into(),
                    ));
                }
                if keys
                    .iter()
                    .any(|key| inner.store.excluded.contains(key) != excluded)
                {
                    let mut next = inner.store.clone();
                    if excluded {
                        for key in keys {
                            if !next.excluded.contains(&key) {
                                next.excluded.push(key);
                            }
                        }
                    } else {
                        next.excluded.retain(|k| !keys.contains(k));
                    }
                    self.persist(&next)?;
                    inner.store = next;
                    // The pool loses the server and is made up again without asking anyone; a
                    // server returned waits for the next full check.
                    let size = inner.store.settings.pool_size;
                    let banned = inner.store.excluded.clone();
                    inner.pool.without(&banned, size);
                    self.publish_store(&inner.store);
                    self.log_in(
                        Category::Core,
                        "info",
                        if excluded {
                            t("core.application.server_excluded")
                        } else {
                            t("core.application.server_returned")
                        },
                    );
                }
            }
            Action::CheckNow => {}
        }
        self.publish_store(&inner.store);
        Ok(())
    }
    async fn stop_locked(&self, inner: &mut Inner) -> Result<()> {
        inner
            .runtime
            .stop()
            .await
            .map_err(|error| error.with_code(FailureCode::SidecarStop))?;
        inner.connected = false;
        self.routes_forgotten();
        inner.failover = Failover::new();
        self.snapshots.send_modify(|s| s.runtime_active = false);
        self.pending.store(0, Ordering::SeqCst);
        inner.schedule.pending = None;
        self.forget_pool(inner);
        self.snapshots.send_modify(|s| {
            s.status = "disconnected".into();
            s.checking = false;
            s.pending = false;
            s.next_check = None;
        });
        Ok(())
    }
    /// `start_initial`: Auto Select just turned on. Turning it off drops queued cycles.
    fn auto_select_changed(&self, inner: &mut Inner, start_initial: bool) {
        if start_initial && inner.connected {
            self.request(Reason::AutoSelectOn);
        }
        if !inner.store.settings.auto_select {
            inner.schedule.pending = None;
            self.snapshots.send_modify(|s| {
                s.pending = false;
                s.next_check = None;
            });
            self.pending.store(0, Ordering::SeqCst);
        }
    }
    /// A new connection, a new sidecar or a disconnect: nothing the last checks learned holds.
    fn forget_pool(&self, inner: &mut Inner) {
        inner.pool = PoolState::default();
        inner.hints = Hints::default();
        inner.urgent = None;
        inner.outage = false;
    }
    /// A token that actions cancel, for work done under the coordinator lock; `None` while
    /// Disconnect or Exit is under way.
    fn work_token(&self) -> Option<CancellationToken> {
        let mut c = self.control.lock().unwrap_or_else(|e| e.into_inner());
        if c.disconnecting {
            return None;
        }
        c.token = CancellationToken::new();
        Some(c.token.clone())
    }
    /// Forgets what the light checks learned: another server or another connection starts over.
    fn failover_reset(&self, inner: &mut Inner) {
        if inner.failover.lost {
            self.snapshots.send_modify(|s| {
                if s.status == "lost" {
                    s.status = "connected".into();
                }
            });
        }
        inner.failover = Failover::new();
    }
    /// The light check: one delay probe of the selected server, every 15 s ± 10 s while
    /// connected. A missed answer is asked again soon; the second in a row is a loss. Returns
    /// whether a cycle must start now (Auto Select) instead of waiting for its time. With a manual
    /// choice nothing is switched: the status and the journal say the connection is lost.
    async fn failover_tick(&self, inner: &mut Inner) -> bool {
        let now = Instant::now();
        let due = *inner
            .failover
            .next_at
            .get_or_insert_with(|| now + self.jitter.spread(FAILOVER_INTERVAL));
        if now < due {
            return false;
        }
        let server = inner.store.selected.clone().and_then(|id| {
            inner
                .store
                .available_servers()
                .into_iter()
                .find(|s| s.id == id)
        });
        let Some(server) = server else {
            inner.failover.next_at = Some(now + self.jitter.spread(FAILOVER_INTERVAL));
            return false;
        };
        let Some(token) = self.work_token() else {
            return false;
        };
        let answer = tokio::select! {biased;
            _=token.cancelled()=>return false,
            result=inner.probe.proxy(&server)=>result,
        };
        let now = Instant::now();
        if answer.is_ok() {
            inner.failover.failures = 0;
            inner.failover.next_at = Some(now + self.jitter.spread(FAILOVER_INTERVAL));
            if inner.failover.lost {
                inner.failover.lost = false;
                self.snapshots.send_modify(|s| {
                    if s.status == "lost" {
                        s.status = "connected".into();
                    }
                });
                self.log_in(
                    Category::Checks,
                    "info",
                    t("core.application.connection_restored"),
                );
            }
            return false;
        }
        inner.failover.failures += 1;
        if inner.failover.failures < FAILOVER_FAILURES {
            inner.failover.next_at = Some(now + self.jitter.spread(FAILOVER_RETRY));
            return false;
        }
        inner.failover.next_at = Some(now + self.jitter.spread(FAILOVER_INTERVAL));
        if inner.store.settings.auto_select {
            if inner.failover.held_until.is_some_and(|until| now < until) {
                return false;
            }
            inner.failover.failures = 0;
            self.record(
                Category::Checks,
                "warn",
                t("core.application.connection_lost_searching"),
                Some(FailureCode::ConnectionLost),
                None,
            );
            return true;
        }
        if !inner.failover.lost {
            inner.failover.lost = true;
            self.snapshots.send_modify(|s| s.status = "lost".into());
            self.record(
                Category::Checks,
                "warn",
                t("core.application.connection_lost_selected"),
                Some(FailureCode::ConnectionLost),
                None,
            );
        }
        false
    }
    /// Publishes an exit of the owned sidecar. A crash needs an explicit Connect, so every
    /// path that could restart the runtime checks this first.
    async fn detect_crash(&self, inner: &mut Inner) -> bool {
        if !inner.connected {
            return false;
        }
        let code = match inner.runtime.alive() {
            Ok(true) => return false,
            // A TUN run through the helper can lose either an engine or the helper itself.
            Ok(false) => inner.runtime.crash_code(),
            Err(_) => FailureCode::Unknown,
        };

        // Reap both owned children before publishing their final exit observations. The
        // runtime preserves the primary cause and marks a killed peer as sibling_failure.
        let _ = inner.runtime.stop().await;
        let (shown, journal) = if code == FailureCode::XrayCrashed {
            (
                t("core.common.xray_exited_unexpectedly"),
                t("core.application.xray_exit_noticed"),
            )
        } else if code == FailureCode::ElevationHelperCrashed {
            (
                t("core.application.helper_exited_unexpectedly"),
                t("core.application.helper_exit_noticed"),
            )
        } else if code == FailureCode::Unknown {
            (
                t("core.common.cores_state_check_failed"),
                t("core.application.cores_state_error"),
            )
        } else {
            (
                t("core.application.singbox_exited_unexpectedly"),
                t("core.application.singbox_exit_noticed"),
            )
        };
        inner.connected = false;
        self.routes_forgotten();
        inner.schedule.pending = None;
        self.pending.store(0, Ordering::SeqCst);
        self.snapshots.send_modify(|s| {
            s.runtime_active = false;
            s.status = "error".into();
            s.error = Some(shown.into());
            s.pending = false;
            s.next_check = None;
        });
        self.forget_profile_error();
        self.log_failure("error", journal, Some(code));
        true
    }
    /// Rollback after a failed replacement: restart the previous runtime and, if it runs,
    /// republish the status it had before `stop_locked`/`start` cleared it.
    async fn restore_runtime(&self, inner: &mut Inner, previous: &Store, status: String) {
        inner.connected = inner.runtime.start(previous, &self.secret).await.is_ok();
        let connected = inner.connected;
        if connected {
            self.routes_in_force(live_server(previous).as_ref());
        }
        if !connected {
            // No sing-box, no rules in force.
            self.routes_forgotten();
        }
        self.snapshots.send_modify(|s| {
            s.runtime_active = connected;
            s.status = if connected { status } else { "error".into() };
        });
        if connected && previous.settings.auto_select {
            self.request(Reason::ListChanged);
        }
    }
    /// Returns whether the sidecar was stopped or restarted, i.e. its selector state is new.
    async fn replace(&self, inner: &mut Inner, next: Store, restart: bool) -> Result<bool> {
        self.replace_with(inner, next, restart, false, false).await
    }
    /// The config of `store` as the sidecar would build it: with the lists of the databases that
    /// are on disk.
    fn config_for(&self, store: &Store) -> Result<serde_json::Value> {
        config::generate_with(store, &self.secret, &self.geo.resolve_store(store))
    }
    /// `force`: check and restart even though the store did not change (a database did).
    /// `quiet`: the caller says what the new list asks for (a refresh of a subscription does, by what
    /// it found), so a restart does not queue a check of its own.
    async fn replace_with(
        &self,
        inner: &mut Inner,
        mut next: Store,
        restart: bool,
        force: bool,
        quiet: bool,
    ) -> Result<bool> {
        self.detect_crash(inner).await;
        if next
            .selected
            .as_ref()
            .is_some_and(|id| !next.available_servers().iter().any(|s| &s.id == id))
        {
            next.selected = None;
        }
        // What the user kept out of Auto Select is forgotten with the server.
        next.prune_excluded();
        // Names, themes and refresh timestamps do not change the
        // generated config: they neither need `sing-box check` nor interrupt network traffic.
        let config_changed =
            force || self.config_for(&inner.store).ok() != self.config_for(&next).ok();
        let supported: Vec<Server> = next
            .available_servers()
            .into_iter()
            .filter(|server| !server.is_retired())
            .collect();
        let restart = restart && config_changed;
        // The server chosen is gone and nothing took its place (see `subscription::merge`). With
        // Auto Select on, a check finds another. Without it, the one the user never saw is not
        // taken blindly: the fastest of what was measured, else the first, and it is checked.
        let mut replaced_blind = false;
        if restart && inner.connected && !next.settings.auto_select && next.selected.is_none() {
            let shown: HashMap<String, u64> = self
                .snapshots
                .borrow()
                .servers
                .iter()
                .filter_map(|s| s.proxy_ms.map(|ms| (s.id.clone(), ms)))
                .collect();
            next.selected = fallback_selection(&supported, |id| shown.get(id).copied());
            replaced_blind = inner.store.selected.is_some() && next.selected.is_some();
        }
        if restart && !supported.is_empty() {
            if inner.connected {
                // Validate exactly what start will receive, including any supported fallback.
                // A refused saved selection must not retire or restart the working runtime.
                inner
                    .runtime
                    .validate(&next, &self.secret)
                    .await
                    .map_err(|error| error.with_code(FailureCode::SidecarConfigRejected))?;
            } else {
                // Editing disconnected data is not a request to launch its saved selection.
                let mut validation = next.clone();
                validation.servers.clone_from(&supported);
                if validation
                    .selected
                    .as_ref()
                    .is_some_and(|id| !supported.iter().any(|server| &server.id == id))
                {
                    validation.selected = None;
                }
                inner
                    .runtime
                    .validate(&validation, &self.secret)
                    .await
                    .map_err(|error| error.with_code(FailureCode::SidecarConfigRejected))?;
            }
        }
        let previous = inner.store.clone();
        let previous_status = self.snapshot().status;
        let restarting = restart && inner.connected;
        if restarting {
            // Due rechecks run outside the coordinator: discard every old-runtime result
            // before retiring its gateway, without cancelling the refresh that owns this edit.
            {
                let mut control = self.control.lock().unwrap_or_else(|e| e.into_inner());
                control.dead.cancel();
                control.dead = CancellationToken::new();
            }
            if supported.is_empty() {
                if let Err(error) = self.stop_locked(inner).await {
                    // A failed stop leaves no owned child behind: bring the previous runtime
                    // back (or publish that it is gone) instead of keeping stale flags.
                    self.restore_runtime(inner, &previous, previous_status)
                        .await;
                    return Err(error);
                }
            } else {
                let mut started = inner
                    .runtime
                    .start(&next, &self.secret)
                    .await
                    .map_err(|error| error.with_code(FailureCode::SidecarStartExit));
                if matches!(&started, Err(e) if is_declined(e)) {
                    // The user would not allow TUN. What ran before keeps running; the rest of
                    // the change is applied to it, and the mode stays the one without rights.
                    next.settings.mode = Mode::Proxy;
                    self.declined_notice();
                    started = if self.config_for(&previous).ok() == self.config_for(&next).ok() {
                        Ok(())
                    } else {
                        inner
                            .runtime
                            .start(&next, &self.secret)
                            .await
                            .map_err(|error| error.with_code(FailureCode::SidecarStartExit))
                    };
                }
                if let Err(error) = started {
                    // A valid config can still fail at run time (occupied port, TUN privileges).
                    self.restore_runtime(inner, &previous, previous_status)
                        .await;
                    return Err(error);
                }
                inner.connected = true;
                self.snapshots.send_modify(|s| s.runtime_active = true);
            }
        }
        // Something was there or was added: the welcome window has done its job, even if the
        // list is emptied later (this also covers stores saved before the window existed).
        let had_content =
            |store: &Store| !store.servers.is_empty() || !store.subscriptions.is_empty();
        if !next.welcome_dismissed && (had_content(&next) || had_content(&inner.store)) {
            next.welcome_dismissed = true;
        }
        if let Err(error) = self.persist(&next) {
            if restarting {
                self.restore_runtime(inner, &previous, previous_status)
                    .await;
            }
            return Err(error);
        }
        inner.store = next;
        // The new sing-box starts in the mode of the server it starts on. The journal says so only
        // when that changes the rules in force, and only once the change is kept: a restart that
        // is taken back (the save failed) has changed nothing.
        if restarting && inner.connected {
            self.routes_in_force(live_server(&inner.store).as_ref());
        }
        if replaced_blind {
            self.log_in(
                Category::Checks,
                "warn",
                t("core.application.selected_server_gone"),
            );
            self.request(Reason::ListChanged);
        } else if restarting && inner.connected && inner.store.settings.auto_select && !quiet {
            self.request(Reason::ListChanged);
        }
        Ok(restarting)
    }
    /// True if any subscription's own backoff has elapsed (or never ran yet).
    fn any_subscription_due(&self, inner: &Inner) -> bool {
        let now = Instant::now();
        inner.store.subscriptions.iter().any(|s| {
            inner
                .subscription_backoff
                .get(&s.id)
                .is_none_or(|b| now >= b.next_due)
        })
    }
    /// Joins a compatible in-flight refresh or merges the request into one follow-up batch.
    /// The leader drains that batch before releasing the gate, so no request can be lost in
    /// the transition between two network phases. Sources whose result the same burst already
    /// stored are skipped when a later refresh-all absorbs narrower requests.
    async fn request_refresh(&self, id: Option<String>, force: bool, by_hand: bool) -> Result<()> {
        let requested = RefreshScope::one(id);
        let forced = if force {
            requested.clone()
        } else {
            RefreshScope::none()
        };
        let manual = if by_hand {
            requested.clone()
        } else {
            RefreshScope::none()
        };
        self.request_scopes(requested, forced, manual).await
    }
    /// `request_refresh` with the sources that skip their backoff named one by one.
    async fn request_scopes(
        &self,
        requested: RefreshScope,
        forced: RefreshScope,
        manual: RefreshScope,
    ) -> Result<()> {
        let follower = {
            let mut gate = self
                .refresh_gate
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(active) = gate.active.as_mut() {
                let (sender, receiver) = oneshot::channel();
                if active.scope.covers(&requested) && active.forced.covers(&forced) {
                    active.manual.merge(manual);
                    active.waiters.push(sender);
                } else if let Some(pending) = gate.pending.as_mut() {
                    pending.scope.merge(requested);
                    pending.forced.merge(forced);
                    pending.manual.merge(manual);
                    pending.waiters.push(sender);
                } else {
                    gate.pending = Some(RefreshBatch {
                        scope: requested,
                        forced,
                        manual,
                        waiters: vec![sender],
                    });
                }
                Some(receiver)
            } else {
                gate.active = Some(RefreshBatch {
                    scope: requested,
                    forced,
                    manual,
                    waiters: Vec::new(),
                });
                self.snapshots
                    .send_modify(|snapshot| snapshot.refreshing = true);
                None
            }
        };
        if let Some(receiver) = follower {
            return receiver
                .await
                .unwrap_or(RefreshOutcome::Cancelled)
                .into_result();
        }

        let mut leader = RefreshLeader {
            app: self,
            released: false,
        };
        let mut completed = HashSet::new();
        let mut leader_outcome = None;
        loop {
            let (scope, forced, manual) = {
                let gate = self
                    .refresh_gate
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let active = gate
                    .active
                    .as_ref()
                    .expect("refresh leader has an active batch");
                (
                    active.scope.clone(),
                    active.forced.clone(),
                    active.manual.clone(),
                )
            };
            let (result, applied) = self
                .refresh_once(&scope, &forced, &manual, &completed)
                .await;
            completed.extend(applied);
            let outcome = RefreshOutcome::from_result(&result);
            leader_outcome.get_or_insert_with(|| outcome.clone());

            let mut gate = self
                .refresh_gate
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let active = gate
                .active
                .take()
                .expect("completed refresh batch is active");
            for waiter in active.waiters {
                let _ = waiter.send(outcome.clone());
            }
            if let Some(pending) = gate.pending.take() {
                gate.active = Some(pending);
                continue;
            }
            self.snapshots
                .send_modify(|snapshot| snapshot.refreshing = false);
            leader.released = true;
            return leader_outcome
                .unwrap_or(RefreshOutcome::Success)
                .into_result();
        }
    }

    /// Performs one gate-owned network batch without holding the coordinator lock.
    /// `forced` sources bypass their backoff, `manual` ones were asked for by hand (the trigger
    /// of their local diagnostic); `completed` prevents duplicate fetches when a
    /// refresh-all request is merged behind an already-running single-source request.
    /// Returns the sources whose result is now stored (a recorded fetch failure counts):
    /// a cancelled batch or a failed replacement returns none, so the next batch of the same
    /// burst downloads them instead of reporting success for work that never happened.
    async fn refresh_once(
        &self,
        scope: &RefreshScope,
        forced: &RefreshScope,
        manual: &RefreshScope,
        completed: &HashSet<String>,
    ) -> (Result<()>, HashSet<String>) {
        let inner = self.inner.lock().await;
        if self.shutdown.is_cancelled()
            || (self
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .disconnecting
                && inner.connected)
        {
            return (Err(Error::Cancelled), HashSet::new());
        }
        let now = Instant::now();
        let connected = inner.connected;
        let mode = inner.store.settings.mode.clone();
        let proxy_port = inner.store.settings.proxy_port;
        let sources: Vec<Subscription> = inner
            .store
            .subscriptions
            .iter()
            .filter(|s| {
                scope.contains(&s.id)
                    && !completed.contains(&s.id)
                    && (forced.contains(&s.id)
                        || inner
                            .subscription_backoff
                            .get(&s.id)
                            .is_none_or(|b| now >= b.next_due))
            })
            .cloned()
            .collect();
        // Subscription downloads must not hold the coordinator: actions and shutdown stay
        // responsive while the subscription request is in flight.
        drop(inner);
        let token = {
            // Re-check after dropping the coordinator: disconnect can race with source
            // selection before the first network request starts.
            let inner = self.inner.lock().await;
            let mut c = self.control.lock().unwrap_or_else(|e| e.into_inner());
            if self.shutdown.is_cancelled() || (c.disconnecting && inner.connected) {
                return (Err(Error::Cancelled), HashSet::new());
            }
            c.refresh = CancellationToken::new();
            c.refresh.clone()
        };
        let sources_by_hand = sources.iter().any(|source| manual.contains(&source.id));
        let mut outcomes = Vec::with_capacity(sources.len());
        for source in sources {
            let request = |proxy_port| subscription::FetchRequest {
                url: &source.url,
                proxy_port,
                user_agent: source.user_agent.as_deref(),
            };
            let mut via = Via::Direct;
            let mut download = tokio::select! { biased;
                _ = token.cancelled() => {
                    return (Err(Error::Cancelled), HashSet::new());
                },
                _ = self.shutdown.cancelled() => {
                    return (Err(Error::Cancelled), HashSet::new());
                },
                result = self.subscriptions.fetch_with(request(None)) => result,
            };
            // Not connected: direct only (unchanged). Connected: direct first, then retry
            // through the active server's tunnel — pure Tun mode has no local inbound to
            // retry through (config.rs skips it there), so a second direct attempt would
            // just repeat the same result.
            let mut first_attempt = None;
            if download.is_err() && connected && mode != Mode::Tun {
                first_attempt = download.as_ref().err().map(|error| Attempt {
                    via: Via::Direct,
                    failure_code: error
                        .failure_code()
                        .unwrap_or(FailureCode::SubscriptionTransport),
                    cause: error.detail().and_then(|detail| detail.cause),
                });
                via = Via::LocalProxy;
                download = tokio::select! { biased;
                    _ = token.cancelled() => {
                        return (Err(Error::Cancelled), HashSet::new());
                    },
                    _ = self.shutdown.cancelled() => {
                        return (Err(Error::Cancelled), HashSet::new());
                    },
                    result = self.subscriptions.fetch_with(request(Some(proxy_port))) => result,
                };
            }
            let parsed = match download {
                Ok(fetched) => {
                    let subscription::Fetched {
                        body,
                        title,
                        usage,
                        mut trail,
                        info,
                    } = fetched;
                    // Xray JSON counts servers where a list of links counts lines.
                    let by_server = crate::xray::looks_like(&body);
                    subscription::parse_subscription_with(&body, &source.id, source.provider_routes)
                        .map_err(|error| {
                            error
                                .with_code(FailureCode::SubscriptionInvalidBody)
                                .at_stage(Stage::Parse)
                                .with_subscription(info)
                                .with_subscription(subscription::body_info(&body))
                                .with_trail(trail)
                        })
                        .and_then(|parsed| {
                            trail.push(Mark::Parsed);
                            subscription::apply_filters(parsed, &source.filters).map_err(|error| {
                                error
                                    .with_code(FailureCode::SubscriptionInvalidBody)
                                    .at_stage(Stage::Filter)
                                    .with_subscription(info)
                                    .with_subscription(subscription::body_info(&body))
                                    .with_trail(trail)
                            })
                        })
                        .map(|parsed| (parsed, title, usage, by_server))
                }
                Err(error) => {
                    let error = error.with_code(FailureCode::SubscriptionTransport);
                    Err(match first_attempt {
                        Some(attempt) => error.with_first_attempt(attempt),
                        None => error,
                    })
                }
            };
            outcomes.push((source, parsed, via));
        }
        let mut inner = self.inner.lock().await;
        let mut next = inner.store.clone();
        let mut changed = false;
        let mut applied = HashSet::with_capacity(outcomes.len());
        // The loop below advances the schedule of every stored result; a failed `replace`
        // stores none of them, so the schedule goes back with it.
        let backoff_before = inner.subscription_backoff.clone();
        // The accounts of the failures belong to the same stored state: they go back with it.
        let reports_before = inner.subscription_failures.clone();
        let rows_before = inner.subscription_rows.clone();
        let refreshed_now = Instant::now();
        let full_interval =
            Duration::from_secs(inner.store.settings.subscription_interval_mins * 60);
        for (source, parsed, via) in outcomes {
            // A concurrent RemoveSubscription may have committed while this was fetching
            // (the coordinator lock was released above): without this check, merge() would
            // resurrect servers under an id that no longer exists in `next`, orphaned and
            // undeletable (RemoveServer only matches manually-added servers). The same goes
            // for SaveSubscription replacing the URL of an existing id in place: content
            // fetched from the old URL is stale for the new one, and its success would also
            // overwrite the backoff SaveSubscription cleared, delaying the new URL's first
            // fetch by a full interval. Such a result is dropped; the edited subscription is
            // already due on the next tick.
            // The same goes for what the result was read with (the User-Agent, the filters, the
            // switch of the rules): the edited subscription is fetched again right away.
            if !next.subscriptions.iter().any(|s| {
                s.id == source.id
                    && s.url == source.url
                    && s.user_agent == source.user_agent
                    && s.filters == source.filters
                    && s.provider_routes == source.provider_routes
            }) {
                continue;
            }
            applied.insert(source.id.clone());
            match parsed {
                Ok((parsed, title, usage, by_server)) => {
                    let (rejected, filtered) = (parsed.rejected, parsed.filtered);
                    let parameter_note = parsed.rows.parameter_note();
                    inner
                        .subscription_rows
                        .insert(source.id.clone(), parsed.rows);
                    let unit = if by_server {
                        t("core.application.unit_servers")
                    } else {
                        t("core.application.unit_lines")
                    };
                    let with_rules = parsed
                        .servers
                        .iter()
                        .filter(|s| s.own_routes().is_some())
                        .count();
                    let keys_before = source_keys(&next, &source.id);
                    next = subscription::merge(&next, &source.id, parsed);
                    let keys_after = source_keys(&next, &source.id);
                    let (added, gone) = (
                        keys_after.difference(&keys_before).count(),
                        keys_before.difference(&keys_after).count(),
                    );
                    if added > 0 || gone > 0 {
                        self.log_in(
                            Category::Subscriptions,
                            "info",
                            &tr!(
                                "core.application.addresses_changed",
                                added = added,
                                gone = gone
                            ),
                        );
                    }
                    // A store holds only so many sets and entries of server rules: the servers
                    // that did not fit are there without theirs, and the journal says how many.
                    let kept = next
                        .servers
                        .iter()
                        .filter(|s| s.source.as_deref() == Some(source.id.as_str()))
                        .filter(|s| s.own_routes().is_some())
                        .count();
                    if kept < with_rules {
                        self.log_in(
                            Category::Core,
                            "warn",
                            &tr!(
                                "core.application.provider_rules_skipped",
                                count = with_rules - kept
                            ),
                        );
                    }
                    if let Some(s) = next.subscriptions.iter_mut().find(|s| s.id == source.id) {
                        s.updated_at = Some(timestamp());
                        s.error = None;
                        s.failure_code = None;
                        // The provider names its subscription; without a name the old one stays.
                        if let Some(name) = title.as_deref().and_then(Subscription::clean_name) {
                            s.name = name;
                        }
                        // What the provider no longer reports is no longer shown or applied.
                        s.usage = usage;
                    }
                    changed = true;
                    inner.subscription_failures.remove(&source.id);
                    if let Some(note) = parameter_note {
                        self.log_in(Category::Subscriptions, "info", &note);
                    }
                    inner.subscription_backoff.insert(
                        source.id.clone(),
                        SourceBackoff {
                            failures: 0,
                            next_due: refreshed_now
                                + self.jitter.spread(source_interval(&source, full_interval)),
                        },
                    );
                    self.log_in(
                        Category::Subscriptions,
                        "info",
                        &if filtered > 0 {
                            tr!(
                                "core.application.refreshed_rejected_filtered",
                                unit = unit,
                                rejected = rejected,
                                filtered = filtered
                            )
                        } else {
                            tr!(
                                "core.application.refreshed_rejected",
                                unit = unit,
                                rejected = rejected
                            )
                        },
                    );
                }
                Err(e) => {
                    if let Some(s) = next.subscriptions.iter_mut().find(|s| s.id == source.id) {
                        s.error = Some(self.clean_message(&e.to_string()));
                        s.failure_code = e
                            .failure_code()
                            .or(Some(FailureCode::SubscriptionInvalidBody));
                    }
                    let failures = inner
                        .subscription_backoff
                        .get(&source.id)
                        .map_or(0, |b| b.failures)
                        .saturating_add(1);
                    inner.subscription_backoff.insert(
                        source.id.clone(),
                        SourceBackoff {
                            failures,
                            next_due: refreshed_now
                                + self.jitter.spread(subscription_backoff(
                                    failures,
                                    source_interval(&source, full_interval),
                                )),
                        },
                    );
                    // The previous servers stay: the failed source replaced nothing.
                    let trigger = if manual.contains(&source.id) {
                        Trigger::Manual
                    } else {
                        Trigger::Scheduled
                    };
                    let e = if e.failure_code().is_some() {
                        e
                    } else {
                        e.with_code(FailureCode::SubscriptionInvalidBody)
                    }
                    .with_context(trigger, via, true);
                    let secrets = self
                        .journal_secrets
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    let details = journal::clean_details(Some(&e.diagnostic_details()), &secrets);
                    drop(secrets);
                    if let Some(details) = details {
                        inner
                            .subscription_failures
                            .insert(source.id.clone(), details);
                    }
                    self.log_error(t("core.application.subscription_refresh_failed"), &e);
                }
            }
        }
        // Drop bookkeeping for subscriptions a concurrent action removed while this ran.
        let known: HashSet<&str> = next.subscriptions.iter().map(|s| s.id.as_str()).collect();
        inner
            .subscription_backoff
            .retain(|id, _| known.contains(id.as_str()));
        inner
            .subscription_failures
            .retain(|id, _| known.contains(id.as_str()));
        inner
            .subscription_rows
            .retain(|id, _| known.contains(id.as_str()));
        let (keys_before, selected_before) =
            (list_keys(&inner.store), inner.store.selected.clone());
        let result = self
            .replace_with(&mut inner, next, changed, false, true)
            .await
            .map(|_| ())
            .map_err(|error| {
                if error.failure_code() != Some(FailureCode::Persistence) {
                    return error;
                }
                // Every download of the batch is stored or none: the write that failed is the end
                // of a refresh, and the servers it would have replaced stay.
                let trigger = if sources_by_hand {
                    Trigger::Manual
                } else {
                    Trigger::Scheduled
                };
                error
                    .at_stage(Stage::Persist)
                    .in_operation(Operation::SubscriptionRefresh)
                    .with_context(trigger, Via::NotApplicable, true)
            });
        if result.is_ok() {
            self.publish_store(&inner.store);
            self.after_refresh(&mut inner, &keys_before, selected_before.as_deref());
            (result, applied)
        } else {
            // The previous store stays: none of these results was stored, so none of them is
            // done, and a source that was due stays due for the next batch.
            inner.subscription_backoff = backoff_before;
            inner.subscription_failures = reports_before;
            inner.subscription_rows = rows_before;
            (result, HashSet::new())
        }
    }
    /// What a stored refresh asks of the checks. The server in use followed its address to the new
    /// id: the journal says so. Another address in the list, or a server in use that is gone, is
    /// looked at at once, without waiting for the cooldown of the last switch. The same addresses
    /// under other credentials need no check: the fast one asks the pool by address anyway.
    fn after_refresh(
        &self,
        inner: &mut Inner,
        keys_before: &HashSet<String>,
        selected_before: Option<&str>,
    ) {
        if selected_before.is_some()
            && inner.store.selected.is_some()
            && inner.store.selected.as_deref() != selected_before
        {
            self.log_in(
                Category::Subscriptions,
                "info",
                t("core.application.selected_moved"),
            );
        }
        if !inner.connected || !inner.store.settings.auto_select {
            return;
        }
        let lost = selected_before.is_some() && inner.store.selected.is_none();
        if lost || list_keys(&inner.store) != *keys_before {
            inner.urgent = Some(Reason::Subscription);
        }
    }
    /// A term that ends while Rift runs changes what Auto Select may use without any refresh:
    /// the views follow the clock.
    fn refresh_availability(&self) {
        let now = timestamp();
        self.snapshots.send_if_modified(|snapshot| {
            let mut changed = false;
            for view in &mut snapshot.subscriptions {
                let current = view
                    .usage
                    .map_or(Availability::Ok, |usage| usage.availability(now));
                if view.availability != current {
                    view.availability = current;
                    changed = true;
                }
            }
            changed
        });
    }
    async fn worker(self: Arc<Self>) {
        loop {
            tokio::select! {_=self.shutdown.cancelled()=>break,_=self.wake.notified()=>{},_=sleep(Duration::from_secs(1))=>{}}
            self.refresh_availability();
            let mut inner = self.inner.lock().await;
            if self.shutdown.is_cancelled() {
                break;
            }
            // Before refresh: a refresh-driven restart must not hide a crashed child.
            self.detect_crash(&mut inner).await;
            // What sing-box said about its own failures, as the journal can bear it: a kind of
            // failure once, and again only when it goes on.
            for note in inner.runtime.log_notes(timestamp()) {
                self.log_in(Category::Core, "warn", &note);
            }
            for note in inner.runtime.informational_notes(timestamp()) {
                self.log_in(Category::Core, "info", &note);
            }
            self.geo_tick(inner.store.settings.geodata_auto_update);
            // No server works: the provider will soon give others, so the subscriptions are asked
            // out of turn, once per fast-check interval, as long as it lasts. Only the sources
            // that did not fail last time are asked before their turn; one that failed keeps the
            // pause that follows a failed download (a minute, doubling), as in any other refresh.
            // A changed list starts the full check by itself.
            let now = Instant::now();
            if inner.outage
                && inner.connected
                && inner.store.settings.auto_select
                && now >= inner.outage_refresh_at
                && now >= inner.subscription_hold_until
            {
                let healthy: HashSet<String> = inner
                    .store
                    .subscriptions
                    .iter()
                    .filter(|s| {
                        inner
                            .subscription_backoff
                            .get(&s.id)
                            .is_none_or(|b| b.failures == 0)
                    })
                    .map(|s| s.id.clone())
                    .collect();
                if !healthy.is_empty() || self.any_subscription_due(&inner) {
                    inner.outage_refresh_at = now
                        + self
                            .jitter
                            .spread(Duration::from_secs(inner.store.settings.interval_secs));
                    drop(inner);
                    let result = self
                        .request_scopes(
                            RefreshScope::All,
                            RefreshScope::Sources(healthy),
                            RefreshScope::none(),
                        )
                        .await;
                    inner = self.inner.lock().await;
                    match result {
                        Err(Error::Cancelled) | Ok(()) => {}
                        Err(e) => {
                            inner.subscription_hold_until =
                                Instant::now() + self.jitter.spread(Duration::from_secs(60));
                            self.log_error(&e.to_string(), &e);
                        }
                    }
                }
            }
            // The safety-net timer forces a full, unfiltered cycle (like a manual refresh):
            // it means the last attempt failed for a reason other than an individual
            // subscription fetch, so a source that fetched fine last time is still retried.
            let now = Instant::now();
            let fallback_due = now >= inner.subscription_retry_at;
            if fallback_due
                || (now >= inner.subscription_hold_until && self.any_subscription_due(&inner))
            {
                drop(inner);
                let result = self.request_refresh(None, fallback_due, false).await;
                inner = self.inner.lock().await;
                let refreshed_now = Instant::now();
                let global =
                    Duration::from_secs(inner.store.settings.subscription_interval_mins * 60);
                // The full forced cycle must not undercut a source that refreshes more rarely
                // than the global interval: it waits for the longest interval of all sources.
                let full_interval = inner
                    .store
                    .subscriptions
                    .iter()
                    .map(|s| source_interval(s, global))
                    .max()
                    .unwrap_or(global);
                // This timer belongs only to the automatic worker. A manual request that
                // joins the same flight cannot cancel a retry owed by an earlier failure.
                inner.subscription_retry_at = refreshed_now
                    + self.jitter.spread(if result.is_err() {
                        Duration::from_secs(60)
                    } else {
                        full_interval
                    });
                // An interrupted refresh (Settings, manual choice, import, Disconnect) is no
                // failure: it stored nothing and wrote no backoff, so its sources stay due and
                // the next tick repeats it, with no pause and no error entry.
                match result {
                    Err(Error::Cancelled) | Ok(()) => {}
                    Err(e) => {
                        inner.subscription_hold_until =
                            refreshed_now + self.jitter.spread(Duration::from_secs(60));
                        self.log_error(&e.to_string(), &e);
                    }
                }
            }
            if inner.connected && !self.detect_crash(&mut inner).await {
                if let Some(reason) = Reason::from_code(self.pending.swap(0, Ordering::SeqCst)) {
                    inner.schedule.request(Plan::full(reason));
                }
            }
            if !inner.connected || self.detect_crash(&mut inner).await {
                inner.failover = Failover::new();
                inner.offline = false;
                inner.deaf_said = false;
                continue;
            }
            self.start_recheck(&inner);
            let urgent = match inner.urgent.take() {
                Some(reason) => Some(reason),
                None => self
                    .failover_tick(&mut inner)
                    .await
                    .then_some(Reason::Failover),
            };
            if !inner.store.settings.auto_select && inner.schedule.pending.is_none() {
                continue;
            }
            let started = match urgent {
                Some(reason) => inner.schedule.begin_urgent(reason),
                None => inner.schedule.begin(Instant::now()),
            };
            if let Some(mut plan) = started {
                let token = {
                    let mut c = self.control.lock().unwrap_or_else(|e| e.into_inner());
                    if c.disconnecting {
                        inner.schedule.finish(Instant::now());
                        continue;
                    }
                    c.token = CancellationToken::new();
                    c.token.clone()
                };
                // A list no longer than the pool, or a pool that lost a member, has nothing to watch:
                // the fast check is a full one.
                if plan.cycle == Cycle::Fast {
                    if let Some(reason) = self.fast_needs_full(&inner) {
                        plan = Plan::full(reason);
                        inner.schedule.became_full();
                    }
                }
                let outage = inner.outage;
                let full = plan.cycle == Cycle::Full;
                self.snapshots.send_modify(|s| {
                    // A fast check passes unseen: the screen does not flicker every minute.
                    s.checking = full;
                    s.pending = false;
                    // The error of a lack of servers stays up while it lasts, and is not raised again.
                    if !outage {
                        s.error = None;
                    }
                });
                // A machine without a network says nothing about its servers: ask the network
                // first, outside them, and leave the servers alone when it does not answer.
                let network = tokio::select! {biased;
                    _=token.cancelled()=>None,
                    up=inner.probe.network()=>Some(up),
                };
                match network {
                    None => {
                        // An interrupted check says nothing; a full one that was asked for is owed still.
                        if owed(plan) {
                            inner.schedule.request(plan);
                            self.snapshots.send_modify(|s| s.pending = true);
                        }
                        inner.schedule.finish(Instant::now());
                        self.checking_ended(&inner);
                        continue;
                    }
                    Some(Network::Down(seen)) => {
                        if !inner.offline {
                            inner.offline = true;
                            self.log_in(
                                Category::Checks,
                                "warn",
                                &tr!("core.application.no_network", seen = seen),
                            );
                        }
                        let now = Instant::now();
                        inner.schedule.skipped(plan, now);
                        if owed(plan) {
                            self.snapshots.send_modify(|s| s.pending = true);
                        }
                        // Nothing was found, so a failover cycle waits for the regular deadline.
                        inner.failover = Failover {
                            next_at: Some(now + self.jitter.spread(FAILOVER_INTERVAL)),
                            held_until: Some(inner.schedule.deadline()),
                            ..Failover::new()
                        };
                        self.checking_ended(&inner);
                        continue;
                    }
                    Some(up) => {
                        if inner.offline {
                            inner.offline = false;
                            self.log_in(
                                Category::Checks,
                                "info",
                                t("core.application.network_back"),
                            );
                            // Every delay known was measured on another network: look at all of them.
                            if plan.cycle == Cycle::Fast {
                                plan = Plan::full(Reason::NetworkBack);
                                inner.schedule.became_full();
                                self.snapshots.send_modify(|s| s.checking = true);
                            }
                        }
                        if let Network::SidecarDeaf(seen) = up {
                            if !inner.deaf_said {
                                inner.deaf_said = true;
                                self.log_in(
                                    Category::Checks,
                                    "warn",
                                    &tr!(
                                        "core.application.singbox_deaf",
                                        detail = inner
                                            .runtime
                                            .last_direct_failure()
                                            .map(|failure| format!("; {failure}"))
                                            .unwrap_or_default(),
                                        seen = seen
                                    ),
                                );
                            }
                        }
                    }
                }
                if plan.cycle == Cycle::Fast {
                    self.fast_check(&mut inner, &token).await;
                    continue;
                }
                self.log_in(
                    Category::Checks,
                    "info",
                    &tr!(
                        "core.application.check_started",
                        reason = reason_text(plan.reason)
                    ),
                );
                let include_dead = self.check_all.swap(false, Ordering::SeqCst);
                let tracked = inner.store.settings.auto_select;
                let pool_size = inner.store.settings.pool_size;
                let mut held_back = HashSet::new();
                let mut pool_incomplete = false;
                let mut prefer = vec![];
                let mut keep_out_of_pool = HashSet::new();
                let servers = if inner.store.settings.auto_select {
                    // Servers of a subscription whose traffic is used up or whose term has
                    // ended are left out; a manual choice can still pick them. The servers that
                    // did not answer are held back until their time to be asked comes (not at
                    // all when the check was asked for by hand, nor when the list is no longer
                    // than the pool: all of it is looked at every time).
                    let mut all = inner.store.auto_candidates(timestamp());
                    let dead = self.dead_among(&all, include_dead);
                    if distinct_servers(&all) > pool_size {
                        held_back = dead;
                    }
                    let present = pool_members(&inner.pool, &all);
                    // Complete is what the last full check could build (`K_eff`, not the setting
                    // `K`): a list or a working set smaller than `K` has a complete pool of what
                    // there is, and the thresholds then apply. A pool that was never built (Auto
                    // Select just on, nothing worked) or lost a member is incomplete.
                    pool_incomplete = inner.pool.target == 0 || present.len() < inner.pool.target;
                    prefer = present.iter().map(|s| s.id.clone()).collect();
                    // A server kept out of Auto Select is asked while it is the one in use, and
                    // only as that.
                    if let Some(server) = inner.store.excluded_in_use() {
                        keep_out_of_pool.insert(server.id.clone());
                        all.push(server);
                    }
                    all
                } else {
                    inner
                        .store
                        .available_servers()
                        .into_iter()
                        .filter(|s| Some(&s.id) == inner.store.selected.as_ref())
                        .collect()
                };
                // What a fast check or the light check learned: servers dead a moment ago are not
                // asked again, nor is the server in use when it is the one that was lost.
                let hints = std::mem::take(&mut inner.hints);
                let current_lost = tracked
                    && (hints.current_lost
                        || matches!(plan.reason, Reason::CurrentLost | Reason::Failover));
                if current_lost {
                    if let Some(id) = inner.store.selected.clone() {
                        let lost = selection::Measurement {
                            latency_ms: None,
                            failure_code: Some(FailureCode::ConnectionLost),
                        };
                        self.snapshots
                            .send_modify(|s| show_progress(s, &Progress::Current(id, lost)));
                    }
                }
                let mut last_switch = None;
                let mut switch_failed = false;
                // The servers that failed in this cycle (once each, whatever the stage).
                let mut failed_now = HashSet::new();
                // Only counts: the journal gets one summary line per cycle, never a line per
                // server (a subscription can hold two thousand).
                let (mut tested, mut reachable, mut working) = (0usize, 0usize, 0usize);
                self.history
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .begin_cycle(|id| inner.store.servers.iter().any(|s| s.id == id));
                let cycle = selection::run_with(
                    inner.probe.as_ref(),
                    &servers,
                    inner.store.selected.as_deref(),
                    selection::Options {
                        held_back,
                        pool_incomplete,
                        prefer,
                        skip: hints.skip,
                        current_lost,
                        keep_out_of_pool,
                        ..selection::Options::from_settings(
                            &inner.store.settings,
                            self.jitter.clone(),
                        )
                    },
                    &token,
                    |event| {
                        if self.note_dead(&event, tracked, &mut failed_now) {
                            token.cancel();
                            return;
                        }
                        self.history
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .record(&event, timestamp());
                        match &event {
                            Progress::Endpoint(_, measurement) => {
                                tested += 1;
                                reachable += usize::from(measurement.latency_ms.is_some());
                            }
                            Progress::Proxy(_, measurement) | Progress::Current(_, measurement) => {
                                working += usize::from(measurement.latency_ms.is_some());
                            }
                            _ => {}
                        }
                        if matches!(&event, Progress::SelectionFailed(..)) {
                            switch_failed = true;
                        }
                        if let Progress::Selected(id, at) = &event {
                            last_switch = Some(*at);
                            self.routes_in_force(servers.iter().find(|s| &s.id == id));
                        }
                        self.snapshots.send_modify(|s| {
                            show_progress(s, &event);
                            self.mark_dead(s);
                        });
                    },
                );
                // The servers that did not answer are asked again at their time even while this
                // runs (a comparison can take minutes): the worker is busy in it, so the clock
                // is looked at from here. Not when the check was asked for by hand: it asks them.
                let result = if tracked && !include_dead {
                    tokio::select! {
                        result = cycle => result,
                        never = self.recheck_during(
                            &servers,
                            inner.probe.clone(),
                            inner.store.settings.concurrency,
                        ) => never,
                    }
                } else {
                    cycle.await
                };
                if !matches!(result, Err(Error::Cancelled)) {
                    self.log_in(
                        Category::Checks,
                        "info",
                        &tr!(
                            "core.application.check_done",
                            tested = tested,
                            reachable = reachable,
                            working = working
                        ),
                    );
                }
                if let Some(at) = last_switch {
                    inner.schedule.switched(at);
                    inner.store.selected = self.snapshot().selected;
                    if let Err(e) = self.persist(&inner.store) {
                        self.log_error(&e.to_string(), &e);
                    }
                }
                // A switch is two steps, the selector and then the rules of the new server. When
                // the second fails the selector has moved without the rules following: back to the
                // choice that was kept, as a manual choice does.
                if switch_failed && inner.connected && !self.shutdown.is_cancelled() {
                    let kept = inner.store.live_selection();
                    let _ = switch_live(inner.probe.as_ref(), &inner.store, &kept).await;
                }
                // Whether the cycle found a working server; an interrupted one says nothing.
                let found = match &result {
                    Ok(_) => Some(true),
                    Err(Error::Cancelled) => None,
                    Err(_) => Some(false),
                };
                if self.detect_crash(&mut inner).await {
                    self.checking_ended(&inner);
                    continue;
                }
                match result {
                    Ok(out) => {
                        if tracked {
                            inner.pool = PoolState::built(&out.pool, &out.ranking, &servers);
                            // One line about what the check decided, never a name or an address.
                            self.log_in(
                                Category::Checks,
                                "info",
                                &decision_text(out.decision, &inner.store.settings),
                            );
                        }
                        if inner.outage {
                            inner.outage = false;
                            self.snapshots.send_modify(|s| s.error = None);
                            self.log_in(
                                Category::Checks,
                                "info",
                                t("core.application.working_again"),
                            );
                        }
                        inner.store.selected = Some(out.selected);
                        if let Err(e) = self.persist(&inner.store) {
                            self.log_error(&e.to_string(), &e);
                        }
                    }
                    // Settings/Refresh interruptions must not drop a check that was asked for.
                    // Disconnect, Select and disabling Auto Select clear this again.
                    Err(Error::Cancelled) if owed(plan) => {
                        inner.schedule.request(plan);
                        self.snapshots.send_modify(|s| s.pending = true);
                    }
                    Err(Error::Cancelled) => {}
                    // Nothing works, and Auto Select is on: said once, however long it lasts. The
                    // subscriptions are asked for new servers meanwhile (once per interval).
                    Err(Error::Unavailable) if tracked => {
                        inner.pool = PoolState::default();
                        if inner.outage {
                            self.log_in(
                                Category::Checks,
                                "warn",
                                t("core.application.still_no_working"),
                            );
                        } else {
                            inner.outage = true;
                            inner.outage_refresh_at = Instant::now();
                            let e = nothing_works(plan, tested, reachable, working);
                            let message = self.clean_message(&e.to_string());
                            self.snapshots.send_modify(|s| {
                                s.status = "error".into();
                                s.error = Some(message);
                            });
                            self.forget_profile_error();
                            self.log_error(&e.to_string(), &e);
                        }
                    }
                    // A check asked for by hand while Auto Select is off, and the server chosen does
                    // not answer: the owner asked and gets the answer in the list and in the
                    // journal; nothing is wrong with Rift, so there is no error toast.
                    Err(Error::Unavailable)
                        if plan.reason == Reason::CheckNow && inner.store.selected.is_some() =>
                    {
                        self.log_in(
                            Category::Checks,
                            "warn",
                            t("core.application.selected_silent"),
                        );
                    }
                    Err(e) => {
                        let e = if matches!(e, Error::Unavailable) {
                            nothing_works(plan, tested, reachable, working)
                        } else {
                            e
                        };
                        let message = self.clean_message(&e.to_string());
                        self.snapshots.send_modify(|s| {
                            s.status = "error".into();
                            s.error = Some(message);
                        });
                        self.forget_profile_error();
                        self.log_error(&e.to_string(), &e);
                    }
                }
                inner.schedule.finish(Instant::now());
                if let Some(found) = found {
                    // The cycle asked the selected server itself: the light checks begin anew,
                    // and after a fruitless one the regular interval is waited out.
                    inner.failover = Failover {
                        next_at: Some(Instant::now() + self.jitter.spread(FAILOVER_INTERVAL)),
                        held_until: (!found).then_some(inner.schedule.deadline()),
                        ..Failover::new()
                    };
                }
                self.publish_dead();
                self.checking_ended(&inner);
            }
        }
    }
    /// Whether a fast check has nothing to watch: the list is no more than the pool, or the pool is
    /// missing a server it had. Says why a full check takes its place.
    fn fast_needs_full(&self, inner: &Inner) -> Option<Reason> {
        let candidates = inner.store.auto_candidates(timestamp());
        if distinct_servers(&candidates) <= inner.store.settings.pool_size {
            return Some(Reason::SmallList);
        }
        let present = pool_members(&inner.pool, &candidates).len();
        (inner.pool.target == 0 || present < inner.pool.target).then_some(Reason::PoolIncomplete)
    }
    /// One answer from each server of the pool and from the server in use, a second look at the
    /// ones that changed. Switches nothing: what it finds sets the full check that follows.
    async fn fast_check(&self, inner: &mut Inner, token: &CancellationToken) {
        let candidates = inner.store.auto_candidates(timestamp());
        let selected = inner.store.selected.clone();
        let mut list: Vec<(Server, Option<u64>)> = pool_members(&inner.pool, &candidates)
            .into_iter()
            .map(|s| (s.clone(), inner.pool.bases.get(&s.stable_key()).copied()))
            .collect();
        // The server in use is watched as the pool is, also when it is not in it (chosen by hand).
        let in_use = selected.as_deref().and_then(|id| {
            inner
                .store
                .available_servers()
                .into_iter()
                .find(|s| s.id == id)
        });
        if let Some(server) = in_use.filter(|s| !list.iter().any(|(m, _)| m.id == s.id)) {
            // Its base is what the last full check found: the middle of its answers when it had
            // them, the one it gave otherwise. Known once, it stays until the next full check:
            // the delay on the screen follows every fast check, and a base that followed it
            // would never see a slow worsening.
            let key = server.stable_key();
            let base = inner.pool.base_of(&key).or_else(|| {
                let shown = self.shown_delay(&server.id);
                if let Some(ms) = shown {
                    inner.pool.bases.insert(key, ms);
                }
                shown
            });
            list.push((server, base));
        }
        let watched: Vec<Watched> = list
            .iter()
            .map(|(server, base_ms)| Watched {
                server,
                base_ms: *base_ms,
            })
            .collect();
        let tolerance = Tolerance::from_settings(&inner.store.settings);
        let mut failed_now = HashSet::new();
        let report = fast::run_fast(
            inner.probe.as_ref(),
            &watched,
            selected.as_deref(),
            tolerance,
            inner.store.settings.concurrency,
            &self.jitter,
            token,
            |event| {
                if self.note_dead(&event, true, &mut failed_now) {
                    token.cancel();
                    return;
                }
                self.snapshots.send_modify(|s| {
                    show_progress(s, &event);
                    self.mark_dead(s);
                });
            },
        )
        .await;
        drop(watched);
        match report {
            Ok(report) => self.after_fast(inner, &list, &report),
            // An action interrupted it: nothing was found, the next one asks again.
            Err(Error::Cancelled) => {}
            Err(error) => self.log_error(&error.to_string(), &error),
        }
        inner.schedule.finish(Instant::now());
        self.publish_dead();
        self.checking_ended(inner);
    }
    /// The delay the screen shows for a server: what the last check of it found.
    fn shown_delay(&self, id: &str) -> Option<u64> {
        self.snapshots
            .borrow()
            .servers
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.proxy_ms)
    }
    /// What a fast check found: all well, or a full check owed, and why.
    fn after_fast(&self, inner: &mut Inner, list: &[(Server, Option<u64>)], report: &fast::Report) {
        // A server with no delay to compare with got one from this check.
        for reading in &report.readings {
            let unknown = list
                .iter()
                .find(|(s, base)| s.id == reading.id && base.is_none());
            if let (Some((server, _)), Some(median)) = (unknown, reading.judgement.median_ms) {
                inner.pool.bases.insert(server.stable_key(), median);
            }
        }
        let text = fast_text(&inner.pool, list, report);
        if report.all_fine() {
            self.log_in(Category::Checks, "info", &text);
            return;
        }
        if report.current_lost {
            // The pool is not asked: the check that replaces the server in use does that, and
            // starts at once.
            inner.hints = Hints {
                skip: HashSet::new(),
                current_lost: true,
            };
            inner.urgent = Some(Reason::CurrentLost);
            self.record(
                Category::Checks,
                "warn",
                &text,
                Some(FailureCode::ConnectionLost),
                None,
            );
            return;
        }
        let lost: HashSet<String> = report.lost().into_iter().map(str::to_owned).collect();
        let reason = if lost.is_empty() {
            Reason::PoolDegraded
        } else {
            Reason::PoolLost
        };
        inner.hints = Hints {
            skip: lost,
            current_lost: false,
        };
        inner.schedule.request(Plan::full(reason));
        self.snapshots.send_modify(|s| s.pending = true);
        self.log_in(Category::Checks, "warn", &text);
    }
    /// The servers of `all` that did not answer and that a cycle holds back (none when the check
    /// was asked for by hand). Servers that are gone are forgotten.
    fn dead_among(&self, all: &[Server], include_dead: bool) -> HashSet<String> {
        let known: HashSet<&str> = all.iter().map(|s| s.id.as_str()).collect();
        let mut dead = self.dead.lock().unwrap_or_else(|e| e.into_inner());
        dead.keep_only(&known);
        if include_dead {
            return HashSet::new();
        }
        all.iter()
            .filter(|s| dead.contains(&s.id))
            .map(|s| s.id.clone())
            .collect()
    }
    /// What an event of a measurement says about the servers that did not answer: a failure puts
    /// the server in the list (once in a pass), an answer through it takes it out.
    fn note_dead(&self, event: &Progress, tracked: bool, failed_now: &mut HashSet<String>) -> bool {
        let code = match event {
            Progress::Endpoint(_, measurement)
            | Progress::Proxy(_, measurement)
            | Progress::Current(_, measurement) => measurement.failure_code,
            _ => None,
        };
        if matches!(
            code,
            Some(
                FailureCode::XrayCrashed
                    | FailureCode::XrayConfigRejected
                    | FailureCode::XrayConfigTimeout
                    | FailureCode::XrayStartExit
                    | FailureCode::XrayReadinessTimeout
                    | FailureCode::SidecarCrashed
                    | FailureCode::ElevationHelperCrashed
                    | FailureCode::ElevationChannel
            )
        ) {
            // Shared infrastructure/capability failure is not evidence against this remote.
            return true;
        }
        if !tracked || code == Some(FailureCode::UnsupportedLegacyParameters) {
            return false;
        }
        let mut dead = self.dead.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let mut failed = |dead: &mut dead::Dead, id: &String| {
            if failed_now.insert(id.clone()) {
                dead.failed(id, now, &self.jitter);
            }
        };
        match event {
            Progress::Endpoint(id, measurement) if measurement.latency_ms.is_none() => {
                failed(&mut dead, id);
            }
            Progress::Proxy(id, measurement) | Progress::Current(id, measurement) => {
                if measurement.latency_ms.is_some() {
                    dead.answered(id);
                } else {
                    failed(&mut dead, id);
                }
            }
            _ => {}
        }
        false
    }
    /// When each server that did not answer is asked again, for the screen.
    fn mark_dead(&self, s: &mut Snapshot) {
        let dead = self.dead.lock().unwrap_or_else(|e| e.into_inner());
        let (now, at) = (Instant::now(), timestamp());
        for v in s.servers.iter_mut() {
            v.retry_at = dead
                .next_at(&v.id)
                .map(|next| at + next.saturating_duration_since(now).as_secs());
        }
    }
    fn publish_dead(&self) {
        self.snapshots.send_modify(|s| self.mark_dead(s));
    }
    /// Asks the servers that did not answer again, those whose time has come, in a task of their
    /// own: it goes on beside a cycle of selection, with connections of its own.
    fn start_recheck(self: &Arc<Self>, inner: &Inner) {
        if !inner.connected || !inner.store.settings.auto_select {
            return;
        }
        // Cheap first: most of the time nobody is due.
        if self.nothing_due() {
            return;
        }
        self.start_recheck_of(
            &inner.store.auto_candidates(timestamp()),
            inner.probe.clone(),
            inner.store.settings.concurrency,
        );
    }
    fn nothing_due(&self) -> bool {
        self.recheck_running.load(Ordering::SeqCst)
            || self
                .dead
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .due(Instant::now())
                .is_empty()
    }
    /// The same, for `candidates` and a probe given: a cycle that is running looks at the clock
    /// itself (`recheck_during`), since the worker is busy inside it.
    fn start_recheck_of(
        self: &Arc<Self>,
        candidates: &[Server],
        probe: Arc<dyn Probe>,
        concurrency: usize,
    ) {
        if self.recheck_running.load(Ordering::SeqCst) {
            return;
        }
        let due = self
            .dead
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .due(Instant::now());
        if due.is_empty() {
            return;
        }
        let wanted: HashSet<&str> = due.iter().map(String::as_str).collect();
        let servers: Vec<Server> = candidates
            .iter()
            .filter(|s| wanted.contains(s.id.as_str()))
            .cloned()
            .collect();
        // Servers that are gone have nothing to be asked again about.
        {
            let present: HashSet<&str> = servers.iter().map(|s| s.id.as_str()).collect();
            let mut dead = self.dead.lock().unwrap_or_else(|e| e.into_inner());
            for id in due.iter().filter(|id| !present.contains(id.as_str())) {
                dead.answered(id);
            }
        }
        if servers.is_empty() || self.recheck_running.swap(true, Ordering::SeqCst) {
            return;
        }
        let token = self
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .dead
            .child_token();
        let app = self.clone();
        tokio::spawn(async move {
            app.recheck_dead(probe, servers, concurrency, token).await;
            app.recheck_running.store(false, Ordering::SeqCst);
        });
    }
    /// Never ends: while a cycle of selection runs (and the worker is inside it), the clock of the
    /// servers that did not answer is still looked at once a second. The cycle ends the wait.
    async fn recheck_during<T>(
        self: &Arc<Self>,
        candidates: &[Server],
        probe: Arc<dyn Probe>,
        concurrency: usize,
    ) -> T {
        loop {
            sleep(Duration::from_secs(1)).await;
            self.start_recheck_of(candidates, probe.clone(), concurrency);
        }
    }
    async fn recheck_dead(
        &self,
        probe: Arc<dyn Probe>,
        servers: Vec<Server>,
        concurrency: usize,
        token: CancellationToken,
    ) {
        let postpone = || {
            // Serialize with generation replacement/disconnect before touching the shared list.
            let _control = self.control.lock().unwrap_or_else(|e| e.into_inner());
            if token.is_cancelled() {
                return;
            }
            let ids: Vec<String> = servers.iter().map(|s| s.id.clone()).collect();
            self.dead
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .postponed(&ids, Instant::now(), &self.jitter);
        };
        // Without a network every server would look dead once more: wait as long as before.
        let up = tokio::select! {biased;
            _=token.cancelled()=>return,
            up=probe.network()=>up,
        };
        if !up.is_up() {
            postpone();
            self.publish_dead();
            return;
        }
        let (mut asked, mut back) = (0usize, 0usize);
        let history_check = self
            .history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .begin_check();
        let mut failed_now = HashSet::new();
        let pass = token.child_token();
        let mut infrastructure_failed = false;
        let result = selection::recheck(
            probe.as_ref(),
            &servers,
            concurrency,
            &self.jitter,
            &pass,
            |event| {
                if pass.is_cancelled() {
                    return;
                }
                if self.note_dead(&event, true, &mut failed_now) {
                    infrastructure_failed = true;
                    pass.cancel();
                    return;
                }
                self.history
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .record_check(&event, timestamp(), history_check);
                match &event {
                    Progress::Endpoint(..) => asked += 1,
                    Progress::Proxy(_, measurement) => {
                        back += usize::from(measurement.latency_ms.is_some());
                    }
                    _ => {}
                }
                self.snapshots.send_modify(|s| {
                    show_progress(s, &event);
                    self.mark_dead(s);
                });
            },
        )
        .await;
        if infrastructure_failed {
            // The remote did not fail: retry at its existing cadence, without another miss.
            postpone();
        }
        if !matches!(result, Err(Error::Cancelled)) {
            self.log_in(
                Category::Checks,
                "info",
                &tr!("core.application.recheck_done", asked = asked, back = back),
            );
        }
        self.publish_dead();
    }
    /// The cycle is over, whatever it did: the screen stops saying so and shows when the next one is.
    fn checking_ended(&self, inner: &Inner) {
        self.snapshots.send_modify(|s| {
            s.checking = false;
            s.next_check = inner.store.settings.auto_select.then(|| {
                timestamp()
                    + inner
                        .schedule
                        .deadline()
                        .saturating_duration_since(Instant::now())
                        .as_secs()
            });
        });
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.shutdown.cancel();
        self.cancel(true);
        let mut inner = self.inner.lock().await;
        self.stop_locked(&mut inner).await
    }
}

impl PoolState {
    /// The pool a full check left, with its ranking: its best servers and the delays they had.
    fn built(pool: &[pool::Ranked], ranking: &[pool::Ranked], servers: &[Server]) -> Self {
        let key_of = |id: &str| servers.iter().find(|s| s.id == id).map(Server::stable_key);
        let mut members = vec![];
        let mut bases = HashMap::new();
        for ranked in pool {
            if let Some(key) = key_of(&ranked.id) {
                bases.insert(key.clone(), ranked.ms);
                members.push(PoolMember { key });
            }
        }
        let ranking = ranking
            .iter()
            .filter_map(|ranked| {
                Some(KeyedRank {
                    key: key_of(&ranked.id)?,
                    host: ranked.host.clone(),
                    ms: ranked.ms,
                })
            })
            .collect();
        Self {
            target: members.len(),
            members,
            bases,
            ranking,
        }
    }
    /// The delay a server is compared with: its base, or its middle in the ranking of the last
    /// full check when it is not one of the pool.
    fn base_of(&self, key: &str) -> Option<u64> {
        self.bases
            .get(key)
            .copied()
            .or_else(|| self.ranking.iter().find(|r| r.key == key).map(|r| r.ms))
    }
    /// The pool without the servers kept out of Auto Select, made up again from the ranking of the
    /// last full check: the next best take the places, nobody is asked.
    fn without(&mut self, banned: &[String], size: usize) {
        if self.members.is_empty() {
            return;
        }
        let ranking: Vec<pool::Ranked> = self
            .ranking
            .iter()
            .filter(|r| !banned.contains(&r.key))
            .map(|r| pool::Ranked {
                id: r.key.clone(),
                host: r.host.clone(),
                ms: r.ms,
            })
            .collect();
        let rebuilt = pool::build(&ranking, size);
        self.members = rebuilt
            .iter()
            .map(|r| PoolMember { key: r.id.clone() })
            .collect();
        self.bases = rebuilt.iter().map(|r| (r.id.clone(), r.ms)).collect();
        self.target = self.members.len();
    }
}
/// The servers of the pool that are still among `servers`, best first. They are found by the key
/// of their address: a subscription may have given them other credentials since.
fn pool_members<'a>(pool: &PoolState, servers: &'a [Server]) -> Vec<&'a Server> {
    pool.members
        .iter()
        .filter_map(|member| servers.iter().find(|s| s.stable_key() == member.key))
        .collect()
}
/// The addresses of the servers one subscription holds.
fn source_keys(store: &Store, source: &str) -> HashSet<String> {
    store
        .servers
        .iter()
        .filter(|s| s.source.as_deref() == Some(source))
        .map(Server::stable_key)
        .collect()
}
/// The addresses Auto Select may use: what a refresh has to change for the list to be new.
fn list_keys(store: &Store) -> HashSet<String> {
    store
        .auto_candidates(timestamp())
        .iter()
        .map(Server::stable_key)
        .collect()
}
/// Where the selection goes when its server is gone from the list and nothing took its place: the
/// fastest server whose delay is known, otherwise the first.
fn fallback_selection(servers: &[Server], delay: impl Fn(&str) -> Option<u64>) -> Option<String> {
    servers
        .iter()
        .filter_map(|s| delay(&s.id).map(|ms| (ms, s)))
        .min_by_key(|(ms, _)| *ms)
        .map(|(_, s)| s.id.clone())
        .or_else(|| servers.first().map(|s| s.id.clone()))
}
/// The address of each subscription of the store, by id.
fn subscription_urls_of(store: &Store) -> HashMap<String, String> {
    store
        .subscriptions
        .iter()
        .map(|s| (s.id.clone(), s.url.clone()))
        .collect()
}
fn distinct_servers(servers: &[Server]) -> usize {
    servers
        .iter()
        .map(|s| s.id.as_str())
        .collect::<HashSet<_>>()
        .len()
}
/// A check that was asked for, not one the timer brings: it is owed again when it was cut short.
/// What started a check, as local operation metadata.
const fn trigger_of(reason: Reason) -> Trigger {
    match reason {
        Reason::Failover | Reason::CurrentLost => Trigger::Failover,
        Reason::CheckNow => Trigger::CheckNow,
        Reason::Connect => Trigger::Startup,
        Reason::AutoSelectOn => Trigger::Manual,
        Reason::Scheduled
        | Reason::ListChanged
        | Reason::Subscription
        | Reason::PoolDegraded
        | Reason::PoolLost
        | Reason::PoolIncomplete
        | Reason::SmallList
        | Reason::NetworkBack => Trigger::Scheduled,
    }
}
/// What a check was for, as local operation metadata.
const fn kind_of(reason: Reason) -> CycleKind {
    match reason {
        Reason::Failover | Reason::CurrentLost => CycleKind::Failover,
        Reason::CheckNow => CycleKind::CheckNow,
        _ => CycleKind::Initial,
    }
}
/// The error of a check that found no working server, with what the check had seen.
fn nothing_works(plan: Plan, tested: usize, reachable: usize, working: usize) -> Error {
    let count = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
    Error::failure(FailureCode::NoAvailableServer, t("core.error.unavailable"))
        .with_trigger(trigger_of(plan.reason))
        .with_selection(SelectionInfo {
            kind: kind_of(plan.reason),
            checked: count(tested),
            reachable: count(reachable),
            working: count(working),
        })
}
fn owed(plan: Plan) -> bool {
    plan.cycle == Cycle::Full && plan.reason != Reason::Scheduled
}
fn reason_text(reason: Reason) -> &'static str {
    match reason {
        Reason::Scheduled => t("core.application.reason_schedule"),
        Reason::Connect => t("core.application.reason_connect"),
        Reason::CheckNow => t("core.application.reason_request"),
        Reason::AutoSelectOn => t("core.application.reason_auto_select_on"),
        Reason::ListChanged => t("core.application.reason_servers_changed"),
        Reason::Subscription => t("core.application.reason_subscription_changed"),
        Reason::PoolDegraded => t("core.application.reason_best_worse"),
        Reason::PoolLost => t("core.application.reason_best_silent"),
        Reason::PoolIncomplete => t("core.application.reason_pool_short"),
        Reason::SmallList => t("core.application.reason_pool_covers_list"),
        Reason::CurrentLost => t("core.application.reason_used_silent"),
        Reason::Failover => t("core.application.reason_connection_lost"),
        Reason::NetworkBack => t("core.application.reason_network_back"),
    }
}
/// The journal line of what a full check decided about the server in use.
fn decision_text(decision: Decision, settings: &Settings) -> String {
    match decision {
        Decision::Chose { ms } => {
            tr!("core.application.pick_best", ms = ms)
        }
        Decision::Best { ms } => tr!("core.application.pick_stay_best", ms = ms),
        Decision::Kept { ms, best_ms } => tr!(
            "core.application.pick_stay_current",
            percent = settings.switch_threshold_percent,
            limit_ms = settings.switch_threshold_ms,
            ms = ms,
            best_ms = best_ms
        ),
        Decision::Faster { from_ms, to_ms } => {
            tr!(
                "core.application.pick_switch_faster",
                from_ms = from_ms,
                to_ms = to_ms
            )
        }
        Decision::Lost { to_ms } => {
            tr!("core.application.pick_switch_silent", to_ms = to_ms)
        }
        Decision::Unreliable { to_ms } => {
            tr!("core.application.pick_switch_unreliable", to_ms = to_ms)
        }
        Decision::Pool {
            from_ms: Some(from_ms),
            to_ms,
        } => tr!(
            "core.application.pick_switch_filling",
            from_ms = from_ms,
            to_ms = to_ms
        ),
        Decision::Pool {
            from_ms: None,
            to_ms,
        } => tr!("core.application.pick_best_filling", to_ms = to_ms),
    }
}
/// The journal line of a fast check, with the servers named by their place in the pool.
fn fast_text(pool: &PoolState, list: &[(Server, Option<u64>)], report: &fast::Report) -> String {
    let name = |server: &Server| -> String {
        match pool
            .members
            .iter()
            .position(|m| m.key == server.stable_key())
        {
            Some(i) => tr!(
                "core.application.place_pool",
                index = i + 1,
                total = pool.members.len()
            ),
            None => t("core.application.place_selected_manually").to_owned(),
        }
    };
    if report.current_lost {
        let place = report
            .readings
            .first()
            .and_then(|r| list.iter().find(|(s, _)| s.id == r.id))
            .map_or_else(String::new, |(s, _)| format!(" ({})", name(s)));
        return tr!("core.application.fast_used_silent", place = place);
    }
    let mut problems = vec![];
    let (mut delays, mut bases) = (vec![], vec![]);
    for (server, base) in list {
        let Some(reading) = report.readings.iter().find(|r| r.id == server.id) else {
            continue;
        };
        let base_text = base.map_or_else(|| "—".to_owned(), |b| b.to_string());
        match reading.judgement.health {
            pool::Health::Fine => {
                delays.push(
                    reading
                        .judgement
                        .median_ms
                        .map_or_else(|| "—".to_owned(), |m| m.to_string()),
                );
                bases.push(base_text);
            }
            pool::Health::Worse { ms, limit_ms } => problems.push(tr!(
                "core.application.worse",
                server = name(server),
                ms = ms,
                base_text = base_text,
                limit_ms = limit_ms
            )),
            pool::Health::Lost => {
                problems.push(tr!("core.application.silent", server = name(server)))
            }
        }
    }
    if problems.is_empty() {
        tr!(
            "core.application.fast_ok",
            ok = delays.len(),
            total = list.len(),
            delays = delays.join(" / "),
            bases = bases.join(" / ")
        )
    } else {
        tr!(
            "core.application.fast_problems",
            problems = problems.join("; ")
        )
    }
}
/// What an event of a measurement changes on the screen.
fn show_progress(s: &mut Snapshot, event: &Progress) {
    match event {
        Progress::Endpoint(id, measurement) => {
            for v in s.servers.iter_mut().filter(|v| &v.id == id) {
                v.endpoint_ms = measurement.latency_ms;
                v.proxy_ms = None;
                v.failure_code = measurement.failure_code;
                v.status = if measurement.latency_ms.is_some() {
                    "reachable"
                } else {
                    "unreachable"
                }
                .into();
                v.checked_at = Some(timestamp());
            }
        }
        Progress::Proxy(id, measurement) | Progress::Current(id, measurement) => {
            for v in s.servers.iter_mut().filter(|v| &v.id == id) {
                v.proxy_ms = measurement.latency_ms;
                v.failure_code = measurement.failure_code;
                v.status = if measurement.latency_ms.is_some() {
                    "online"
                } else {
                    "failed"
                }
                .into();
                v.checked_at = Some(timestamp());
            }
            if measurement.latency_ms.is_some() && s.selected.as_ref() == Some(id) {
                s.status = "connected".into();
            }
        }
        Progress::Median {
            id,
            proxy_ms,
            endpoint_ms,
        } => {
            for v in s.servers.iter_mut().filter(|v| &v.id == id) {
                v.proxy_ms = Some(*proxy_ms);
                if endpoint_ms.is_some() {
                    v.endpoint_ms = *endpoint_ms;
                }
            }
        }
        Progress::SelectionFailed(id, failure_code) => {
            for v in s.servers.iter_mut().filter(|v| &v.id == id) {
                v.failure_code = Some(*failure_code);
                v.status = "failed".into();
                v.checked_at = Some(timestamp());
            }
        }
        Progress::Selected(id, _) => {
            s.selected = Some(id.clone());
            s.status = "connected".into();
        }
    }
}
/// The user refused the Windows prompt for the TUN helper.
fn is_declined(error: &Error) -> bool {
    error.failure_code() == Some(FailureCode::ElevationDeclined)
}
/// The settings `NetworkProbe` reads: it is rebuilt when one of them changes.
fn probe_inputs(settings: &Settings) -> (u16, u64, u64, &str) {
    (
        settings.api_port,
        settings.endpoint_timeout_ms,
        settings.proxy_timeout_ms,
        &settings.connectivity_url,
    )
}

fn failure_counts(
    codes: impl Iterator<Item = Option<FailureCode>>,
) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for code in codes.flatten() {
        *counts.entry(code.as_str()).or_insert(0) += 1;
    }
    counts
}

/// Delay before the next subscription refresh. A failure backs off from a 1-minute base,
/// doubling per consecutive failed cycle and capped at `full_interval`: a transient outage is
/// retried soon, while a subscription that never recovers settles back to the normal cadence
/// instead of retrying forever at a fixed short period.
/// What the settings show about the databases.
fn geodata_view(
    geo: &GeoCache,
    (sites, ips): &(Vec<String>, Vec<String>),
    available: bool,
    plan: &GeoPlan,
) -> GeodataView {
    let db = |kind| {
        geo.info(kind).map(|info| DbView {
            updated_at: info.updated_at,
            bytes: info.bytes,
            checked_at: plan.checked.get(&kind).copied(),
        })
    };
    GeodataView {
        geosite: db(geodata::Kind::Geosite),
        geoip: db(geodata::Kind::Geoip),
        updating: plan.updating,
        current: plan.current.map(|kind| kind.name().to_owned()),
        uses_geosite: !sites.is_empty(),
        uses_geoip: !ips.is_empty(),
        failure_code: plan.failure,
        available,
        wanted: !sites.is_empty() || !ips.is_empty(),
    }
}
/// How often one subscription is refreshed: its own interval, or the global one.
fn source_interval(source: &Subscription, global: Duration) -> Duration {
    source
        .interval_mins
        .map_or(global, |mins| Duration::from_secs(mins * 60))
}
fn subscription_backoff(consecutive_failures: u32, full_interval: Duration) -> Duration {
    if consecutive_failures == 0 {
        return full_interval;
    }
    // Shift capped at 14 (60 * 2^14s ≈ 273h) so persistent failures can still reach
    // Settings::validate's maximum subscription_interval_mins of 10_080 (168h), not just
    // plateau short of it.
    let backoff = Duration::from_secs(60) * (1u32 << (consecutive_failures - 1).min(14));
    backoff.min(full_interval)
}
/// The server the `main` selector runs for this Store.
fn live_server(store: &Store) -> Option<Server> {
    let id = store.live_selection();
    store.available_servers().into_iter().find(|s| s.id == id)
}
/// Moves traffic to server `id`: the selector, then the rules that server brings along. The
/// rules come from `store`, which is the one the server is chosen in.
async fn switch_live(probe: &dyn Probe, store: &Store, id: &str) -> Result<()> {
    probe.select(id).await?;
    probe
        .routes(
            &store
                .routes_mode_of(id)
                .unwrap_or_else(|| NO_ROUTES_MODE.to_owned()),
        )
        .await
}

fn os_version() -> String {
    #[cfg(windows)]
    {
        let v = windows_version::OsVersion::current();
        format!("Windows {}.{}.{}", v.major, v.minor, v.build)
    }
    #[cfg(not(windows))]
    {
        std::env::consts::OS.into()
    }
}

#[cfg(test)]
mod tests {
    use super::{fallback_selection, subscription_backoff};
    use crate::parser::parse_uri;
    use std::time::Duration;

    #[test]
    fn the_selection_that_lost_its_server_goes_to_the_fastest_known_else_the_first() {
        let server =
            |port: u16| parse_uri(&format!("trojan://p@h{port}.example.com:{port}"), None).unwrap();
        let list = [server(1), server(2), server(3)];
        let ids: Vec<String> = list.iter().map(|s| s.id.clone()).collect();
        let delay = |known: &'static [(usize, u64)]| {
            let ids = ids.clone();
            move |id: &str| known.iter().find(|(i, _)| ids[*i] == id).map(|(_, ms)| *ms)
        };
        assert_eq!(
            fallback_selection(&list, delay(&[(1, 90), (2, 40)])),
            Some(ids[2].clone()),
            "the fastest that was measured"
        );
        assert_eq!(
            fallback_selection(&list, delay(&[])),
            Some(ids[0].clone()),
            "nothing measured: the first"
        );
        assert_eq!(fallback_selection(&[], delay(&[])), None);
    }

    #[test]
    fn subscription_backoff_is_disabled_without_a_failure() {
        let full = Duration::from_secs(3600);
        assert_eq!(subscription_backoff(0, full), full);
    }

    #[test]
    fn subscription_backoff_doubles_per_consecutive_failure() {
        let full = Duration::from_secs(3600);
        assert_eq!(subscription_backoff(1, full), Duration::from_secs(60));
        assert_eq!(subscription_backoff(2, full), Duration::from_secs(120));
        assert_eq!(subscription_backoff(3, full), Duration::from_secs(240));
    }

    #[test]
    fn subscription_backoff_never_exceeds_the_configured_interval() {
        let full = Duration::from_secs(3600);
        assert_eq!(
            subscription_backoff(7, full),
            full,
            "60 * 2^6 = 3840 > 3600"
        );
        assert_eq!(
            subscription_backoff(50, full),
            full,
            "stays capped, no overflow"
        );
    }

    #[test]
    fn subscription_backoff_never_exceeds_a_short_interval_either() {
        // subscription_interval_mins can be as low as 1 minute (model::Settings::validate).
        let full = Duration::from_secs(60);
        assert_eq!(subscription_backoff(1, full), full);
        assert_eq!(subscription_backoff(5, full), full);
    }

    #[test]
    fn subscription_backoff_reaches_the_configured_maximum_interval() {
        // model::Settings::validate allows subscription_interval_mins up to 10_080 (7 days).
        // Persistent failures must eventually settle at that cadence, not plateau below it.
        let full = Duration::from_secs(10_080 * 60);
        assert_ne!(
            subscription_backoff(11, full),
            full,
            "sanity: 11 failures must not already be capped by the interval"
        );
        assert_eq!(
            subscription_backoff(15, full),
            full,
            "60 * 2^14s ≈ 273h exceeds the 168h maximum interval"
        );
    }
}

#[cfg(test)]
mod infrastructure_tests {
    use super::*;
    pub(super) struct NoCore;
    #[async_trait::async_trait]
    impl Runtime for NoCore {
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
    #[tokio::test]
    async fn shared_core_and_capability_failures_never_quarantine_remote_servers() {
        let directory = tempfile::tempdir().unwrap();
        let app =
            Application::new(Store::default(), directory.path().into(), Box::new(NoCore)).unwrap();
        for code in [
            FailureCode::XrayCrashed,
            FailureCode::XrayReadinessTimeout,
            FailureCode::ElevationHelperCrashed,
            FailureCode::UnsupportedLegacyParameters,
        ] {
            for _ in 0..2 {
                app.note_dead(
                    &Progress::Proxy(
                        "synthetic".into(),
                        selection::Measurement {
                            latency_ms: None,
                            failure_code: Some(code),
                        },
                    ),
                    true,
                    &mut HashSet::new(),
                );
            }
        }
        assert_eq!(app.dead.lock().unwrap().misses("synthetic"), 0);
        // Negative control: two real remote failures still put the server in the dead list.
        for _ in 0..2 {
            app.note_dead(
                &Progress::Proxy(
                    "synthetic".into(),
                    selection::Measurement {
                        latency_ms: None,
                        failure_code: Some(FailureCode::ProxyTimeout),
                    },
                ),
                true,
                &mut HashSet::new(),
            );
        }
        assert!(app.dead.lock().unwrap().contains("synthetic"));
    }
}

#[cfg(test)]
mod panic_journal_tests {
    use super::infrastructure_tests::NoCore;
    use super::*;

    fn app(directory: &Path) -> Arc<Application> {
        let mut store = Store::default();
        store
            .servers
            .push(parse_uri("trojan://panic-canary@panic.example:443", None).unwrap());
        Application::new(store, directory.to_owned(), Box::new(NoCore)).unwrap()
    }

    fn saved(directory: &Path) -> Vec<Entry> {
        serde_json::from_slice(&std::fs::read(directory.join("journal.json")).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn live_panic_survives_a_normal_append_and_explicit_export() {
        let directory = tempfile::tempdir().unwrap();
        let app = app(directory.path());
        app.log_external_error("previous event", None);
        let detail = app.log_panic(
            "background panic",
            "worker failed; stage=watch; field=settings.vnext; panic-canary",
        );
        assert!(detail.contains("worker failed"));
        assert!(!detail.contains("panic-canary"));
        app.log_external_error("normal event after panic", None);
        let entries = app.snapshot().journal;
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.crash_id.is_some())
                .count(),
            1
        );
        assert_eq!(saved(directory.path()), entries);
        app.execute(Action::ExportDiagnostics).await.unwrap();
        let bytes = std::fs::read(directory.path().join("diagnostics.json")).unwrap();
        let report: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let journal = report["journal"].as_array().unwrap();
        assert_eq!(
            journal
                .iter()
                .filter(|entry| entry.get("crash_id").is_some())
                .count(),
            1
        );
        assert!(journal
            .iter()
            .any(|entry| entry["message"] == "normal event after panic"));
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("worker failed"));
        assert!(!text.contains("panic-canary"));
    }

    #[tokio::test]
    async fn explicit_export_recovers_a_pending_panic_without_an_unrelated_append() {
        let directory = tempfile::tempdir().unwrap();
        let app = app(directory.path());
        app.log_panic("background panic", "cause recovered by explicit export");
        app.execute(Action::ExportDiagnostics).await.unwrap();
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.path().join("diagnostics.json")).unwrap(),
        )
        .unwrap();
        assert!(report["journal"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| { entry["details"] == "cause recovered by explicit export" }));
        assert_eq!(
            app.snapshot()
                .journal
                .iter()
                .filter(|entry| entry.crash_id.is_some())
                .count(),
            1
        );
        assert!(!journal::has_pending_crashes(directory.path()).unwrap());
    }

    #[tokio::test]
    async fn native_error_consumer_recovery_updates_the_live_journal_without_another_error() {
        let directory = tempfile::tempdir().unwrap();
        let app = app(directory.path());
        app.log_panic("background panic", "actual recoverable cause");
        assert!(app.snapshot().journal.is_empty());
        app.recover_local_errors().unwrap();
        let entries = app.snapshot().journal;
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].details.as_deref(),
            Some("actual recoverable cause")
        );
        assert_eq!(saved(directory.path()), entries);
        app.recover_local_errors().unwrap();
        assert_eq!(app.snapshot().journal, entries);
    }

    #[tokio::test]
    async fn panic_capture_never_reenters_the_live_journal_or_snapshot_locks() {
        let directory = tempfile::tempdir().unwrap();
        let app = app(directory.path());
        let worker = app.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _journal = worker.journal_gate.lock().unwrap();
            worker.snapshots.send_modify(|_| {
                let detail = worker.log_panic("background panic", "cause while snapshot is held");
                send.send(detail).unwrap();
            });
        });
        let detail = receive
            .recv_timeout(Duration::from_secs(2))
            .expect("panic hook must not wait on live locks");
        thread.join().unwrap();
        assert_eq!(detail, "cause while snapshot is held");
        app.recover_local_errors().unwrap();
        assert_eq!(
            app.snapshot().journal[0].details.as_deref(),
            Some(detail.as_str())
        );
    }

    #[tokio::test]
    async fn a_reentrant_busy_sanitizer_fails_closed_without_blocking() {
        let directory = tempfile::tempdir().unwrap();
        let app = app(directory.path());
        let worker = app.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _secrets = worker.journal_secrets.lock().unwrap();
            let detail = worker.log_panic("panic-canary", "unlabelled panic-canary");
            send.send(detail).unwrap();
        });
        assert_eq!(
            receive
                .recv_timeout(Duration::from_secs(2))
                .expect("busy sanitizer must not block"),
            "[redacted]",
        );
        thread.join().unwrap();
        app.recover_local_errors().unwrap();
        let bytes = std::fs::read(directory.path().join("journal.json")).unwrap();
        assert!(!String::from_utf8(bytes).unwrap().contains("panic-canary"));
        assert_eq!(
            app.snapshot().journal[0].details.as_deref(),
            Some("[redacted]")
        );
    }

    #[tokio::test]
    async fn startup_recovers_a_panic_not_yet_seen_by_the_native_error_consumer() {
        let directory = tempfile::tempdir().unwrap();
        let original = app(directory.path());
        original.log_panic("background panic", "cause before restart");
        drop(original);
        let restarted = app(directory.path());
        assert_eq!(restarted.snapshot().journal.len(), 1);
        assert_eq!(
            restarted.snapshot().journal[0].details.as_deref(),
            Some("cause before restart")
        );
        assert_eq!(saved(directory.path()), restarted.snapshot().journal);
        assert!(!journal::has_pending_crashes(directory.path()).unwrap());
    }

    #[tokio::test]
    async fn failed_live_persistence_does_not_acknowledge_or_lose_a_pending_panic() {
        let directory = tempfile::tempdir().unwrap();
        let app = app(directory.path());
        app.log_external_error("previous event", None);
        app.log_panic("background panic", "cause before persistence failure");
        let path = directory.path().join("journal.json");
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        app.log_external_error("normal event while disk is unavailable", None);
        assert!(app.recover_local_errors().is_err());
        assert!(journal::has_pending_crashes(directory.path()).unwrap());
        std::fs::remove_dir(&path).unwrap();
        app.recover_local_errors().unwrap();
        let entries = saved(directory.path());
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.crash_id.is_some())
                .count(),
            1
        );
        assert!(entries
            .iter()
            .any(|entry| entry.message == "normal event while disk is unavailable"));
        assert!(!journal::has_pending_crashes(directory.path()).unwrap());
        app.execute(Action::ExportDiagnostics).await.unwrap();
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.path().join("diagnostics.json")).unwrap(),
        )
        .unwrap();
        assert!(report["journal"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| { entry["details"] == "cause before persistence failure" }));
    }
}
