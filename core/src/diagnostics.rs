//! Local operation metadata and human-readable failure details. Nothing here sends data.
use crate::tr;
use crate::{failure::FailureCode, Error};
use serde_json::{Map as JsonMap, Value};
use std::{collections::BTreeMap, panic::Location};

/// Most completed steps retained for one operation.
pub const MAX_TRAIL: usize = 8;

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum $name { $($(#[$vmeta])* $variant),+ }
        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            /// The stable name used in local diagnostic metadata.
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
            /// The value a name stands for; `None` for any other text.
            pub fn parse(text: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|value| value.as_str() == text)
            }
        }
    };
}

string_enum! {
    /// What Rift was doing when it failed.
    Operation {
        SubscriptionRefresh => "subscription_refresh",
        Selection => "selection",
        Connection => "connection",
        Sidecar => "sidecar",
        Profile => "profile",
        Settings => "settings",
        Geodata => "geodata",
        Autostart => "autostart",
        Elevation => "elevation",
        Other => "other",
    }
}

string_enum! {
    /// Where in the operation the failure happened. `Unspecified` until the code is marked.
    #[derive(Default)]
    Stage {
        #[default]
        Unspecified => "unspecified",
        BuildClient => "build_client",
        SendRequest => "send_request",
        ReadBody => "read_body",
        Parse => "parse",
        Filter => "filter",
        Persist => "persist",
    }
}

string_enum! {
    /// What started the operation.
    #[derive(Default)]
    Trigger {
        Manual => "manual",
        Scheduled => "scheduled",
        Startup => "startup",
        /// A check begun by the loss of the server in use.
        Failover => "failover",
        /// A check asked for by hand ("Check now").
        CheckNow => "check_now",
        #[default]
        Unknown => "unknown",
    }
}

string_enum! {
    /// The route of the failed request.
    #[derive(Default)]
    Via {
        Direct => "direct",
        LocalProxy => "local_proxy",
        #[default]
        NotApplicable => "not_applicable",
    }
}

string_enum! {
    /// A step an operation has completed. A diagnostic carries the steps done before the failure.
    Mark {
        RefreshStarted => "refresh_started",
        ClientBuilt => "client_built",
        RequestSent => "request_sent",
        HeadersReceived => "headers_received",
        BodyRead => "body_read",
        Parsed => "parsed",
        Filtered => "filtered",
    }
}

string_enum! {
    /// What the body of a subscription answer is, judged by its first bytes and nothing else.
    BodyKind {
        Empty => "empty",
        Html => "html",
        JsonXray => "json_xray",
        JsonOther => "json_other",
        Base64Links => "base64_links",
        PlainLinks => "plain_links",
        TextOther => "text_other",
        NotUtf8 => "not_utf8",
    }
}

string_enum! {
    /// A coarse body size retained alongside parsing observations.
    SizeBucket {
        Zero => "0",
        Under1k => "lt_1k",
        Under16k => "lt_16k",
        Under256k => "lt_256k",
        Under1m => "lt_1m",
        Over1m => "ge_1m",
    }
}

string_enum! {
    /// The class of the `Content-Type` of the answer. The header itself stays home.
    ContentClass {
        TextPlain => "text_plain",
        Json => "json",
        Html => "html",
        Octet => "octet",
        Other => "other",
        Absent => "absent",
    }
}

string_enum! {
    /// Why a line of a subscription is not a server.
    Rejection {
        UnsupportedScheme => "unsupported_scheme",
        BadUri => "bad_uri",
        MissingField => "missing_field",
        BadReality => "bad_reality",
        UnsupportedTransport => "unsupported_transport",
        NotALink => "not_a_link",
        UnknownParam => "unknown_param",
        UnsupportedParam => "unsupported_param",
        UnsupportedMode => "unsupported_mode",
        IncompatibleParam => "incompatible_param",
        InvalidFinalmask => "invalid_finalmask",
        InvalidExtra => "invalid_extra",
        InvalidField => "invalid_field",
        InvalidCredentials => "invalid_credentials",
        InvalidEndpoint => "invalid_endpoint",
        UnsupportedSecurity => "unsupported_security",
        Other => "other",
    }
}

