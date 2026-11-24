#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(unsafe_code)]

include!(concat!(env!("OUT_DIR"), "/update_endpoint.rs"));
include!(concat!(env!("OUT_DIR"), "/build_commit.rs"));

mod update_schedule;

#[cfg(windows)]
use rift_core::elevation::{
    client::HelperRuntime, mode::ModeRuntime, uac::ShellElevator, windows::PipeTransport,
};
use rift_core::text::t;
use rift_core::tr;
use rift_core::{
    application::{Action, Application, Snapshot},
    autostart, history,
    jitter::Jitter,
    journal::{self, clean, clean_message, Secrets},
    link,
    model::{ServerView, Store},
    notifications, parser, profile, routing,
    runtime::{Runtime, Sidecar},
    startup::{Milestones, READY_FILE_ENV},
    storage,
};
use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, PoisonError,
};
use std::time::Duration;
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_updater::{Update, UpdaterExt};
use url::Url;

type Backend = Arc<Application>;

struct PendingProfile {
    request_id: u64,
    store: Store,
}

#[derive(Default)]
struct ProfileImports {
    next_id: AtomicU64,
    pending: Mutex<Option<PendingProfile>>,
}

/// A `rift://` link opened Rift: what it asks to import waits here until the page is ready to
/// show it (a link that starts Rift arrives before the page exists), later links go straight
/// to the page. Nothing is added by the link itself.
#[derive(Default)]
struct ImportInbox(Mutex<InboxState>);
#[derive(Default)]
struct InboxState {
    /// The page has asked for pending imports and listens for new ones.
    ready: bool,
    pending: Option<link::InputPreview>,
}
impl ImportInbox {
    /// `Some`: the page listens, emit it now. `None`: kept until the page asks (`take`).
    fn offer(&self, preview: link::InputPreview) -> Option<link::InputPreview> {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.ready {
            return Some(preview);
        }
        state.pending = Some(preview);
        None
    }
    /// The page is listening from now on; hands over what arrived before.
    fn take(&self) -> Option<link::InputPreview> {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        state.ready = true;
        state.pending.take()
    }
}

/// The `rift://` link among the arguments of a start (Windows passes the clicked link as one).
fn deep_link_in(arguments: &[String]) -> Option<String> {
    arguments
        .iter()
        .find(|argument| {
            argument
                .get(..7)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("rift://"))
        })
        .cloned()
}

#[derive(Clone, Serialize)]
struct ProfilePreview {
    request_id: u64,
    #[serde(flatten)]
    summary: profile::Summary,
}

impl ProfileImports {
    fn clear(&self) {
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    fn stage(&self, store: Store) -> ProfilePreview {
        let request_id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let preview = ProfilePreview {
            request_id,
            summary: profile::summary(&store),
        };
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(PendingProfile { request_id, store });
        preview
    }

    fn take(&self, request_id: u64) -> Result<PendingProfile, String> {
        let mut slot = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if slot
            .as_ref()
            .is_none_or(|pending| pending.request_id != request_id)
        {
            return Err(t("shell.error.pending_profile_gone").into());
        }
        Ok(slot.take().expect("pending profile checked above"))
    }

    fn restore(&self, pending: PendingProfile) {
        let mut slot = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(pending);
        }
    }
}

#[tauri::command]
fn snapshot(state: tauri::State<'_, Backend>) -> Snapshot {
    state.snapshot()
}
#[tauri::command]
async fn execute(action: Action, state: tauri::State<'_, Backend>) -> Result<Snapshot, String> {
    state.execute(action).await.map_err(|e| e.to_string())
}
/// The running version, for the footer (it must not go stale after an update).
#[tauri::command]
fn app_version(app: tauri::AppHandle) -> String {
    app.package_info().version.to_string()
}
#[tauri::command]
fn preview_server(uri: String) -> Result<ServerView, String> {
    parser::parse_uri(&uri, None)
        .map(|s| s.summary())
        .map_err(|e| e.to_string())
}

/// The latest measurements of one server (up to 20, the oldest first), from memory. Not part of
/// the snapshot; the screen asks when it shows them.
#[tauri::command]
fn server_history(id: String, state: tauri::State<'_, Backend>) -> Vec<history::Sample> {
    state.history(&id)
}

/// The address of a subscription, for the dialog that edits it. This is the only way the address
/// reaches the interface: it is not in the snapshot, journal or diagnostics.
#[tauri::command]
async fn subscription_url(id: String, state: tauri::State<'_, Backend>) -> Result<String, String> {
    state.subscription_url(&id).await.map_err(|e| e.to_string())
}

