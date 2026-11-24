//! Supplemental syntax observations. Only fixed enums and bits escape this module.
use crate::diagnostics::{ParameterKind, Rows, TransportKind};
use serde_json::Value;

#[derive(Clone, Copy, Default)]
pub(crate) struct Observation {
    parameters: [bool; ParameterKind::ALL.len()],
    transport: Option<TransportKind>,
}

impl Observation {
    pub(crate) fn uri(line: &str) -> Self {
        // Match the existing parser budget. Do not allocate based on oversized rejected input.
        if line.len() > 16_384 {
            return Self::default();
        }
        if line
            .get(..8)
            .is_some_and(|s| s.eq_ignore_ascii_case("vmess://"))
        {
            return crate::parser::decode64(&line[8..])
                .ok()
                .and_then(|text| crate::parameters::parse_xray_json(&text).ok())
                .map(|value| Self::vmess(&value))
                .unwrap_or_default();
        }
        let Ok(uri) = url::Url::parse(line) else {
            return Self::default();
        };
        let mut observation = Self::default();
        for (key, value) in uri.query_pairs() {
            match key.as_ref() {
                "fm" => observation.mark(ParameterKind::Fm),
                "mode" => observation.mark(ParameterKind::Mode),
                "extra" => observation.mark(ParameterKind::Extra),
                // Duplicate type keys are invalid; use the first spelling for observation only.
                "type" if observation.transport.is_none() => {
                    observation.transport = Some(transport(&value))
                }
                _ => {}
            }
        }
        observation.transport.get_or_insert(TransportKind::Tcp);
        observation
    }

    fn vmess(value: &Value) -> Self {
        let mut observation = Self::default();
        for (key, kind) in [
            ("fm", ParameterKind::Fm),
            ("mode", ParameterKind::Mode),
            ("extra", ParameterKind::Extra),
        ] {
            if value.get(key).is_some() {
                observation.mark(kind);
            }
        }
        observation.transport = Some(field_transport(value.get("net")));
        observation
    }

    pub(crate) fn xray(raw: &Value) -> Self {
        let mut observation = Self::default();
        let Some(stream) = raw.get("streamSettings") else {
            return observation;
        };
        if stream.get("finalmask").is_some() {
            observation.mark(ParameterKind::Fm);
        }
        // Look at known blocks independently of the network/primary validation failure.
        for block in ["xhttpSettings", "splithttpSettings", "grpcSettings"] {
            if let Some(settings) = stream.get(block) {
                if block == "grpcSettings" && settings.get("multiMode").is_some() {
                    observation.mark(ParameterKind::Mode);
                }
                for (key, kind) in [
                    ("mode", ParameterKind::Mode),
                    ("extra", ParameterKind::Extra),
                ] {
                    if settings.get(key).is_some() {
                        observation.mark(kind);
                    }
                }
            }
        }
        observation.transport = Some(field_transport(stream.get("network")));
        observation
    }

    fn mark(&mut self, kind: ParameterKind) {
        self.parameters[kind as usize] = true;
    }

    pub(crate) fn count_into(self, rows: &mut Rows) {
        if !self.parameters.iter().any(|present| *present) {
            return;
        }
        for (index, present) in self.parameters.into_iter().enumerate() {
            if present {
                rows.rejected_parameters[index] = rows.rejected_parameters[index].saturating_add(1);
            }
        }
        if let Some(kind) = self.transport {
            rows.rejected_transports[kind as usize] =
                rows.rejected_transports[kind as usize].saturating_add(1);
        }
    }
    /// Count validated explicit keys. Validation must have succeeded before calling this.
    pub(crate) fn count_accepted_into(self, rows: &mut Rows) {
        for (index, present) in self.parameters.into_iter().enumerate() {
            if present {
                rows.accepted_parameters[index] = rows.accepted_parameters[index].saturating_add(1);
            }
        }
    }
}

fn transport(value: &str) -> TransportKind {
    match value {
        "" | "tcp" | "raw" => TransportKind::Tcp,
        "ws" => TransportKind::Ws,
        "grpc" => TransportKind::Grpc,
        "xhttp" | "splithttp" => TransportKind::Xhttp,
        _ => TransportKind::Other,
    }
}

fn field_transport(value: Option<&Value>) -> TransportKind {
    match value {
        None => TransportKind::Tcp,
        Some(Value::String(value)) => transport(value),
        Some(_) => TransportKind::Other,
    }
}
