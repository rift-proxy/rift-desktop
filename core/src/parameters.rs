//! Closed, bounded protocol parameters. Never includes file/process/API/socket authority.
use crate::text::t;
use crate::{diagnostics::Rejection, Error, Result};
use serde::{
    de::{DeserializeSeed, MapAccess, SeqAccess, Visitor},
    Deserialize, Serialize,
};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum ParameterProblem {
    #[error("{}", t("core.parameters.parameter_unsupported"))]
    Unsupported,
    #[error("{}", t("core.parameters.transport_mode_unsupported"))]
    Mode,
    #[error("{}", t("core.parameters.parameter_not_applicable"))]
    Incompatible,
    #[error("{}", t("core.parameters.bad_final_mask"))]
    FinalMask,
    #[error("{}", t("core.parameters.bad_xhttp_extra"))]
    Extra,
}
impl ParameterProblem {
    pub fn rejection(self) -> Rejection {
        match self {
            Self::Unsupported => Rejection::UnsupportedParam,
            Self::Mode => Rejection::UnsupportedMode,
            Self::Incompatible => Rejection::IncompatibleParam,
            Self::FinalMask => Rejection::InvalidFinalmask,
            Self::Extra => Rejection::InvalidExtra,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Range {
    Number(u32),
    Text(String),
}
impl Range {
    fn bounds(&self) -> Option<(u32, u32)> {
        match self {
            Self::Number(n) => Some((*n, *n)),
            Self::Text(s) if !s.is_empty() && s.len() <= 32 => {
                let mut parts = s.split('-');
                let parse = |s: &str| {
                    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                        .then(|| s.parse::<u32>().ok())
                        .flatten()
                };
                let a = parse(parts.next()?)?;
                let b = match parts.next() {
                    Some(s) => parse(s)?,
                    None => a,
                };
                if parts.next().is_some() || a > b {
                    None
                } else {
                    Some((a, b))
                }
            }
            _ => None,
        }
    }
    fn valid(&self, min: u32, max: u32) -> bool {
        self.bounds().is_some_and(|(a, b)| a >= min && b <= max)
    }
    fn positive(&self) -> bool {
        self.bounds().is_some_and(|(_, b)| b > 0)
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Xmux {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_concurrency: Option<Range>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_connections: Option<Range>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    c_max_reuse_times: Option<Range>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    h_max_request_times: Option<Range>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    h_max_reusable_secs: Option<Range>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    h_keep_alive_period: Option<u32>,
}
impl Xmux {
    fn valid(&self) -> bool {
        [
            &self.max_concurrency,
            &self.max_connections,
            &self.c_max_reuse_times,
            &self.h_max_request_times,
        ]
        .iter()
        .all(|r| r.as_ref().is_none_or(|r| r.valid(0, 65536)))
            && self
                .h_max_reusable_secs
                .as_ref()
                .is_none_or(|r| r.valid(0, 86400))
            && self.h_keep_alive_period.is_none_or(|v| v <= 86400)
            && !(self.max_connections.as_ref().is_some_and(Range::positive)
                && self.max_concurrency.as_ref().is_some_and(Range::positive))
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct XhttpExtra {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    headers: Option<EmptyHeaders>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    x_padding_bytes: Option<Range>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "noGRPCHeader"
    )]
    no_grpc_header: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "noSSEHeader"
    )]
    no_sse_header: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sc_max_each_post_bytes: Option<Range>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sc_min_posts_interval_ms: Option<Range>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sc_max_buffered_posts: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sc_stream_up_server_secs: Option<Range>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    xmux: Option<Xmux>,
}
// An explicitly empty map is harmless. Nonempty maps remain outside the supported contract.
#[derive(Clone, Serialize)]
struct EmptyHeaders {}
impl<'de> Deserialize<'de> for EmptyHeaders {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let headers = Map::<String, Value>::deserialize(deserializer)?;
        if !headers.is_empty() {
            return Err(serde::de::Error::custom("only empty headers are supported"));
        }
        Ok(Self {})
    }
}

fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}
impl XhttpExtra {
    fn valid(&self) -> bool {
        self.x_padding_bytes
            .as_ref()
            .is_none_or(|v| v.valid(1, 65536))
            && self
                .sc_max_each_post_bytes
                .as_ref()
                .is_none_or(|v| v.valid(1, 16 * 1024 * 1024))
            && self
                .sc_min_posts_interval_ms
                .as_ref()
                .is_none_or(|v| v.valid(0, 60000))
            && self
                .sc_max_buffered_posts
                .is_none_or(|v| (1..=1024).contains(&v))
            && self
                .sc_stream_up_server_secs
                .as_ref()
                .is_none_or(|v| v.valid(0, 86400))
            && self.xmux.as_ref().is_none_or(Xmux::valid)
    }
    pub fn read(value: Value) -> Result<Self> {
        bounded_value(&value, ParameterProblem::Extra)?;
        let s: Self = serde_json::from_value(value).map_err(|_| ParameterProblem::Extra)?;
        if !s.valid() {
            return Err(ParameterProblem::Extra.into());
        }
        Ok(s)
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub enum XhttpMode {
    #[default]
    #[serde(rename = "auto", alias = "")]
    Auto,
    #[serde(rename = "packet-up")]
    PacketUp,
    #[serde(rename = "stream-up")]
    StreamUp,
    #[serde(rename = "stream-one")]
    StreamOne,
}
fn default_path() -> String {
    "/".into()
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Xhttp {
    #[serde(default = "default_path")]
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default)]
    pub mode: XhttpMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<XhttpExtra>,
}
impl Xhttp {
    pub fn legacy(mut value: Value) -> Result<Self> {
        let o = value
            .as_object_mut()
            .ok_or(ParameterProblem::Incompatible)?;
        if o.remove("type") != Some(Value::String("xhttp".into())) {
            return Err(ParameterProblem::Incompatible.into());
        }
        Self::read(value)
    }
    pub fn read(mut value: Value) -> Result<Self> {
        let o = value
            .as_object_mut()
            .ok_or(ParameterProblem::Incompatible)?;
        if let Some(extra) = o.get("extra") {
            XhttpExtra::read(extra.clone())?;
        }
        if let Some(mode) = o.get("mode") {
            let _: XhttpMode =
                serde_json::from_value(mode.clone()).map_err(|_| ParameterProblem::Mode)?;
        }
        let mut s: Self = serde_json::from_value(value).map_err(|_| ParameterProblem::Extra)?;
        if s.path.is_empty() {
            s.path = default_path();
        }
        if s.path.len() > 16384
            || s.path.chars().any(char::is_control)
            || s.host.as_ref().is_some_and(|s| {
                s.len() > 253 || s.chars().any(|c| c.is_control() || c.is_whitespace())
            })
            || s.extra.as_ref().is_some_and(|s| !s.valid())
        {
            return Err(ParameterProblem::Extra.into());
        }
        if s.host.as_deref() == Some("") {
            s.host = None;
        }
        Ok(s)
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FragmentSettings {
    pub packets: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub length: Option<Range>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub delay: Option<Range>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub lengths: Option<Vec<Range>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub delays: Option<Vec<Range>>,
    #[serde(
        default,
        rename = "maxSplit",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub max_split: Option<Range>,
}
impl FragmentSettings {
    fn valid(&self) -> bool {
        let list_valid = |values: &Option<Vec<Range>>, min, max| {
            values.as_ref().is_none_or(|v| {
                !v.is_empty() && v.len() <= 32 && v.iter().all(|range| range.valid(min, max))
            })
        };
        self.packets == "tlshello"
            && (self.length.is_some() || self.lengths.is_some())
            && self.length.as_ref().is_none_or(|v| v.valid(1, 16384))
            && self.delay.as_ref().is_none_or(|v| v.valid(0, 1000))
            && list_valid(&self.lengths, 1, 16384)
            && list_valid(&self.delays, 0, 1000)
            && self.max_split.as_ref().is_none_or(|v| v.valid(0, 65536))
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Mask {
    #[serde(rename = "fragment")]
    Fragment { settings: FragmentSettings },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalMask {
    tcp: Vec<Mask>,
}
impl FinalMask {
    pub fn read(value: Value) -> Result<Self> {
        bounded_value(&value, ParameterProblem::FinalMask)?;
        let s: Self = serde_json::from_value(value).map_err(|_| ParameterProblem::FinalMask)?;
        if s.tcp.len() != 1
            || !s.tcp.iter().all(|m| match m {
                Mask::Fragment { settings: s } => s.valid(),
            })
        {
            return Err(ParameterProblem::FinalMask.into());
        }
        Ok(s)
    }
}
fn bounded_value(value: &Value, problem: ParameterProblem) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::from(problem))?;
    if bytes.len() > 8192 || !value.is_object() {
        return Err(problem.into());
    }
    Ok(())
}

// Per-field JSON parser: duplicate keys must not become last-value-wins settings. Depth and
// collection limits are checked while reading, before creating an unbounded nested tree.
#[derive(Clone, Copy)]
struct Bounded {
    depth: usize,
    max_depth: usize,
    max_items: usize,
}
impl Bounded {
    fn child(self) -> Self {
        Self {
            depth: self.depth + 1,
            ..self
        }
    }
}
impl<'de> DeserializeSeed<'de> for Bounded {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        de: D,
    ) -> std::result::Result<Value, D::Error> {
        if self.depth > self.max_depth {
            return Err(serde::de::Error::custom("depth"));
        }
        de.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Bounded {
    type Value = Value;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded JSON")
    }
    fn visit_bool<E: serde::de::Error>(self, v: bool) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Value, E> {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("number"))
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(v) = seq.next_element_seed(self.child())? {
            if values.len() >= self.max_items {
                return Err(serde::de::Error::custom("array"));
            }
            values.push(v);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.len() >= self.max_items || values.contains_key(&key) {
                return Err(serde::de::Error::custom("object"));
            }
            let value = map.next_value_seed(self.child())?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}
fn limited_json(raw: &str, max_bytes: usize, max_depth: usize, max_items: usize) -> Option<Value> {
    if raw.is_empty() || raw.len() > max_bytes {
        return None;
    }
    let mut de = serde_json::Deserializer::from_str(raw);
    let value = Bounded {
        depth: 0,
        max_depth,
        max_items,
    }
    .deserialize(&mut de)
    .ok()?;
    de.end().ok()?;
    Some(value)
}
pub fn parse_json(raw: &str, problem: ParameterProblem) -> Result<Value> {
    limited_json(raw, 8192, 8, 32).ok_or_else(|| problem.into())
}
pub(crate) fn parse_xray_json(raw: &str) -> Result<Value> {
    // The subscription body retains serde_json's ordinary duplicate/depth policy. Only
    // parameter fragments have the stricter wire contract, and only their server is refused.
    if raw.len() > 4 * 1024 * 1024 {
        return Err(Error::Invalid(t("core.parameters.not_xray_json").into()));
    }
    let mut value: Value = serde_json::from_str(raw).map_err(|error| {
        Error::failure(
            crate::failure::FailureCode::SubscriptionInvalidBody,
            t("core.parameters.not_xray_json"),
        )
        .at_stage(crate::diagnostics::Stage::Parse)
        .with_field_path("$")
        .with_local_details(crate::tr!(
            "core.diagnostics.json_position",
            reason = t("core.diagnostics.json_invalid"),
            category = format!("{:?}", error.classify()),
            line = error.line(),
            column = error.column()
        ))
    })?;
    strict_fragments(&mut value, raw, JsonSection::Root);
    Ok(value)
}

#[derive(Clone, Copy)]
enum JsonSection {
    Root,
    Outbound,
    Stream,
    Xhttp,
}

type SectionChildren = &'static [(&'static str, JsonSection)];
type SectionFragments = &'static [(&'static str, ParameterProblem)];

/// Inspect original spans before duplicate keys disappear in Value. Invalid fragments become
/// null, which the existing per-server typed validators reject; unrelated JSON stays untouched.
fn strict_fragments(value: &mut Value, raw: &str, section: JsonSection) {
    use serde_json::value::RawValue;
    if let Value::Array(values) = value {
        if let Ok(items) = serde_json::from_str::<Vec<&RawValue>>(raw) {
            for (value, raw) in values.iter_mut().zip(items) {
                strict_fragments(value, raw.get(), section);
            }
        }
        return;
    }
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let (children, fragments): (SectionChildren, SectionFragments) = match section {
        JsonSection::Root if object.contains_key("outbounds") => {
            (&[("outbounds", JsonSection::Outbound)], &[])
        }
        JsonSection::Root => (
            &[],
            &[
                ("fm", ParameterProblem::FinalMask),
                ("extra", ParameterProblem::Extra),
            ],
        ),
        JsonSection::Outbound => (&[("streamSettings", JsonSection::Stream)], &[]),
        JsonSection::Stream => (
            &[("xhttpSettings", JsonSection::Xhttp)],
            &[("finalmask", ParameterProblem::FinalMask)],
        ),
        JsonSection::Xhttp => (&[], &[("extra", ParameterProblem::Extra)]),
    };
    // Most VMess payloads have no strict fragments. Inspect the parsed keys before allocating
    // and parsing the original spans; when a relevant key exists its strict policy is unchanged.
    if !children.iter().any(|(key, _)| object.contains_key(*key))
        && !fragments.iter().any(|(key, _)| object.contains_key(*key))
    {
        return;
    }
    let Ok(fields) = serde_json::from_str::<std::collections::HashMap<String, &RawValue>>(raw)
    else {
        return;
    };
    for &(key, child) in children {
        if let (Some(value), Some(raw)) = (object.get_mut(key), fields.get(key)) {
            strict_fragments(value, raw.get(), child);
        }
    }
    for &(key, problem) in fragments {
        if let (Some(value), Some(raw)) = (object.get_mut(key), fields.get(key)) {
            // Only VMess's root-level fm/extra permit JSON encoded in a string.
            // Xray stream/xhttp fields must retain their original object-only shape.
            let text = if matches!(section, JsonSection::Root) {
                value.as_str().unwrap_or_else(|| raw.get())
            } else {
                raw.get()
            };
            *value = parse_json(text, problem).unwrap_or(Value::Null);
        }
    }
}
