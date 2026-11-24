//! Process-local notification channels. No transport, persistence, or network access.
use std::sync::LazyLock;
use tokio::sync::broadcast;

static ERRORS: LazyLock<broadcast::Sender<String>> = LazyLock::new(|| broadcast::channel(64).0);
static NOTICES: LazyLock<broadcast::Sender<String>> = LazyLock::new(|| broadcast::channel(64).0);

/// Show a local error toast to current subscribers.
pub fn error(message: &str) {
    let _ = ERRORS.send(message.to_owned());
}

/// Show a local informational toast to current subscribers.
pub fn inform(message: &str) {
    let _ = NOTICES.send(message.to_owned());
}

/// Subscribe to local error messages.
pub fn subscribe() -> broadcast::Receiver<String> {
    ERRORS.subscribe()
}

/// Subscribe to local informational messages.
pub fn subscribe_notices() -> broadcast::Receiver<String> {
    NOTICES.subscribe()
}