/// The screen with the speed opened or closed: Rust reads `/traffic` only while it is open.
#[tauri::command]
fn watch_traffic(on: bool, state: tauri::State<'_, Backend>) {
    state.watch_traffic(on);
}

/// The connections page opened or closed: Rust asks sing-box for the list only while it is open.
#[tauri::command]
fn watch_connections(on: bool, state: tauri::State<'_, Backend>) {
    state.watch_connections(on);
}

/// The page is ready for `rift://` links; returns the one that opened Rift before that.
#[tauri::command]
fn take_import(inbox: tauri::State<'_, ImportInbox>) -> Option<link::InputPreview> {
    inbox.take()
}

/// What a pasted text, a QR code or a `rift://` link asks to import, checked like the real
/// import. Nothing is added; the text is neither logged nor kept.
#[tauri::command]
fn preview_input(text: String) -> Result<link::InputPreview, String> {
    link::preview_input(&text).map_err(|e| e.to_string())
}

/// Picks a program for a routing rule (`process:name.exe`). Only the file name reaches React,
/// never the path; a name the rule syntax would refuse is an error. `None`: cancelled.
#[tauri::command]
async fn pick_application(
    app: tauri::AppHandle,
    backend: tauri::State<'_, Backend>,
) -> Result<Option<String>, String> {
    let Some(file) = app
        .dialog()
        .file()
        .set_title(t("shell.dialog.program_title"))
        .add_filter(t("shell.dialog.programs_filter"), &["exe"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let path = chosen_path(&backend, file).await?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .filter(|name| routing::parse_rule(&format!("process:{name}")).is_ok());
    match name {
        Some(name) => Ok(Some(name)),
        None => {
            let error = rift_core::Error::Invalid(program_name_error().into());
            backend.report_failure(&error).await;
            Err(program_name_error().into())
        }
    }
}
fn program_name_error() -> &'static str {
    t("shell.error.program_name")
}

fn no_clipboard_image() -> &'static str {
    t("shell.error.no_clipboard_image")
}

/// The picture on the clipboard as a bitmap file. No picture, or a clipboard that is busy,
/// is the same answer for the user.
#[cfg(windows)]
fn clipboard_image() -> Option<Vec<u8>> {
    clipboard_win::get_clipboard::<Vec<u8>, _>(clipboard_win::formats::Bitmap).ok()
}
#[cfg(not(windows))]
fn clipboard_image() -> Option<Vec<u8>> {
    None
}

/// Picks an image file and reads its QR code. The path stays in Rust; React gets the same
/// preview as for a pasted text and nothing is added. `None`: the dialog was cancelled.
#[tauri::command]
async fn scan_qr_file(
    app: tauri::AppHandle,
    backend: tauri::State<'_, Backend>,
) -> Result<Option<link::InputPreview>, String> {
    let Some(file) = app
        .dialog()
        .file()
        .set_title(t("shell.dialog.qr_title"))
        .add_filter(
            t("shell.dialog.image_filter"),
            &["png", "jpg", "jpeg", "bmp"],
        )
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let path = chosen_path(&backend, file).await?;
    backend
        .scan_qr_file(&path)
        .await
        .map(Some)
        .map_err(|error| error.to_string())
}

/// Reads the QR code of the picture on the clipboard (a screenshot, "Copy image").
#[tauri::command]
async fn scan_qr_clipboard(
    backend: tauri::State<'_, Backend>,
) -> Result<link::InputPreview, String> {
    let picture = tauri::async_runtime::spawn_blocking(clipboard_image)
        .await
        .ok()
        .flatten();
    let Some(bytes) = picture else {
        backend
            .report_failure(&rift_core::Error::Invalid(no_clipboard_image().into()))
            .await;
        return Err(no_clipboard_image().into());
    };
    backend
        .scan_qr_image(bytes)
        .await
        .map_err(|error| error.to_string())
}

fn path_error() -> &'static str {
    t("shell.error.path")
}

/// A path the dialog could not turn into a file system path is reported like any failure.
async fn chosen_path(
    backend: &Backend,
    file: tauri_plugin_dialog::FilePath,
) -> Result<std::path::PathBuf, String> {
    match file.into_path() {
        Ok(path) => Ok(path),
        Err(_) => {
            backend
                .report_failure(&rift_core::Error::Invalid(path_error().into()))
                .await;
            Err(path_error().into())
        }
    }
}

