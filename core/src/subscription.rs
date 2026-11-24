use crate::text::t;
use crate::{
    diagnostics::{
        BodyKind, Cause, ContentClass, Mark, Rows, Scheme, SizeBucket, Stage, SubscriptionInfo,
        Trail, MAX_REDIRECTS,
    },
    failure::FailureCode,
    model::{Filters, Server, Store, Usage},
    parser::{decode64, parse_uri, rejection},
    routing::{ServerRoutes, MAX_ROUTE_ENTRIES, MAX_ROUTE_SETS},
    Error, Result,
};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
};
pub const MAX_SUBSCRIPTION_BYTES: usize = 4 * 1024 * 1024;
/// A request for one subscription.
#[derive(Clone, Copy, Debug)]
pub struct FetchRequest<'a> {
    pub url: &'a str,
    /// `None` fetches direct (no proxy); `Some(port)` routes through the local mixed inbound at
    /// `127.0.0.1:port` (the active server's tunnel).
    pub proxy_port: Option<u16>,
    /// `None`: Rift's own `User-Agent` (`Rift/<version>`).
    pub user_agent: Option<&'a str>,
}
/// The answer: the body and what the headers say about the subscription.
#[derive(Clone, Debug, Default)]
pub struct Fetched {
    pub body: String,
    /// The name the provider gives the subscription (`profile-title`, then the file name of
    /// `Content-Disposition`), not yet cleaned.
    pub title: Option<String>,
    /// Traffic and term from `subscription-userinfo`.
    pub usage: Option<Usage>,
    /// The steps of the download, to go on with when a later step fails.
    pub trail: Trail,
    /// What the answer was, in classes: it goes on with a later failure.
    pub info: SubscriptionInfo,
}
#[async_trait::async_trait]
pub trait SubscriptionSource: Send + Sync {
    /// `proxy_port`: `None` fetches direct (no proxy); `Some(port)` routes through the local
    /// mixed inbound at `127.0.0.1:port` (the active server's tunnel).
    async fn fetch(&self, url: &str, proxy_port: Option<u16>) -> Result<String>;
    /// The body with its metadata. A source without headers (tests) needs only `fetch`.
    async fn fetch_with(&self, request: FetchRequest<'_>) -> Result<Fetched> {
        let body = self.fetch(request.url, request.proxy_port).await?;
        Ok(Fetched {
            body,
            ..Fetched::default()
        })
    }
}
pub struct HttpSubscriptionSource;
#[async_trait::async_trait]
impl SubscriptionSource for HttpSubscriptionSource {
    async fn fetch(&self, url: &str, proxy_port: Option<u16>) -> Result<String> {
        download(url, proxy_port).await
    }
    async fn fetch_with(&self, request: FetchRequest<'_>) -> Result<Fetched> {
        request_subscription(
            request.url,
            request.proxy_port,
            request.user_agent.unwrap_or(SUBSCRIPTION_USER_AGENT),
        )
        .await
    }
}
pub struct ParsedSubscription {
    pub servers: Vec<Server>,
    pub rejected: usize,
    /// Servers dropped by the filters of the subscription.
    pub filtered: usize,
    /// Safe parse observations before source filters, including successful explicit parameters.
    pub rows: Rows,
}
/// Keeps the servers the filters allow. No filters: the same list. Filters that drop everything
/// are an error of the subscription, so the previous servers stay.
pub fn apply_filters(
    mut parsed: ParsedSubscription,
    filters: &Filters,
) -> Result<ParsedSubscription> {
    if filters.is_empty() {
        return Ok(parsed);
    }
    let before = parsed.servers.len();
    parsed.servers.retain(|server| filters.keeps(server));
    parsed.filtered = before - parsed.servers.len();
    if parsed.servers.is_empty() {
        return Err(Error::failure(
            FailureCode::SubscriptionFiltered,
            t("core.subscription.filters_excluded_all"),
        )
        .with_subscription(SubscriptionInfo {
            filtered: Some(u16::try_from(parsed.filtered).unwrap_or(u16::MAX)),
            rows: Some(parsed.rows),
            ..SubscriptionInfo::default()
        }));
    }
    Ok(parsed)
}
/// What a body that gave no server says.
pub fn no_servers() -> &'static str {
    t("core.subscription.no_supported_servers")
}

/// What a body is, from its first bytes and nothing else. The journal records the class,
/// never the subscription body.
pub fn body_kind(text: &str) -> BodyKind {
    let text = text.trim_start_matches('\u{feff}').trim();
    if text.is_empty() {
        BodyKind::Empty
    } else if text.starts_with('<') {
        BodyKind::Html
    } else if crate::xray::looks_like(text) {
        if crate::xray::is_profile_list(text) {
            BodyKind::JsonXray
        } else {
            BodyKind::JsonOther
        }
    } else if text.contains("://") {
        BodyKind::PlainLinks
    } else if decode64(text).is_ok_and(|decoded| decoded.contains("://")) {
        BodyKind::Base64Links
    } else {
        BodyKind::TextOther
    }
}

/// The class and the size of a body that was read.
pub fn body_info(text: &str) -> SubscriptionInfo {
    SubscriptionInfo {
        body_kind: Some(body_kind(text)),
        body_size: Some(SizeBucket::of(text.len())),
        ..SubscriptionInfo::default()
    }
}

