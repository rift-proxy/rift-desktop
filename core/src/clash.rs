//! What the sing-box Clash API shows about the connection: the speed of the traffic and the
//! connections. Only for the screen: nothing here decides anything, and nothing is kept on disk,
//! in the journal, in diagnostics, in a profile or in a report. The API listens on loopback only.
use crate::text::t;
use crate::{failure::FailureCode, Error, Result};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

/// sing-box writes a line of `/traffic` every second; this long without one is a dead stream.
const TRAFFIC_IDLE: Duration = Duration::from_secs(10);
/// The list of connections is asked for within this time, and each part of it comes within it.
const CONNECTIONS_WAIT: Duration = Duration::from_secs(5);
/// The answer is read up to this size; a busy machine has some thousand connections, a few
/// hundred bytes each.
const MAX_BODY: usize = 16 * 1024 * 1024;
/// Rows the screen is given: the newest connections. The rest are only counted.
pub const MAX_CONNECTIONS: usize = 500;
const MAX_HOST: usize = 253;
const MAX_PROCESS: usize = 128;
const MAX_SERVER: usize = 120;
/// A line of the stream is some thirty bytes. A longer one is not the stream.
const MAX_LINE: usize = 8 * 1024;

/// Bytes per second through the proxy, in each direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Speed {
    pub up: u64,
    pub down: u64,
}

/// One line of `/traffic`. Fields outside `up` and `down` are ignored.
pub fn parse_traffic_line(line: &[u8]) -> Result<Speed> {
    #[derive(Deserialize)]
    struct Line {
        up: u64,
        down: u64,
    }
    let parsed: Line = serde_json::from_slice(line).map_err(|error| {
        failure(t("core.clash.speed_unexpected")).with_local_details(format!(
            "stage=traffic_json; category={:?}; line={}; column={}",
            error.classify(),
            error.line(),
            error.column()
        ))
    })?;
    Ok(Speed {
        up: parsed.up,
        down: parsed.down,
    })
}

#[derive(Clone)]
pub struct Clash {
    client: reqwest::Client,
    base: String,
    secret: String,
}
fn failure(message: &str) -> Error {
    Error::failure(FailureCode::ClashApi, message)
}
fn network_failure(message: &str, error: reqwest::Error) -> Error {
    let error = error.without_url();
    let mut detail = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        detail.push_str("; ");
        detail.push_str(&crate::tr!(
            "core.diagnostics.caused_by",
            cause = cause.to_string()
        ));
        source = cause.source();
    }
    failure(message).with_local_details(detail)
}
impl Clash {
    pub fn new(api_port: u16, secret: &str) -> Result<Self> {
        // No total timeout: `/traffic` never ends. Waiting for the answer is bounded instead.
        let client = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .build()
            .map_err(|error| network_failure(t("core.clash.prepare_request_failed"), error))?;
        Ok(Self {
            client,
            base: format!("http://127.0.0.1:{api_port}"),
            secret: secret.to_owned(),
        })
    }
    /// Follows `/traffic` and gives every second's speed to `on`, until `cancel` fires (`Ok`) or
    /// the stream fails, stalls or ends (an error: the API is the one thing that does not stop).
    pub async fn traffic(
        &self,
        cancel: &CancellationToken,
        mut on: impl FnMut(Speed) + Send,
    ) -> Result<()> {
        let sent = self
            .client
            .get(format!("{}/traffic", self.base))
            .bearer_auth(&self.secret)
            .send();
        let mut response = tokio::select! {biased;
            _=cancel.cancelled()=>return Ok(()),
            result=timeout(TRAFFIC_IDLE, sent)=>result
                .map_err(|_| failure(t("core.clash.speed_no_answer")))?
                .map_err(|error| network_failure(t("core.clash.connect_failed"), error))?,
        };
        if !response.status().is_success() {
            return Err(
                failure(t("core.clash.speed_refused")).with_status(response.status().as_u16())
            );
        }
        let mut buffer: Vec<u8> = Vec::new();
        loop {
            let chunk = tokio::select! {biased;
                _=cancel.cancelled()=>return Ok(()),
                result=timeout(TRAFFIC_IDLE, response.chunk())=>result
                    .map_err(|_| failure(t("core.clash.speed_stopped")))?
                    .map_err(|error| network_failure(t("core.clash.speed_interrupted"), error))?,
            };
            let Some(chunk) = chunk else {
                return Err(failure(t("core.clash.speed_closed")));
            };
            buffer.extend_from_slice(&chunk);
            while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
                if end > MAX_LINE {
                    return Err(failure(t("core.clash.speed_unexpected")));
                }
                let line: Vec<u8> = buffer.drain(..=end).collect();
                // A line that is not the shape ends the stream rather than showing a guess.
                on(parse_traffic_line(&line)?);
            }
            if buffer.len() > MAX_LINE {
                return Err(failure(t("core.clash.speed_unexpected")));
            }
        }
    }
}