string_enum! {
    /// Known parameter keys present in refused input; never their values or unknown key names.
    ParameterKind {
        Fm => "fm",
        Mode => "mode",
        Extra => "extra",
    }
}

string_enum! {
    /// Transport spelling in refused rows that contain a known parameter.
    TransportKind {
        Tcp => "tcp",
        Ws => "ws",
        Grpc => "grpc",
        Xhttp => "xhttp",
        Other => "other",
    }
}

string_enum! {
    /// The scheme of a line that was rejected, from a closed list.
    Scheme {
        Vless => "vless",
        Vmess => "vmess",
        Trojan => "trojan",
        Ss => "ss",
        Hysteria2 => "hysteria2",
        Tuic => "tuic",
        Other => "other",
    }
}

string_enum! {
    /// The best guess at why a request failed, read from the kind of the error and never from its
    /// text.
    Cause {
        Timeout => "timeout",
        Connect => "connect",
        Tls => "tls",
        Dns => "dns",
        Reset => "reset",
        Other => "other",
    }
}

string_enum! {
    /// What a selection cycle was for.
    CycleKind {
        Initial => "initial",
        Failover => "failover",
        CheckNow => "check_now",
    }
}

impl Scheme {
    /// The scheme of a line of a subscription: what stands before `://`, from the closed list.
    pub fn of_line(line: &str) -> Self {
        let Some((scheme, _)) = line.split_once("://") else {
            return Self::Other;
        };
        let scheme = scheme.to_ascii_lowercase();
        match scheme.as_str() {
            "vless" => Self::Vless,
            "vmess" => Self::Vmess,
            "trojan" => Self::Trojan,
            "ss" => Self::Ss,
            "hysteria2" | "hy2" => Self::Hysteria2,
            "tuic" => Self::Tuic,
            _ => Self::Other,
        }
    }
}

impl SizeBucket {
    /// The class of a body of `bytes` bytes.
    pub const fn of(bytes: usize) -> Self {
        match bytes {
            0 => Self::Zero,
            1..=1023 => Self::Under1k,
            1024..=16_383 => Self::Under16k,
            16_384..=262_143 => Self::Under256k,
            262_144..=1_048_575 => Self::Under1m,
            _ => Self::Over1m,
        }
    }
}

/// The lines of one subscription body, counted. Counts stop at [`u16::MAX`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Rows {
    /// Lines that became servers.
    pub accepted: u16,
    /// Explicit fm/mode/extra keys on successfully validated rows; never their values.
    pub accepted_parameters: [u16; ParameterKind::ALL.len()],
    /// Lines that did not, by the reason.
    pub rejected_by: [u16; Rejection::ALL.len()],
    /// The schemes of the lines that did not.
    pub rejected_schemes: [bool; Scheme::ALL.len()],
    /// Overlapping syntactic presence counts, independent of the primary rejection reason.
    pub rejected_parameters: [u16; ParameterKind::ALL.len()],
    /// One transport per refused row that has at least one known parameter.
    pub rejected_transports: [u16; TransportKind::ALL.len()],
}

impl Rows {
    pub(crate) fn parameter_note(&self) -> Option<String> {
        self.accepted_parameters
            .iter()
            .any(|count| *count > 0)
            .then(|| {
                let counts = ParameterKind::ALL
                    .iter()
                    .map(|kind| {
                        format!(
                            "{}={}",
                            kind.as_str(),
                            self.accepted_parameters[*kind as usize]
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                tr!("core.report.parameters_accepted", counts = counts)
            })
    }
    pub fn accept(&mut self) {
        self.accepted = self.accepted.saturating_add(1);
    }

    pub fn reject(&mut self, why: Rejection, scheme: Scheme) {
        let count = &mut self.rejected_by[why as usize];
        *count = count.saturating_add(1);
        self.rejected_schemes[scheme as usize] = true;
    }

    /// Lines that did not become servers.
    pub fn rejected(&self) -> u16 {
        self.rejected_by
            .iter()
            .fold(0, |sum, count| sum.saturating_add(*count))
    }

    pub fn total(&self) -> u16 {
        self.accepted.saturating_add(self.rejected())
    }
}

/// What the answer of a subscription was, in classes and counts. Every part is known only once
/// the step that learns it has run, so each is optional.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SubscriptionInfo {
    pub body_kind: Option<BodyKind>,
    pub body_size: Option<SizeBucket>,
    pub content_type: Option<ContentClass>,
    /// Redirects followed before the answer (0–5).
    pub redirects: Option<u8>,
    pub rows: Option<Rows>,
    /// Servers the filters of the subscription removed.
    pub filtered: Option<u16>,
}

/// A request that failed before the one that is diagnosticed: the route and the code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Attempt {
    pub via: Via,
    pub failure_code: FailureCode,
    pub cause: Option<Cause>,
}

/// What a selection cycle that found nothing had seen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectionInfo {
    pub kind: CycleKind,
    pub checked: u16,
    pub reachable: u16,
    pub working: u16,
}

