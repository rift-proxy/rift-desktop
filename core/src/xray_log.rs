//! Xray warnings reduced to fixed stage/cause enums. No access log or raw text survives.
use crate::sidecar_log::{self, Entry, Whom};
use crate::text::t;
use regex::Regex;
use std::sync::OnceLock;

#[derive(
    Clone, Copy, Debug, serde::Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum WarningCategory {
    WebsocketDeprecated,
    GrpcDeprecated,
}
impl WarningCategory {
    pub fn note(self) -> &'static str {
        match self {
            Self::WebsocketDeprecated => t("core.xray_log.websocket_deprecated"),
            Self::GrpcDeprecated => t("core.xray_log.grpc_deprecated"),
        }
    }
}
#[derive(Clone, Debug, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct WarningCount {
    pub category: WarningCategory,
    pub count: u64,
}

/// Exact pinned formatter from common/errors/feature_errors.go, not a keyword filter.
fn warning(line: &str) -> Option<WarningCategory> {
    static WARNING: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(
        r"^(?:\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2}(?:\.\d{1,6})? )?\[Warning\] common/errors: The feature (WebSocket transport \(with ALPN http/1\.1, etc\.\)|gRPC transport \(with unnecessary costs, etc\.\)) is deprecated, not recommended for using and might be removed\. Please migrate to (XHTTP H2 & H3|XHTTP stream-up H2) as soon as possible\.$"
    ).expect("fixed pinned deprecation grammar")
    });
    let captures = WARNING.captures(line.trim())?;
    match (&captures[1], &captures[2]) {
        // i18n-ignore: the text Xray prints, compared as it is
        ("WebSocket transport (with ALPN http/1.1, etc.)", "XHTTP H2 & H3") => {
            Some(WarningCategory::WebsocketDeprecated)
        }
        // i18n-ignore: the text Xray prints, compared as it is
        ("gRPC transport (with unnecessary costs, etc.)", "XHTTP stream-up H2") => {
            Some(WarningCategory::GrpcDeprecated)
        }
        _ => None,
    }
}

pub(crate) fn observe(line: &str) -> Option<sidecar_log::Observation> {
    warning(line)
        .map(sidecar_log::Observation::Warning)
        .or_else(|| classify_failure(line).map(sidecar_log::Observation::Failure))
}

