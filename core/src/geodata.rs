//! Lists of domains (geosite) and address ranges (geoip) by name, from the databases Rift
//! downloads after its first start (`Loyalsoldier/v2ray-rules-dat`, built from
//! `v2fly/domain-list-community` and country data). The databases are not part of the
//! installation: until they are on disk, rules that refer to them are left out of the config.
//!
//! A database is a protobuf file (`geosite.dat`, `geoip.dat`) of a few thousand lists. It is
//! read here by a small decoder of its own (nothing from the file reaches sing-box as text or
//! as a path), and only the lists a routing profile asks for are decoded.
use crate::text::t;
use crate::{failure::FailureCode, model::Store, persistence, routing::Routing, Error, Result};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    net::{Ipv4Addr, Ipv6Addr},
    ops::Range,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Largest database file accepted (the real ones are 11 and 17 MB).
pub const MAX_DB_BYTES: usize = 64 * 1024 * 1024;
/// Most domains or ranges taken from one list.
const MAX_ITEMS: usize = 500_000;

/// A geosite list: domains by kind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SiteList {
    /// The domain and its subdomains.
    pub suffix: Vec<String>,
    /// Exactly this domain.
    pub full: Vec<String>,
    pub keyword: Vec<String>,
    pub regex: Vec<String>,
}
impl SiteList {
    pub fn len(&self) -> usize {
        self.suffix.len() + self.full.len() + self.keyword.len() + self.regex.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The lists a config refers to, read from the databases: by name (geosite) and by country
/// code (geoip). A name that is not here has no database or is not in it.
#[derive(Clone, Debug, Default)]
pub struct GeoSet {
    pub sites: HashMap<String, SiteList>,
    /// Address ranges, `a.b.c.d/n` or IPv6.
    pub ips: HashMap<String, Vec<String>>,
}
impl GeoSet {
    /// No databases: rules that need them are not applied.
    pub fn none() -> Self {
        Self::default()
    }
}

/// The two databases.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Geosite,
    Geoip,
}
impl Kind {
    pub const ALL: [Kind; 2] = [Kind::Geosite, Kind::Geoip];
    pub const fn file(self) -> &'static str {
        match self {
            Kind::Geosite => "geosite.dat",
            Kind::Geoip => "geoip.dat",
        }
    }
    /// The database as the settings and the journal call it.
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Geosite => "geosite",
            Kind::Geoip => "geoip",
        }
    }
}

fn damaged() -> Error {
    Error::failure(FailureCode::GeodataInvalid, t("core.geodata.corrupted"))
}

// ---- the protobuf of a database ---------------------------------------------------------------

enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}
fn varint(buf: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let byte = *buf.get(*at)?;
        *at += 1;
        if shift == 63 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}
/// Walks the fields of one message; `visit` returns `false` to stop early.
fn fields<'a>(
    buf: &'a [u8],
    mut visit: impl FnMut(u32, Wire<'a>, Range<usize>) -> bool,
) -> Result<()> {
    let mut at = 0;
    while at < buf.len() {
        let key = varint(buf, &mut at).ok_or_else(damaged)?;
        let number = u32::try_from(key >> 3).map_err(|_| damaged())?;
        match key & 7 {
            0 => {
                let start = at;
                let value = varint(buf, &mut at).ok_or_else(damaged)?;
                if !visit(number, Wire::Varint(value), start..at) {
                    return Ok(());
                }
            }
            1 => {
                at = at
                    .checked_add(8)
                    .filter(|end| *end <= buf.len())
                    .ok_or_else(damaged)?
            }
            5 => {
                at = at
                    .checked_add(4)
                    .filter(|end| *end <= buf.len())
                    .ok_or_else(damaged)?
            }
            2 => {
                let length = usize::try_from(varint(buf, &mut at).ok_or_else(damaged)?)
                    .map_err(|_| damaged())?;
                let end = at
                    .checked_add(length)
                    .filter(|end| *end <= buf.len())
                    .ok_or_else(damaged)?;
                if !visit(number, Wire::Bytes(&buf[at..end]), at..end) {
                    return Ok(());
                }
                at = end;
            }
            _ => return Err(damaged()),
        }
    }
    Ok(())
}