/// One connection as sing-box reports it, reduced to the fields the screen uses. Everything else
/// in the answer is dropped when it is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raw {
    host: String,
    port: u16,
    network: String,
    process_path: String,
    upload: u64,
    download: u64,
    started: Option<u64>,
    chains: Vec<String>,
}
/// The newest `MAX_CONNECTIONS` connections and how many there are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub total: usize,
    pub items: Vec<Raw>,
}

/// A connection for the screen. The host and the program name are what the user's own
/// applications asked for: shown while the page is open, never kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConnectionView {
    pub host: String,
    pub port: u16,
    /// `tcp`, `udp` or `other`.
    pub network: &'static str,
    /// `proxy` (through a server), `direct` or `other`.
    pub route: &'static str,
    /// The name of the server, when `route` is `proxy`.
    pub server: Option<String>,
    /// The file name of the program, when sing-box looks programs up (a routing rule by program
    /// turns it on). Never the path.
    pub process: Option<String>,
    pub upload: u64,
    pub download: u64,
    /// How long the connection has lasted.
    pub seconds: Option<u64>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ConnectionsView {
    /// All connections; `items` holds the newest few of them.
    pub total: usize,
    pub items: Vec<ConnectionView>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Port {
    Number(u64),
    Text(String),
}
/// Go writes a missing list as `null`: that is the default here, like an absent field.
fn null_as_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}
#[derive(Deserialize, Default)]
struct Meta {
    #[serde(default, deserialize_with = "null_as_default")]
    network: String,
    #[serde(default, deserialize_with = "null_as_default")]
    host: String,
    #[serde(
        default,
        rename = "destinationIP",
        deserialize_with = "null_as_default"
    )]
    destination_ip: String,
    #[serde(default, rename = "destinationPort")]
    destination_port: Option<Port>,
    #[serde(default, rename = "processPath", deserialize_with = "null_as_default")]
    process_path: String,
}
#[derive(Deserialize)]
struct Item {
    #[serde(default, deserialize_with = "null_as_default")]
    metadata: Meta,
    #[serde(default, deserialize_with = "null_as_default")]
    upload: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    download: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    start: String,
    #[serde(default, deserialize_with = "null_as_default")]
    chains: Vec<String>,
}
#[derive(Deserialize)]
struct Body {
    #[serde(default, deserialize_with = "null_as_default")]
    connections: Vec<Item>,
}

/// Text from the network, shown on screen: no control characters, cut to `max` characters.
fn clean(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}
/// The file name of a program path, either kind of separator.
fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}
/// Days since 1970-01-01 of a calendar date (proleptic Gregorian).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}
/// Unix seconds of an RFC 3339 time (`2026-10-17T12:00:00.123456789+03:00`); `None` for anything
/// else. The fraction is dropped.
pub fn unix_from_rfc3339(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't' | b' ')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let number = |from: usize, to: usize| -> Option<i64> {
        let digits = text.get(from..to)?;
        digits
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| digits.parse().ok())
            .flatten()
    };
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut at = 19;
    if bytes[at] == b'.' {
        at += 1;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
    }
    let offset = match bytes.get(at)? {
        b'Z' | b'z' if at + 1 == bytes.len() => 0,
        sign @ (b'+' | b'-') if at + 6 == bytes.len() && bytes[at + 3] == b':' => {
            let (hours, minutes) = (number(at + 1, at + 3)?, number(at + 4, at + 6)?);
            if hours > 23 || minutes > 59 {
                return None;
            }
            let seconds = hours * 3600 + minutes * 60;
            if *sign == b'+' {
                seconds
            } else {
                -seconds
            }
        }
        _ => return None,
    };
    let seconds =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset;
    u64::try_from(seconds).ok()
}

