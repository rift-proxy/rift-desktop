//! The wire between Rift and the elevated TUN helper: frames of a four-byte length and
//! JSON. The helper reads these in a process with administrator rights, so every limit is checked
//! before memory is spent on it, and nothing is believed before it has been validated.
use crate::text::t;
use crate::{failure::FailureCode, model::Store, Error, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fmt;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Bumped whenever a message changes shape.
pub const VERSION: u32 = 4;
/// The largest frame either side accepts: a store of two thousand servers is some megabytes.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// What Rift asks of the helper.
#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToHelper {
    /// Run sing-box for this store, replacing a run that is going. The helper builds the config
    /// itself; the secret is the one Rift's own requests to the Clash API carry.
    Start { store: Box<Store>, secret: String },
    /// Stop sing-box and end the helper.
    Stop,
}
/// What the helper tells Rift.
#[derive(PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromHelper {
    /// The first frame: the version spoken and the token of this launch.
    Hello {
        version: u32,
        token: String,
    },
    Started,
    StartedWithHandoff {
        port: u16,
        secret: String,
    },
    /// Bounded, closed observations only; no log text or helper-generated messages.
    Diagnostics {
        diagnostics: crate::runtime::RuntimeDiagnostics,
    },
    /// `Start` did not end in a running sing-box.
    Failed {
        #[serde(deserialize_with = "deserialize_exit_code")]
        code: FailureCode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        startup: Option<StartupFailure>,
    },
    /// Answer to `Stop`, sent just before the helper ends.
    Stopped,
    /// sing-box ended by itself; the helper ends too.
    Exited,
    /// The runtime ended after a known engine crash or an unsuccessful status query.
    ExitedWithCode {
        #[serde(deserialize_with = "deserialize_exit_code")]
        code: FailureCode,
    },
}
fn deserialize_exit_code<'de, D>(deserializer: D) -> std::result::Result<FailureCode, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct ExitCode;
    impl serde::de::Visitor<'_> for ExitCode {
        type Value = FailureCode;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a defined runtime failure code")
        }

        fn visit_str<E: serde::de::Error>(self, code: &str) -> std::result::Result<Self::Value, E> {
            FailureCode::parse(code).ok_or_else(|| E::custom("unknown runtime failure code"))
        }
    }
    deserializer.deserialize_str(ExitCode)
}
impl fmt::Debug for FromHelper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Hello { .. } => "FromHelper::Hello",
            Self::Started => "FromHelper::Started",
            Self::StartedWithHandoff { .. } => "FromHelper::StartedWithHandoff",
            Self::Diagnostics { .. } => "FromHelper::Diagnostics",
            Self::Failed { .. } => "FromHelper::Failed",
            Self::Stopped => "FromHelper::Stopped",
            Self::Exited => "FromHelper::Exited",
            Self::ExitedWithCode { .. } => "FromHelper::ExitedWithCode",
        })
    }
}
// A store holds credentials: the debug form names the message and nothing else.
impl fmt::Debug for ToHelper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Start { .. } => "ToHelper::Start",
            Self::Stop => "ToHelper::Stop",
        })
    }
}