// The core records every export/inspect/import failure in the local journal and error toast;
// the host only turns it into the localized text React shows inline.
#[tauri::command]
async fn export_profile(
    app: tauri::AppHandle,
    backend: tauri::State<'_, Backend>,
) -> Result<bool, String> {
    let bytes = backend
        .export_profile()
        .await
        .map_err(|error| profile::export_error_text(&error))?;
    let Some(file) = app
        .dialog()
        .file()
        .set_title(t("shell.dialog.export_title"))
        .set_file_name("rift-profile.json")
        .add_filter(t("shell.dialog.profile_filter"), &["json"])
        .blocking_save_file()
    else {
        return Ok(false);
    };
    let path = chosen_path(&backend, file).await?;
    backend
        .write_profile(&path, &bytes)
        .await
        .map_err(|error| profile::export_error_text(&error))?;
    Ok(true)
}

#[tauri::command]
async fn inspect_profile(
    app: tauri::AppHandle,
    backend: tauri::State<'_, Backend>,
    imports: tauri::State<'_, ProfileImports>,
) -> Result<Option<ProfilePreview>, String> {
    imports.clear();
    let Some(file) = app
        .dialog()
        .file()
        .set_title(t("shell.dialog.import_title"))
        .add_filter(t("shell.dialog.profile_filter"), &["json"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let path = chosen_path(&backend, file).await?;
    let store = backend
        .inspect_profile(&path)
        .await
        .map_err(|error| profile::inspect_error_text(&error))?;
    Ok(Some(imports.stage(store)))
}

#[tauri::command]
async fn import_profile(
    request_id: u64,
    backend: tauri::State<'_, Backend>,
    imports: tauri::State<'_, ProfileImports>,
) -> Result<Snapshot, String> {
    let pending = imports.take(request_id)?;
    match backend.import_profile(pending.store.clone()).await {
        Ok(snapshot) => Ok(snapshot),
        Err(_) => {
            imports.restore(pending);
            Err(profile::import_failed_text().into())
        }
    }
}
/// Frontend error strings stay local; the application redacts and bounds both before use.
#[tauri::command]
fn log_frontend_error(message: String, detail: Option<String>, backend: tauri::State<'_, Backend>) {
    backend.log_external_error(&message, detail.as_deref());
}

/// Available after initialization, without retaining the application beyond its normal lifetime.
static LOCAL_BACKEND: std::sync::OnceLock<std::sync::Weak<Application>> =
    std::sync::OnceLock::new();

/// Host failures use Application once available, so the visible journal updates too.
fn log_host_error(message: &str, detail: &str) {
    if let Some(backend) = LOCAL_BACKEND.get().and_then(std::sync::Weak::upgrade) {
        backend.log_external_error(message, Some(detail));
    } else {
        let _ = record_fatal_error(message, detail);
    }
}

/// Panic handling must not reacquire Application locks held by the panicking thread.
/// Returns the cleaned detail for stderr, preserving panic output without leaking secrets.
fn record_fatal_error(message: &str, detail: &str) -> String {
    if let Some(backend) = LOCAL_BACKEND.get().and_then(std::sync::Weak::upgrade) {
        return backend.log_panic(message, detail);
    }
    let directory = std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(|parent| parent.join("data")));
    let secrets = directory
        .as_deref()
        .and_then(|directory| {
            rift_core::persistence::load(&directory.join("state.json"))
                .ok()
                .map(|store| Secrets::from_store(&store, directory))
        })
        .unwrap_or_default();
    let message = clean_message(message, &secrets);
    let detail = clean(detail, &secrets);
    if let Some(directory) = directory {
        let _ = journal::record_external(&directory, &message, Some(&detail));
    }
    notifications::error(&message);
    detail
}
/// The frontend shows its first usable screen. Only the performance harness sets
/// RIFT_READY_FILE; the path comes from the environment, never from the frontend.
#[tauri::command]
fn ui_ready(milestones: tauri::State<'_, Arc<Milestones>>) {
    milestones.mark("ui_ready");
    if let Some(path) = std::env::var_os(READY_FILE_ENV) {
        let _ = milestones.write(std::path::Path::new(&path));
    }
}
/// Pushed to the frontend when a release newer than the running version is found.
#[derive(Clone, Serialize)]
struct UpdateInfo {
    version: String,
    notes: Option<String>,
    pub_date: Option<String>,
}
/// `pending` holds the live handle needed to actually install; `dismissed` is the last
/// version the user declined this session (in-memory only, resets on restart).
#[derive(Default)]
struct UpdateState {
    pending: Mutex<Option<Update>>,
    dismissed: Mutex<Option<String>>,
}
/// Checks the endpoint baked in at build time; `None` when unset, never an error.
async fn run_update_check(app: &tauri::AppHandle) -> Result<Option<UpdateInfo>, String> {
    let Some(endpoint) = RIFT_UPDATE_ENDPOINT else {
        return Ok(None);
    };
    let url = Url::parse(endpoint).map_err(|e| e.to_string())?;
    let updater = app
        .updater_builder()
        .endpoints(vec![url])
        .map_err(|e| e.to_string())?
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let state = app.state::<UpdateState>();
    let Some(found) = updater.check().await.map_err(|e| e.to_string())? else {
        *state.pending.lock().unwrap_or_else(PoisonError::into_inner) = None;
        return Ok(None);
    };
    let info = UpdateInfo {
        version: found.version.clone(),
        notes: found.body.clone(),
        pub_date: found.date.map(|date| date.to_string()),
    };
    let dismissed = state
        .dismissed
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let notify = rift_core::update::should_notify(&info.version, dismissed.as_deref());
    *state.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(found);
    // A dismissed version stays dismissed on a manual recheck too, not just for the push event.
    if !notify {
        return Ok(None);
    }
    let _ = app.emit("rift://update", &info);
    Ok(Some(info))
}
#[tauri::command]
async fn check_for_update(
    app: tauri::AppHandle,
    backend: tauri::State<'_, Backend>,
) -> Result<Option<UpdateInfo>, String> {
    run_update_check(&app).await.map_err(|error| {
        backend.log_external_error(t("shell.error.update_check"), Some(&error));
        clean(&error, &Secrets::default())
    })
}
#[tauri::command]
fn dismiss_update(version: String, update: tauri::State<'_, UpdateState>) {
    *update
        .dismissed
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(version);
}
/// Stops the sidecar before handing off to the installer (no orphan child
/// processes) — but only once the update is downloaded and its signature verified, so a
/// network or signature failure (the common case) never touches a running connection.
/// `Application::shutdown()` is one-way (cancels the coordinator's token for good, like the
/// existing `ExitRequested` handler), so once it runs, this command cannot be retried: a
/// `download()` failure restores `pending` for a real retry, but a post-shutdown `install()`
/// failure (rare — e.g. the OS blocking the installer launch) cannot recover the connection,
/// only report it clearly. On Windows, a successful `install` never returns: it launches the
/// NSIS installer (passive mode, auto-relaunch) and exits the process itself.
#[tauri::command]
async fn install_update(
    backend: tauri::State<'_, Backend>,
    update: tauri::State<'_, UpdateState>,
) -> Result<(), String> {
    let result = async {
        let found = update
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(found) = found else {
            return Err(t("shell.error.update_not_found").to_owned());
        };
        // `Update`'s own `timeout` field never reaches here (the plugin's `check()` hardcodes it to
        // `None` on the value it returns, independent of the builder's `.timeout()` below) — bound
        // the download explicitly, or a stalled connection hangs forever behind the install dialog,
        // which disables Escape/close for the same duration.
        let download = tokio::time::timeout(
            Duration::from_secs(300),
            found.download(|_chunk, _total| {}, || {}),
        )
        .await;
        let bytes = match download {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(e)) => {
                // Download/signature failure: nothing was stopped, so the same handle can retry.
                *update
                    .pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(found);
                return Err(e.to_string());
            }
            Err(_) => {
                *update
                    .pending
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(found);
                return Err(t("shell.error.update_timeout").to_owned());
            }
        };
        // Past this point the coordinator is dead either way: `shutdown()` cancels its token
        // unconditionally as its first step, before the fallible sidecar-stop that follows it.
        // Both failure branches below get the same honest message, not just the `install` one.
        let coordinator_dead = |detail: String| tr!("shell.error.stopped", detail = detail);
        backend
            .inner()
            .clone()
            .shutdown()
            .await
            .map_err(|e| coordinator_dead(e.to_string()))?;
        found
            .install(bytes)
            .map_err(|e| coordinator_dead(e.to_string()))
    }
    .await;
    result.map_err(|error: String| {
        backend.log_external_error(t("shell.error.update_install"), Some(&error));
        clean(&error, &Secrets::default())
    })
}
/// A cleaned local error toast in the main window. Hidden windows still retain the journal.
fn show_error(app: &tauri::AppHandle, message: String) {
    let _ = app.emit_to("main", "rift://error", message);
}
/// An information toast: something the user should know that is no failure.
fn show_notice(app: &tauri::AppHandle, message: String) {
    let _ = app.emit_to("main", "rift://notice", message);
}
/// Checks a `rift://` link like a paste and puts the result in front of the user; a link that
/// asks for nothing valid is an error like any other. The link is never logged or kept.
fn open_deep_link(app: &tauri::AppHandle, link_text: String) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let backend = app.state::<Backend>();
        match link::preview_input(&link_text) {
            Ok(preview) => {
                if let Some(ready) = app.state::<ImportInbox>().offer(preview) {
                    let _ = app.emit_to("main", "rift://import", ready);
                }
                show(&app);
            }
            Err(error) => backend.report_failure(&error).await,
        }
    });
}
/// The title of the main window, in the language of the interface.
fn set_window_title(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_title(t("shell.window.title"));
    }
}
fn show(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
/// Fallible backend initialization extracted from setup so errors reach startup_error().
fn initialize_backend(app: &tauri::App) -> Result<Backend, Box<dyn std::error::Error>> {
    let directory = app.path().app_data_dir()?;
    storage::prepare(&directory)?;
    // The updater uses tempfile, not Tauri's directory resolver. Keep its
    // installer staging here too, without changing process environment.
    tempfile::env::override_temp_dir(&directory.join("tmp"))
        .map_err(|_| t("shell.error.temp_dir"))?;
    let legacy = app.path().data_dir()?.join(&app.config().identifier);
    let store = storage::load_or_migrate(&directory, &legacy)?;
    // Only locates the bundled sidecar next to Rift.exe; no trust decision.
    let executable = std::env::current_exe()?; // nosemgrep: rust.lang.security.current-exe.current-exe
    let binary = executable
        .parent()
        .ok_or(t("shell.error.executable_path"))?
        .join(if cfg!(windows) {
            "sing-box.exe"
        } else {
            "sing-box"
        });
    let xray_binary = executable
        .parent()
        .ok_or(t("shell.error.executable_path"))?
        .join(if cfg!(windows) { "xray.exe" } else { "xray" });
    // A TUN mode without administrator rights runs sing-box in a helper that Windows asks the
    // user to allow; everything else, and a Rift that has the rights, runs it directly.
    #[cfg(windows)]
    let runtime: Box<dyn Runtime> = Box::new(ModeRuntime::new(
        Box::new(Sidecar::new(binary, directory.clone()).with_xray_binary(xray_binary)),
        HelperRuntime::new(
            Arc::new(ShellElevator::new(executable.clone())),
            Arc::new(PipeTransport),
        ),
        rift_core::elevation::is_elevated,
    ));
    #[cfg(not(windows))]
    let runtime: Box<dyn Runtime> =
        Box::new(Sidecar::new(binary, directory.clone()).with_xray_binary(xray_binary));
    let backend = Application::new(store, directory.clone(), runtime)?;
    // The `Run` entry of the current user for this very Rift.exe (no administrator rights).
    #[cfg(windows)]
    backend.set_autostart(Arc::new(autostart::RegistryAutostart::new(&executable)?));
    Ok(backend)
}
/// Rift was started by Windows at logon (`autostart::ARG`): the window stays in the tray.
fn launched_by_autostart() -> bool {
    // nosemgrep: rust.lang.security.args.args -- only looks for a flag of this very program; nothing is trusted from it
    std::env::args().any(|argument| argument == autostart::ARG)
}
fn main() {
    // The elevated TUN helper is this same program without anything of the
    // application: no window and no state. It starts first so that nothing else does.
    // nosemgrep: rust.lang.security.args.args -- picks a mode of this same program; the helper checks its own arguments
    let arguments: Vec<String> = std::env::args().collect();
    if arguments.get(1).map(String::as_str) == Some(rift_core::elevation::HELPER_ARG) {
        std::process::exit(rift_core::elevation::run_helper(&arguments[2..]));
    }
    let milestones = Arc::new(Milestones::new());
    milestones.mark("main");
    std::panic::set_hook(Box::new(move |info| {
        let detail = record_fatal_error(t("shell.error.internal"), &info.to_string());
        // Preserve panic output, but never print unredacted credentials or subscription data.
        eprintln!("{detail}");
    }));
    // Core uses Tokio directly, so Tauri and the coordinator share this runtime.
    // i18n-ignore: a panic message for a developer, before any language is chosen
    let runtime = tokio::runtime::Runtime::new().expect("Cannot initialize Rift runtime");
    tauri::async_runtime::set(runtime.handle().clone());
    let _guard = runtime.enter();
    let exiting = Arc::new(AtomicBool::new(false));
    let exit_requested = exiting.clone();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, arguments, _| {
            // A `rift://` link opens its dialog (and raises the window by itself). Windows
            // starting Rift at logon while it already runs must not raise the window.
            if let Some(link_text) = deep_link_in(&arguments) {
                open_deep_link(app, link_text);
            } else if !arguments.iter().any(|argument| argument == autostart::ARG) {
                show(app);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(milestones.clone())
        .manage(UpdateState::default())
        .manage(ProfileImports::default())
        .manage(ImportInbox::default())
        .invoke_handler(tauri::generate_handler![
            snapshot,
            execute,
            app_version,
            preview_server,
            preview_input,
            scan_qr_file,
            scan_qr_clipboard,
            take_import,
            server_history,
            subscription_url,
            watch_traffic,
            watch_connections,
            pick_application,
            export_profile,
            inspect_profile,
            import_profile,
            log_frontend_error,
            ui_ready,
            check_for_update,
            dismiss_update,
            install_update
        ])
        .setup(move |app| {
            milestones.mark("setup_started");
            let mut errors = notifications::subscribe();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tokio::sync::broadcast::error::RecvError;
                loop {
                    match errors.recv().await {
                        Ok(message) => {
                            if let Some(backend) = handle.try_state::<Backend>() {
                                let _ = backend.recover_local_errors();
                            }
                            show_error(&handle, message);
                        }
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => break,
                    }
                }
            });
            let mut notices = notifications::subscribe_notices();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tokio::sync::broadcast::error::RecvError;
                loop {
                    match notices.recv().await {
                        Ok(message) => show_notice(&handle, message),
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => break,
                    }
                }
            });
            let backend = match initialize_backend(app) {
                Ok(backend) => backend,
                Err(e) => {
                    log_host_error(
                        t("shell.error.startup"),
                        &format!(
                            "initialize_backend: {e}; build={}",
                            RIFT_BUILD_COMMIT.unwrap_or("unknown")
                        ),
                    );
                    startup_error();
                    return Err(e);
                }
            };
            let _ = LOCAL_BACKEND.set(Arc::downgrade(&backend));
            app.manage(backend.clone());
            let status = MenuItem::with_id(
                app,
                "status",
                t("shell.tray.status_off"),
                false,
                None::<&str>,
            )?;
            let server = MenuItem::with_id(
                app,
                "server",
                t("shell.tray.server_none"),
                false,
                None::<&str>,
            )?;
            let auto = CheckMenuItem::with_id(
                app,
                "auto",
                t("shell.tray.auto_select"),
                true,
                backend.snapshot().settings.auto_select,
                None::<&str>,
            )?;
            let check =
                MenuItem::with_id(app, "check", t("shell.tray.check_now"), false, None::<&str>)?;
            let connect =
                MenuItem::with_id(app, "connect", t("shell.tray.connect"), true, None::<&str>)?;
            let open = MenuItem::with_id(app, "open", t("shell.tray.open"), true, None::<&str>)?;
            let exit = MenuItem::with_id(app, "exit", t("shell.tray.exit"), true, None::<&str>)?;
            let separator = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(
                app,
                &[
                    &status, &server, &separator, &auto, &check, &connect, &open, &exit,
                ],
            )?;
            let tray = TrayIconBuilder::with_id("rift")
                .icon(
                    app.default_window_icon()
                        .ok_or(t("shell.error.icon_missing"))?
                        .clone(),
                )
                .tooltip(t("shell.tray.tooltip_initial"))
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    if matches!(event, TrayIconEvent::DoubleClick { .. }) {
                        show(tray.app_handle());
                    }
                })
                .on_menu_event(|app, event| {
                    let backend = app.state::<Backend>().inner().clone();
                    let action = match event.id.as_ref() {
                        "open" => {
                            show(app);
                            None
                        }
                        "exit" => {
                            app.exit(0);
                            None
                        }
                        "auto" => Some(Action::SetAuto {
                            enabled: !backend.snapshot().settings.auto_select,
                        }),
                        "check" => Some(Action::CheckNow),
                        "connect" => Some(if backend.snapshot().runtime_active {
                            Action::Disconnect
                        } else {
                            Action::Connect
                        }),
                        _ => None,
                    };
                    if let Some(action) = action {
                        tauri::async_runtime::spawn(async move {
                            let _ = backend.execute(action).await;
                        });
                    }
                })
                .build(app)?;
            set_window_title(app.handle());
            let handle = app.handle().clone();
            let mut receiver = backend.snapshots.subscribe();
            // The language the menu is written in; a change of the setting writes it again.
            let mut menu_language = rift_core::text::language();
            tauri::async_runtime::spawn(async move {
                while receiver.changed().await.is_ok() {
                    let state = receiver.borrow_and_update().clone();
                    if menu_language != rift_core::text::language() {
                        menu_language = rift_core::text::language();
                        let _ = auto.set_text(t("shell.tray.auto_select"));
                        let _ = check.set_text(t("shell.tray.check_now"));
                        let _ = open.set_text(t("shell.tray.open"));
                        let _ = exit.set_text(t("shell.tray.exit"));
                        set_window_title(&handle);
                    }
                    let text = match state.status.as_str() {
                        "connecting" => t("shell.tray.status_connecting"),
                        "connected" => t("shell.tray.status_connected"),
                        "lost" => t("shell.tray.status_lost"),
                        "error" => t("shell.tray.status_error"),
                        _ => t("shell.tray.status_off"),
                    };
                    let _ = status.set_text(text);
                    let selected = state
                        .servers
                        .iter()
                        .find(|s| Some(&s.id) == state.selected.as_ref())
                        .map(|s| s.name.as_str())
                        .unwrap_or(t("shell.tray.server_none"));
                    let _ = server.set_text(selected);
                    let _ = auto.set_checked(state.settings.auto_select);
                    let _ = check.set_enabled(state.runtime_active);
                    let _ = connect.set_text(if state.runtime_active {
                        t("shell.tray.disconnect")
                    } else {
                        t("shell.tray.connect")
                    });
                    let _ = tray.set_tooltip(Some(tr!("shell.tray.tooltip", status = text)));
                    let _ = handle.emit("rift://state", &state);
                }
            });
            // The speed has its own channel: it changes every second and only one screen shows it.
            let handle = app.handle().clone();
            let mut speeds = backend.traffic();
            tauri::async_runtime::spawn(async move {
                while speeds.changed().await.is_ok() {
                    let speed = *speeds.borrow_and_update();
                    let _ = handle.emit("rift://traffic", speed);
                }
            });
            // The connections of the open page; they are not kept anywhere else.
            let handle = app.handle().clone();
            let mut lists = backend.connections();
            tauri::async_runtime::spawn(async move {
                while lists.changed().await.is_ok() {
                    let list = lists.borrow_and_update().clone();
                    let _ = handle.emit("rift://connections", list);
                }
            });
            // Independent of the proxy-selection coordinator:
            // auto-update has nothing to do with sidecar/selection lifecycle.
            if RIFT_UPDATE_ENDPOINT.is_some() {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    // Periodic interval, spread by up to 10 s like every other schedule of Rift.
                    let jitter = Jitter::random();
                    tokio::time::sleep(jitter.spread(Duration::from_secs(60))).await;
                    // Keep the first failure of each streak in the local journal, without
                    // flooding the user on every scheduled retry.
                    let mut failing = false;
                    loop {
                        let result = run_update_check(&handle).await;
                        update_schedule::record_scheduled_check(
                            handle.state::<Backend>().as_ref(),
                            &mut failing,
                            result.as_ref().err().map(String::as_str),
                        );
                        let next = jitter.spread(Duration::from_secs(6 * 60 * 60));
                        tokio::time::sleep(next).await;
                    }
                });
            }
            backend.start();
            // The window is created hidden (tauri.conf.json): only a start by Windows keeps it so.
            if !launched_by_autostart() {
                show(app.handle());
            }
            let launch = backend.clone();
            tauri::async_runtime::spawn(async move { launch.auto_connect().await });
            // Rift was started by a `rift://` link: its dialog waits for the page.
            // nosemgrep: rust.lang.security.args.args -- a rift:// link is checked by `deep_link_in` before anything uses it
            let arguments: Vec<String> = std::env::args().collect();
            if let Some(link_text) = deep_link_in(&arguments) {
                open_deep_link(app.handle(), link_text);
            }
            milestones.mark("setup_done");
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the main window hides it to the tray.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(error) => {
            log_host_error(
                t("shell.error.startup"),
                &format!(
                    "build_app: {error}; build={}",
                    RIFT_BUILD_COMMIT.unwrap_or("unknown")
                ),
            );
            startup_error();
            return;
        }
    };
    app.run(move |app, event| {
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            if !exit_requested.load(Ordering::SeqCst) {
                api.prevent_exit();
                let backend = app.state::<Backend>().inner().clone();
                let handle = app.clone();
                let flag = exit_requested.clone();
                tauri::async_runtime::spawn(async move {
                    let _ = backend.shutdown().await;
                    flag.store(true, Ordering::SeqCst);
                    handle.exit(0);
                });
            }
        }
    });
}