/// The most redirects a subscription request follows.
pub const MAX_REDIRECTS: u8 = 5;

/// The steps one operation has completed, oldest first, at most [`MAX_TRAIL`]; later ones are
/// not recorded. It belongs to the operation (a local value, not a shared buffer), so two
/// operations running side by side never share steps, and it ends with the operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Trail {
    marks: [Mark; MAX_TRAIL],
    len: u8,
}

impl Default for Trail {
    fn default() -> Self {
        Self::new()
    }
}

impl Trail {
    pub const fn new() -> Self {
        Self {
            marks: [Mark::RefreshStarted; MAX_TRAIL],
            len: 0,
        }
    }

    pub fn push(&mut self, mark: Mark) {
        if usize::from(self.len) < MAX_TRAIL {
            self.marks[usize::from(self.len)] = mark;
            self.len += 1;
        }
    }

    pub fn marks(&self) -> &[Mark] {
        &self.marks[..usize::from(self.len)]
    }
}

/// Local context attached where a failure occurs. Clean free text before journaling.
#[derive(Clone, Debug, Default)]
pub struct Detail {
    pub stage: Stage,
    /// Status of the answer, for the codes that name one.
    pub http_status: Option<u16>,
    /// The place in Rift's sources where the failure was created.
    pub at: Option<&'static Location<'static>>,
    pub trail: Trail,
    pub trigger: Trigger,
    pub via: Via,
    pub previous_servers_preserved: Option<bool>,
    /// The operation, when the code alone does not say it: a write that fails at the end of a
    /// refresh has the code of every other write.
    pub operation: Option<Operation>,
    /// The best guess at why the request failed.
    pub cause: Option<Cause>,
    /// What the answer of a subscription was.
    pub subscription: Option<SubscriptionInfo>,
    /// The request that failed before this one.
    pub first_attempt: Option<Attempt>,
    /// What a selection cycle that found nothing had seen.
    pub selection: Option<SelectionInfo>,
    /// Parser field or fixed schema path, not the input value.
    pub field_path: Option<String>,
    /// Concrete technical reason retained locally, cleaned before journaling.
    pub local_details: Option<String>,
}

impl Detail {
    /// Detail of a failure created by the caller.
    #[track_caller]
    pub fn here() -> Self {
        Self {
            at: Some(Location::caller()),
            ..Self::default()
        }
    }
}

impl Operation {
    /// The operation a failure code belongs to.
    pub const fn of(code: Option<FailureCode>) -> Self {
        match code {
            Some(
                FailureCode::SubscriptionTimeout
                | FailureCode::SubscriptionTransport
                | FailureCode::SubscriptionHttpStatus
                | FailureCode::SubscriptionInvalidBody
                | FailureCode::SubscriptionFiltered,
            ) => Self::SubscriptionRefresh,
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
                | FailureCode::NoAvailableServer,
            ) => Self::Selection,
            Some(FailureCode::ConnectionLost) => Self::Connection,
            Some(
                FailureCode::SidecarConfigRejected
                | FailureCode::XrayConfigRejected
                | FailureCode::XrayConfigTimeout
                | FailureCode::XrayStartExit
                | FailureCode::XrayReadinessTimeout
                | FailureCode::XrayCrashed
                | FailureCode::UnsupportedLegacyParameters
                | FailureCode::SidecarConfigTimeout
                | FailureCode::SidecarStartExit
                | FailureCode::LocalPortInUse
                | FailureCode::LocalPortDenied
                | FailureCode::SidecarReadinessTimeout
                | FailureCode::SidecarCrashed
                | FailureCode::SidecarStop
                | FailureCode::ClashApi,
            ) => Self::Sidecar,
            Some(
                FailureCode::ElevationDeclined
                | FailureCode::ElevationFailed
                | FailureCode::ElevationChannel
                | FailureCode::ElevationHelperCrashed,
            ) => Self::Elevation,
            Some(FailureCode::GeodataDownload | FailureCode::GeodataInvalid) => Self::Geodata,
            Some(FailureCode::Autostart) => Self::Autostart,
            Some(FailureCode::Persistence | FailureCode::Cancelled | FailureCode::Unknown)
            | None => Self::Other,
        }
    }
}