pub fn parse_subscription(text: &str, source: &str) -> Result<ParsedSubscription> {
    parse_subscription_with(text, source, true)
}
/// The servers of a subscription body: a list of links (plain or Base64) or Xray JSON. With
/// `provider_routes` the servers of Xray JSON keep the rules their profile routes by.
pub fn parse_subscription_with(
    text: &str,
    source: &str,
    provider_routes: bool,
) -> Result<ParsedSubscription> {
    if text.len() > MAX_SUBSCRIPTION_BYTES {
        return Err(Error::Invalid(t("core.subscription.too_large").into()));
    }
    if crate::xray::looks_like(text) {
        return crate::xray::parse(text, source, provider_routes);
    }
    let decoded;
    let text = if text.contains("://") {
        text
    } else {
        decoded = decode64(text)?;
        &decoded
    };
    let mut servers = vec![];
    let mut rejected = 0;
    let mut rows = Rows::default();
    let mut seen = HashSet::new();
    let mut first_rejection = None;
    for line in text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
    {
        match parse_uri(line, Some(source.into())) {
            Ok(server) => {
                rows.accept();
                crate::subscription_observation::Observation::uri(line)
                    .count_accepted_into(&mut rows);
                if seen.insert(server.id.clone()) {
                    servers.push(server);
                }
            }
            Err(error) => {
                rejected += 1;
                rows.reject(rejection(line, &error), Scheme::of_line(line));
                crate::subscription_observation::Observation::uri(line).count_into(&mut rows);
                if first_rejection.is_none() {
                    first_rejection = Some(error.diagnostic_details());
                }
            }
        }
        if servers.len() > 2000 {
            return Err(Error::Invalid(
                t("core.subscription.too_many_servers").into(),
            ));
        }
    }
    if servers.is_empty() {
        return Err(
            Error::failure(FailureCode::SubscriptionInvalidBody, no_servers())
                .with_local_details(
                    first_rejection.unwrap_or_else(|| t("core.diagnostics.no_links").into()),
                )
                .with_subscription(SubscriptionInfo {
                    rows: Some(rows),
                    ..SubscriptionInfo::default()
                }),
        );
    }
    Ok(ParsedSubscription {
        servers,
        rejected,
        filtered: 0,
        rows,
    })
}
pub fn merge(store: &Store, source: &str, parsed: ParsedSubscription) -> Store {
    let mut next = store.clone();
    // A refreshed row with the same fingerprint keeps its already resolved country.
    let countries: std::collections::HashMap<String, String> = next
        .servers
        .iter()
        .filter(|s| s.source.as_deref() == Some(source) && !s.country.is_empty())
        .map(|s| (s.id.clone(), s.country.clone()))
        .collect();
    // The server selected, if it is one of this source: a provider that rotates credentials gives
    // the same server another id, and the selection follows it by its address.
    let selected = next.selected.as_ref().and_then(|id| {
        next.servers
            .iter()
            .find(|s| &s.id == id && s.source.as_deref() == Some(source))
            .cloned()
    });
    next.servers.retain(|s| s.source.as_deref() != Some(source));
    // The rules of servers are copies in the config and in the store: a store holds only so many
    // different sets and so many entries in all, and a server whose set would be over either
    // comes without it.
    let mut sets: std::collections::HashSet<String> = next
        .servers
        .iter()
        .filter_map(|s| s.own_routes().map(ServerRoutes::mode))
        .collect();
    let mut entries = next.route_entries();
    next.servers.extend(parsed.servers.into_iter().map(|mut s| {
        if let Some(country) = countries.get(&s.id) {
            s.country.clone_from(country);
        }
        if let Some((mode, size)) = s.own_routes().map(|routes| (routes.mode(), routes.len())) {
            if (!sets.contains(&mode) && sets.len() >= MAX_ROUTE_SETS)
                || entries + size > MAX_ROUTE_ENTRIES
            {
                s.routes = None;
            } else {
                sets.insert(mode);
                entries += size;
            }
        }
        s
    }));
    if next
        .selected
        .as_ref()
        .is_some_and(|id| !next.available_servers().iter().any(|s| &s.id == id))
    {
        next.selected = selected.and_then(|old| successor(&next, source, &old));
    }
    next.prune_excluded();
    next
}
/// The server of `source` that stands where `old` stood: the same address, preferably the same
/// protocol, otherwise the first row at that address. None when the address is gone.
fn successor(store: &Store, source: &str, old: &Server) -> Option<String> {
    let key = old.stable_key();
    let at_address: Vec<&Server> = store
        .available_iter()
        .filter(|s| s.source.as_deref() == Some(source) && s.stable_key() == key)
        .collect();
    at_address
        .iter()
        .find(|s| s.protocol == old.protocol)
        .or_else(|| at_address.first())
        .map(|s| s.id.clone())
}
/// Sent with every subscription request. Subscription panels and the CDNs in front of them
/// identify clients by this header; one such panel answered 502 to a request without it.
const SUBSCRIPTION_USER_AGENT: &str = concat!("Rift/", env!("CARGO_PKG_VERSION"));
/// One attempt, connect to last body byte.
const SUBSCRIPTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// The client of one download, and the redirects it has followed.
struct Downloader {
    client: reqwest::Client,
    redirects: Arc<AtomicU8>,
}

