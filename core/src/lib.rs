#![deny(unsafe_code)]

pub mod application;
pub mod autostart;
pub mod clash;
pub mod config;
pub mod connection;
mod connection_wire;
pub mod dead;
pub mod diagnostics;
pub mod dns;
pub mod elevation;
pub mod failure;
pub mod fast;
pub mod geodata;
pub mod history;
pub mod jitter;
pub mod journal;
pub mod launch;
pub mod link;
pub mod model;
mod network_binding;
pub mod notifications;
pub mod outbound;
pub mod parameters;
pub mod parser;
pub mod persistence;
pub mod pool;
pub mod probe;
pub mod profile;
pub mod proxy_channel;
pub mod qr;
pub mod routing;
pub mod runtime;
pub mod selection;
pub mod sidecar_log;
pub mod startup;
pub mod storage;
pub mod subscription;
mod subscription_observation;
pub mod text;
pub mod update;
#[cfg(windows)]
#[allow(unsafe_code)] // Audited Win32 FFI; every block has a SAFETY contract and Windows tests.
mod windows;
pub mod xray;
pub mod xray_log;
mod xray_process;

use diagnostics::{
    Attempt, Cause, Detail, Operation, SelectionInfo, Stage, SubscriptionInfo, Trail, Trigger, Via,
};
use failure::FailureCode;
use thiserror::Error;
#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Parameter(#[from] parameters::ParameterProblem),
    /// A rejected server row, classified where validation failed; never carries input text.
    #[error("{}", crate::text::t("core.error.rejected"))]
    Rejected(diagnostics::Rejection),
    #[error("{0}")]
    Invalid(String),
    #[error("{}", crate::text::t("core.error.io"))]
    Io(#[from] std::io::Error),
    #[error("{}", crate::text::t("core.error.json"))]
    Json(#[from] serde_json::Error),
    #[error("{}", crate::text::t("core.error.network"))]
    Network,
    #[error("{}", crate::text::t("core.error.cancelled"))]
    Cancelled,
    #[error("{}", crate::text::t("core.error.unavailable"))]
    Unavailable,
    #[error("{message}")]
    Failure {
        code: FailureCode,
        message: String,
        /// Local context, cleaned before it is persisted.
        detail: Box<Detail>,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
}
impl Error {
    /// A failure with a stable code. It remembers the place that created it.
    #[track_caller]
    pub fn failure(code: FailureCode, message: impl Into<String>) -> Self {
        Self::Failure {
            code,
            message: message.into(),
            detail: Box::new(Detail::here()),
            source: None,
        }
    }
    #[track_caller]
    pub fn with_code(self, code: FailureCode) -> Self {
        if matches!(self, Self::Failure { .. }) {
            self
        } else {
            Self::Failure {
                code,
                message: self.to_string(),
                detail: Box::new(Detail::here()),
                source: Some(Box::new(self)),
            }
        }
    }
    /// What the error knows about where and how it happened.
    pub fn detail(&self) -> Option<&Detail> {
        match self {
            Self::Failure { detail, .. } => Some(detail),
            _ => None,
        }
    }
    fn with_detail(mut self, change: impl FnOnce(&mut Detail)) -> Self {
        if let Self::Failure { detail, .. } = &mut self {
            change(detail);
        }
        self
    }
    /// The stage the failure happened in. Has no effect on an error that carries no code.
    pub fn at_stage(self, stage: Stage) -> Self {
        self.with_detail(|detail| detail.stage = stage)
    }
    /// The operation the failure belongs to when its code alone does not say it.
    pub fn in_operation(self, operation: Operation) -> Self {
        self.with_detail(|detail| detail.operation = Some(operation))
    }
    pub fn with_status(self, status: u16) -> Self {
        self.with_detail(|detail| detail.http_status = Some(status))
    }
    /// The steps the operation completed before the failure.
    pub fn with_trail(self, trail: Trail) -> Self {
        self.with_detail(|detail| detail.trail = trail)
    }
    /// What started the operation, the route it took and whether the previous servers stayed.
    pub fn with_context(
        self,
        trigger: Trigger,
        via: Via,
        previous_servers_preserved: bool,
    ) -> Self {
        self.with_detail(|detail| {
            detail.trigger = trigger;
            detail.via = via;
            detail.previous_servers_preserved = Some(previous_servers_preserved);
        })
    }
    /// The best guess at why the request failed.
    pub fn with_cause(self, cause: Cause) -> Self {
        self.with_detail(|detail| detail.cause = Some(cause))
    }
    /// What the answer of a subscription was; what an earlier step knew stays where this one
    /// knows nothing.
    pub fn with_subscription(self, info: SubscriptionInfo) -> Self {
        self.with_detail(|detail| {
            let before = detail.subscription.unwrap_or_default();
            detail.subscription = Some(SubscriptionInfo {
                body_kind: info.body_kind.or(before.body_kind),
                body_size: info.body_size.or(before.body_size),
                content_type: info.content_type.or(before.content_type),
                redirects: info.redirects.or(before.redirects),
                rows: info.rows.or(before.rows),
                filtered: info.filtered.or(before.filtered),
            });
        })
    }
    /// The request that failed before this one.
    pub fn with_first_attempt(self, attempt: Attempt) -> Self {
        self.with_detail(|detail| detail.first_attempt = Some(attempt))
    }
    /// What a selection cycle that found nothing had seen.
    pub fn with_selection(self, selection: SelectionInfo) -> Self {
        self.with_detail(|detail| detail.selection = Some(selection))
    }
    /// What started the operation, when that is all the caller knows.
    pub fn with_trigger(self, trigger: Trigger) -> Self {
        self.with_detail(|detail| detail.trigger = trigger)
    }
    pub const fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Failure { code, .. } => Some(*code),
            Self::Cancelled => Some(FailureCode::Cancelled),
            Self::Unavailable => Some(FailureCode::NoAvailableServer),
            _ => None,
        }
    }
}