/// A source location with build-machine directory prefixes removed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Origin {
    pub file: String,
    pub line: u32,
}
impl Origin {
    pub fn new(file: &str, line: u32) -> Option<Self> {
        let normalized = file.replace('\\', "/");
        let start = normalized
            .find("core/src/")
            .or_else(|| normalized.find("desktop/src-tauri/src/"))?;
        let file = &normalized[start..];
        (line > 0
            && file
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."))
        .then(|| Self {
            file: file.to_owned(),
            line,
        })
    }
    #[track_caller]
    pub fn here() -> Option<Self> {
        let at = Location::caller();
        Self::new(at.file(), at.line())
    }
}

/// One failure, described by names only.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub operation: Operation,
    pub stage: Stage,
    pub failure_code: FailureCode,
    pub trigger: Trigger,
    pub via: Via,
    /// Status of the answer, for the codes that name one.
    pub http_status: Option<u16>,
    pub previous_servers_preserved: Option<bool>,
    pub origin: Option<Origin>,
    /// Steps completed before the failure.
    pub trail: Trail,
    pub cause: Option<Cause>,
    pub subscription: Option<SubscriptionInfo>,
    pub first_attempt: Option<Attempt>,
    pub selection: Option<SelectionInfo>,
    pub field_path: Option<String>,
    pub local_details: Option<String>,
}

impl Diagnostic {
    pub fn new(operation: Operation, stage: Stage, failure_code: FailureCode) -> Self {
        Self {
            operation,
            stage,
            failure_code,
            trigger: Trigger::Unknown,
            via: Via::NotApplicable,
            http_status: None,
            previous_servers_preserved: None,
            origin: None,
            trail: Trail::new(),
            cause: None,
            subscription: None,
            first_attempt: None,
            selection: None,
            field_path: None,
            local_details: None,
        }
    }

    /// A diagnostic of an error: its code, and the stage, status, place and steps it carries.
    pub fn from_error(error: &Error) -> Self {
        let mut diagnostic = Self::of(error.failure_code());
        if let Some(detail) = error.detail() {
            diagnostic.stage = detail.stage;
            diagnostic.http_status = detail.http_status;
            diagnostic.origin = detail.at.and_then(|at| Origin::new(at.file(), at.line()));
            diagnostic.trail = detail.trail;
            diagnostic.trigger = detail.trigger;
            diagnostic.via = detail.via;
            diagnostic.previous_servers_preserved = detail.previous_servers_preserved;
            if let Some(operation) = detail.operation {
                diagnostic.operation = operation;
            }
            diagnostic.cause = detail.cause;
            diagnostic.subscription = detail.subscription;
            diagnostic.first_attempt = detail.first_attempt;
            diagnostic.selection = detail.selection;
            diagnostic.field_path.clone_from(&detail.field_path);
            diagnostic.local_details.clone_from(&detail.local_details);
        }
        diagnostic
    }

    /// A diagnostic of the failure a code names, in the operation the code belongs to; with no code,
    /// the general one.
    pub fn of(code: Option<FailureCode>) -> Self {
        Self::new(
            Operation::of(code),
            Stage::Unspecified,
            code.unwrap_or(FailureCode::Unknown),
        )
    }

