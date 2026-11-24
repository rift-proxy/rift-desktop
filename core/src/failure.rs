use serde::{Deserialize, Serialize};

/// Stable, non-sensitive reason codes. They describe a stage and broad cause only; never
/// embed dependency messages, hosts, URLs, credentials or child output.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCode {
    EndpointTimeout,
    EndpointConnect,
    ProxyTimeout,
    ProxyTransport,
    ProxyHttpStatus,
    ProxyTestFailed,
    ProxyMissing,
    ProxyInvalidResponse,
    SelectorSwitch,
    SubscriptionTimeout,
    SubscriptionTransport,
    SubscriptionHttpStatus,
    SubscriptionInvalidBody,
    SubscriptionFiltered,
    UnsupportedLegacyParameters,
    XrayConfigRejected,
    XrayConfigTimeout,
    XrayStartExit,
    XrayReadinessTimeout,
    XrayCrashed,

    SidecarConfigRejected,
    SidecarConfigTimeout,
    SidecarStartExit,
    SidecarReadinessTimeout,
    SidecarCrashed,
    LocalPortInUse,
    LocalPortDenied,
    SidecarStop,
    Persistence,
    Cancelled,
    NoAvailableServer,
    ConnectionLost,
    Autostart,
    GeodataDownload,
    GeodataInvalid,
    ClashApi,
    ElevationDeclined,
    ElevationFailed,
    ElevationChannel,
    ElevationHelperCrashed,
    #[serde(other)]
    Unknown,
}

impl FailureCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EndpointTimeout => "endpoint_timeout",
            Self::EndpointConnect => "endpoint_connect",
            Self::ProxyTimeout => "proxy_timeout",
            Self::ProxyTransport => "proxy_transport",
            Self::ProxyHttpStatus => "proxy_http_status",
            Self::ProxyTestFailed => "proxy_test_failed",
            Self::ProxyMissing => "proxy_missing",
            Self::ProxyInvalidResponse => "proxy_invalid_response",
            Self::SelectorSwitch => "selector_switch",
            Self::SubscriptionTimeout => "subscription_timeout",
            Self::SubscriptionTransport => "subscription_transport",
            Self::SubscriptionHttpStatus => "subscription_http_status",
            Self::SubscriptionInvalidBody => "subscription_invalid_body",
            Self::SubscriptionFiltered => "subscription_filtered",
            Self::UnsupportedLegacyParameters => "unsupported_legacy_parameters",
            Self::XrayConfigRejected => "xray_config_rejected",
            Self::XrayConfigTimeout => "xray_config_timeout",
            Self::XrayStartExit => "xray_start_exit",
            Self::XrayReadinessTimeout => "xray_readiness_timeout",
            Self::XrayCrashed => "xray_crashed",

            Self::SidecarConfigRejected => "sidecar_config_rejected",
            Self::SidecarConfigTimeout => "sidecar_config_timeout",
            Self::SidecarStartExit => "sidecar_start_exit",
            Self::SidecarReadinessTimeout => "sidecar_readiness_timeout",
            Self::SidecarCrashed => "sidecar_crashed",
            Self::LocalPortInUse => "local_port_in_use",
            Self::LocalPortDenied => "local_port_denied",
            Self::SidecarStop => "sidecar_stop",
            Self::Persistence => "persistence",
            Self::Cancelled => "cancelled",
            Self::NoAvailableServer => "no_available_server",
            Self::ConnectionLost => "connection_lost",
            Self::Autostart => "autostart",
            Self::GeodataDownload => "geodata_download",
            Self::GeodataInvalid => "geodata_invalid",
            Self::ClashApi => "clash_api",
            Self::ElevationDeclined => "elevation_declined",
            Self::ElevationFailed => "elevation_failed",
            Self::ElevationChannel => "elevation_channel",
            Self::ElevationHelperCrashed => "elevation_helper_crashed",
            Self::Unknown => "unknown",
        }
    }

    /// Every variant, in declaration order.
    pub const ALL: [FailureCode; 41] = [
        Self::EndpointTimeout,
        Self::EndpointConnect,
        Self::ProxyTimeout,
        Self::ProxyTransport,
        Self::ProxyHttpStatus,
        Self::ProxyTestFailed,
        Self::ProxyMissing,
        Self::ProxyInvalidResponse,
        Self::SelectorSwitch,
        Self::SubscriptionTimeout,
        Self::SubscriptionTransport,
        Self::SubscriptionHttpStatus,
        Self::SubscriptionInvalidBody,
        Self::SubscriptionFiltered,
        Self::UnsupportedLegacyParameters,
        Self::XrayConfigRejected,
        Self::XrayConfigTimeout,
        Self::XrayStartExit,
        Self::XrayReadinessTimeout,
        Self::XrayCrashed,
        Self::SidecarConfigRejected,
        Self::SidecarConfigTimeout,
        Self::SidecarStartExit,
        Self::SidecarReadinessTimeout,
        Self::SidecarCrashed,
        Self::LocalPortInUse,
        Self::LocalPortDenied,
        Self::SidecarStop,
        Self::Persistence,
        Self::Cancelled,
        Self::NoAvailableServer,
        Self::ConnectionLost,
        Self::Autostart,
        Self::GeodataDownload,
        Self::GeodataInvalid,
        Self::ClashApi,
        Self::ElevationDeclined,
        Self::ElevationFailed,
        Self::ElevationChannel,
        Self::ElevationHelperCrashed,
        Self::Unknown,
    ];

    /// The code a wire name stands for. Unlike serde, which maps any unknown text to `Unknown`,
    /// this says `None`: a report may only carry names Rift itself defines.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|code| code.as_str() == text)
    }
}

#[cfg(test)]
mod tests {
    use super::FailureCode;

    const ALL: [FailureCode; 41] = FailureCode::ALL;

    #[test]
    fn parse_knows_only_the_defined_names() {
        for code in ALL {
            assert_eq!(FailureCode::parse(code.as_str()), Some(code));
        }
        assert_eq!(FailureCode::parse("future_code"), None);
        assert_eq!(FailureCode::parse(""), None);
        let mut names: Vec<_> = ALL.iter().map(|code| code.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ALL.len(), "wire names are unique");
    }

    #[test]
    fn every_variant_serializes_to_its_diagnostics_name() {
        // Diagnostics counts use `as_str`, the journal and snapshot use serde: they must agree.
        for code in ALL {
            assert_eq!(
                serde_json::to_string(&code).unwrap(),
                format!("\"{}\"", code.as_str())
            );
            if code != FailureCode::Unknown {
                assert_eq!(
                    serde_json::from_str::<FailureCode>(&format!("\"{}\"", code.as_str())).unwrap(),
                    code
                );
            }
        }
    }

    #[test]
    fn wire_names_are_stable_and_future_values_are_safe() {
        assert_eq!(
            serde_json::to_string(&FailureCode::ProxyInvalidResponse).unwrap(),
            r#""proxy_invalid_response""#
        );
        assert_eq!(
            serde_json::from_str::<FailureCode>(r#""future_code""#).unwrap(),
            FailureCode::Unknown
        );
    }
}
