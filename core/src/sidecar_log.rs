//! What sing-box says about its own failures, kept without keeping its words.
//!
//! The log of sing-box holds the names of the sites that are visited, the addresses of the servers
//! and sometimes their credentials, so no line of it is stored or shown. Each line of a warning or
//! an error is read once and reduced to three facts: the stage that failed (a name lookup, a
//! connection, a handshake, a transfer), the cause (a timeout, a refusal, a reset, a block by the
//! system...) and whom it concerned (the direct outbound, a server by its id, something else).
//! Only counts and times are kept. Unrecognized warning/error facts count as Other; lines
//! without a recognized warning/error level are dropped.
//!
//! The cause is the point: "the server did not answer in time" and "the system forbids this
//! program to open the connection" look the same from outside and need opposite answers.
use crate::text::t;
use crate::tr;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex, OnceLock},
};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Longest line read; the rest of a longer line is dropped.
const MAX_LINE: usize = 2048;
/// Most servers whose last error is kept (a subscription can hold two thousand).
const MAX_SERVERS: usize = 5000;
/// How long a kind of failure stays quiet in the journal after it was said.
pub(crate) const QUIET_SECS: u64 = 300;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListenerPurpose {
    Proxy,
    ClashApi,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListenerCause {
    InUse,
    AccessDenied,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ListenerFailure {
    pub purpose: ListenerPurpose,
    pub port: u16,
    pub cause: ListenerCause,
}
impl ListenerFailure {
    pub fn error(self) -> crate::Error {
        let purpose = match self.purpose {
            ListenerPurpose::Proxy => t("core.sidecar_log.purpose_proxy"),
            ListenerPurpose::ClashApi => t("core.sidecar_log.purpose_clash_api"),
        };
        let (code, reason) = match self.cause {
            ListenerCause::InUse => (
                crate::failure::FailureCode::LocalPortInUse,
                t("core.sidecar_log.port_busy"),
            ),
            ListenerCause::AccessDenied => (
                crate::failure::FailureCode::LocalPortDenied,
                t("core.sidecar_log.port_forbidden"),
            ),
        };
        crate::Error::failure(
            code,
            tr!(
                "core.sidecar_log.port_failed",
                port = self.port,
                purpose = purpose,
                reason = reason
            ),
        )
    }
}

/// A closed observation; raw text and arbitrary operands never reach the collector.
pub(crate) enum Observation {
    Failure(Entry),
    Listener(ListenerFailure),
    Warning(crate::xray_log::WarningCategory),
}

fn observe(line: &str) -> Option<Observation> {
    let (colors, shape, _) = patterns();
    let line = colors.replace_all(line, "");
    let captures = shape.captures(line.trim())?;
    if matches!(&captures[1], "ERROR" | "FATAL") {
        // Pinned 1.14.1 Start wrappers plus Go's TCP bind errors. Do not match outbound
        // connect errors, arbitrary hosts, unknown tags, trailing text or another operation.
        static LISTENER: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
            r"^start service: (start inbound/mixed\[mixed\]|finish-start clash server: external controller listen error): listen tcp 127\.0\.0\.1:([0-9]{1,5}): bind: (address already in use|Only one usage of each socket address \(protocol/network address/port\) is normally permitted\.|permission denied|An attempt was made to access a socket in a way forbidden by its access permissions\.)$"
        ).expect("fixed sing-box listener grammar")
        });
        if let Some(found) = LISTENER.captures(&captures[2]) {
            let Some(port) = found[2].parse::<u16>().ok().filter(|p| *p != 0) else {
                return Some(Observation::Failure(classify_body(&captures[2])));
            };
            let purpose = if &found[1] == "start inbound/mixed[mixed]" {
                ListenerPurpose::Proxy
            } else {
                ListenerPurpose::ClashApi
            };
            // i18n-ignore: the text sing-box and Windows print, compared as it is
            let cause = if matches!(&found[3], "address already in use" | "Only one usage of each socket address (protocol/network address/port) is normally permitted.") {
                ListenerCause::InUse
            } else {
                ListenerCause::AccessDenied
            };
            return Some(Observation::Listener(ListenerFailure {
                purpose,
                port,
                cause,
            }));
        }
    }
    Some(Observation::Failure(classify_body(&captures[2])))
}