    /// The title: `operation/stage: failure_code`.
    pub fn message(&self) -> String {
        format!(
            "{}/{}: {}",
            self.operation.as_str(),
            self.stage.as_str(),
            self.failure_code.as_str()
        )
    }

    /// What the diagnostic says beyond the tags: the fields of the `rift` context.
    fn context(&self) -> BTreeMap<String, Value> {
        let mut rift = BTreeMap::new();
        if let Some(status) = self.http_status.filter(|s| (100..=599).contains(s)) {
            rift.insert("http_status".into(), status.into());
        }
        if let Some(kept) = self.previous_servers_preserved {
            rift.insert("previous_servers_preserved".into(), kept.into());
        }
        if let Some(origin) = &self.origin {
            rift.insert("origin".into(), origin_json(origin));
        }
        if !self.trail.marks().is_empty() {
            let marks = self.trail.marks().iter().map(|mark| mark.as_str());
            rift.insert("trail".into(), marks.collect::<Vec<_>>().into());
        }
        if let Some(cause) = self.cause {
            rift.insert("cause".into(), cause.as_str().into());
        }
        if let Some(attempt) = self.first_attempt {
            rift.insert("first_attempt".into(), attempt_json(attempt));
        }
        if let Some(info) = &self.subscription {
            rift.insert("subscription".into(), subscription_json(info));
        }
        if let Some(info) = self.selection {
            rift.insert("selection".into(), selection_json(info));
        }
        if let Some(path) = &self.field_path {
            rift.insert("field_path".into(), path.clone().into());
        }
        if let Some(details) = &self.local_details {
            rift.insert("local_details".into(), details.clone().into());
        }
        rift
    }

    /// Local metadata for explicit diagnostics export. Free-text fields still need
    /// cleaning at the export boundary.
    pub fn to_json(&self) -> Value {
        let mut out = JsonMap::new();
        out.insert("operation".into(), self.operation.as_str().into());
        out.insert("stage".into(), self.stage.as_str().into());
        out.insert("failure_code".into(), self.failure_code.as_str().into());
        out.insert("trigger".into(), self.trigger.as_str().into());
        out.insert("transport".into(), self.via.as_str().into());
        out.extend(self.context());
        Value::Object(out)
    }

    /// Technical local details; callers must clean these before persistence or display.
    pub fn details(&self, error: &Error) -> String {
        use std::fmt::Write;
        fn write_fields(out: &mut String, prefix: &str, value: &Value) {
            use std::fmt::Write;
            match value {
                Value::Object(fields) => {
                    for (key, value) in fields {
                        let key = if prefix.is_empty() {
                            key.clone()
                        } else {
                            format!("{prefix}.{key}")
                        };
                        write_fields(out, &key, value);
                    }
                }
                Value::Array(values) => {
                    for (index, value) in values.iter().enumerate() {
                        write_fields(out, &format!("{prefix}[{index}]"), value);
                    }
                }
                Value::String(value) => {
                    let _ = writeln!(out, "{prefix}: {value}");
                }
                value => {
                    let _ = writeln!(out, "{prefix}: {value}");
                }
            }
        }
        let mut out = String::new();
        write_fields(&mut out, "", &self.to_json());
        let _ = write!(out, "error: {error}");
        let mut source = std::error::Error::source(error);
        // Bound even an accidentally cyclic custom source chain.
        for index in 0..16 {
            let Some(cause) = source else {
                break;
            };
            if let Some(json) = cause.downcast_ref::<serde_json::Error>() {
                // Serde data errors may quote a provider-controlled scalar. Preserve
                // the observed category and position, never that scalar or raw body.
                let reason = match json.classify() {
                    serde_json::error::Category::Io => crate::text::t("core.diagnostics.json_io"),
                    serde_json::error::Category::Syntax => {
                        crate::text::t("core.diagnostics.json_syntax")
                    }
                    serde_json::error::Category::Data => {
                        crate::text::t("core.diagnostics.json_data")
                    }
                    serde_json::error::Category::Eof => crate::text::t("core.diagnostics.json_eof"),
                };
                let cause = tr!(
                    "core.diagnostics.json_position",
                    reason = reason,
                    category = format!("{:?}", json.classify()),
                    line = json.line(),
                    column = json.column()
                );
                let _ = write!(
                    out,
                    "\n{}",
                    tr!(
                        "core.diagnostics.numbered_cause",
                        index = index + 1,
                        cause = cause
                    )
                );
            } else {
                let _ = write!(
                    out,
                    "\n{}",
                    tr!(
                        "core.diagnostics.numbered_cause",
                        index = index + 1,
                        cause = cause.to_string()
                    )
                );
            }
            source = cause.source();
        }
        out
    }
}