/// `https_only` is false and `timeout` is short only in tests, which talk to a loopback HTTP
/// listener.
fn http_client(
    proxy_port: Option<u16>,
    https_only: bool,
    timeout: std::time::Duration,
    user_agent: &str,
) -> Result<Downloader> {
    let setup_error = || {
        Error::failure(
            FailureCode::SubscriptionTransport,
            t("core.subscription.download_setup_failed"),
        )
        .at_stage(Stage::BuildClient)
    };
    let redirects = Arc::new(AtomicU8::new(0));
    let counted = redirects.clone();
    // Up to `MAX_REDIRECTS` redirects, as `Policy::limited` would, with the count
    // retained for local diagnostics.
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        let followed = attempt.previous().len();
        if followed > usize::from(MAX_REDIRECTS) {
            return attempt.error("too many redirects");
        }
        counted.store(
            u8::try_from(followed).unwrap_or(MAX_REDIRECTS),
            Ordering::Relaxed,
        );
        attempt.follow()
    });
    let mut builder = reqwest::Client::builder()
        .user_agent(user_agent)
        .https_only(https_only)
        .timeout(timeout)
        .redirect(policy);
    builder = match proxy_port {
        Some(port) => builder.proxy(
            reqwest::Proxy::all(format!("http://127.0.0.1:{port}"))
                .map_err(|error| setup_error().with_local_details(network_observation(&error)))?,
        ),
        None => builder.no_proxy(),
    };
    let client = builder
        .build()
        .map_err(|error| setup_error().with_local_details(network_observation(&error)))?;
    Ok(Downloader { client, redirects })
}
/// `proxy_port`: `None` fetches direct; `Some(port)` routes through the local mixed inbound
/// (HTTP CONNECT) at `127.0.0.1:port`, i.e. the currently active server's tunnel.
pub async fn download(url: &str, proxy_port: Option<u16>) -> Result<String> {
    request_subscription(url, proxy_port, SUBSCRIPTION_USER_AGENT)
        .await
        .map(|fetched| fetched.body)
}
/// One download, from the address to the body. Every failure knows the stage it happened in and
/// the steps that came before it.
async fn request_subscription(
    url: &str,
    proxy_port: Option<u16>,
    user_agent: &str,
) -> Result<Fetched> {
    let mut trail = Trail::new();
    trail.push(Mark::RefreshStarted);
    let parsed = validate_url(url).map_err(|error| {
        error
            .with_code(FailureCode::SubscriptionTransport)
            .at_stage(Stage::BuildClient)
            .with_trail(trail)
    })?;
    let downloader = http_client(proxy_port, true, SUBSCRIPTION_TIMEOUT, user_agent)
        .map_err(|error| error.with_trail(trail))?;
    trail.push(Mark::ClientBuilt);
    fetch_response(&downloader, parsed, trail).await
}
/// Longest header value read for the name or the usage: a provider's line, not a payload.
const MAX_HEADER_BYTES: usize = 1024;
/// The name of a subscription from the response headers: `profile-title` (plain, percent-encoded
/// or `base64:`), then the file name of `Content-Disposition`.
pub fn title_from_headers(headers: &reqwest::header::HeaderMap) -> Option<String> {
    title_from_values(
        headers.get("profile-title").map(|value| value.as_bytes()),
        headers
            .get("content-disposition")
            .map(|value| value.as_bytes()),
    )
}
/// `title_from_headers` on raw header values; also the entry point of the fuzz target.
pub fn title_from_values(
    profile_title: Option<&[u8]>,
    content_disposition: Option<&[u8]>,
) -> Option<String> {
    let text = |value: Option<&[u8]>| {
        value
            .filter(|value| value.len() <= MAX_HEADER_BYTES)
            .and_then(|value| std::str::from_utf8(value).ok())
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    if let Some(title) = text(profile_title) {
        let decoded = match title.get(..7).filter(|p| p.eq_ignore_ascii_case("base64:")) {
            Some(_) => decode64(&title[7..]).ok(),
            None => Some(
                percent_encoding::percent_decode_str(&title)
                    .decode_utf8()
                    .map_or_else(|_| title.clone(), |decoded| decoded.into_owned()),
            ),
        };
        if let Some(decoded) = decoded.filter(|decoded| !decoded.trim().is_empty()) {
            return Some(decoded);
        }
    }
    text(content_disposition).and_then(|value| filename_of(&value))
}
/// `filename*=UTF-8''…` (percent-encoded) or `filename="…"` of a `Content-Disposition` value.
fn filename_of(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    if let Some(at) = lower.find("filename*=") {
        let rest = value[at + 10..].split(';').next()?.trim();
        // `charset'language'value`: the value is what follows the second quote.
        let encoded = rest.splitn(3, '\'').nth(2).unwrap_or(rest);
        let decoded = percent_encoding::percent_decode_str(encoded.trim_matches('"'))
            .decode_utf8()
            .ok()?;
        return Some(decoded.into_owned()).filter(|name| !name.trim().is_empty());
    }
    let at = lower.find("filename=")?;
    let name = value[at + 9..].split(';').next()?.trim().trim_matches('"');
    Some(name.to_owned()).filter(|name| !name.is_empty())
}
/// `subscription-userinfo: upload=1; download=2; total=3; expire=4` (all optional).
pub fn usage_from_userinfo(value: &str) -> Option<Usage> {
    if value.len() > MAX_HEADER_BYTES {
        return None;
    }
    let (mut upload, mut download, mut total, mut expire) = (None, None, None, None);
    for pair in value.split(';') {
        let Some((key, number)) = pair.split_once('=') else {
            continue;
        };
        let Ok(number) = number.trim().parse::<u64>() else {
            continue;
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "upload" => upload = Some(number),
            "download" => download = Some(number),
            "total" => total = Some(number),
            "expire" => expire = Some(number),
            _ => {}
        }
    }
    if upload.is_none() && download.is_none() && total.is_none() && expire.is_none() {
        return None;
    }
    Some(Usage {
        used: upload.unwrap_or(0).saturating_add(download.unwrap_or(0)),
        total: total.unwrap_or(0),
        // `0` is how providers write "never".
        expire: expire.filter(|&at| at > 0),
    })
}
/// The request itself, split from `download` so tests can run it against a loopback listener.
/// Each failure keeps the stage it happened in (`Stage`) and as a `FailureCode` its cause.
async fn fetch_response(
    downloader: &Downloader,
    url: url::Url,
    mut trail: Trail,
) -> Result<Fetched> {
    // `send` ends when the headers are in: connection, TLS and the wait for the answer.
    trail.push(Mark::RequestSent);
    let response = downloader.client.get(url).send().await.map_err(|error| {
        subscription_request_error(&error, Stage::SendRequest)
            .with_subscription(redirected(downloader))
            .with_trail(trail)
    })?;
    trail.push(Mark::HeadersReceived);
    let mut info = SubscriptionInfo {
        content_type: Some(content_class(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .map(|value| value.as_bytes()),
        )),
        ..redirected(downloader)
    };
    let status = response.status().as_u16();
    let mut response = response.error_for_status().map_err(|_| {
        Error::failure(
            FailureCode::SubscriptionHttpStatus,
            t("core.subscription.server_error"),
        )
        .at_stage(Stage::SendRequest)
        .with_status(status)
        .with_local_details(crate::tr!("core.diagnostics.http_refusal", status = status))
        .with_subscription(info)
        .with_trail(trail)
    })?;
    check_declared_size(response.content_length())
        .map_err(|error| error.with_subscription(info).with_trail(trail))?;
    let title = title_from_headers(response.headers());
    let usage = response
        .headers()
        .get("subscription-userinfo")
        .and_then(|value| value.to_str().ok())
        .and_then(usage_from_userinfo);
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        subscription_request_error(&error, Stage::ReadBody)
            .with_subscription(info)
            .with_trail(trail)
    })? {
        append_limited(&mut bytes, &chunk)
            .map_err(|error| error.with_subscription(info).with_trail(trail))?;
    }
    info.body_size = Some(SizeBucket::of(bytes.len()));
    let body = String::from_utf8(bytes).map_err(|_| {
        Error::failure(
            FailureCode::SubscriptionInvalidBody,
            t("core.subscription.not_utf8"),
        )
        .at_stage(Stage::ReadBody)
        .with_subscription(SubscriptionInfo {
            body_kind: Some(BodyKind::NotUtf8),
            ..info
        })
        .with_trail(trail)
    })?;
    trail.push(Mark::BodyRead);
    Ok(Fetched {
        body,
        title,
        usage,
        trail,
        info,
    })
}

/// What is known of the answer once the redirects are behind: how many were followed.
fn redirected(downloader: &Downloader) -> SubscriptionInfo {
    SubscriptionInfo {
        redirects: Some(downloader.redirects.load(Ordering::Relaxed)),
        ..SubscriptionInfo::default()
    }
}

/// The class of a `Content-Type` value: the media type before any parameters, never the value.
pub fn content_class(value: Option<&[u8]>) -> ContentClass {
    let Some(value) = value else {
        return ContentClass::Absent;
    };
    let media: String = value
        .iter()
        .take_while(|byte| **byte != b';')
        .map(|byte| char::from(byte.to_ascii_lowercase()))
        .collect();
    let media = media.trim();
    match media {
        "text/plain" => ContentClass::TextPlain,
        "text/html" | "application/xhtml+xml" => ContentClass::Html,
        "application/octet-stream" => ContentClass::Octet,
        "application/json" => ContentClass::Json,
        _ if media.ends_with("+json") => ContentClass::Json,
        _ => ContentClass::Other,
    }
}

