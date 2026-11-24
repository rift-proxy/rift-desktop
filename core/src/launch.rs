//! The trusted two-core launch plan. Input is a checked Store, not raw backend JSON.
use crate::text::t;
use crate::{
    config::{self, Resolver},
    connection::Connection,
    failure::FailureCode,
    geodata::GeoSet,
    model::Store,
    Error, Result,
};
use serde_json::{json, Value};
use std::collections::HashSet;

/// Contains credentials: deliberately no Debug/Serialize or public mutation.
pub struct LaunchPlan {
    sing_box: Value,
    xray: Value,
    retired: usize,
}
impl LaunchPlan {
    pub fn sing_box(&self) -> &Value {
        &self.sing_box
    }
    pub fn xray(&self) -> &Value {
        &self.xray
    }
    pub const fn retired_count(&self) -> usize {
        self.retired
    }
    pub fn with_interface(mut self, name: &str) -> Result<Self> {
        crate::network_binding::validate_name(name)?;
        for outbound in self.xray["outbounds"]
            .as_array_mut()
            .expect("trusted builder")
        {
            if outbound["streamSettings"]["sockopt"]["interface"] == "rift-binding-required" {
                outbound["streamSettings"]["sockopt"]["interface"] = json!(name);
            }
        }
        Ok(self)
    }
    pub fn requires_interface(&self) -> bool {
        self.xray["outbounds"]
            .as_array()
            .expect("trusted builder")
            .iter()
            .any(|outbound| {
                outbound["streamSettings"]["sockopt"]["interface"] == "rift-binding-required"
            })
    }

    pub fn build(
        store: &Store,
        secret: &str,
        geo: &GeoSet,
        resolver: Resolver,
        handoff_port: u16,
        handoff_secret: &str,
    ) -> Result<Self> {
        let Resolver::Bridge(dns_port) = resolver else {
            return Err(Error::Invalid(
                t("core.launch.xray_needs_dns_bridge").into(),
            ));
        };
        if dns_port == 0
            || handoff_port == 0
            || handoff_secret.len() < 32
            || handoff_secret.len() > 128
            || !handoff_secret
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || handoff_port == dns_port
            || [store.settings.api_port, store.settings.proxy_port].contains(&handoff_port)
        {
            return Err(Error::Invalid(t("core.launch.bad_channel_params").into()));
        }
        let mut cloned = store.clone();
        let mut seen = HashSet::new();
        let mut accounts = Vec::new();
        // Unmatched traffic fails closed, even if a future routing bug omits one user rule.
        let mut outbounds = vec![json!({"protocol":"blackhole","tag":"blocked"})];
        let mut rules = Vec::new();
        let mut retired = 0;
        let mut accepted = Vec::new();
        for mut server in store.available_servers() {
            if !seen.insert(server.id.clone()) {
                continue;
            }
            let node = Connection::from_legacy(&server.outbound)?;
            let tag = format!("proxy-{}", server.id);
            let outbound = match node.xray_outbound(&tag) {
                Ok(outbound) => outbound,
                Err(error)
                    if error.failure_code() == Some(FailureCode::UnsupportedLegacyParameters) =>
                {
                    if store.selected.as_ref() == Some(&server.id) {
                        return Err(error);
                    }
                    retired += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let user = format!("u{}", server.id);
            accounts.push(json!({"user":user,"pass":handoff_secret}));
            rules.push(json!({"type":"field","user":[user],"outboundTag":tag}));
            let mut outbound = outbound;
            // Resolve every endpoint using Xray's explicit Rift DNS server, without OS fallback.
            outbound["streamSettings"]["sockopt"] = json!({"domainStrategy":"ForceIP"});
            if store.settings.mode != crate::model::Mode::Proxy
                && !node
                    .host()
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
            {
                // A forgotten binding must fail closed at dial, never recurse into our TUN.
                outbound["streamSettings"]["sockopt"]["interface"] = json!("rift-binding-required");
            }
            outbounds.push(outbound);
            server.outbound = json!({"type":"socks","server":"127.0.0.1","server_port":handoff_port,
                "version":"5","username":user,"password":handoff_secret});
            accepted.push(server);
        }
        if accepted.is_empty() {
            return Err(if retired > 0 {
                Error::failure(
                    FailureCode::UnsupportedLegacyParameters,
                    t("core.launch.old_servers_params"),
                )
            } else {
                Error::Unavailable
            });
        }
        // This clone is ephemeral; the stored outbound/ID/selected/history are untouched.
        cloned.servers = accepted;
        let sing_box = config::generate_resolving(&cloned, secret, geo, resolver)?;
        let xray = json!({
            "log":{"loglevel":"info","access":"none"},
            "dns":{"servers":[{"address":format!("tcp+local://127.0.0.1:{dns_port}"),"timeoutMs":15000}],"queryStrategy":"UseIP"},
            "inbounds":[{"tag":"handoff","listen":"127.0.0.1","port":handoff_port,"protocol":"socks",
                "settings":{"auth":"password","udp":true,"accounts":accounts}}],
            "outbounds":outbounds,"routing":{"rules":rules}
        });
        Ok(Self {
            sing_box,
            xray,
            retired,
        })
    }
}