fn origin_json(origin: &Origin) -> Value {
    serde_json::json!({ "file": origin.file, "line": origin.line })
}

fn attempt_json(attempt: Attempt) -> Value {
    let mut out = serde_json::json!({
        "via": attempt.via.as_str(),
        "failure_code": attempt.failure_code.as_str(),
    });
    if let Some(cause) = attempt.cause {
        out["cause"] = cause.as_str().into();
    }
    out
}

fn selection_json(info: SelectionInfo) -> Value {
    serde_json::json!({
        "kind": info.kind.as_str(),
        "checked": info.checked,
        "reachable": info.reachable,
        "working": info.working,
    })
}

/// The subscription part of a diagnostic: classes by name, counts as numbers, and the reasons and
/// schemes of rejected lines only where there are any.
pub(crate) fn subscription_json(info: &SubscriptionInfo) -> Value {
    let mut out = JsonMap::new();
    if let Some(kind) = info.body_kind {
        out.insert("body_kind".into(), kind.as_str().into());
    }
    if let Some(size) = info.body_size {
        out.insert("body_size".into(), size.as_str().into());
    }
    if let Some(class) = info.content_type {
        out.insert("content_type".into(), class.as_str().into());
    }
    if let Some(redirects) = info.redirects {
        out.insert("redirects".into(), redirects.min(MAX_REDIRECTS).into());
    }
    if let Some(filtered) = info.filtered {
        out.insert("filtered".into(), filtered.into());
    }
    if let Some(rows) = &info.rows {
        out.insert(
            "rows".into(),
            serde_json::json!({
                "total": rows.total(),
                "accepted": rows.accepted,
                "rejected": rows.rejected(),
            }),
        );
        let by: JsonMap<String, Value> = Rejection::ALL
            .iter()
            .filter(|why| rows.rejected_by[**why as usize] > 0)
            .map(|why| {
                (
                    why.as_str().to_owned(),
                    rows.rejected_by[*why as usize].into(),
                )
            })
            .collect();
        if !by.is_empty() {
            out.insert("rejected_by".into(), Value::Object(by));
        }
        let schemes: Vec<Value> = Scheme::ALL
            .iter()
            .filter(|scheme| rows.rejected_schemes[**scheme as usize])
            .map(|scheme| scheme.as_str().into())
            .collect();
        if !schemes.is_empty() {
            out.insert("rejected_schemes".into(), Value::Array(schemes));
        }
        for (key, counts) in [
            (
                "accepted_parameters",
                ParameterKind::ALL
                    .iter()
                    .map(|kind| (kind.as_str(), rows.accepted_parameters[*kind as usize]))
                    .collect::<Vec<_>>(),
            ),
            (
                "rejected_parameters",
                ParameterKind::ALL
                    .iter()
                    .map(|kind| (kind.as_str(), rows.rejected_parameters[*kind as usize]))
                    .collect::<Vec<_>>(),
            ),
            (
                "rejected_transports",
                TransportKind::ALL
                    .iter()
                    .map(|kind| (kind.as_str(), rows.rejected_transports[*kind as usize]))
                    .collect(),
            ),
        ] {
            let counts: JsonMap<String, Value> = counts
                .into_iter()
                .filter(|(_, count)| *count > 0)
                .map(|(kind, count)| (kind.to_owned(), count.into()))
                .collect();
            if !counts.is_empty() {
                out.insert(key.into(), Value::Object(counts));
            }
        }
        if rows.accepted_parameters.iter().any(|count| *count > 0) {
            // The strict parser never silently ignores these parameters. Keep zero explicit.
            out.insert(
                "ignored_parameters".into(),
                serde_json::json!({"fm":0,"mode":0,"extra":0}),
            );
        }
    }
    Value::Object(out)
}