/// What failed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// A name could not be resolved.
    Dns,
    /// A connection was not opened.
    Dial,
    /// TLS or Reality did not complete.
    Handshake,
    /// A connection that was open broke.
    Transfer,
    Other,
}
impl Stage {
    fn said(self) -> &'static str {
        match self {
            Self::Dns => t("core.sidecar_log.cause_dns"),
            Self::Dial => t("core.sidecar_log.cause_connect"),
            Self::Handshake => t("core.sidecar_log.cause_handshake"),
            Self::Transfer => t("core.sidecar_log.cause_lost"),
            Self::Other => t("core.sidecar_log.cause_reported"),
        }
    }
}

/// Why it failed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// No answer in time.
    Timeout,
    /// The system denied the operation; the responsible policy is not known.
    Blocked,
    /// The other side refused.
    Refused,
    /// The connection was reset.
    Reset,
    /// The connection ended without an answer.
    Closed,
    /// No route to the other side.
    Unreachable,
    /// The name does not exist.
    NotFound,
    Tls,
    Reality,
    Other,
}
impl Cause {
    fn said(self) -> &'static str {
        match self {
            Self::Timeout => t("core.sidecar_log.reason_timeout"),
            Self::Blocked => t("core.sidecar_log.reason_forbidden"),
            Self::Refused => t("core.sidecar_log.reason_refused"),
            Self::Reset => t("core.sidecar_log.reason_reset"),
            Self::Closed => t("core.sidecar_log.reason_closed"),
            Self::Unreachable => t("core.sidecar_log.reason_network_unreachable"),
            Self::NotFound => t("core.sidecar_log.reason_name_not_found"),
            Self::Tls => t("core.sidecar_log.reason_tls"),
            Self::Reality => t("core.sidecar_log.reason_reality"),
            Self::Other => t("core.sidecar_log.reason_unknown"),
        }
    }
}

/// Whom it concerned.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Whom {
    /// The outbound that leaves by the real interface.
    Direct,
    /// A server of the subscription, named by its id (its address is not kept).
    Server,
    /// The server that is selected: a connection of the user, which goes through the selector.
    Selected,
    Other,
}

/// One line of the log, reduced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub stage: Stage,
    pub cause: Cause,
    pub whom: Whom,
    /// The id of the server, when `whom` is `Server`.
    pub server: Option<String>,
}

fn patterns() -> &'static (Regex, Regex, Regex) {
    static PATTERNS: OnceLock<(Regex, Regex, Regex)> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        (
            // Colors of the terminal, which sing-box writes into a pipe as well.
            Regex::new("\x1b\\[[0-9;]*m").expect("a fixed pattern"),
            // `+0300 2026-10-19 17:09:42 ERROR [id 10.1s] text` or `ERROR[0000] text`.
            Regex::new(
                r"^(?:\S+ \d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} )?(WARN|ERROR|FATAL|PANIC)(?:\[\d+\])? (?:\[[^\]]*\] )?(.*)$",
            )
            .expect("a fixed pattern"),
            // `outbound/vless[<64 hex>]`, `outbound/direct[direct]`, `outbound/selector[main]`.
            Regex::new(r"outbound/[a-z0-9-]+\[([0-9a-f]{64}|direct|main)\]").expect("a fixed pattern"),
        )
    })
}

