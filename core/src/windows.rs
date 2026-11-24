//! Windows handles are confined here; no process-name enumeration or global termination.
use crate::text::t;
use crate::{Error, Result};
use std::{
    ffi::OsStr,
    io::{Error as IoError, ErrorKind},
    mem::{size_of, zeroed},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY},
    System::{
        Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE},
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        Threading::{
            GetCurrentProcess, GetProcessIdOfThread, OpenProcessToken, OpenThread, ResumeThread,
            CREATE_SUSPENDED, THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
        },
    },
    UI::Shell::{ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW},
};

/// Creation flags of the sing-box child: no console window, and not running until it is in
/// the Job Object ([`ChildJob::attach`], then [`resume`]). No shell is ever involved.
pub const SUSPENDED_NO_WINDOW: u32 = CREATE_SUSPENDED | 0x0800_0000; // | CREATE_NO_WINDOW

pub fn require_elevation() -> Result<()> {
    // SAFETY: all buffers use the exact API layout and size; owned token is closed once.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage -- audited Win32 FFI, see SAFETY
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut elevation: TOKEN_ELEVATION = zeroed();
        let mut size = 0;
        let success = GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        );
        CloseHandle(token);
        if success == 0 || elevation.TokenIsElevated == 0 {
            return Err(Error::Invalid(t("core.windows.admin_required").into()));
        }
    }
    Ok(())
}

pub struct ChildJob(OwnedHandle);
impl ChildJob {
    pub fn attach(child: &tokio::process::Child) -> Result<Self> {
        // SAFETY: the live child owns the process handle; the job gets a unique owned handle.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage -- audited Win32 FFI, see SAFETY
        unsafe {
            let job = CreateJobObjectW(null(), null());
            if job.is_null() {
                return Err(std::io::Error::last_os_error().into());
            }
            let owned = Self(OwnedHandle::from_raw_handle(job));
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            let process = child
                .raw_handle()
                .ok_or_else(|| Error::Invalid(t("core.windows.singbox_exited").into()))?;
            if AssignProcessToJobObject(owned.0.as_raw_handle(), process) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(owned)
        }
    }
}

/// Starts a child created with [`SUSPENDED_NO_WINDOW`] once it is in its Job Object, so it
/// never runs outside the job. Only threads of this child's PID are touched; the
/// PID cannot be reused while `child` holds its process handle. Unlike the process, a thread
/// ID is recycled the moment its thread ends, so ownership is re-checked right before acting
/// on it, narrowing (not eliminating) the gap between the snapshot and `ResumeThread`.
pub fn resume(child: &tokio::process::Child) -> Result<()> {
    let pid = child
        .id()
        .ok_or_else(|| Error::Invalid(t("core.windows.singbox_exited").into()))?;
    // SAFETY: THREADENTRY32 is zero-initialised with its size set as the API requires; every
    // handle is owned and closed once; ResumeThread gets a handle opened for that right.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage -- audited Win32 FFI, see SAFETY
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().into());
        }
        let snapshot = OwnedHandle::from_raw_handle(snapshot);
        let mut entry: THREADENTRY32 = zeroed();
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        let mut resumed = 0;
        let mut more = Thread32First(snapshot.as_raw_handle(), &mut entry) != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                let thread = OpenThread(
                    THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
                    0,
                    entry.th32ThreadID,
                );
                if thread.is_null() {
                    return Err(std::io::Error::last_os_error().into());
                }
                let thread = OwnedHandle::from_raw_handle(thread);
                // The snapshot can already be stale: if this TID was freed and reissued to an
                // unrelated thread between the snapshot and OpenThread above, its owner no
                // longer matches and it must not be resumed.
                if GetProcessIdOfThread(thread.as_raw_handle()) == pid {
                    if ResumeThread(thread.as_raw_handle()) == u32::MAX {
                        return Err(std::io::Error::last_os_error().into());
                    }
                    resumed += 1;
                }
            }
            more = Thread32Next(snapshot.as_raw_handle(), &mut entry) != 0;
        }
        if resumed == 0 {
            return Err(Error::Invalid(
                t("core.windows.singbox_start_failed").into(),
            ));
        }
    }
    Ok(())
}

/// The shell does not show a window for this program (`SW_HIDE`).
const SHOW_HIDDEN: i32 = 0;

/// Text for the shell: UTF-16 with its end mark. A NUL inside would end the text early, so it is
/// refused.
fn wide(text: &OsStr) -> std::io::Result<Vec<u16>> {
    let mut units: Vec<u16> = text.encode_wide().collect();
    if units.contains(&0) {
        return Err(IoError::new(
            ErrorKind::InvalidInput,
            // i18n-ignore: an internal error of a bad argument, not shown to a person
            "NUL in a shell argument",
        ));
    }
    units.push(0);
    Ok(units)
}

