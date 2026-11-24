//! TUN without restarting Rift as administrator: a short-lived elevated helper that
//! only runs sing-box, reached over one channel, asked for with UAC.
pub mod client;
pub mod helper;
pub mod mode;
pub mod protocol;
pub mod uac;
#[cfg(windows)]
pub mod windows;

pub use uac::HELPER_ARG;

/// Whether this Rift has administrator rights itself.
#[cfg(windows)]
pub use windows::is_elevated;
/// `rift.exe --tun-helper …`: the elevated helper's process; returns its exit code. Only Windows
/// has one.
#[cfg(windows)]
pub use windows::run_helper;
#[cfg(not(windows))]
pub fn run_helper(_args: &[String]) -> i32 {
    1
}