/// Closed startup facts. No process output, paths, config or authentication text crosses
/// this boundary. The same value is an Error source locally and a validated wire payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupStage {
    DnsBridgeStart,
    SingBoxConfigCheckSpawn,
    SingBoxConfigCheck,
    SingBoxSpawn,
    SingBoxConfigFeed,
    XrayConfigCheckSpawn,
    XrayConfigCheck,
    XraySpawn,
    XrayConfigFeed,
}
impl StartupStage {
    fn name(self) -> &'static str {
        match self {
            Self::DnsBridgeStart => "dns_bridge_start",
            Self::SingBoxConfigCheckSpawn => "sing_box_config_check_spawn",
            Self::SingBoxConfigCheck => "sing_box_config_check",
            Self::SingBoxSpawn => "sing_box_spawn",
            Self::SingBoxConfigFeed => "sing_box_config_feed",
            Self::XrayConfigCheckSpawn => "xray_config_check_spawn",
            Self::XrayConfigCheck => "xray_config_check",
            Self::XraySpawn => "xray_spawn",
            Self::XrayConfigFeed => "xray_config_feed",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum StartupCause {
    Io {
        os_code: Option<i32>,
    },
    Exit {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Timeout,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct StartupFailure {
    stage: StartupStage,
    cause: StartupCause,
}
impl<'de> Deserialize<'de> for StartupFailure {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            stage: StartupStage,
            cause: StartupCause,
        }
        let wire = Wire::deserialize(deserializer)?;
        let value = Self {
            stage: wire.stage,
            cause: wire.cause,
        };
        let valid = match value.cause {
            StartupCause::Exit { code, signal } => {
                matches!(
                    value.stage,
                    StartupStage::SingBoxConfigCheck | StartupStage::XrayConfigCheck
                ) && (code.is_some() ^ signal.is_some())
                    && code != Some(0)
                    && signal.is_none_or(|signal| (1..=127).contains(&signal))
            }
            StartupCause::Timeout => matches!(
                value.stage,
                StartupStage::SingBoxConfigCheck
                    | StartupStage::XrayConfigCheck
                    | StartupStage::SingBoxConfigFeed
                    | StartupStage::XrayConfigFeed
            ),
            StartupCause::Io { .. } => true,
        };
        if !valid {
            return Err(serde::de::Error::custom("invalid startup observation"));
        }
        Ok(value)
    }
}
impl StartupFailure {
    pub(crate) fn io(stage: StartupStage, error: &std::io::Error) -> Self {
        Self {
            stage,
            cause: StartupCause::Io {
                os_code: error.raw_os_error(),
            },
        }
    }
    pub(crate) fn exit(stage: StartupStage, status: std::process::ExitStatus) -> Self {
        #[cfg(unix)]
        let signal = {
            use std::os::unix::process::ExitStatusExt;
            status.signal()
        };
        #[cfg(not(unix))]
        let signal = None;
        Self {
            stage,
            cause: StartupCause::Exit {
                code: status.code(),
                signal,
            },
        }
    }
    pub(crate) fn timeout(stage: StartupStage) -> Self {
        Self {
            stage,
            cause: StartupCause::Timeout,
        }
    }
    pub(crate) fn from_error(error: &Error) -> Option<Self> {
        std::error::Error::source(error)?
            .downcast_ref::<Self>()
            .copied()
    }
    pub(crate) fn matches_code(self, code: FailureCode) -> bool {
        use StartupStage::*;
        match (self.stage, self.cause) {
            (SingBoxConfigCheck, StartupCause::Timeout) => {
                code == FailureCode::SidecarConfigTimeout
            }
            (XrayConfigCheck, StartupCause::Timeout) => code == FailureCode::XrayConfigTimeout,
            (SingBoxConfigCheck, StartupCause::Exit { .. }) => {
                code == FailureCode::SidecarConfigRejected
            }
            (XrayConfigCheck, StartupCause::Exit { .. }) => code == FailureCode::XrayConfigRejected,
            (XrayConfigCheckSpawn | XrayConfigCheck | XraySpawn | XrayConfigFeed, _) => {
                code == FailureCode::XrayStartExit
            }
            _ => code == FailureCode::SidecarStartExit,
        }
    }
    pub(crate) fn attach(self, mut error: Error) -> Error {
        if let Error::Failure { code, source, .. } = &mut error {
            if self.matches_code(*code) {
                *source = Some(Box::new(self));
                return error.with_local_details(self.to_string());
            }
        }
        error
    }
}
impl fmt::Display for StartupFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stage={}; ", self.stage.name())?;
        match self.cause {
            StartupCause::Io {
                os_code: Some(code),
            } => write!(f, "os_error={code}"),
            StartupCause::Io { os_code: None } => f.write_str("cause=io_error"),
            StartupCause::Exit {
                code: Some(code), ..
            } => write!(f, "exit_code={code}"),
            StartupCause::Exit {
                signal: Some(signal),
                ..
            } => write!(f, "signal={signal}"),
            StartupCause::Exit { .. } => f.write_str("cause=unknown_exit"),
            StartupCause::Timeout => f.write_str("cause=timeout"),
        }
    }
}
impl std::error::Error for StartupFailure {}

fn channel_error(message: &str) -> Error {
    Error::failure(FailureCode::ElevationChannel, message)
}
/// The length a frame announces, or an error when it is more than `MAX_FRAME`.
pub fn frame_length(header: [u8; 4]) -> Result<usize> {
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_FRAME {
        return Err(channel_error(t("core.elevation.helper_message_too_large")));
    }
    Ok(length)
}
/// The message in a frame's body.
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
    serde_json::from_slice(body)
        .map_err(|_| channel_error(t("core.elevation.helper_unexpected_message")))
}
pub async fn write_frame<W, T>(writer: &mut W, message: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = serde_json::to_vec(message)?;
    let length = u32::try_from(body.len())
        .ok()
        .filter(|length| *length as usize <= MAX_FRAME)
        .ok_or_else(|| channel_error(t("core.elevation.message_to_helper_too_large")))?;
    let io = |_| channel_error(t("core.elevation.helper_link_lost_short"));
    writer.write_all(&length.to_be_bytes()).await.map_err(io)?;
    writer.write_all(&body).await.map_err(io)?;
    writer.flush().await.map_err(io)
}
/// The next message; `None` when the other side has closed the channel.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<Option<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut header = [0u8; 4];
    match reader.read(&mut header[..1]).await {
        Ok(0) => return Ok(None),
        Ok(_) => {}
        Err(_) => return Err(channel_error(t("core.elevation.helper_link_lost_short"))),
    }
    reader
        .read_exact(&mut header[1..])
        .await
        .map_err(|_| channel_error(t("core.elevation.helper_header_truncated")))?;
    let mut body = vec![0u8; frame_length(header)?];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|_| channel_error(t("core.elevation.helper_link_lost_short")))?;
    decode(&body).map(Some)
}
