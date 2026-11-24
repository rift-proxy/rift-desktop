//! Rust-only local gateway capability. Never serialized into a snapshot or diagnostic.
use crate::text::t;
use crate::{failure::FailureCode, model::Store, Error, Result};
use std::{
    collections::HashSet,
    sync::{Arc, RwLock},
};

#[derive(Clone)]
pub struct ProxyHandoff {
    port: u16,
    secret: Arc<str>,
    candidates: Arc<HashSet<String>>,
}
pub type ProxyChannel = Arc<RwLock<Option<ProxyHandoff>>>;
impl ProxyHandoff {
    pub fn new(port: u16, secret: String, store: &Store) -> Result<Self> {
        if port == 0
            || [store.settings.api_port, store.settings.proxy_port].contains(&port)
            || secret.len() < 32
            || secret.len() > 128
            || !secret
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(Error::failure(
                FailureCode::ElevationChannel,
                t("core.proxy_channel.bad_channel"),
            ));
        }
        if store
            .available_servers()
            .iter()
            .any(|s| s.id.len() != 64 || !s.id.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(Error::failure(
                FailureCode::ElevationChannel,
                t("core.proxy_channel.bad_channel_ids"),
            ));
        }
        let candidates = store
            .available_servers()
            .into_iter()
            .filter(|s| {
                crate::connection::Connection::from_legacy(&s.outbound)
                    .and_then(|c| c.check_xray())
                    .is_ok()
            })
            .map(|s| s.id)
            .collect::<HashSet<_>>();
        Ok(Self {
            port,
            secret: secret.into(),
            candidates: Arc::new(candidates),
        })
    }
    pub const fn port(&self) -> u16 {
        self.port
    }
    pub(crate) fn secret(&self) -> &str {
        &self.secret
    }
    pub(crate) fn same_generation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.candidates, &other.candidates)
    }
    pub(crate) fn proxy(&self, id: &str) -> Result<reqwest::Proxy> {
        if !self.candidates.contains(id) {
            return Err(Error::failure(
                FailureCode::ProxyMissing,
                t("core.proxy_channel.server_not_in_plan"),
            ));
        }
        reqwest::Proxy::all(format!(
            "socks5h://u{id}:{}@127.0.0.1:{}",
            self.secret, self.port
        ))
        .map_err(|_| {
            Error::failure(
                FailureCode::XrayCrashed,
                t("core.proxy_channel.channel_unavailable"),
            )
        })
    }
}
pub(crate) fn set(channel: &ProxyChannel, value: Option<ProxyHandoff>) {
    *channel
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = value;
}