/// Where each list of the file lies: its code (upper case) and the bytes of its entry.
fn scan_index(bytes: &[u8]) -> Result<HashMap<String, Range<usize>>> {
    let mut index = HashMap::new();
    let mut failure = None;
    fields(bytes, |number, wire, range| {
        let (1, Wire::Bytes(entry)) = (number, wire) else {
            return true;
        };
        let mut code = None;
        let inner = fields(entry, |number, wire, _| {
            if let (1, Wire::Bytes(text)) = (number, wire) {
                code = std::str::from_utf8(text).ok().map(str::to_ascii_uppercase);
                return false;
            }
            true
        });
        match (inner, code) {
            (Ok(()), Some(code)) if !code.is_empty() => {
                index.insert(code, range);
            }
            (Err(error), _) => failure = Some(error),
            _ => {}
        }
        failure.is_none()
    })?;
    match failure {
        Some(error) => Err(error),
        None if index.is_empty() => Err(damaged()),
        None => Ok(index),
    }
}

/// A domain from a database: lowercase ASCII, nothing a sing-box rule could choke on.
fn site_name(value: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(value).ok()?;
    let fine = !text.is_empty()
        && text.len() <= 253
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_');
    fine.then(|| text.to_ascii_lowercase())
}
fn decode_site(entry: &[u8]) -> Result<SiteList> {
    let mut list = SiteList::default();
    let mut failure = None;
    fields(entry, |number, wire, _| {
        let (2, Wire::Bytes(domain)) = (number, wire) else {
            return true;
        };
        let (mut kind, mut value) = (0, None);
        let inner = fields(domain, |number, wire, _| {
            match (number, wire) {
                (1, Wire::Varint(code)) => kind = code,
                (2, Wire::Bytes(text)) => value = Some(text),
                _ => {}
            }
            true
        });
        if let Err(error) = inner {
            failure = Some(error);
            return false;
        }
        if let Some(text) = value {
            match kind {
                // Plain, regex, domain (suffix), full: the order of the protobuf enum.
                0 => {
                    if let Some(word) = site_name(text) {
                        list.keyword.push(word);
                    }
                }
                1 => {
                    if let Ok(expression) = std::str::from_utf8(text) {
                        if !expression.is_empty() && expression.len() <= 256 {
                            list.regex.push(expression.to_owned());
                        }
                    }
                }
                2 => list.suffix.extend(site_name(text)),
                3 => list.full.extend(site_name(text)),
                _ => {}
            }
        }
        list.len() <= MAX_ITEMS
    })?;
    failure.map_or(Ok(list), Err)
}
fn decode_ips(entry: &[u8]) -> Result<Vec<String>> {
    let mut ranges = Vec::new();
    let mut failure = None;
    fields(entry, |number, wire, _| {
        let (2, Wire::Bytes(cidr)) = (number, wire) else {
            return true;
        };
        let (mut ip, mut prefix) = (None, 0u64);
        let inner = fields(cidr, |number, wire, _| {
            match (number, wire) {
                (1, Wire::Bytes(bytes)) => ip = Some(bytes),
                (2, Wire::Varint(bits)) => prefix = bits,
                _ => {}
            }
            true
        });
        if let Err(error) = inner {
            failure = Some(error);
            return false;
        }
        match ip {
            Some(b) if b.len() == 4 && prefix <= 32 => {
                ranges.push(format!(
                    "{}/{prefix}",
                    Ipv4Addr::new(b[0], b[1], b[2], b[3])
                ));
            }
            Some(b) if b.len() == 16 && prefix <= 128 => {
                let octets: [u8; 16] = b.try_into().unwrap_or_default();
                ranges.push(format!("{}/{prefix}", Ipv6Addr::from(octets)));
            }
            _ => {}
        }
        ranges.len() <= MAX_ITEMS
    })?;
    failure.map_or(Ok(ranges), Err)
}