impl Clash {
    /// The connections now: the newest `MAX_CONNECTIONS` and the count of all of them. The
    /// answer is read within a size and a time bound, and only the fields the screen uses are
    /// kept.
    pub async fn connections(&self) -> Result<Fetched> {
        let mut response = timeout(
            CONNECTIONS_WAIT,
            self.client
                .get(format!("{}/connections", self.base))
                .bearer_auth(&self.secret)
                .send(),
        )
        .await
        .map_err(|_| failure(t("core.clash.connections_no_answer")))?
        .map_err(|error| network_failure(t("core.clash.connect_failed"), error))?;
        if !response.status().is_success() {
            return Err(failure(t("core.clash.connections_refused"))
                .with_status(response.status().as_u16()));
        }
        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = timeout(CONNECTIONS_WAIT, response.chunk())
            .await
            .map_err(|_| failure(t("core.clash.connections_incomplete")))?
            .map_err(|error| network_failure(t("core.clash.connections_interrupted"), error))?
        {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_BODY {
                return Err(failure(t("core.clash.connections_too_large")));
            }
        }
        parse_connections(&body)
    }
}

/// The answer of `/connections`: the newest `MAX_CONNECTIONS` connections and the count of all of
/// them. Only the fields the screen uses are kept.
pub fn parse_connections(body: &[u8]) -> Result<Fetched> {
    let parsed: Body = serde_json::from_slice(body).map_err(|error| {
        failure(t("core.clash.connections_unexpected")).with_local_details(format!(
            "stage=connections_json; category={:?}; line={}; column={}",
            error.classify(),
            error.line(),
            error.column()
        ))
    })?;
    let total = parsed.connections.len();
    let mut items: Vec<Raw> = parsed
        .connections
        .into_iter()
        .map(|item| {
            let host = if item.metadata.host.is_empty() {
                item.metadata.destination_ip
            } else {
                item.metadata.host
            };
            let port = match item.metadata.destination_port {
                Some(Port::Number(port)) => u16::try_from(port).unwrap_or(0),
                Some(Port::Text(port)) => port.parse().unwrap_or(0),
                None => 0,
            };
            Raw {
                host: clean(&host, MAX_HOST),
                port,
                network: item.metadata.network.to_lowercase(),
                process_path: item.metadata.process_path,
                upload: item.upload,
                download: item.download,
                started: unix_from_rfc3339(&item.start),
                chains: item.chains.into_iter().take(8).collect(),
            }
        })
        .collect();
    // Newest first: the ones the user just opened. Unknown start times go last.
    items.sort_by_key(|item| std::cmp::Reverse(item.started));
    items.truncate(MAX_CONNECTIONS);
    Ok(Fetched { total, items })
}

/// The rows of the screen. `servers` are (id, name) pairs: sing-box names the outbound of a
/// connection by the id of the server, which the user never sees.
pub fn view(fetched: Fetched, servers: &[(String, String)], now: SystemTime) -> ConnectionsView {
    let now = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let items = fetched
        .items
        .into_iter()
        .map(|raw| {
            let server = raw.chains.iter().find_map(|tag| {
                servers
                    .iter()
                    .find(|(id, _)| id == tag)
                    .map(|(_, name)| clean(name, MAX_SERVER))
            });
            let route = if server.is_some() {
                "proxy"
            } else if raw.chains.iter().any(|tag| tag == "direct") {
                "direct"
            } else {
                "other"
            };
            let process = clean(file_name(&raw.process_path), MAX_PROCESS);
            ConnectionView {
                host: raw.host,
                port: raw.port,
                network: match raw.network.as_str() {
                    "tcp" => "tcp",
                    "udp" => "udp",
                    _ => "other",
                },
                route,
                server,
                process: (!process.is_empty()).then_some(process),
                upload: raw.upload,
                download: raw.download,
                seconds: raw.started.map(|started| now.saturating_sub(started)),
            }
        })
        .collect();
    ConnectionsView {
        total: fetched.total,
        items,
    }
}