impl Error {
    /// Attach a fixed parser field/path; never include the input value here.
    pub fn with_field_path(self, path: impl Into<String>) -> Self {
        self.with_detail(|detail| detail.field_path = Some(path.into()))
    }

    /// Attach a concrete local technical reason, cleaned at the journal boundary.
    pub fn with_local_details(self, details: impl Into<String>) -> Self {
        self.with_detail(|detail| detail.local_details = Some(details.into()))
    }

    /// Rich local diagnostics including the original source chain and operation context.
    /// This is not sanitized: persistence and UI boundaries must use `journal::clean`.
    pub fn diagnostic_details(&self) -> String {
        diagnostics::Diagnostic::from_error(self).details(self)
    }
}
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod local_diagnostic_tests {
    use super::*;
    use std::error::Error as _;

    #[test]
    fn adding_a_failure_code_preserves_the_original_source_chain() {
        let error = Error::from(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "local write was denied",
        ))
        .with_code(FailureCode::Persistence)
        .at_stage(Stage::Persist)
        .with_field_path("state.version");
        assert!(matches!(
            error.source().unwrap().downcast_ref::<Error>(),
            Some(Error::Io(_))
        ));
        assert_eq!(
            error.source().unwrap().source().unwrap().to_string(),
            "local write was denied"
        );
        let details = error.diagnostic_details();
        assert!(details.contains("local write was denied"));
        assert!(details.contains("stage: persist"));
        assert!(details.contains("field_path: state.version"));
    }

    #[test]
    fn recoding_a_failure_keeps_its_cause_and_local_context() {
        let error = Error::from(serde_json::from_str::<serde_json::Value>("{").unwrap_err())
            .with_code(FailureCode::SubscriptionInvalidBody)
            .with_local_details("JSON parser failed at line 1 column 1")
            .with_code(FailureCode::Unknown);
        assert_eq!(
            error.failure_code(),
            Some(FailureCode::SubscriptionInvalidBody)
        );
        assert!(error
            .diagnostic_details()
            .contains("JSON parser failed at line 1 column 1"));
        assert!(error.source().is_some());
    }

    #[test]
    fn json_data_error_diagnostics_do_not_quote_unknown_input_values() {
        let parse_error =
            serde_json::from_str::<u64>("\"unknown-provider-credential\"").unwrap_err();
        let error = Error::from(parse_error).with_code(FailureCode::SubscriptionInvalidBody);
        let details = error.diagnostic_details();
        assert!(!details.contains("unknown-provider-credential"));
    }
}