// ---- the databases on disk -----------------------------------------------------------------

/// A database file that has been scanned: where its lists are.
struct Loaded {
    /// Size and time of the file when it was scanned: a file replaced since is scanned again.
    stamp: (u64, u128),
    index: HashMap<String, Range<usize>>,
}
/// What the settings show about a database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    /// Unix seconds the file was written.
    pub updated_at: u64,
    pub bytes: u64,
}

/// The databases in a folder, read on demand. Several caches over one folder agree: a file
/// replaced by one is noticed by the others.
pub struct GeoCache {
    directory: PathBuf,
    loaded: Mutex<HashMap<Kind, Option<Loaded>>>,
}
impl GeoCache {
    pub fn open(directory: PathBuf) -> Self {
        Self {
            directory,
            loaded: Mutex::new(HashMap::new()),
        }
    }
    pub fn path(&self, kind: Kind) -> PathBuf {
        self.directory.join(kind.file())
    }
    fn stamp(path: &Path) -> Option<(u64, u128)> {
        let meta = std::fs::metadata(path).ok()?;
        let modified = meta
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos();
        Some((meta.len(), modified))
    }
    /// The index of a database, scanning the file if it is new or changed. `None`: no file, or
    /// one that is not a database.
    fn with_index<T>(
        &self,
        kind: Kind,
        use_index: impl FnOnce(&HashMap<String, Range<usize>>) -> T,
    ) -> Option<T> {
        let path = self.path(kind);
        let stamp = Self::stamp(&path)?;
        let mut loaded = self.loaded.lock().unwrap_or_else(|e| e.into_inner());
        let fresh = matches!(loaded.get(&kind), Some(Some(known)) if known.stamp == stamp);
        if !fresh {
            let scanned = std::fs::read(&path)
                .ok()
                .filter(|bytes| bytes.len() <= MAX_DB_BYTES)
                .and_then(|bytes| scan_index(&bytes).ok())
                .map(|index| Loaded { stamp, index });
            loaded.insert(kind, scanned);
        }
        loaded
            .get(&kind)
            .and_then(Option::as_ref)
            .map(|known| use_index(&known.index))
    }
    fn read_entry(&self, kind: Kind, range: &Range<usize>) -> Option<Vec<u8>> {
        let mut file = std::fs::File::open(self.path(kind)).ok()?;
        file.seek(SeekFrom::Start(range.start as u64)).ok()?;
        let mut entry = vec![0; range.len()];
        file.read_exact(&mut entry).ok()?;
        Some(entry)
    }
    /// The database is on disk and readable.
    pub fn has(&self, kind: Kind) -> bool {
        self.with_index(kind, |_| ()).is_some()
    }
    pub fn info(&self, kind: Kind) -> Option<Info> {
        self.has(kind).then_some(())?;
        let meta = std::fs::metadata(self.path(kind)).ok()?;
        let updated_at = meta
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs();
        Some(Info {
            updated_at,
            bytes: meta.len(),
        })
    }
    fn checked_path(&self, kind: Kind) -> PathBuf {
        self.directory.join(format!("{}.checked", kind.file()))
    }
    /// When the source last said that this database is the latest (Unix seconds), kept beside the
    /// file so that a restart does not forget it. `None`: never, or not a time.
    pub fn checked_at(&self, kind: Kind) -> Option<u64> {
        std::fs::read_to_string(self.checked_path(kind))
            .ok()?
            .trim()
            .parse()
            .ok()
    }
    pub fn set_checked_at(&self, kind: Kind, at: u64) -> Result<()> {
        std::fs::create_dir_all(&self.directory)?;
        crate::persistence::atomic_write(&self.checked_path(kind), at.to_string().as_bytes())
    }
    /// The SHA-256 of the installed database in lowercase hex, the version a source compares with
    /// the latest one. `None`: there is no database.
    pub fn digest(&self, kind: Kind) -> Option<String> {
        self.has(kind).then_some(())?;
        let bytes = std::fs::read(self.path(kind)).ok()?;
        Some(format!("{:x}", Sha256::digest(bytes)))
    }
    /// Whether the database knows a list. `None`: there is no database to ask.
    pub fn knows(&self, kind: Kind, name: &str) -> Option<bool> {
        let code = name.to_ascii_uppercase();
        self.with_index(kind, |index| index.contains_key(&code))
    }
    /// The lists a routing profile (the lists of it that are applied) refers to, as far as the
    /// databases have them.
    pub fn resolve(&self, routing: &Routing) -> GeoSet {
        let (sites, ips) = routing.geo_names();
        self.resolve_names(sites, ips)
    }
    /// The lists a store refers to: its routing profile and the rules its servers bring.
    pub fn resolve_store(&self, store: &Store) -> GeoSet {
        let (sites, ips) = store.geo_names();
        self.resolve_names(sites, ips)
    }
    fn resolve_names(&self, sites: Vec<String>, ips: Vec<String>) -> GeoSet {
        let mut set = GeoSet::none();
        for name in sites {
            let range = self
                .with_index(Kind::Geosite, |index| {
                    index.get(&name.to_ascii_uppercase()).cloned()
                })
                .flatten();
            let decoded = range
                .and_then(|range| self.read_entry(Kind::Geosite, &range))
                .and_then(|entry| decode_site(&entry).ok());
            if let Some(list) = decoded {
                set.sites.insert(name, list);
            }
        }
        for code in ips {
            let range = self
                .with_index(Kind::Geoip, |index| {
                    index.get(&code.to_ascii_uppercase()).cloned()
                })
                .flatten();
            let decoded = range
                .and_then(|range| self.read_entry(Kind::Geoip, &range))
                .and_then(|entry| decode_ips(&entry).ok());
            if let Some(ranges) = decoded {
                set.ips.insert(code, ranges);
            }
        }
        set
    }
    /// The entries of the applied lists that name a list the (present) database does not have.
    pub fn unknown_names(&self, routing: &Routing) -> Vec<String> {
        let (sites, ips) = routing.geo_names();
        let mut unknown = Vec::new();
        for name in sites {
            if self.knows(Kind::Geosite, &name) == Some(false) {
                unknown.push(format!("geosite:{name}"));
            }
        }
        for code in ips {
            if self.knows(Kind::Geoip, &code) == Some(false) {
                unknown.push(format!("geoip:{code}"));
            }
        }
        unknown
    }
    /// Puts a downloaded database in place, atomically; a file that is not a database is
    /// refused and the one that is there stays.
    pub fn install(&self, kind: Kind, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_DB_BYTES {
            return Err(damaged());
        }
        scan_index(bytes)?;
        persistence::atomic_write(&self.path(kind), bytes)
            .map_err(|error| error.with_code(FailureCode::Persistence))?;
        self.loaded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&kind);
        Ok(())
    }
}