/// Starts `file` with `parameters` through the Windows shell (`ShellExecuteExW`) under `verb`:
/// `runas` makes Windows ask the user for administrator rights, and the program then runs with
/// them. Returns when the shell has answered, which with `runas` is after the user has: waiting
/// for the prompt is the whole cost of the call, so callers keep it off their async threads.
/// The error carries the OS error number (`ERROR_CANCELLED`, 1223, is a "No" at the prompt). The
/// caller passes only what it has checked itself: this function quotes nothing.
pub fn shell_execute(
    verb: &str,
    file: &Path,
    parameters: &str,
    directory: Option<&Path>,
) -> std::io::Result<()> {
    let verb = wide(OsStr::new(verb))?;
    let file = wide(file.as_os_str())?;
    let parameters = wide(OsStr::new(parameters))?;
    let directory = directory.map(|path| wide(path.as_os_str())).transpose()?;
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        // No window of the shell for its own errors; the call returns when it is done (the
        // thread that calls it may end right after).
        fMask: SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: parameters.as_ptr(),
        lpDirectory: directory.as_ref().map_or(null(), |path| path.as_ptr()),
        nShow: SHOW_HIDDEN,
        ..SHELLEXECUTEINFOW::default()
    };
    // SAFETY: `info` has its size set and every text pointer in it points into a NUL-terminated
    // buffer (`verb`, `file`, `parameters`, `directory`) that lives until the end of this
    // function; the call does not keep them. All other fields are zero, which the API takes as
    // "not used". COM is started for this thread in the apartment the Shell extensions ask for
    // and left again only if this call started it (a thread already in another mode fails the
    // start, and is then used as it is). The OS error is read before COM is left.
    // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage -- audited Win32 FFI, see SAFETY
    unsafe {
        let com = CoInitializeEx(
            null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        ) >= 0;
        let started = ShellExecuteExW(&mut info) != 0;
        let error = IoError::last_os_error();
        if com {
            CoUninitialize();
        }
        if !started {
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use windows_sys::Win32::System::JobObjects::IsProcessInJob;

    fn cmd_exe() -> std::path::PathBuf {
        let root = std::env::var_os("SystemRoot").expect("SystemRoot");
        Path::new(&root).join("System32").join("cmd.exe")
    }

    /// The call itself, with the verb that needs no prompt: the structure, the strings and the
    /// start of COM are what is checked. The prompt (`runas`) cannot be answered by a test.
    #[test]
    fn the_shell_starts_a_program_with_its_arguments() {
        shell_execute("open", &cmd_exe(), "/c exit 0", None).unwrap();
    }

    #[test]
    fn the_shell_starts_a_program_in_the_directory_given() {
        let system = cmd_exe().parent().unwrap().to_path_buf();
        shell_execute("open", &cmd_exe(), "/c exit 0", Some(&system)).unwrap();
    }

    #[test]
    fn a_missing_program_is_an_error_with_an_os_code() {
        let missing = Path::new(r"C:\rift-no-such-directory\no-such-program.exe");
        let error = shell_execute("open", missing, "", None).unwrap_err();
        assert!(error.raw_os_error().is_some(), "{error:?}");
        assert_ne!(error.raw_os_error(), Some(1223), "not a refusal: {error:?}");
    }

    #[test]
    fn text_with_a_nul_never_reaches_the_shell() {
        let error = shell_execute("open", &cmd_exe(), "a\0b", None).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        let error = shell_execute("open", Path::new("a\0b"), "", None).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
    }

    fn suspended(args: &[&str]) -> tokio::process::Child {
        let mut cmd = tokio::process::Command::new("cmd.exe");
        cmd.args(args)
            .creation_flags(SUSPENDED_NO_WINDOW)
            .kill_on_drop(true);
        cmd.spawn().unwrap()
    }

    fn in_job(job: &ChildJob, child: &tokio::process::Child) -> bool {
        let mut result = 0;
        // SAFETY: both handles are alive for the call.
        // nosemgrep: rust.lang.security.unsafe-usage.unsafe-usage -- test-only Win32 FFI
        let ok = unsafe {
            IsProcessInJob(
                child.raw_handle().unwrap(),
                job.0.as_raw_handle(),
                &mut result,
            )
        };
        ok != 0 && result != 0
    }

    #[tokio::test]
    async fn suspended_child_runs_only_after_it_is_in_the_job() {
        let mut child = suspended(&["/c", "exit", "7"]);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(child.try_wait().unwrap().is_none(), "suspended child ran");
        let job = ChildJob::attach(&child).unwrap();
        assert!(in_job(&job, &child));
        resume(&child).unwrap();
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .expect("resumed child did not finish")
            .unwrap();
        assert_eq!(status.code(), Some(7));
    }

    #[tokio::test]
    async fn closing_the_job_ends_the_child_even_before_it_ran() {
        let mut child = suspended(&["/c", "ping", "-n", "30", "127.0.0.1"]);
        let job = ChildJob::attach(&child).unwrap();
        drop(job);
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .expect("KILL_ON_JOB_CLOSE did not end the child");
        assert!(status.is_ok());
    }

    #[tokio::test]
    async fn resume_of_a_finished_child_fails() {
        let mut child = suspended(&["/c", "exit", "0"]);
        let _job = ChildJob::attach(&child).unwrap();
        resume(&child).unwrap();
        child.wait().await.unwrap();
        assert!(resume(&child).is_err());
    }
}
