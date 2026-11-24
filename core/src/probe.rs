use crate::text::t;
use crate::tr;
use crate::{
    failure::FailureCode,
    model::{Server, Settings},
    selection::{Network, Probe},
    Error, Result,
};
use async_trait::async_trait;
use serde_json::json;
use std::time::Duration;
use tokio::{
    net::TcpStream,
    time::{timeout, Instant},
};

/// The outbound of the generated config that leaves by the real interface.
const DIRECT_OUTBOUND: &str = "direct";
/// Sites the machine is asked for, one after another until one answers: any answer from any of
/// them means there is a network.
const NETWORK_URLS: [&str; 3] = [
    "https://www.google.com/generate_204",
    "https://ya.ru",
    "https://vk.ru",
];

#[derive(Clone)]
pub struct NetworkProbe {
    client: reqwest::Client,
    /// Asks the sites itself, from this process (no proxy, no redirects).
    machine: reqwest::Client,
    /// The sites that request goes to.
    machine_sites: Vec<String>,
    base: String,
    secret: String,
    settings: Settings,
    channel: Option<crate::proxy_channel::ProxyChannel>,
    test_root: Option<reqwest::Certificate>,
}
impl NetworkProbe {
    pub fn new(settings: Settings, secret: String) -> Result<Self> {
        settings.validate()?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_millis(settings.proxy_timeout_ms + 1000))
            .build()
            .map_err(|_| Error::Network)?;
        let machine = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(settings.proxy_timeout_ms))
            .build()
            .map_err(|_| Error::Network)?;
        Ok(Self {
            client,
            machine,
            machine_sites: NETWORK_URLS.map(str::to_owned).to_vec(),
            base: format!("http://127.0.0.1:{}", settings.api_port),
            secret,
            settings,
            channel: None,
            test_root: None,
        })
    }
    /// Other sites for the machine's own request (tests answer from a loopback listener).
    pub fn with_machine_sites(mut self, sites: Vec<String>) -> Self {
        self.machine_sites = sites;
        self
    }
    pub fn with_channel(mut self, channel: Option<crate::proxy_channel::ProxyChannel>) -> Self {
        self.channel = channel;
        self
    }
    /// Dependency injection of a synthetic CA, without changing OS or Go system roots.
    #[doc(hidden)]
    pub fn with_test_root(mut self, certificate: reqwest::Certificate) -> Self {
        self.test_root = Some(certificate);
        self
    }
    pub async fn ready(&self) -> Result<()> {
        self.client
            .get(format!("{}/version", self.base))
            .bearer_auth(&self.secret)
            .send()
            .await
            .map_err(|_| Error::Network)?
            .error_for_status()
            .map_err(|_| Error::Network)?;
        Ok(())
    }
}
#[async_trait]
impl Probe for NetworkProbe {
    async fn endpoint(&self, s: &Server) -> Result<u64> {
        if self.channel.is_some() {
            crate::connection::Connection::from_legacy(&s.outbound)?.check_xray()?;
        }
        let start = Instant::now();
        let _stream = timeout(
            Duration::from_millis(self.settings.endpoint_timeout_ms),
            TcpStream::connect((s.host.as_str(), s.port)),
        )
        .await
        .map_err(|_| Error::failure(FailureCode::EndpointTimeout, t("core.probe.server_timeout")))?
        .map_err(|_| {
            Error::failure(
                FailureCode::EndpointConnect,
                t("core.probe.server_connect_failed"),
            )
        })?;
        Ok(start.elapsed().as_millis().max(1) as u64)
    }
    async fn proxy(&self, s: &Server) -> Result<u64> {
        if let Some(channel) = &self.channel {
            crate::connection::Connection::from_legacy(&s.outbound)?.check_xray()?;
            let gateway = channel
                .read()
                .map_err(|_| proxy_failure(FailureCode::XrayCrashed))?
                .clone()
                .ok_or(Error::Cancelled)?;
            let mut builder = reqwest::Client::builder()
                .no_proxy()
                .proxy(gateway.proxy(&s.id)?)
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .pool_max_idle_per_host(0)
                .timeout(Duration::from_millis(self.settings.proxy_timeout_ms));
            if let Some(root) = &self.test_root {
                builder = builder.tls_certs_only([root.clone()]);
            }
            let client = builder
                .build()
                .map_err(|_| proxy_failure(FailureCode::ProxyTransport))?;
            let start = Instant::now();
            let response = client.head(&self.settings.connectivity_url).send().await;
            let current = || {
                channel.read().ok().is_some_and(|value| {
                    value
                        .as_ref()
                        .is_some_and(|value| value.same_generation(&gateway))
                })
            };
            if !current() {
                return Err(Error::Cancelled);
            }
            match response {
                Ok(_) => return Ok(start.elapsed().as_millis().max(1) as u64),
                Err(error) => {
                    // A dead shared local gateway is infrastructure failure, not one bad remote.
                    let gateway_alive = timeout(
                        Duration::from_millis(300),
                        TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, gateway.port())),
                    )
                    .await
                    .is_ok_and(|r| r.is_ok());
                    if !current() {
                        return Err(Error::Cancelled);
                    }
                    return Err(proxy_failure(if !gateway_alive {
                        FailureCode::XrayCrashed
                    } else if error.is_timeout() {
                        FailureCode::ProxyTimeout
                    } else {
                        FailureCode::ProxyTransport
                    }));
                }
            }
        }
        self.delay(&s.id, &self.settings.connectivity_url).await
    }
    async fn select(&self, id: &str) -> Result<()> {
        self.client
            .put(format!("{}/proxies/main", self.base))
            .bearer_auth(&self.secret)
            .json(&json!({"name":id}))
            .send()
            .await
            .map_err(|_| selector_failure())?
            .error_for_status()
            .map_err(|_| selector_failure())?;
        Ok(())
    }
    async fn routes(&self, mode: &str) -> Result<()> {
        self.client
            .patch(format!("{}/configs", self.base))
            .bearer_auth(&self.secret)
            .json(&json!({"mode":mode}))
            .send()
            .await
            .map_err(|_| selector_failure())?
            .error_for_status()
            .map_err(|_| selector_failure())?;
        // sing-box answers 204 to a mode it does not know and keeps the one it had: ask what it is
        // in now.
        let now: serde_json::Value = self
            .client
            .get(format!("{}/configs", self.base))
            .bearer_auth(&self.secret)
            .send()
            .await
            .map_err(|_| selector_failure())?
            .error_for_status()
            .map_err(|_| selector_failure())?
            .json()
            .await
            .map_err(|_| selector_failure())?;
        if now["mode"] == mode {
            Ok(())
        } else {
            Err(selector_failure())
        }
    }
    async fn network(&self) -> Network {
        // sing-box first; if it hears nothing, the machine asks itself: a sidecar that cannot get
        // out (a firewall that does not know it, an interface it binds wrongly) says nothing
        // about the network, and a cycle must not be lost to it. Through a tunnel the machine's
        // own request goes into it, which still proves there is a network.
        let mut seen = None;
        for url in NETWORK_URLS {
            match self.delay(DIRECT_OUTBOUND, url).await {
                Ok(_) => return Network::Up,
                Err(error) => seen = error.failure_code(),
            }
        }
        let by_sing_box = reason(seen);
        let mut seen = None;
        for url in &self.machine_sites {
            match self.machine.head(url).send().await {
                // Any answer, whatever its status, is the network.
                Ok(_) => {
                    return Network::SidecarDeaf(tr!(
                        "core.probe.via_singbox",
                        by_sing_box = by_sing_box
                    ))
                }
                Err(error) => seen = Some(error.is_timeout()),
            }
        }
        let by_machine = match seen {
            Some(true) => t("core.probe.timeout"),
            _ => t("core.probe.no_connection"),
        };
        Network::Down(tr!(
            "core.probe.via_singbox_and_rift",
            by_sing_box = by_sing_box,
            by_machine = by_machine
        ))
    }
}
/// What a failed delay test comes to, for the journal.
fn reason(code: Option<FailureCode>) -> &'static str {
    match code {
        Some(FailureCode::ProxyTimeout) => t("core.probe.timeout"),
        Some(FailureCode::ProxyTransport) => t("core.probe.no_connection"),
        Some(FailureCode::ProxyTestFailed) => t("core.probe.check_failed"),
        Some(FailureCode::ProxyMissing) => t("core.probe.no_direct_exit"),
        _ => t("core.probe.unparsed_answer"),
    }
}
impl NetworkProbe {
    /// The answer of sing-box to a request for `target` made through the outbound `outbound`.
    async fn delay(&self, outbound: &str, target: &str) -> Result<u64> {
        let mut url = url::Url::parse(&format!("{}/proxies/", self.base))
            .map_err(|_| proxy_failure(FailureCode::ProxyTransport))?;
        url.path_segments_mut()
            .map_err(|_| proxy_failure(FailureCode::ProxyTransport))?
            .pop_if_empty()
            .push(outbound)
            .push("delay");
        let response = self
            .client
            .get(url)
            .bearer_auth(&self.secret)
            .query(&[
                ("url", target.to_owned()),
                ("timeout", self.settings.proxy_timeout_ms.to_string()),
            ])
            .send()
            .await
            .map_err(|error| {
                proxy_failure(if error.is_timeout() {
                    FailureCode::ProxyTimeout
                } else {
                    FailureCode::ProxyTransport
                })
            })?;
        // sing-box answers its own deadline with 504 (the client's timeout is a second later, so
        // that one never fires first), a test that failed for any other reason with 503 and an
        // outbound it does not have with 404. None of them is the server "refusing" anything.
        let response = match response.status() {
            status if status.is_success() => response,
            reqwest::StatusCode::GATEWAY_TIMEOUT => {
                return Err(proxy_failure(FailureCode::ProxyTimeout))
            }
            reqwest::StatusCode::SERVICE_UNAVAILABLE => {
                return Err(proxy_failure(FailureCode::ProxyTestFailed))
            }
            reqwest::StatusCode::NOT_FOUND => return Err(proxy_failure(FailureCode::ProxyMissing)),
            _ => return Err(proxy_failure(FailureCode::ProxyHttpStatus)),
        };
        // The client timeout also covers the body: a stalled body is a timeout, not a
        // malformed answer.
        let body: serde_json::Value = response.json().await.map_err(|error| {
            proxy_failure(if error.is_timeout() {
                FailureCode::ProxyTimeout
            } else {
                FailureCode::ProxyInvalidResponse
            })
        })?;
        body["delay"]
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or_else(|| proxy_failure(FailureCode::ProxyInvalidResponse))
    }
}

fn proxy_failure(code: FailureCode) -> Error {
    Error::failure(code, t("core.probe.access_check_failed"))
}

fn selector_failure() -> Error {
    Error::failure(FailureCode::SelectorSwitch, t("core.probe.switch_failed"))
}