/// What a line of the log of sing-box says, if it is a warning or an error that says something.
pub fn classify(line: &str) -> Option<Entry> {
    let (colors, shape, _) = patterns();
    let line = colors.replace_all(line, "");
    Some(classify_body(shape.captures(line.trim())?.get(2)?.as_str()))
}
pub(crate) fn classify_body(text: &str) -> Entry {
    let (_, _, outbound) = patterns();
    let text = text.to_lowercase();
    let (whom, server) = match outbound.captures(&text).and_then(|c| c.get(1)) {
        Some(tag) if tag.as_str() == "direct" => (Whom::Direct, None),
        Some(tag) if tag.as_str() == "main" => (Whom::Selected, None),
        Some(tag) => (Whom::Server, Some(tag.as_str().to_owned())),
        None => (Whom::Other, None),
    };
    // Destination names may themselves contain words such as reality, eof or x509.
    // Remove operands before recognizing the error grammar, including single-label names.
    static OPERANDS: OnceLock<Regex> = OnceLock::new();
    static HOSTS: OnceLock<Regex> = OnceLock::new();
    let text = OPERANDS.get_or_init(|| Regex::new(
        r"\b(open connection to|lookup|dial (?:tcp|udp)[46]?|read (?:tcp|udp)[46]?|write (?:tcp|udp)[46]?)\s+\S+"
    ).expect("a fixed pattern")).replace_all(&text, "$1 ");
    let text = HOSTS
        .get_or_init(|| {
            Regex::new(r"\b(?:[a-z0-9_-]+\.)+[a-z0-9_-]+(?::[0-9]+)?\b").expect("a fixed pattern")
        })
        .replace_all(&text, "[host]");
    let cause = if has(
        &text,
        &[
            "forbidden by its access permissions",
            "permission denied",
            "access is denied",
        ],
    ) {
        Cause::Blocked
    } else if has(&text, &["actively refused", "connection refused"]) {
        Cause::Refused
    } else if has(
        &text,
        &["forcibly closed", "connection reset", "broken pipe"],
    ) {
        Cause::Reset
    } else if has(
        &text,
        &[
            "i/o timeout",
            "context deadline exceeded",
            "timed out",
            "deadline exceeded",
        ],
    ) {
        Cause::Timeout
    } else if has(
        &text,
        &["unreachable", "no route to host", "network is down"],
    ) {
        Cause::Unreachable
    } else if has(&text, &["no such host", "name not known", "nxdomain"]) {
        Cause::NotFound
    } else if text.contains("reality") {
        Cause::Reality
    } else if has(&text, &["tls:", "x509", "certificate"]) {
        Cause::Tls
    } else if has(&text, &["eof"]) {
        Cause::Closed
    } else {
        Cause::Other
    };
    let stage = if has(&text, &["lookup ", "dns: ", "dns/"]) {
        Stage::Dns
    } else if has(&text, &["reality", "tls:", "x509", "handshake"]) {
        Stage::Handshake
    } else if has(&text, &["dial ", "connectex", "connect:"]) {
        Stage::Dial
    } else if has(
        &text,
        &["read ", "write ", "wsarecv", "wsasend", "eof", "reset"],
    ) {
        Stage::Transfer
    } else {
        Stage::Other
    };
    Entry {
        stage,
        cause,
        whom,
        server,
    }
}
fn has(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

/// How often one kind of failure happened.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Group {
    pub stage: Stage,
    pub cause: Cause,
    pub whom: Whom,
    pub count: u64,
    pub first_at: u64,
    pub last_at: u64,
}
/// The last failure of one server.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ServerError {
    pub stage: Stage,
    pub cause: Cause,
    pub count: u64,
    pub last_at: u64,
}
/// What the diagnostics get.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct Summary {
    /// Lines of warning level or above that were read.
    pub lines: u64,
    pub groups: Vec<Group>,
    /// By server id.
    pub servers: HashMap<String, ServerError>,
}

#[derive(Default)]
struct Counted {
    count: u64,
    first_at: u64,
    last_at: u64,
    /// How many were said in the journal already, and when.
    said: u64,
    said_at: Option<u64>,
}
#[derive(Default)]
struct State {
    lines: u64,
    groups: HashMap<(Stage, Cause, Whom), Counted>,
    servers: HashMap<String, ServerError>,
    last_direct: Option<(Stage, Cause)>,
    listener: Option<ListenerFailure>,
    warnings: HashMap<crate::xray_log::WarningCategory, Counted>,
}

