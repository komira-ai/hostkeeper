//! The boot id of a Mac: the kernel's `kern.bootsessionuuid`, a UUID made anew at each boot.
//! It is the step runner's [`BootIdSource`] on a Mac, the way the kernel's `boot_id` file is on
//! Linux. The reading of the sysctl's text is [`parse_boot_session_uuid`], measured on Linux;
//! only [`SysctlBootId`], the call itself, is compiled on macOS alone.

#[cfg(target_os = "macos")]
use hostkeeper_steps::BootIdSource;
use hostkeeper_steps::{BootId, BootIdError};

/// The sysctl of the Mac's boot id.
pub const BOOT_SESSION_UUID: &str = "kern.bootsessionuuid";

/// The boot id in `text`, the value of [`BOOT_SESSION_UUID`]: trimmed of white space (a line
/// end, as `sysctl -n` prints it), and refused when it is empty or not a UUID.
pub fn parse_boot_session_uuid(text: &str) -> Result<BootId, BootIdError> {
    BootId::uuid(BOOT_SESSION_UUID, text)
}

/// [`BOOT_SESSION_UUID`] read from the kernel with `sysctlbyname`, through the `sysctl` crate's
/// safe interface; compiled on macOS only.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Default)]
pub struct SysctlBootId;

#[cfg(target_os = "macos")]
impl BootIdSource for SysctlBootId {
    fn read(&self) -> Result<BootId, BootIdError> {
        use sysctl::Sysctl;
        let read = |error: sysctl::SysctlError| BootIdError::Read {
            from: BOOT_SESSION_UUID,
            source: std::io::Error::other(error),
        };
        let text = sysctl::Ctl::new(BOOT_SESSION_UUID)
            .and_then(|ctl| ctl.value_string())
            .map_err(read)?;
        parse_boot_session_uuid(&text)
    }
}
