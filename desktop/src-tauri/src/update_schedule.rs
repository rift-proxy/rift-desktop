use rift_core::{application::Application, text::t};

/// Keep one local failure per streak; a successful check re-arms the next failure.
pub(crate) fn record_scheduled_check(
    backend: &Application,
    failing: &mut bool,
    error: Option<&str>,
) {
    match error {
        None => *failing = false,
        Some(error) if !*failing => {
            *failing = true;
            backend.log_background_error(t("shell.error.update_check"), Some(error));
        }
        Some(_) => {}
    }
}