fn subscription_request_error(error: &reqwest::Error, stage: Stage) -> Error {
    if error.is_timeout() {
        Error::failure(
            FailureCode::SubscriptionTimeout,
            t("core.subscription.download_timeout"),
        )
    } else {
        Error::failure(
            FailureCode::SubscriptionTransport,
            t("core.subscription.download_failed"),
        )
    }
    .at_stage(stage)
    .with_cause(cause_of(error))
    .with_local_details(network_observation(error))
}

/// A cause chain made only from library classifications and numeric OS codes. Error display
/// strings can contain the request URL or credentials, even after `without_url()`.
fn network_observation(error: &reqwest::Error) -> String {
    use std::fmt::Write;
    let mut details = crate::tr!(
        "core.diagnostics.network_failure",
        cause = cause_of(error).as_str()
    );
    for (present, kind) in [
        (error.is_builder(), t("core.diagnostics.network_builder")),
        (error.is_redirect(), t("core.diagnostics.network_redirect")),
        (error.is_body(), t("core.diagnostics.network_body")),
        (error.is_decode(), t("core.diagnostics.network_decode")),
        (error.is_connect(), t("core.diagnostics.network_connect")),
    ] {
        if present {
            let _ = write!(details, "; {kind}");
        }
    }
    let mut source = std::error::Error::source(error);
    for _ in 0..16 {
        let Some(current) = source else { break };
        if let Some(io) = current.downcast_ref::<std::io::Error>() {
            let _ = write!(details, "; io_kind={:?}", io.kind());
            if let Some(code) = io.raw_os_error() {
                let _ = write!(details, "; os_error={code}");
            }
        } else if current.is::<rustls::Error>() {
            details.push_str("; ");
            details.push_str(t("core.diagnostics.network_tls"));
        }
        source = current.source();
    }
    details
}

/// The best guess at why a request failed, read from the kind of the error and of the errors
/// under it, never from their text. A TLS failure comes before a reset, which comes before the
/// connection that did not open; whatever is none of those is `other`.
pub fn cause_of(error: &reqwest::Error) -> Cause {
    if error.is_timeout() {
        return Cause::Timeout;
    }
    if error.is_dns() {
        return Cause::Dns;
    }
    cause_in_chain(std::error::Error::source(error), error.is_connect())
}

/// The part of [`cause_of`] that reads the chain of sources, so that it can be tried on errors
/// built by hand.
fn cause_in_chain(first: Option<&(dyn std::error::Error + 'static)>, connect: bool) -> Cause {
    /// What the errors under one error held.
    #[derive(Default)]
    struct Seen {
        tls: bool,
        reset: bool,
    }
    // An I/O error can hold another one (the stream under a TLS session wraps its own), so the
    // search goes into those as well as along `source`.
    fn scan(error: &(dyn std::error::Error + 'static), seen: &mut Seen) {
        seen.tls |= error.is::<rustls::Error>();
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            seen.reset |= matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::UnexpectedEof
            );
            if let Some(inner) = io.get_ref() {
                scan(inner, seen);
            }
        }
        if let Some(next) = error.source() {
            scan(next, seen);
        }
    }
    let mut seen = Seen::default();
    if let Some(first) = first {
        scan(first, &mut seen);
    }
    if seen.tls {
        Cause::Tls
    } else if seen.reset {
        Cause::Reset
    } else if connect {
        Cause::Connect
    } else {
        Cause::Other
    }
}

fn too_large() -> Error {
    Error::failure(
        FailureCode::SubscriptionTransport,
        t("core.subscription.too_large"),
    )
    .at_stage(Stage::ReadBody)
}
/// Rejects before reading when the server announces more than the limit.
fn check_declared_size(content_length: Option<u64>) -> Result<()> {
    if content_length.is_some_and(|n| n > MAX_SUBSCRIPTION_BYTES as u64) {
        return Err(too_large());
    }
    Ok(())
}
/// Streams without trusting Content-Length: stops as soon as the limit would be crossed.
fn append_limited(bytes: &mut Vec<u8>, chunk: &[u8]) -> Result<()> {
    if bytes.len() + chunk.len() > MAX_SUBSCRIPTION_BYTES {
        return Err(too_large());
    }
    bytes.extend_from_slice(chunk);
    Ok(())
}

/// A name for a subscription before its provider has named it: the host of its URL.
pub fn host_name(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .and_then(|host| crate::model::Subscription::clean_name(&host))
        .unwrap_or_else(|| t("core.subscription.default_name").into())
}