// ---- downloading -----------------------------------------------------------------------------

/// What a source answers to a request for the latest database.
#[derive(Debug, PartialEq, Eq)]
pub enum Fetched {
    /// The installed database is the latest one: nothing was downloaded.
    Same,
    /// The verified bytes of the latest database.
    Latest(Vec<u8>),
}

/// Where the databases come from.
#[async_trait]
pub trait GeoSource: Send + Sync {
    /// The latest database, or `Same` when `installed` (the SHA-256 of the file on disk) is already
    /// it. `proxy_port`: `None` fetches direct, `Some(port)` through the local proxy of the active
    /// server.
    async fn fetch(
        &self,
        kind: Kind,
        proxy_port: Option<u16>,
        installed: Option<&str>,
    ) -> Result<Fetched>;
}

const RELEASES: &str = "https://github.com/Loyalsoldier/v2ray-rules-dat/releases/latest/download";
/// A connection that is not made in this time is not going to be.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// A transfer that brings nothing for this long has stalled; one that keeps bringing bytes,
/// however slowly, is let finish (GitHub is slow on some networks).
const STALL_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest one request may take altogether.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// A `.sha256sum` file is one line.
const MAX_SUM_BYTES: usize = 1024;

pub struct HttpGeoSource {
    base: String,
    https_only: bool,
    stall: Duration,
}
impl HttpGeoSource {
    pub fn new() -> Self {
        Self::with_base(RELEASES.into(), true)
    }
    /// A source elsewhere; `https_only: false` only for tests on a loopback listener.
    pub fn with_base(base: String, https_only: bool) -> Self {
        Self {
            base,
            https_only,
            stall: STALL_TIMEOUT,
        }
    }
    pub fn for_tests(base: String) -> Self {
        Self::with_base(base, false)
    }
    /// How long a transfer may bring nothing before it counts as stalled.
    pub fn with_stall(mut self, stall: Duration) -> Self {
        self.stall = stall;
        self
    }
    fn client(&self, proxy_port: Option<u16>) -> Result<reqwest::Client> {
        let failed = || {
            Error::failure(
                FailureCode::GeodataDownload,
                t("core.geodata.download_setup_failed"),
            )
        };
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("Rift/", env!("CARGO_PKG_VERSION")))
            .https_only(self.https_only)
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(self.stall)
            .timeout(FETCH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(5));
        builder = match proxy_port {
            Some(port) => builder.proxy(
                reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).map_err(|_| failed())?,
            ),
            None => builder.no_proxy(),
        };
        builder.build().map_err(|_| failed())
    }
}
impl Default for HttpGeoSource {
    fn default() -> Self {
        Self::new()
    }
}
fn download_failed(error: &reqwest::Error) -> Error {
    Error::failure(
        FailureCode::GeodataDownload,
        if error.is_timeout() {
            t("core.geodata.download_timeout")
        } else {
            t("core.geodata.download_failed")
        },
    )
}
/// The body of a response, at most `limit` bytes: the declared size is checked first and the
/// stream is cut as soon as it crosses the limit.
async fn body(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    let too_large = || Error::failure(FailureCode::GeodataInvalid, t("core.geodata.too_large"));
    let mut response = response.error_for_status().map_err(|_| {
        Error::failure(FailureCode::GeodataDownload, t("core.geodata.server_error"))
    })?;
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| download_failed(&e))? {
        if bytes.len() + chunk.len() > limit {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
/// The digest of a `.sha256sum` file: its first word, 64 hex digits.
fn expected_digest(sum: &[u8]) -> Option<String> {
    let word = std::str::from_utf8(sum).ok()?.split_whitespace().next()?;
    (word.len() == 64 && word.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| word.to_ascii_lowercase())
}
#[async_trait]
impl GeoSource for HttpGeoSource {
    async fn fetch(
        &self,
        kind: Kind,
        proxy_port: Option<u16>,
        installed: Option<&str>,
    ) -> Result<Fetched> {
        let client = self.client(proxy_port)?;
        let url = format!("{}/{}", self.base, kind.file());
        let sum = body(
            client
                .get(format!("{url}.sha256sum"))
                .send()
                .await
                .map_err(|e| download_failed(&e))?,
            MAX_SUM_BYTES,
        )
        .await?;
        let expected = expected_digest(&sum).ok_or_else(damaged)?;
        // The checksum file is a line long: the database is downloaded only when it differs.
        if installed.is_some_and(|digest| digest.eq_ignore_ascii_case(&expected)) {
            return Ok(Fetched::Same);
        }
        let data = body(
            client
                .get(&url)
                .send()
                .await
                .map_err(|e| download_failed(&e))?,
            MAX_DB_BYTES,
        )
        .await?;
        if format!("{:x}", Sha256::digest(&data)) != expected {
            return Err(Error::failure(
                FailureCode::GeodataInvalid,
                t("core.geodata.checksum_mismatch"),
            ));
        }
        Ok(Fetched::Latest(data))
    }
}

/// Seconds since the epoch of a file's `SystemTime`, for display.
pub fn seconds(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
