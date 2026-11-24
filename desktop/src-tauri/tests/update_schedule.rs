//! Exercise the scheduler's actual journal/notification consumer without a GUI runtime.
#[path = "../src/update_schedule.rs"]
mod update_schedule;
#[cfg(test)]
mod tests {
    use super::update_schedule::record_scheduled_check;
    use rift_core::{
        application::{Action, Application},
        model::Store,
        notifications,
        runtime::Sidecar,
        text::t,
    };

    #[tokio::test]
    async fn scheduled_failures_are_local_silent_deduplicated_and_rearmed_by_success() {
        let directory = tempfile::tempdir().unwrap();
        let backend = Application::new(
            Store::default(),
            directory.path().into(),
            Box::new(Sidecar::new(
                directory.path().join("absent-sidecar"),
                directory.path().into(),
            )),
        )
        .unwrap();
        let mut errors = notifications::subscribe();
        let mut failing = false;
        let detail =
            "stage=update; connection refused https://foreign.example/feed?token=update-canary";
        record_scheduled_check(&backend, &mut failing, Some(detail));
        record_scheduled_check(&backend, &mut failing, Some(detail));
        assert_eq!(backend.snapshot().journal.len(), 1);
        assert!(
            errors.try_recv().is_err(),
            "scheduled failures must not notify"
        );
        record_scheduled_check(&backend, &mut failing, None);
        record_scheduled_check(&backend, &mut failing, Some(detail));
        assert_eq!(backend.snapshot().journal.len(), 2);
        assert!(
            errors.try_recv().is_err(),
            "new streak is still noninteractive"
        );
        // The manual command uses the ordinary host error boundary, not this scheduler.
        backend.log_external_error(t("shell.error.update_check"), Some(detail));
        assert_eq!(errors.try_recv().unwrap(), t("shell.error.update_check"));
        assert_eq!(backend.snapshot().journal.len(), 3);
        backend.execute(Action::ExportDiagnostics).await.unwrap();
        for name in ["journal.json", "diagnostics.json"] {
            let text = std::fs::read_to_string(directory.path().join(name)).unwrap();
            assert!(text.contains("connection refused"));
            assert!(!text.contains("foreign.example"));
            assert!(!text.contains("update-canary"));
        }
    }
}