pub fn validate_url(url: &str) -> Result<url::Url> {
    let parsed =
        url::Url::parse(url).map_err(|_| Error::Invalid(t("core.subscription.bad_url").into()))?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(Error::Invalid(t("core.subscription.https_required").into()));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The steps `request_subscription` has done before the request itself.
    fn started() -> Trail {
        let mut trail = Trail::new();
        trail.push(Mark::RefreshStarted);
        trail.push(Mark::ClientBuilt);
        trail
    }
    async fn fetch_body(client: &Downloader, url: url::Url) -> Result<String> {
        fetch_response(client, url, started())
            .await
            .map(|fetched| fetched.body)
    }

    // The client that `download` uses must identify itself: a panel answered 502 without it.
    #[tokio::test]
    async fn subscription_requests_carry_the_rift_user_agent() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let read = socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .unwrap();
            String::from_utf8_lossy(&request[..read]).to_lowercase()
        });
        let client =
            http_client(None, false, SUBSCRIPTION_TIMEOUT, SUBSCRIPTION_USER_AGENT).unwrap();
        let body = client
            .client
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(body, "ok");
        let request = server.await.unwrap();
        let expected = format!("user-agent: rift/{}\r\n", env!("CARGO_PKG_VERSION"));
        assert!(request.contains(&expected), "request was: {request}");
    }

    /// Answers one connection with `reply`. With `hold` it then stays open and silent, so only
    /// the client's own timeout can end the request. Returns the URL to fetch.
    async fn serve_once(reply: &'static [u8], hold: bool) -> url::Url {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            // Read the whole request head first: closing a socket with unread data resets the
            // connection on some systems and could swallow the reply.
            let mut head = Vec::new();
            let mut chunk = [0; 1024];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match socket.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.extend_from_slice(&chunk[..n]),
                }
            }
            let _ = socket.write_all(reply).await;
            if hold {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        });
        url::Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap()
    }
    /// `download` without the HTTPS-only rule and with the given client timeout.
    async fn fetched(
        reply: &'static [u8],
        hold: bool,
        timeout: std::time::Duration,
    ) -> Result<String> {
        let url = serve_once(reply, hold).await;
        let client = http_client(None, false, timeout, SUBSCRIPTION_USER_AGENT).unwrap();
        fetch_body(&client, url).await
    }
    const PATIENT: std::time::Duration = std::time::Duration::from_secs(10);
    const QUICK: std::time::Duration = std::time::Duration::from_millis(300);
    /// For the tests that name the stage of a stalled download: the headers must arrive inside
    /// the deadline for the stage to be the body, and a loaded machine can be slower than QUICK.
    const STALLED: std::time::Duration = std::time::Duration::from_millis(2000);

    #[tokio::test]
    async fn a_good_answer_is_the_body() {
        let reply = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
        assert_eq!(fetched(reply, false, PATIENT).await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn a_gateway_error_is_a_subscription_http_status() {
        // The 254 incident: a provider panel answered 502.
        let reply = b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let error = fetched(reply, false, PATIENT).await.unwrap_err();
        assert_eq!(
            error.failure_code(),
            Some(FailureCode::SubscriptionHttpStatus)
        );
    }

    #[tokio::test]
    async fn a_silent_server_is_a_subscription_timeout() {
        let error = fetched(b"", true, QUICK).await.unwrap_err();
        assert_eq!(error.failure_code(), Some(FailureCode::SubscriptionTimeout));
    }

    #[tokio::test]
    async fn a_stalled_body_is_a_subscription_timeout() {
        // Headers and the start of the list arrive, the rest never does.
        let reply = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\ntrojan://";
        let error = fetched(reply, true, QUICK).await.unwrap_err();
        assert_eq!(error.failure_code(), Some(FailureCode::SubscriptionTimeout));
    }

    #[tokio::test]
    async fn a_body_that_is_not_utf8_is_an_invalid_subscription_body() {
        let reply = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n\xff\xfe";
        let error = fetched(reply, false, PATIENT).await.unwrap_err();
        assert_eq!(
            error.failure_code(),
            Some(FailureCode::SubscriptionInvalidBody)
        );
    }

    #[tokio::test]
    async fn a_connection_closed_without_an_answer_is_a_subscription_transport_failure() {
        let error = fetched(b"", false, PATIENT).await.unwrap_err();
        assert_eq!(
            error.failure_code(),
            Some(FailureCode::SubscriptionTransport)
        );
    }

    /// Local failure metadata, without any remote event projection.
    fn diagnosed(error: &Error) -> crate::diagnostics::Diagnostic {
        crate::diagnostics::Diagnostic::from_error(error)
    }
    fn marks(report: &crate::diagnostics::Diagnostic) -> Vec<&'static str> {
        report.trail.marks().iter().map(|m| m.as_str()).collect()
    }

    #[tokio::test]
    async fn a_gateway_error_keeps_its_local_status_stage_and_steps() {
        use crate::diagnostics::{Operation, Stage};
        let reply = b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let error = fetched(reply, false, PATIENT).await.unwrap_err();
        let report = diagnosed(&error);
        assert_eq!(report.operation, Operation::SubscriptionRefresh);
        assert_eq!(report.stage, Stage::SendRequest);
        assert_eq!(report.failure_code, FailureCode::SubscriptionHttpStatus);
        assert_eq!(report.http_status, Some(502));
        assert_eq!(
            marks(&report),
            [
                "refresh_started",
                "client_built",
                "request_sent",
                "headers_received"
            ]
        );
        assert_eq!(
            report.origin.map(|origin| origin.file),
            Some("core/src/subscription.rs".to_owned()),
            "the place that created the failure, not the one that showed it"
        );
    }

    #[tokio::test]
    async fn forbidden_http_status_is_explicit_without_guessing_expiry() {
        let reply = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let error = fetched(reply, false, PATIENT).await.unwrap_err();
        assert_eq!(error.detail().unwrap().http_status, Some(403));
        let details = error.diagnostic_details();
        assert!(details.contains("403"));
        assert!(!details.contains("expired"));
    }

    #[tokio::test]
    async fn waiting_for_the_headers_and_a_stalled_body_are_told_apart() {
        use crate::diagnostics::Stage;
        let silent = fetched(b"", true, STALLED).await.unwrap_err();
        let report = diagnosed(&silent);
        assert_eq!(report.failure_code, FailureCode::SubscriptionTimeout);
        assert_eq!(report.stage, Stage::SendRequest);
        assert_eq!(
            marks(&report),
            ["refresh_started", "client_built", "request_sent"]
        );

        let reply = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\ntrojan://";
        let stalled = fetched(reply, true, STALLED).await.unwrap_err();
        let report = diagnosed(&stalled);
        assert_eq!(report.failure_code, FailureCode::SubscriptionTimeout);
        assert_eq!(report.stage, Stage::ReadBody);
        assert_eq!(
            marks(&report),
            [
                "refresh_started",
                "client_built",
                "request_sent",
                "headers_received"
            ]
        );
    }

    #[tokio::test]
    async fn the_other_causes_of_a_failed_download_name_their_stage() {
        use crate::diagnostics::Stage;
        let not_utf8 = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n\xff\xfe";
        let report = diagnosed(&fetched(not_utf8, false, PATIENT).await.unwrap_err());
        assert_eq!(
            (report.failure_code, report.stage),
            (FailureCode::SubscriptionInvalidBody, Stage::ReadBody)
        );
        let report = diagnosed(&fetched(b"", false, PATIENT).await.unwrap_err());
        assert_eq!(
            (report.failure_code, report.stage),
            (FailureCode::SubscriptionTransport, Stage::SendRequest)
        );
        let too_big = b"HTTP/1.1 200 OK\r\nContent-Length: 4194305\r\nConnection: close\r\n\r\n";
        let report = diagnosed(&fetched(too_big, false, PATIENT).await.unwrap_err());
        assert_eq!(
            (report.failure_code, report.stage),
            (FailureCode::SubscriptionTransport, Stage::ReadBody)
        );
        assert!(report.http_status.is_none());
    }

    #[tokio::test]
    async fn an_address_or_client_that_cannot_be_used_fails_before_the_request() {
        use crate::diagnostics::Stage;
        let error = request_subscription("http://plain.example/", None, "Rift")
            .await
            .unwrap_err();
        let report = diagnosed(&error);
        assert_eq!(report.stage, Stage::BuildClient);
        assert_eq!(marks(&report), ["refresh_started"]);
        assert_eq!(report.failure_code, FailureCode::SubscriptionTransport);
    }

    #[tokio::test]
    async fn downloads_side_by_side_keep_their_own_steps() {
        let stalled = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\ntrojan://";
        let gateway = b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (first, second, third) = tokio::join!(
            fetched(stalled, true, STALLED),
            fetched(gateway, false, PATIENT),
            fetched(b"", true, STALLED),
        );
        let first = diagnosed(&first.unwrap_err());
        let second = diagnosed(&second.unwrap_err());
        let third = diagnosed(&third.unwrap_err());
        assert_eq!(first.trail.marks().len(), 4);
        assert_eq!(second.trail.marks().len(), 4);
        assert_eq!(third.trail.marks().len(), 3, "no step of another download");
        assert_eq!(first.http_status, None);
        assert_eq!(second.http_status, Some(502));
        assert_eq!(third.http_status, None);
    }

    #[test]
    fn malformed_utf8_titles_are_ignored_before_trying_the_filename() {
        assert_eq!(title_from_values(Some(b"bad\xff"), None), None);
        assert_eq!(
            title_from_values(Some(b"bad\xff"), Some(b"attachment; filename=good")),
            Some("good".into())
        );
    }

    #[test]
    fn declared_size_limit_is_inclusive() {
        assert!(check_declared_size(None).is_ok());
        assert!(check_declared_size(Some(MAX_SUBSCRIPTION_BYTES as u64)).is_ok());
        assert!(check_declared_size(Some(MAX_SUBSCRIPTION_BYTES as u64 + 1)).is_err());
    }

    #[test]
    fn streamed_size_limit_is_inclusive_and_keeps_order() {
        let mut bytes = vec![b'a'; MAX_SUBSCRIPTION_BYTES - 3];
        append_limited(&mut bytes, b"xyz").unwrap();
        assert_eq!(bytes.len(), MAX_SUBSCRIPTION_BYTES);
        assert!(bytes.ends_with(b"xyz"));
        assert!(append_limited(&mut bytes, b"!").is_err());
        assert_eq!(
            bytes.len(),
            MAX_SUBSCRIPTION_BYTES,
            "rejected chunk is not kept"
        );
    }

    use reqwest::header::{HeaderMap, HeaderValue};
    fn headers(pairs: &[(&'static str, &[u8])]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_bytes(value).unwrap());
        }
        map
    }

    #[test]
    fn userinfo_gives_traffic_quota_and_term() {
        let usage = usage_from_userinfo(
            "upload=455727941; download=6174315083; total=1073741824000; expire=2218532293",
        )
        .unwrap();
        assert_eq!(usage.used, 455_727_941 + 6_174_315_083);
        assert_eq!(usage.total, 1_073_741_824_000);
        assert_eq!(usage.expire, Some(2_218_532_293));
        // Any key may be missing; names and spacing are forgiving; "never" is 0.
        let partial = usage_from_userinfo(" DOWNLOAD = 5 ;expire=0").unwrap();
        assert_eq!((partial.used, partial.total, partial.expire), (5, 0, None));
        // Garbage values are skipped, and nothing usable means no usage at all.
        let skipping = usage_from_userinfo("upload=x; download=7; total=-1; foo=bar;;").unwrap();
        assert_eq!((skipping.used, skipping.total), (7, 0));
        for nothing in ["", "garbage", "upload=; download=", "a=1; b=2"] {
            assert_eq!(usage_from_userinfo(nothing), None, "{nothing:?}");
        }
    }

    #[test]
    fn userinfo_numbers_cannot_overflow_or_make_the_header_unbounded() {
        let big = format!("upload={0}; download={0}", u64::MAX);
        assert_eq!(usage_from_userinfo(&big).unwrap().used, u64::MAX);
        assert_eq!(
            usage_from_userinfo(&format!("upload=1; {}", "x".repeat(1100))),
            None
        );
    }

    #[test]
    fn the_title_comes_from_profile_title_in_every_spelling() {
        let title = |value: &[u8]| title_from_headers(&headers(&[("profile-title", value)]));
        assert_eq!(title(b"My VPN").as_deref(), Some("My VPN"));
        assert_eq!(title(b"base64:TXkgVlBO").as_deref(), Some("My VPN"));
        assert_eq!(title(b"BASE64:TXkgVlBO").as_deref(), Some("My VPN"));
        // "Мой VPN" in base64 of UTF-8, percent-encoded, and as raw UTF-8 bytes.
        assert_eq!(
            title(b"base64:0JzQvtC5IFZQTg==").as_deref(),
            Some("Мой VPN")
        );
        assert_eq!(
            title(b"%D0%9C%D0%BE%D0%B9%20VPN").as_deref(),
            Some("Мой VPN")
        );
        assert_eq!(title("Мой VPN".as_bytes()).as_deref(), Some("Мой VPN"));
        assert_eq!(title(b""), None);
        assert_eq!(title(b"   "), None);
        // A bad payload is not a title.
        assert_eq!(title(b"base64:!!!not base64!!!"), None);
        assert_eq!(title(&vec![b'a'; 2000]), None, "a header, not a payload");
    }

    #[test]
    fn without_a_profile_title_the_file_name_names_the_subscription() {
        let name = |value: &[u8]| title_from_headers(&headers(&[("content-disposition", value)]));
        assert_eq!(
            name(b"attachment; filename=\"Mine.txt\"").as_deref(),
            Some("Mine.txt")
        );
        assert_eq!(name(b"attachment; filename=Mine").as_deref(), Some("Mine"));
        assert_eq!(
            name(b"attachment; filename*=UTF-8''%D0%9C%D0%BE%D0%B9").as_deref(),
            Some("Мой")
        );
        assert_eq!(
            name(b"attachment; filename=\"plain\"; filename*=UTF-8''%D0%9C").as_deref(),
            Some("М"),
            "the extended form wins"
        );
        assert_eq!(name(b"inline"), None);
        assert_eq!(name(b"attachment; filename=\"\""), None);
        // The title has priority; an unusable title falls back to the file name.
        let both = headers(&[
            ("profile-title", b"Title"),
            ("content-disposition", b"attachment; filename=\"File\""),
        ]);
        assert_eq!(title_from_headers(&both).as_deref(), Some("Title"));
        let broken = headers(&[
            ("profile-title", b"base64:!!"),
            ("content-disposition", b"attachment; filename=\"File\""),
        ]);
        assert_eq!(title_from_headers(&broken).as_deref(), Some("File"));
        assert_eq!(title_from_headers(&HeaderMap::new()), None);
    }

    #[test]
    fn arbitrary_header_bytes_never_panic() {
        for value in [
            &b"\xff\xfe"[..],
            b"base64:\xff",
            b"%",
            b"%zz%",
            b"filename*=''",
        ] {
            let _ = title_from_headers(&headers(&[("profile-title", value)]));
            let _ = title_from_headers(&headers(&[("content-disposition", value)]));
            let _ = usage_from_userinfo(&String::from_utf8_lossy(value));
        }
    }

    #[tokio::test]
    async fn the_answer_carries_the_title_and_the_traffic_from_its_headers() {
        let reply = b"HTTP/1.1 200 OK\r\nProfile-Title: base64:TXkgVlBO\r\n\
Subscription-Userinfo: upload=1; download=2; total=30; expire=4102444800\r\n\
Content-Length: 2\r\nConnection: close\r\n\r\nok";
        let url = serve_once(reply, false).await;
        let client = http_client(None, false, PATIENT, SUBSCRIPTION_USER_AGENT).unwrap();
        let fetched = fetch_response(&client, url, started()).await.unwrap();
        assert_eq!(fetched.body, "ok");
        assert_eq!(fetched.title.as_deref(), Some("My VPN"));
        assert_eq!(
            fetched.usage,
            Some(Usage {
                used: 3,
                total: 30,
                expire: Some(4_102_444_800)
            })
        );
    }

    #[tokio::test]
    async fn a_subscription_user_agent_replaces_the_default_one() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let read = socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .unwrap();
            String::from_utf8_lossy(&request[..read]).to_lowercase()
        });
        let client = http_client(None, false, PATIENT, "Happ/1.0").unwrap();
        let url = url::Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        fetch_response(&client, url, started()).await.unwrap();
        let request = server.await.unwrap();
        assert!(
            request.contains("user-agent: happ/1.0\r\n"),
            "request was: {request}"
        );
        assert!(!request.contains("rift/"), "request was: {request}");
    }

    fn named(name: &str, tag: &str) -> Server {
        let mut server =
            parse_uri(&format!("trojan://p@example.com:{tag}"), Some("s".into())).unwrap();
        server.name = name.into();
        server
    }
    #[test]
    fn filters_that_keep_nothing_are_an_error_with_their_own_code() {
        let parsed = || ParsedSubscription {
            servers: vec![named("Frankfurt", "1"), named("Helsinki", "2")],
            rejected: 0,
            filtered: 0,
            rows: Rows::default(),
        };
        let none = apply_filters(parsed(), &Filters::default()).unwrap();
        assert_eq!((none.servers.len(), none.filtered), (2, 0));
        let some = apply_filters(
            parsed(),
            &Filters {
                include: vec!["frank".into()],
                ..Filters::default()
            },
        )
        .unwrap();
        assert_eq!((some.servers.len(), some.filtered), (1, 1));
        let error = apply_filters(
            parsed(),
            &Filters {
                include: vec!["oslo".into()],
                ..Filters::default()
            },
        )
        .err()
        .unwrap();
        assert_eq!(
            error.failure_code(),
            Some(FailureCode::SubscriptionFiltered)
        );
    }

    #[test]
    fn a_new_subscription_is_named_by_the_host_of_its_url() {
        assert_eq!(
            host_name("https://panel.example/sub/abc?token=secret"),
            "panel.example"
        );
        assert_eq!(host_name("not a url"), "Подписка");
    }

    #[test]
    fn limit_is_four_mebibytes() {
        assert_eq!(MAX_SUBSCRIPTION_BYTES, 4_194_304);
    }

    /// Serves one reply to each connection, in order; `{url}` in a reply is the address of the
    /// listener. Returns that address.
    async fn serve_each(replies: Vec<&'static str>) -> url::Url {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let served = base.clone();
        tokio::spawn(async move {
            for reply in replies {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut head = Vec::new();
                let mut chunk = [0; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => head.extend_from_slice(&chunk[..n]),
                    }
                }
                let _ = socket
                    .write_all(reply.replace("{url}", &served).as_bytes())
                    .await;
            }
        });
        url::Url::parse(&format!("{base}/")).unwrap()
    }

    const MOVED: &str =
        "HTTP/1.1 302 Found\r\nLocation: {url}/next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const ANSWER: &str = "HTTP/1.1 200 OK\r\nContent-Type: Text/Plain; charset=utf-8\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

    #[tokio::test]
    async fn the_answer_is_described_by_class_and_the_redirects_before_it_are_counted() {
        let url = serve_each(vec![MOVED, MOVED, ANSWER]).await;
        let client = http_client(None, false, PATIENT, SUBSCRIPTION_USER_AGENT).unwrap();
        let fetched = fetch_response(&client, url, started()).await.unwrap();
        assert_eq!(fetched.body, "ok");
        assert_eq!(
            fetched.info,
            SubscriptionInfo {
                content_type: Some(ContentClass::TextPlain),
                redirects: Some(2),
                body_size: Some(SizeBucket::Under1k),
                ..SubscriptionInfo::default()
            }
        );
    }

    #[tokio::test]
    async fn five_redirects_are_followed_and_the_sixth_is_not() {
        let url = serve_each(vec![MOVED, MOVED, MOVED, MOVED, MOVED, ANSWER]).await;
        let client = http_client(None, false, PATIENT, SUBSCRIPTION_USER_AGENT).unwrap();
        let fetched = fetch_response(&client, url, started()).await.unwrap();
        assert_eq!(fetched.info.redirects, Some(5));

        let url = serve_each(vec![MOVED; 7]).await;
        let client = http_client(None, false, PATIENT, SUBSCRIPTION_USER_AGENT).unwrap();
        let error = fetch_response(&client, url, started()).await.unwrap_err();
        let detail = error.detail().unwrap();
        assert_eq!(
            error.failure_code(),
            Some(FailureCode::SubscriptionTransport)
        );
        assert_eq!(detail.subscription.unwrap().redirects, Some(5));
    }

    #[tokio::test]
    async fn a_failure_after_the_headers_keeps_what_the_headers_said() {
        let reply = b"HTTP/1.1 502 Bad Gateway\r\nContent-Type: text/html\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let error = fetched(reply, false, PATIENT).await.unwrap_err();
        let info = error.detail().unwrap().subscription.unwrap();
        assert_eq!(info.content_type, Some(ContentClass::Html));
        assert_eq!(info.redirects, Some(0));
    }

    #[tokio::test]
    async fn a_body_that_is_not_utf8_is_a_class_and_a_size() {
        let reply = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n\xff\xfe";
        let error = fetched(reply, false, PATIENT).await.unwrap_err();
        assert_eq!(
            error.failure_code(),
            Some(FailureCode::SubscriptionInvalidBody)
        );
        let info = error.detail().unwrap().subscription.unwrap();
        assert_eq!(info.body_kind, Some(BodyKind::NotUtf8));
        assert_eq!(info.body_size, Some(SizeBucket::Under1k));
        assert_eq!(info.content_type, Some(ContentClass::Absent));
    }

    #[test]
    fn a_content_type_is_a_class_whatever_its_parameters() {
        let class = |value: &[u8]| content_class(Some(value)).as_str();
        assert_eq!(class(b"text/plain"), "text_plain");
        assert_eq!(class(b"Text/Plain; charset=utf-8"), "text_plain");
        assert_eq!(class(b" text/html;charset=UTF-8"), "html");
        assert_eq!(class(b"application/json"), "json");
        assert_eq!(class(b"application/vnd.api+json; x=1"), "json");
        assert_eq!(class(b"application/octet-stream"), "octet");
        assert_eq!(class(b"image/png"), "other");
        assert_eq!(class(b"secret-token=abc"), "other");
        assert_eq!(content_class(None).as_str(), "absent");
    }

    #[test]
    fn a_body_is_classed_by_what_it_starts_with() {
        let kind = |text: &str| body_kind(text).as_str();
        assert_eq!(kind(""), "empty");
        assert_eq!(kind(" \r\n\t"), "empty");
        assert_eq!(kind("\u{feff}"), "empty");
        assert_eq!(kind("<!doctype html><title>x</title>"), "html");
        assert_eq!(kind("  <html>"), "html");
        assert_eq!(kind(r#"[{"outbounds":[]}]"#), "json_xray");
        assert_eq!(kind(r#"{"outbounds":[]}"#), "json_xray");
        assert_eq!(kind(r#"{"error":"denied"}"#), "json_other");
        assert_eq!(kind("[1,2"), "json_other", "broken JSON is not Xray");
        assert_eq!(kind("trojan://p@h:1\nvless://x"), "plain_links");
        assert_eq!(kind("dHJvamFuOi8vcEBoOjE="), "base64_links");
        assert_eq!(kind("aGVsbG8gd29ybGQ="), "text_other");
        assert_eq!(kind("plain words"), "text_other");
    }

    #[test]
    fn a_list_without_a_server_carries_the_counts_of_its_lines() {
        let vless = "vless://00000000-0000-4000-8000-000000000000@h.example:443";
        let text = format!(
            "# a comment\n\nhy2://p@h:1\nsome words\n{vless}?riftUnknown=1\n{vless}?type=kcp\n{vless}?riftUnknown=2\n"
        );
        let error = parse_subscription(&text, "s").map(|_| ()).unwrap_err();
        assert_eq!(
            error.failure_code(),
            Some(FailureCode::SubscriptionInvalidBody)
        );
        let rows = error.detail().unwrap().subscription.unwrap().rows.unwrap();
        let by = |why: crate::diagnostics::Rejection| rows.rejected_by[why as usize];
        use crate::diagnostics::Rejection::*;
        assert_eq!(
            (rows.accepted, rows.rejected(), rows.total()),
            (0, 5, 5),
            "comments and empty lines are no rows"
        );
        assert_eq!(by(UnsupportedScheme), 1);
        assert_eq!(by(NotALink), 1);
        assert_eq!(by(UnknownParam), 2);
        assert_eq!(by(UnsupportedTransport), 1);
        let seen = |scheme: Scheme| rows.rejected_schemes[scheme as usize];
        assert!(seen(Scheme::Vless) && seen(Scheme::Hysteria2) && seen(Scheme::Other));
        assert!(!seen(Scheme::Trojan));
    }

    #[test]
    fn a_list_with_a_server_counts_it_as_accepted() {
        let text = "trojan://p@h.example:443#ok\nhy2://p@h:1\n";
        let parsed = parse_subscription(text, "s").unwrap();
        assert_eq!((parsed.servers.len(), parsed.rejected), (1, 1));
    }

    #[tokio::test]
    async fn a_timeout_is_told_from_a_closed_port() {
        let silent = fetched(b"", true, QUICK).await.unwrap_err();
        assert_eq!(silent.detail().unwrap().cause, Some(Cause::Timeout));

        // A port nobody listens on: the connection is refused.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let client = http_client(None, false, PATIENT, SUBSCRIPTION_USER_AGENT).unwrap();
        let url = url::Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
        let closed = fetch_response(&client, url, started()).await.unwrap_err();
        assert_eq!(closed.detail().unwrap().cause, Some(Cause::Connect));
    }

    /// A resolver that finds no name, so that the test needs no network.
    struct NoNames;
    impl reqwest::dns::Resolve for NoNames {
        fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
            Box::pin(async { Err("no such name".into()) })
        }
    }

    #[tokio::test]
    async fn a_name_that_does_not_resolve_is_dns() {
        let client = reqwest::Client::builder()
            .no_proxy()
            .dns_resolver(NoNames)
            .timeout(PATIENT)
            .build()
            .unwrap();
        let error = client
            .get("https://no-such-host.invalid/")
            .send()
            .await
            .unwrap_err();
        assert_eq!(cause_of(&error), Cause::Dns);
    }

    #[tokio::test]
    async fn a_peer_that_is_not_tls_is_tls() {
        let client = http_client(None, true, PATIENT, SUBSCRIPTION_USER_AGENT).unwrap();
        // A server that answers a TLS hello with plain HTTP, without waiting for the hello to end.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await;
            let _ = socket.shutdown().await;
        });
        let url = url::Url::parse(&format!("https://127.0.0.1:{port}/")).unwrap();
        let error = fetch_response(&client, url, started()).await.unwrap_err();
        assert_eq!(error.detail().unwrap().cause, Some(Cause::Tls));
    }

    #[test]
    fn the_chain_of_an_error_says_tls_before_reset_before_connect() {
        use std::io::{Error as Io, ErrorKind};
        let reset = Io::from(ErrorKind::ConnectionReset);
        assert_eq!(cause_in_chain(Some(&reset), false), Cause::Reset);
        assert_eq!(cause_in_chain(Some(&reset), true), Cause::Reset);
        let eof = Io::from(ErrorKind::UnexpectedEof);
        assert_eq!(cause_in_chain(Some(&eof), false), Cause::Reset);
        let refused = Io::from(ErrorKind::ConnectionRefused);
        assert_eq!(cause_in_chain(Some(&refused), true), Cause::Connect);
        assert_eq!(cause_in_chain(Some(&refused), false), Cause::Other);
        let tls = Io::new(ErrorKind::InvalidData, rustls::Error::General("x".into()));
        assert_eq!(cause_in_chain(Some(&tls), true), Cause::Tls);
        let bare = rustls::Error::HandshakeNotComplete;
        assert_eq!(cause_in_chain(Some(&bare), false), Cause::Tls);
        assert_eq!(cause_in_chain(None, false), Cause::Other);
        assert_eq!(cause_in_chain(None, true), Cause::Connect);
    }
}
