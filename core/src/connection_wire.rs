//! Persistence/helper wire. Legacy objects retain their values without gaining trust;
//! a neutral node must pass both bounded model and row identity/metadata checks.
use crate::text::t;
use crate::{connection::Connection, model::Server, outbound, routing::ServerRoutes};
use serde::{de::Error as _, ser::Error as _, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

#[derive(Serialize, Deserialize)]
#[serde(tag = "format", deny_unknown_fields)]
enum Wire {
    #[serde(rename = "rift-connection")]
    Node { version: u32, node: Box<Connection> },
    #[serde(rename = "rift-legacy-connection")]
    Legacy { version: u32, outbound: Value },
}
// Separate fields deliberately: an alias would lose whether a v1 outbound is an opaque
// object (possibly containing a `format` key) or a strictly versioned v2 connection.
#[derive(Serialize, Deserialize)]
struct ServerFile {
    id: String,
    name: String,
    #[serde(default)]
    country: String,
    protocol: String,
    host: String,
    port: u16,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    connection: Option<Value>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    outbound: Option<Value>,
    source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    routes: Option<ServerRoutes>,
}
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}
fn consistent(server: &Server) -> bool {
    outbound::inspect(&server.outbound).is_ok_and(|endpoint| {
        server.host == endpoint.host
            && server.port == endpoint.port
            && server.protocol == endpoint.protocol
            && server.id == Server::fingerprint(&server.outbound)
    })
}
impl Serialize for Server {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let wire = match Connection::from_legacy(&self.outbound) {
            Ok(node) if consistent(self) && node.legacy_value() == self.outbound => Wire::Node {
                version: 1,
                node: Box::new(node),
            },
            _ => Wire::Legacy {
                version: 1,
                outbound: self.outbound.clone(),
            },
        };
        ServerFile {
            id: self.id.clone(),
            name: self.name.clone(),
            country: self.country.clone(),
            protocol: self.protocol.clone(),
            host: self.host.clone(),
            port: self.port,
            connection: Some(
                serde_json::to_value(wire).map_err(|_| {
                    S::Error::custom(t("core.connection_wire.bad_connection_model"))
                })?,
            ),
            outbound: None,
            source: self.source.clone(),
            routes: self.routes.clone(),
        }
        .serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for Server {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let file = ServerFile::deserialize(deserializer)?;
        let (outbound, neutral) = match (file.connection, file.outbound) {
            (None, Some(outbound)) => (outbound, false),
            (Some(value), None) => match serde_json::from_value::<Wire>(value)
                .map_err(|_| D::Error::custom(t("core.connection_wire.bad_connection_model")))?
            {
                Wire::Node { version: 1, node } => {
                    // Storage validity is separate from executable capability. Retired nodes
                    // still have a valid bounded storage representation.
                    let checked =
                        Connection::from_neutral(serde_json::to_value(node).map_err(|_| {
                            D::Error::custom(t("core.connection_wire.bad_connection_model"))
                        })?)
                        .map_err(|_| {
                            D::Error::custom(t("core.connection_wire.bad_connection_model"))
                        })?;
                    (checked.legacy_value(), true)
                }
                Wire::Legacy {
                    version: 1,
                    outbound,
                } => (outbound, false),
                _ => {
                    return Err(D::Error::custom(t(
                        "core.connection_wire.unsupported_connection_version",
                    )))
                }
            },
            _ => {
                return Err(D::Error::custom(t(
                    "core.connection_wire.one_representation",
                )))
            }
        };
        let server = Server {
            id: file.id,
            name: file.name,
            country: file.country,
            protocol: file.protocol,
            host: file.host,
            port: file.port,
            outbound,
            source: file.source,
            routes: file.routes,
        };
        if neutral && !consistent(&server) {
            return Err(D::Error::custom(t("core.common.bad_server_data")));
        }
        Ok(server)
    }
}