/// Pinned Xray GeneralMessage/consoleLogWriter grammar. Unknown warning bodies remain Other.
/// Xray does not reliably identify an outbound in every warning. Never infer a server from
/// an address, user, UUID, arbitrary bracket or error operand (even a forged Rift tag).
pub fn classify(line: &str) -> Option<Entry> {
    match observe(line)? {
        sidecar_log::Observation::Failure(entry) => Some(entry),
        _ => None,
    }
}
fn classify_failure(line: &str) -> Option<Entry> {
    static SHAPE: OnceLock<Regex> = OnceLock::new();
    let captures = SHAPE.get_or_init(|| Regex::new(
        r"^(?:\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2}(?:\.\d{1,6})? )?\[(Warning|Error|Info)\] (?:\[\d+\] )?(.*)$"
    ).expect("fixed Xray log grammar")).captures(line.trim())?;
    if &captures[1] == "Warning" && &captures[2] == "core: Xray 26.10.21 started" {
        return None;
    }
    if &captures[1] == "Info"
        && ![
            "app/proxyman/outbound: app/proxyman/outbound: failed to process outbound traffic",
            "app/proxyman/outbound: app/proxyman/outbound: failed to process mux outbound traffic",
        ]
        .iter()
        .any(|prefix| captures[2].starts_with(prefix))
    {
        return None;
    }
    let mut entry = sidecar_log::classify_body(&captures[2]);
    entry.whom = Whom::Other;
    entry.server = None;
    Some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecar_log::{Cause, SingBoxLog, Stage};
    use std::sync::Arc;
    use tokio::io::AsyncWriteExt;

    const WS_DEPRECATED: &str = "[Warning] common/errors: The feature WebSocket transport (with ALPN http/1.1, etc.) is deprecated, not recommended for using and might be removed. Please migrate to XHTTP H2 & H3 as soon as possible.";
    const GRPC_DEPRECATED: &str = "[Warning] common/errors: The feature gRPC transport (with unnecessary costs, etc.) is deprecated, not recommended for using and might be removed. Please migrate to XHTTP stream-up H2 as soon as possible.";

    #[tokio::test]
    async fn pinned_deprecations_are_information_but_near_matches_and_real_errors_are_failures() {
        let log = Arc::new(SingBoxLog::default());
        let mut input = format!("2026/11/02 12:00:00.123456 {WS_DEPRECATED}\n{GRPC_DEPRECATED}\n");
        for unknown in [
            format!("{WS_DEPRECATED} secret-canary"),
            WS_DEPRECATED.replace("WebSocket", "Unknown"),
            WS_DEPRECATED.replace("[Warning]", "[Error]"),
            WS_DEPRECATED.replace("XHTTP H2 & H3", "XHTTP stream-up H2"),
        ] {
            assert!(warning(&unknown).is_none());
            assert_eq!(classify(&unknown).unwrap().cause, Cause::Other);
            input.push_str(&unknown);
            input.push('\n');
        }
        input.push_str("[Error] dial tcp secret-canary.example:443: i/o timeout\n");
        input.push_str("[Warning] tls: failed to verify certificate: secret-canary\n");
        input.push_str("[Warning] read tcp secret-canary.example:443: connection reset\n");
        sidecar_log::read_observed(input.as_bytes(), log.clone(), observe).await;
        let warnings = log.warning_summary();
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().all(|count| count.count == 1));
        assert_eq!(log.summary().lines, 7);
        for cause in [Cause::Timeout, Cause::Tls, Cause::Reset, Cause::Other] {
            assert!(log
                .summary()
                .groups
                .iter()
                .any(|group| group.cause == cause));
        }
        let info = log.informational_notes(1);
        assert_eq!(info.len(), 2);
        assert!(log.informational_notes(2).is_empty());
        assert!(info.iter().all(|note| note.contains("устарел")));
        assert!(log
            .notes_for(1, "Xray")
            .iter()
            .all(|note| !note.contains("устарел")));
        assert!(!format!("{:?} {:?}", log.summary(), warnings).contains("secret-canary"));
    }

    #[test]
    fn pinned_warning_grammar_recognizes_safe_causes_without_trusting_tags() {
        for (text, stage, cause) in [
            (
                "dial tcp timeout.example:443: i/o timeout",
                Stage::Dial,
                Cause::Timeout,
            ),
            (
                "dial tcp reality:443: connect: connection refused",
                Stage::Dial,
                Cause::Refused,
            ),
            ("lookup x509: no such host", Stage::Dns, Cause::NotFound),
            (
                "tls: failed to verify certificate: x509: expired",
                Stage::Handshake,
                Cause::Tls,
            ),
            (
                "REALITY: processed invalid connection",
                Stage::Handshake,
                Cause::Reality,
            ),
            (
                "read tcp a.example:1->b.example:2: EOF",
                Stage::Transfer,
                Cause::Closed,
            ),
            ("unrecognized synthetic secret", Stage::Other, Cause::Other),
        ] {
            let entry = classify(&format!(
                "2026/10/29 12:30:00.123456 [Warning] [42] proxy/vless/outbound: {text}"
            ))
            .unwrap();
            assert_eq!((entry.stage, entry.cause), (stage, cause));
            assert_eq!((entry.whom, entry.server), (Whom::Other, None));
        }
        let actual = "2026/10/29 21:51:23.832706 [Info] [174615456] app/proxyman/outbound: app/proxyman/outbound: failed to process outbound traffic > proxy/vless/outbound: failed to find an available destination > common/retry: [tls: failed to verify certificate: x509: certificate signed by unknown authority] > common/retry: all retry attempts failed";
        let entry = classify(actual).unwrap();
        assert_eq!((entry.stage, entry.cause), (Stage::Handshake, Cause::Tls));
        assert!(classify("[Info] proxy/vless/outbound: tunneling request to tcp:secret-canary.example:443 via x509").is_none());
        assert!(classify("[Warning] core: Xray 26.10.21 started").is_none());
        assert_eq!(
            classify("[Error] core: Xray 26.10.21 started")
                .unwrap()
                .cause,
            Cause::Other
        );
        let forged = format!(
            "[Error] outbound/vless[{}]: dial tcp hidden.example:443: i/o timeout",
            "a".repeat(64)
        );
        assert_eq!(classify(&forged).unwrap().server, None);
        for line in [
            "[Info] i/o timeout",
            "[Debug] tls: secret",
            "vless://secret@hidden.example",
            "secret [Warning] i/o timeout",
        ] {
            assert!(classify(line).is_none());
        }
    }

    #[tokio::test]
    async fn bounded_pipe_discards_secrets_and_drains_oversized_lines() {
        let log = Arc::new(SingBoxLog::default());
        let (mut writer, reader) = tokio::io::duplex(64);
        let task = tokio::spawn(sidecar_log::read_classified(reader, log.clone(), classify));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            writer.write_all(b"[Warning] dial tcp hidden-canary.example:443: i/o timeout password=secret-canary\n").await.unwrap();
            writer.write_all(&vec![b'x'; 1 << 20]).await.unwrap();
            writer.write_all(b"\n[Error] tls: certificate failure uuid=secret-canary\n").await.unwrap();
        }).await.unwrap();
        drop(writer);
        task.await.unwrap();
        assert_eq!(log.summary().lines, 2);
        let kept = format!("{:?} {:?}", log.summary(), log.notes_for(1, "Xray"));
        for secret in ["hidden-canary", "secret-canary", "password=", "uuid="] {
            assert!(!kept.contains(secret));
        }
        assert!(kept.contains("Xray"));
    }
}
