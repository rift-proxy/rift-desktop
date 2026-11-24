//! Checked connection model; storage v1 is an input adapter, never raw Xray config.
//! No Debug: serialization is for persistence/helper only, never diagnostics.
use crate::text::t;
use crate::{failure::FailureCode, outbound, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    host: String,
    port: u16,
    protocol: Protocol,
    transport: Transport,
    security: Security,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finalmask: Option<crate::parameters::FinalMask>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Protocol {
    Vless {
        id: String,
        vision: bool,
    },
    Vmess {
        id: String,
        alter_id: u32,
        security: String,
    },
    Trojan {
        password: String,
    },
    Shadowsocks {
        method: String,
        password: String,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Transport {
    Tcp {},
    WebSocket {
        path: String,
        host: Option<String>,
    },
    Grpc {
        service: String,
    },
    Xhttp {
        settings: Box<crate::parameters::Xhttp>,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Security {
    Plain {},
    Tls {
        name: String,
        fingerprint: Option<String>,
        alpn: Option<Vec<String>>,
    },
    Reality {
        name: String,
        fingerprint: String,
        key: String,
        short_id: String,
        alpn: Option<Vec<String>>,
    },
}

pub const XRAY_SS_METHODS: [&str; 7] = [
    "2022-blake3-aes-128-gcm",
    "2022-blake3-aes-256-gcm",
    "2022-blake3-chacha20-poly1305",
    "aes-128-gcm",
    "aes-256-gcm",
    "chacha20-ietf-poly1305",
    "xchacha20-ietf-poly1305",
];
pub const XRAY_VMESS_SECURITIES: [&str; 3] = ["auto", "aes-128-gcm", "chacha20-poly1305"];

fn text(value: &Value, key: &str) -> String {
    // Required fields and nested types were checked by outbound::inspect first.
    value[key].as_str().unwrap_or_default().to_owned()
}
fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
fn retired() -> Error {
    Error::failure(
        FailureCode::UnsupportedLegacyParameters,
        t("core.connection.old_server_params"),
    )
}

impl Connection {
    /// Reads old bounded allowlisted data without changing or executing it.
    pub fn from_legacy(value: &Value) -> Result<Self> {
        let endpoint = outbound::inspect(value)?;
        let protocol = match value["type"].as_str() {
            Some("vless") => Protocol::Vless {
                id: text(value, "uuid"),
                vision: value.get("flow").is_some(),
            },
            Some("vmess") => Protocol::Vmess {
                id: text(value, "uuid"),
                alter_id: value["alter_id"].as_u64().unwrap_or_default() as u32,
                security: text(value, "security"),
            },
            Some("trojan") => Protocol::Trojan {
                password: text(value, "password"),
            },
            Some("shadowsocks") => Protocol::Shadowsocks {
                method: text(value, "method"),
                password: text(value, "password"),
            },
            _ => {
                return Err(Error::Invalid(
                    t("core.common.unsupported_server_type").into(),
                ))
            }
        };
        let transport = match value["transport"]["type"].as_str() {
            Some("ws") => Transport::WebSocket {
                path: text(&value["transport"], "path"),
                host: value["transport"]["headers"]["Host"]
                    .as_str()
                    .map(str::to_owned),
            },
            Some("grpc") => Transport::Grpc {
                service: text(&value["transport"], "service_name"),
            },
            Some("xhttp") => Transport::Xhttp {
                settings: Box::new(crate::parameters::Xhttp::legacy(
                    value["transport"].clone(),
                )?),
            },
            _ => Transport::Tcp {},
        };
        let security = match value.get("tls") {
            None => Security::Plain {},
            Some(tls) if tls.get("reality").is_some() => Security::Reality {
                name: text(tls, "server_name"),
                fingerprint: text(&tls["utls"], "fingerprint"),
                key: text(&tls["reality"], "public_key"),
                short_id: text(&tls["reality"], "short_id"),
                alpn: tls.get("alpn").map(strings),
            },
            Some(tls) => Security::Tls {
                name: text(tls, "server_name"),
                fingerprint: tls["utls"]["fingerprint"].as_str().map(str::to_owned),
                alpn: tls.get("alpn").map(strings),
            },
        };
        Ok(Self {
            host: endpoint.host,
            port: endpoint.port,
            protocol,
            transport,
            security,
            finalmask: value
                .get("finalmask")
                .map(|v| crate::parameters::FinalMask::read(v.clone()))
                .transpose()?,
        })
    }
    pub fn host(&self) -> &str {
        &self.host
    }
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// Canonical compatibility projection. Keeps old fingerprints and all optional-field
    /// distinctions; this shape is not sent to a remote core by the production launch plan.
    pub fn legacy_value(&self) -> Value {
        let mut value = match &self.protocol {
            Protocol::Vless { id, vision } => {
                let mut value = json!({"type":"vless","uuid":id});
                if *vision {
                    value["flow"] = json!("xtls-rprx-vision");
                }
                value
            }
            Protocol::Vmess {
                id,
                alter_id,
                security,
            } => json!({"type":"vmess","uuid":id,"alter_id":alter_id,"security":security}),
            Protocol::Trojan { password } => json!({"type":"trojan","password":password}),
            Protocol::Shadowsocks { method, password } => {
                json!({"type":"shadowsocks","method":method,"password":password})
            }
        };
        value["server"] = json!(self.host);
        value["server_port"] = json!(self.port);
        match &self.transport {
            Transport::Tcp {} => {}
            Transport::WebSocket { path, host } => {
                value["transport"] = json!({"type":"ws","path":path});
                if let Some(host) = host {
                    value["transport"]["headers"] = json!({"Host":host});
                }
            }
            Transport::Grpc { service } => {
                value["transport"] = json!({"type":"grpc","service_name":service})
            }
            Transport::Xhttp { settings } => {
                value["transport"] = serde_json::to_value(settings).expect("bounded types");
                value["transport"]["type"] = json!("xhttp");
            }
        }
        match &self.security {
            Security::Plain {} => {}
            Security::Tls {
                name,
                fingerprint,
                alpn,
            } => {
                value["tls"] = json!({"enabled":true,"server_name":name});
                if let Some(fp) = fingerprint {
                    value["tls"]["utls"] = json!({"enabled":true,"fingerprint":fp});
                }
                if let Some(alpn) = alpn {
                    value["tls"]["alpn"] = json!(alpn);
                }
            }
            Security::Reality {
                name,
                fingerprint,
                key,
                short_id,
                alpn,
            } => {
                value["tls"] = json!({"enabled":true,"server_name":name,
                    "utls":{"enabled":true,"fingerprint":fingerprint},
                    "reality":{"enabled":true,"public_key":key,"short_id":short_id}});
                if let Some(alpn) = alpn {
                    value["tls"]["alpn"] = json!(alpn);
                }
            }
        }
        if let Some(mask) = &self.finalmask {
            value["finalmask"] = serde_json::to_value(mask).expect("bounded types");
        }
        value
    }

    pub(crate) fn from_neutral(value: Value) -> Result<Self> {
        let node: Self = serde_json::from_value(value)?;
        outbound::inspect(&node.legacy_value())?;
        Ok(node)
    }

    /// A storage value may remain readable while it can no longer be connected.
    pub fn check_xray(&self) -> Result<()> {
        if matches!(&self.protocol, Protocol::Vless { vision: true, .. })
            && (!matches!(&self.transport, Transport::Tcp {})
                || matches!(&self.security, Security::Plain {}))
        {
            return Err(retired());
        }
        if matches!(&self.security,Security::Reality{alpn,..} if alpn.as_ref().is_some_and(|values| !values.is_empty()))
        {
            return Err(retired());
        }
        if matches!(
            (&self.security, &self.transport),
            (Security::Reality { .. }, Transport::WebSocket { .. })
        ) {
            return Err(retired());
        }
        match &self.protocol {
            Protocol::Shadowsocks { method, .. } if !XRAY_SS_METHODS.contains(&method.as_str()) => {
                Err(retired())
            }
            Protocol::Vmess { security, .. }
                if !XRAY_VMESS_SECURITIES.contains(&security.as_str()) =>
            {
                Err(retired())
            }
            Protocol::Vmess { alter_id, .. } if *alter_id != 0 => Err(retired()),
            _ => Ok(()),
        }
    }

    /// Generates one remote outbound; never an inbound, program, file or API from input.
    pub fn xray_outbound(&self, tag: &str) -> Result<Value> {
        outbound::inspect(&self.legacy_value())?;
        self.check_xray()?;
        if tag.is_empty()
            || tag.len() > 128
            || !tag
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        {
            return Err(Error::Invalid(t("core.connection.bad_local_id").into()));
        }
        let (protocol, settings) = match &self.protocol {
            Protocol::Vless { id, vision } => {
                let mut user = json!({"id":id,"encryption":"none"});
                if *vision {
                    // Xray strips this local flag before writing Vision flow on the wire.
                    // Keep the old sing-box XUDP behaviour for UDP destination port 443.
                    user["flow"] = json!("xtls-rprx-vision-udp443");
                }
                (
                    "vless",
                    json!({"vnext":[{"address":self.host,"port":self.port,"users":[user]}]}),
                )
            }
            Protocol::Vmess { id, security, .. } => (
                "vmess",
                json!({"vnext":[{
                "address":self.host,"port":self.port,"users":[{"id":id,"security":security}]}]}),
            ),
            Protocol::Trojan { password } => (
                "trojan",
                json!({"servers":[{
                "address":self.host,"port":self.port,"password":password}]}),
            ),
            Protocol::Shadowsocks { method, password } => (
                "shadowsocks",
                json!({"servers":[{
                "address":self.host,"port":self.port,"method":method,"password":password}]}),
            ),
        };
        let mut stream = match &self.transport {
            Transport::Tcp {} => json!({"network":"tcp"}),
            Transport::WebSocket { path, host } => {
                let mut ws = json!({"path":path});
                if let Some(host) = host {
                    ws["headers"] = json!({"Host":host});
                }
                json!({"network":"ws","wsSettings":ws})
            }
            Transport::Grpc { service } => {
                json!({"network":"grpc","grpcSettings":{"serviceName":service,"multiMode":false}})
            }
            Transport::Xhttp { settings } => json!({"network":"xhttp","xhttpSettings":settings}),
        };
        match &self.security {
            Security::Plain {} => {
                stream["security"] = json!("none");
            }
            Security::Tls {
                name,
                fingerprint,
                alpn,
            } => {
                let mut tls = json!({"serverName":name});
                if let Some(fp) = fingerprint {
                    tls["fingerprint"] = json!(fp);
                }
                if let Some(alpn) = alpn {
                    tls["alpn"] = json!(alpn);
                }
                stream["security"] = json!("tls");
                stream["tlsSettings"] = tls;
            }
            Security::Reality {
                name,
                fingerprint,
                key,
                short_id,
                alpn: _,
            } => {
                let reality = json!({"serverName":name,"fingerprint":fingerprint,"publicKey":key,"shortId":short_id});
                stream["security"] = json!("reality");
                stream["realitySettings"] = reality;
            }
        }
        if let Some(mask) = &self.finalmask {
            stream["finalmask"] = json!(mask);
        }
        Ok(json!({"tag":tag,"protocol":protocol,"settings":settings,"streamSettings":stream}))
    }
}
