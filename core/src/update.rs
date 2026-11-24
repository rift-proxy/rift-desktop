//! Whether a freshly checked release should trigger a fresh UI notification.
//! Everything else about auto-update (fetching, signature verification, install)
//! belongs to `tauri-plugin-updater` in the desktop crate — this module only
//! decides whether to re-nag about a version the user already dismissed.

/// `dismissed` is the version the user last dismissed this session, if any.
pub fn should_notify(latest_version: &str, dismissed: Option<&str>) -> bool {
    dismissed != Some(latest_version)
}

#[cfg(test)]
mod tests {
    use super::should_notify;

    #[test]
    fn notifies_when_nothing_was_dismissed_yet() {
        assert!(should_notify("0.2.0", None));
    }

    #[test]
    fn does_not_renotify_the_exact_dismissed_version() {
        assert!(!should_notify("0.2.0", Some("0.2.0")));
    }

    #[test]
    fn notifies_when_a_newer_version_appears_after_an_older_dismissal() {
        assert!(should_notify("0.3.0", Some("0.2.0")));
    }
}