/// The reduced log of one run of sing-box, filled by [`read`] and read by whoever shows it.
#[derive(Default)]
pub struct SingBoxLog {
    state: Mutex<State>,
    listeners: Option<(Option<u16>, u16)>,
}
impl SingBoxLog {
    pub(crate) fn with_listeners(proxy: Option<u16>, api: u16) -> Self {
        Self {
            state: Mutex::default(),
            listeners: Some((proxy, api)),
        }
    }
    pub fn listener_failure(&self) -> Option<ListenerFailure> {
        self.state().listener
    }
    pub fn warning_summary(&self) -> Vec<crate::xray_log::WarningCount> {
        let state = self.state();
        let mut counts: Vec<_> = state
            .warnings
            .iter()
            .map(|(category, count)| crate::xray_log::WarningCount {
                category: *category,
                count: count.count,
            })
            .collect();
        counts.sort_by_key(|count| count.category);
        counts
    }
    pub fn informational_notes(&self, now: u64) -> Vec<String> {
        let mut state = self.state();
        let mut notes = Vec::new();
        for (category, counted) in &mut state.warnings {
            if counted.count > counted.said
                && counted
                    .said_at
                    .is_none_or(|at| now.saturating_sub(at) >= QUIET_SECS)
            {
                notes.push(category.note().to_owned());
                counted.said = counted.count;
                counted.said_at = Some(now);
            }
        }
        notes.sort();
        notes
    }
    pub(crate) fn record_observation(&self, observation: Observation, now: u64) {
        match observation {
            Observation::Failure(entry) => self.record_entry(entry, now),
            Observation::Listener(failure) => {
                let expected = self
                    .listeners
                    .is_some_and(|(proxy, api)| match failure.purpose {
                        ListenerPurpose::Proxy => proxy == Some(failure.port),
                        ListenerPurpose::ClashApi => api == failure.port,
                    });
                if expected {
                    self.state().listener.get_or_insert(failure);
                } else {
                    self.record_entry(
                        Entry {
                            stage: Stage::Other,
                            cause: Cause::Other,
                            whom: Whom::Other,
                            server: None,
                        },
                        now,
                    );
                }
            }
            Observation::Warning(category) => {
                let mut state = self.state();
                let counted = state.warnings.entry(category).or_default();
                counted.count = counted.count.saturating_add(1);
            }
        }
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Reads one line of the log; `now` is Unix seconds.
    pub fn record(&self, line: &str, now: u64) {
        if let Some(observation) = observe(line) {
            self.record_observation(observation, now);
        }
    }
    pub(crate) fn record_entry(&self, entry: Entry, now: u64) {
        let mut state = self.state();
        state.lines += 1;
        let group = state
            .groups
            .entry((entry.stage, entry.cause, entry.whom))
            .or_default();
        if group.count == 0 {
            group.first_at = now;
        }
        group.count += 1;
        group.last_at = now;
        if entry.whom == Whom::Direct {
            state.last_direct = Some((entry.stage, entry.cause));
        }
        if let Some(id) = entry.server {
            if state.servers.len() < MAX_SERVERS || state.servers.contains_key(&id) {
                let last = state.servers.entry(id).or_insert(ServerError {
                    stage: entry.stage,
                    cause: entry.cause,
                    count: 0,
                    last_at: now,
                });
                last.stage = entry.stage;
                last.cause = entry.cause;
                last.count += 1;
                last.last_at = now;
            }
        }
    }
    /// Forgets everything: a new run of sing-box starts with a clean log.
    pub fn clear(&self) {
        *self.state() = State::default();
    }
    pub fn summary(&self) -> Summary {
        let state = self.state();
        let mut groups: Vec<Group> = state
            .groups
            .iter()
            .map(|((stage, cause, whom), counted)| Group {
                stage: *stage,
                cause: *cause,
                whom: *whom,
                count: counted.count,
                first_at: counted.first_at,
                last_at: counted.last_at,
            })
            .collect();
        groups.sort_by(|a, b| b.count.cmp(&a.count).then(a.last_at.cmp(&b.last_at)));
        Summary {
            lines: state.lines,
            groups,
            servers: state.servers.clone(),
        }
    }
    /// The failure of the direct outbound that happened last, as a phrase for the journal.
    pub fn last_direct(&self) -> Option<String> {
        let state = self.state();
        state
            .last_direct
            .map(|(stage, cause)| format!("{}: {}", stage.said(), cause.said()))
    }
    /// What is worth a line of the journal since the last call: a kind of failure the first time,
    /// and again, with how many more happened, when it went on after [`QUIET_SECS`].
    pub fn notes(&self, now: u64) -> Vec<String> {
        self.notes_for(now, "sing-box")
    }
    pub(crate) fn notes_for(&self, now: u64, engine: &'static str) -> Vec<String> {
        let mut state = self.state();
        let mut notes = Vec::new();
        for ((stage, cause, whom), counted) in &mut state.groups {
            let new = counted.count - counted.said;
            let due = counted
                .said_at
                .is_none_or(|at| now.saturating_sub(at) >= QUIET_SECS);
            if new == 0 || !due {
                continue;
            }
            let subject = match whom {
                Whom::Direct => t("core.sidecar_log.whom_direct"),
                Whom::Server => t("core.sidecar_log.whom_server"),
                Whom::Selected => t("core.sidecar_log.whom_selected_server"),
                Whom::Other => "",
            };
            let more = if counted.said > 0 {
                tr!("core.sidecar_log.more", new = new)
            } else {
                String::new()
            };
            notes.push(format!(
                "{engine} {}{subject}: {}{more}",
                stage.said(),
                cause.said()
            ));
            counted.said = counted.count;
            counted.said_at = Some(now);
        }
        notes.sort();
        notes
    }
}

/// Reads the log of the child from `pipe` until it closes, line by line, and keeps only what
/// [`classify`] makes of each. Memory stays bounded: a line is cut at [`MAX_LINE`] and the rest
/// of it is dropped, so a child that never ends a line cannot grow it.
pub async fn read(pipe: impl AsyncRead + Unpin, log: std::sync::Arc<SingBoxLog>) {
    read_observed(pipe, log, observe).await;
}
#[cfg(test)]
pub(crate) async fn read_classified(
    pipe: impl AsyncRead + Unpin,
    log: std::sync::Arc<SingBoxLog>,
    classify: fn(&str) -> Option<Entry>,
) {
    read_observed(pipe, log, |line| classify(line).map(Observation::Failure)).await;
}
pub(crate) async fn read_observed(
    mut pipe: impl AsyncRead + Unpin,
    log: std::sync::Arc<SingBoxLog>,
    observe: impl Fn(&str) -> Option<Observation>,
) {
    let mut buffer = [0u8; 4096];
    let mut line: Vec<u8> = Vec::with_capacity(256);
    while let Ok(n) = pipe.read(&mut buffer).await {
        if n == 0 {
            break;
        }
        for &byte in &buffer[..n] {
            if byte == b'\n' {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                if let Some(observation) = observe(&String::from_utf8_lossy(&line)) {
                    log.record_observation(observation, now);
                }
                line.clear();
            } else if line.len() < MAX_LINE {
                line.push(byte);
            }
        }
    }
    if !line.is_empty() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        if let Some(observation) = observe(&String::from_utf8_lossy(&line)) {
            log.record_observation(observation, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_fixed_local_bind_grammar_can_name_a_listener() {
        for (purpose, operation, port) in [
            (ListenerPurpose::Proxy, "start inbound/mixed[mixed]", 2080),
            (
                ListenerPurpose::ClashApi,
                "finish-start clash server: external controller listen error",
                2081,
            ),
        ] {
            for (cause, message) in [
                (ListenerCause::InUse, "address already in use"),
                (ListenerCause::InUse, "Only one usage of each socket address (protocol/network address/port) is normally permitted."),
                (ListenerCause::AccessDenied, "permission denied"),
                (ListenerCause::AccessDenied, "An attempt was made to access a socket in a way forbidden by its access permissions."),
            ] {
                let text = format!("FATAL[0000] start service: {operation}: listen tcp 127.0.0.1:{port}: bind: {message}");
                let log = SingBoxLog::with_listeners(Some(2080), 2081);
                log.record(&text, 1);
                assert_eq!(log.listener_failure(), Some(ListenerFailure { purpose, port, cause }));
                assert!(log.summary().groups.is_empty(), "a bind operation is not a remote failure");
                for near in [
                    text.replace("127.0.0.1", "remote-canary.example"),
                    text.replace("bind:", "connect:"),
                    text.replace("start service:", "unknown service:"),
                    text.replace(&format!(":{port}:"), ":0:"),
                    text.replace(&format!(":{port}:"), ":65536:"),
                    format!("{text} secret-canary"),
                ] {
                    let log = SingBoxLog::with_listeners(Some(2080), 2081);
                    log.record(&near, 1);
                    assert!(log.listener_failure().is_none());
                    assert_eq!(log.summary().lines, 1, "unknown errors remain visible");
                    assert!(!format!("{:?}", log.summary()).contains("canary"));
                }
            }
        }
    }

    #[test]
    fn destination_words_do_not_change_the_failure_stage_or_cause() {
        for host in [
            "reality-tv.example",
            "x509.example",
            "eof.example",
            "unreachable.example",
            "nxdomain.example",
            "eof",
        ] {
            let entry = classify(&format!("ERROR connection: open connection to {host}:443 using outbound/direct[direct]: dial tcp {host}:443: i/o timeout")).unwrap();
            assert_eq!(entry.stage, Stage::Dial, "{host}");
            assert_eq!(entry.cause, Cause::Timeout, "{host}");
            let entry = classify(&format!("ERROR dns: lookup {host}: no such host")).unwrap();
            assert_eq!(entry.stage, Stage::Dns);
            assert_eq!(entry.cause, Cause::NotFound);
        }
    }

    #[test]
    fn the_latest_direct_failure_follows_event_order_within_one_second() {
        let log = SingBoxLog::default();
        log.record(
            "ERROR dial tcp site.example:443: i/o timeout using outbound/direct[direct]",
            1000,
        );
        log.record(
            "ERROR dial tcp site.example:443: connection refused using outbound/direct[direct]",
            1000,
        );
        assert!(log.last_direct().unwrap().contains(Cause::Refused.said()));
        log.record(
            "ERROR dial tcp site.example:443: i/o timeout using outbound/direct[direct]",
            1000,
        );
        assert!(log.last_direct().unwrap().contains(Cause::Timeout.said()));
    }

    use std::sync::Arc;
    use tokio::io::AsyncWriteExt;

    const ID: &str = "28c6ff378ad0abce958033c7b52f9e948e21c6a6abe50c0a1812e2ba0706408a";

    fn entry(line: &str) -> Entry {
        classify(line).unwrap_or_else(|| panic!("not classified: {line}"))
    }

    // The lines below are what sing-box 1.14.1 wrote on the Windows machine where the names of
    // the sites could not be resolved (a VPN client's filter dropped port 53).
    #[test]
    fn a_name_lookup_that_got_no_answer_over_udp_is_a_dns_timeout_of_the_direct_outbound() {
        let line = "+0300 2026-10-19 17:09:42 ERROR [3118982087 10.1s] connection: open connection to ya.ru:443 using outbound/direct[direct]: lookup ya.ru: (exchange4: ya.ru.: dial UDP connection: dial udp 8.8.4.4:53: i/o timeout | exchange6: ya.ru.: context deadline exceeded)";
        let e = entry(line);
        assert_eq!(
            (e.stage, e.cause, e.whom),
            (Stage::Dns, Cause::Timeout, Whom::Direct)
        );
        assert_eq!(e.server, None);
    }

    #[test]
    fn a_name_lookup_the_system_forbids_is_a_block_not_a_timeout() {
        let line = "+0300 2026-10-19 17:30:11 ERROR [1665504163 1ms] connection: open connection to ya.ru:443 using outbound/direct[direct]: lookup ya.ru: (exchange6: dial TCP connection: dial tcp 8.8.8.8:53: connectex: An attempt was made to access a socket in a way forbidden by its access permissions.)";
        let e = entry(line);
        assert_eq!((e.stage, e.cause), (Stage::Dns, Cause::Blocked));
    }

    #[test]
    fn a_connection_to_a_server_that_timed_out_names_the_server_by_its_id() {
        let line = format!("+0300 2026-10-19 17:30:11 ERROR [1 5.0s] connection: open connection to www.gstatic.com:443 using outbound/vless[{ID}]: dial tcp 203.0.113.5:443: i/o timeout");
        let e = entry(&line);
        assert_eq!(
            (e.stage, e.cause, e.whom),
            (Stage::Dial, Cause::Timeout, Whom::Server)
        );
        assert_eq!(e.server.as_deref(), Some(ID));
    }

    // A line as sing-box 1.14.1 writes it (colors included) when the selected server refuses.
    #[test]
    fn a_user_connection_through_a_server_that_refuses_is_a_failure_of_the_selected_server() {
        let line = "+0300 2026-10-19 18:04:02 \x1b[31mERROR\x1b[0m [\x1b[38;5;102m2269456982\x1b[0m 0ms] connection: open connection to rift-refused-test.example:80 using outbound/selector[main]: dial tcp 127.0.0.1:9: connect: connection refused\n";
        let e = entry(line);
        assert_eq!(
            (e.stage, e.cause, e.whom),
            (Stage::Dial, Cause::Refused, Whom::Selected)
        );
        assert_eq!(e.server, None);
    }

    #[test]
    fn the_other_things_that_go_wrong_with_a_server_are_told_apart() {
        let on = |text: &str| {
            entry(&format!("+0300 2026-10-19 17:30:11 WARN [1 1s] connection: {text} using outbound/vless[{ID}]: {text}"))
        };
        assert_eq!(on("dial tcp 1.2.3.4:443: connectex: No connection could be made because the target machine actively refused it.").cause, Cause::Refused);
        assert_eq!(on("read tcp 10.0.0.2:5000->1.2.3.4:443: wsarecv: An existing connection was forcibly closed by the remote host.").cause, Cause::Reset);
        assert_eq!(
            on("REALITY: processed invalid connection").stage,
            Stage::Handshake
        );
        assert_eq!(
            on("REALITY: processed invalid connection").cause,
            Cause::Reality
        );
        assert_eq!(
            on("tls: failed to verify certificate: x509: certificate has expired").cause,
            Cause::Tls
        );
        assert_eq!(
            on("read tcp 10.0.0.2:5000->1.2.3.4:443: EOF").cause,
            Cause::Closed
        );
        assert_eq!(
            on("dial tcp 1.2.3.4:443: connect: network is unreachable").cause,
            Cause::Unreachable
        );
        assert_eq!(
            on("lookup nope.example: no such host").cause,
            Cause::NotFound
        );
    }

    #[test]
    fn colors_and_the_short_form_of_the_level_are_read() {
        let colored = "+0300 2026-10-19 17:09:42 \x1b[31mERROR\x1b[0m [\x1b[38;5;215m3118982087\x1b[0m 10.1s] connection: lookup ya.ru: i/o timeout";
        assert_eq!(entry(colored).cause, Cause::Timeout);
        assert_eq!(
            entry("ERROR[0000] dial tcp 1.2.3.4:443: i/o timeout").stage,
            Stage::Dial
        );
    }

    #[test]
    fn what_is_not_a_warning_or_an_error_says_nothing() {
        for line in [
            "",
            "+0300 2026-10-19 17:09:42 DEBUG [1 0ms] dns: lookup domain ya.ru",
            "+0300 2026-10-19 17:09:42 INFO [1 0ms] inbound/mixed[mixed]: tcp server started at 127.0.0.1:2080",
            "dial tcp secret.example:443 uuid 11111111-1111-4111-8111-111111111111 password=hunter2",
            "vless://11111111-1111-4111-8111-111111111111@secret.example:443",
        ] {
            assert_eq!(classify(line), None, "{line}");
        }
    }

    #[test]
    fn nothing_the_log_said_is_kept_but_counts_and_ids() {
        let log = SingBoxLog::default();
        log.record("+0300 2026-10-19 17:09:42 ERROR [1 10.1s] connection: open connection to private-site.example:443 using outbound/direct[direct]: lookup private-site.example: i/o timeout to 198.51.100.9:53 token=hunter2", 100);
        log.record(&format!("+0300 2026-10-19 17:09:43 ERROR [2 5s] connection: open connection to other.example:443 using outbound/vless[{ID}]: dial tcp 203.0.113.5:443: i/o timeout"), 101);
        let kept = format!(
            "{:?} {}",
            log.summary(),
            serde_json::to_string(&log.summary()).unwrap()
        );
        let said = log.notes(200).join(" ");
        for word in [
            "private-site",
            "other.example",
            "198.51.100.9",
            "203.0.113.5",
            "hunter2",
        ] {
            assert!(!kept.contains(word), "{word} kept: {kept}");
            assert!(!said.contains(word), "{word} said: {said}");
        }
        assert!(kept.contains(ID), "the id of a server is kept");
    }

    #[test]
    fn the_journal_hears_a_kind_of_failure_once_and_again_only_after_a_quiet_time() {
        let log = SingBoxLog::default();
        let line = "+0300 2026-10-19 17:09:42 ERROR [1 10s] connection: open connection to a.example:443 using outbound/direct[direct]: lookup a.example: i/o timeout";
        log.record(line, 1000);
        log.record(line, 1001);
        let first = log.notes(1001);
        assert_eq!(first.len(), 1);
        assert!(
            first[0].contains("не смог найти адрес по имени"),
            "{first:?}"
        );
        assert!(first[0].contains("напрямую"));
        assert!(!first[0].contains("ещё"), "nothing was said before");
        // More of the same, at once: quiet.
        log.record(line, 1002);
        assert!(log.notes(1002).is_empty());
        // After the quiet time, with how many more.
        assert!(log.notes(1001 + QUIET_SECS - 1).is_empty());
        let again = log.notes(1001 + QUIET_SECS);
        assert_eq!(again.len(), 1);
        assert!(again[0].contains("(ещё 1)"), "{again:?}");
        assert!(log.notes(1001 + QUIET_SECS + 1).is_empty(), "said once");
    }

    #[test]
    fn the_last_failure_of_the_direct_outbound_is_a_phrase_for_the_journal() {
        let log = SingBoxLog::default();
        assert_eq!(log.last_direct(), None);
        log.record("+0300 2026-10-19 17:09:42 ERROR [1 10s] connection: open connection to a.example:443 using outbound/direct[direct]: lookup a.example: i/o timeout", 5);
        assert_eq!(
            log.last_direct().as_deref(),
            Some("не смог найти адрес по имени: нет ответа (таймаут)")
        );
    }

    #[test]
    fn a_new_run_starts_with_a_clean_log() {
        let log = SingBoxLog::default();
        log.record("ERROR[0000] dial tcp 1.2.3.4:443: i/o timeout", 5);
        assert_eq!(log.summary().lines, 1);
        log.clear();
        assert_eq!(log.summary(), Summary::default());
    }

    #[test]
    fn the_servers_whose_error_is_kept_are_bounded() {
        let log = SingBoxLog::default();
        for n in 0..MAX_SERVERS + 10 {
            let id = format!("{n:064x}");
            log.record(
                &format!(
                    "ERROR[0000] using outbound/vless[{id}]: dial tcp 1.2.3.4:443: i/o timeout"
                ),
                5,
            );
        }
        assert_eq!(log.summary().servers.len(), MAX_SERVERS);
        // A server that is kept goes on being counted.
        let kept = format!("{:064x}", 0);
        log.record(
            &format!("ERROR[0000] using outbound/vless[{kept}]: dial tcp 1.2.3.4:443: i/o timeout"),
            6,
        );
        assert_eq!(log.summary().servers[&kept].count, 2);
    }

    #[tokio::test]
    async fn the_pipe_is_read_to_its_end_line_by_line_with_bounded_memory() {
        // A small pipe: the writer only finishes if every chunk is read.
        let (mut writer, reader) = tokio::io::duplex(64);
        let log = Arc::new(SingBoxLog::default());
        let reading = tokio::spawn(read(reader, log.clone()));
        let long = "x".repeat(1 << 20);
        let written = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            writer
                .write_all(b"ERROR[0000] dial tcp 1.2.3.4:443: i/o timeout\n")
                .await?;
            writer.write_all(long.as_bytes()).await?;
            writer
                .write_all(b"\nERROR[0000] dial tcp 1.2.3.4:443: connect: connection refused\n")
                .await
        })
        .await
        .expect("the reader stalled");
        assert!(written.is_ok());
        drop(writer);
        reading.await.unwrap();
        let summary = log.summary();
        assert_eq!(summary.lines, 2);
        let counts: Vec<_> = summary.groups.iter().map(|g| (g.cause, g.count)).collect();
        assert!(counts.contains(&(Cause::Timeout, 1)) && counts.contains(&(Cause::Refused, 1)));
    }
}