#[allow(unsafe_code)] // Audited MessageBoxW FFI; the block documents its SAFETY contract.
fn startup_error() {
    let message = t("shell.startup.message");
    #[cfg(windows)]
    {
        let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
        let title: Vec<u16> = t("shell.startup.title")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: NUL-terminated strings remain alive for the synchronous dialog call.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage -- audited Win32 FFI, see SAFETY
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
            );
        }
    }
    #[cfg(not(windows))]
    eprintln!("{message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(list: &[&str]) -> Vec<String> {
        list.iter().map(|argument| argument.to_string()).collect()
    }
    #[test]
    fn the_link_that_opened_rift_is_found_among_its_arguments() {
        let link = "rift://add?url=https%3A%2F%2Fpanel.example%2Fsub";
        assert_eq!(
            deep_link_in(&arguments(&["C:\\Rift\\Rift.exe", link])).as_deref(),
            Some(link)
        );
        assert_eq!(
            deep_link_in(&arguments(&["Rift.exe", "RIFT://add?uri=x"])).as_deref(),
            Some("RIFT://add?uri=x")
        );
        for none in [
            vec![],
            vec!["Rift.exe"],
            vec!["Rift.exe", "--autostart"],
            vec!["Rift.exe", "https://panel.example/sub", "vless://x"],
            vec!["Rift.exe", "rift:/add", "rift"],
            vec!["Rift.exe", "–rift://add"],
        ] {
            assert_eq!(deep_link_in(&arguments(&none)), None, "{none:?}");
        }
    }

    fn preview() -> link::InputPreview {
        link::InputPreview::Subscription {
            url: "https://panel.example/sub".into(),
        }
    }
    #[test]
    fn a_link_waits_for_the_page_and_later_ones_go_straight_to_it() {
        let inbox = ImportInbox::default();
        // The link that started Rift arrives before the page exists.
        assert!(inbox.offer(preview()).is_none());
        assert!(matches!(
            inbox.take(),
            Some(link::InputPreview::Subscription { url }) if url == "https://panel.example/sub"
        ));
        // Handed over once; the page listens from now on.
        assert!(inbox.take().is_none());
        assert!(inbox.offer(preview()).is_some());
        assert!(
            inbox.take().is_none(),
            "an emitted link is not kept as well"
        );
    }

    #[test]
    fn only_the_latest_link_waits() {
        let inbox = ImportInbox::default();
        assert!(inbox
            .offer(link::InputPreview::Subscription {
                url: "https://first.example/s".into()
            })
            .is_none());
        assert!(inbox.offer(preview()).is_none());
        assert!(matches!(
            inbox.take(),
            Some(link::InputPreview::Subscription { url }) if url == "https://panel.example/sub"
        ));
    }

    #[test]
    fn profile_preview_is_opaque_and_pending_request_is_single_use() {
        let imports = ProfileImports::default();
        let mut store = Store::default();
        store.subscriptions.push(rift_core::model::Subscription {
            id: "main".into(),
            name: "Основная".into(),
            url: "https://example.com/list?token=secret".into(),
            updated_at: None,
            error: None,
            failure_code: None,
            ..Default::default()
        });
        let preview = imports.stage(store);
        let json = serde_json::to_string(&preview).unwrap();
        assert!(!json.contains("secret"));
        assert!(!json.contains("example.com"));
        assert_eq!(preview.summary.subscriptions, 1);

        assert!(imports.take(preview.request_id + 1).is_err());
        let pending = match imports.take(preview.request_id) {
            Ok(pending) => pending,
            Err(error) => panic!("pending profile missing: {error}"),
        };
        assert!(imports.take(preview.request_id).is_err());
        imports.restore(pending);
        assert!(imports.take(preview.request_id).is_ok());
    }

    #[test]
    fn the_release_build_has_no_developer_tools() {
        // Tauri turns the inspector (F12, "Inspect") on in every build with this feature.
        let manifest = include_str!("../Cargo.toml");
        let tauri = manifest
            .lines()
            .find(|line| line.starts_with("tauri = "))
            .expect("the tauri dependency");
        assert!(!tauri.contains("devtools"), "{tauri}");
    }
}
