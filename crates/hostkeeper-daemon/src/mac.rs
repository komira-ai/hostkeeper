//! The real adapters of a Mac's package checks and enrollment claims (design section 5.9, rows
//! Package verification and Enrollment), behind the ports their libraries own:
//!
//! - [`HostMacVerify`], `hostkeeper_install`'s [`MacVerify`]: the quarantine attribute of a staged
//!   entry, read with `getxattr` without following a link, and the code signature of a Mach-O
//!   file, judged by `codesign --verify --strict`;
//! - [`HostIoreg`], `hostkeeper_enroll`'s [`Ioreg`]: the `IOPlatformExpertDevice` entry of
//!   `ioreg`, which `parse_ioreg` reads the serial and the platform UUID from.
//!
//! Each tool runs through the process port the launchd adapters use ([`Launchctl`], whose real
//! side [`LaunchctlProcess`] runs any program): a fixed argument list, no shell, an empty
//! environment, at most 64 KiB of each stream, and a deadline after which the tool is killed.
//! A signature is valid by `codesign --verify --strict`'s own rule: an ad-hoc signature or one of
//! any identity, over the binary's own bytes; which identities are trusted is the attestation's
//! business (design 5.5 step 2), not this check's. Only the constant of an absent attribute
//! differs by system (`ENOATTR` on macOS, `ENODATA` on Linux); every other line is compiled and
//! measured on Linux, the attribute read being `rustix`'s and the tools processes, against the
//! fake port and a scratch file.

use std::io;
use std::path::Path;
use std::time::Duration;

use hostkeeper_enroll::Ioreg;
use hostkeeper_install::mac_verify::{MacVerify, SignatureVerdict, VerifyIoError};
use hostkeeper_launchd::{Launchctl, LaunchctlError, LaunchctlOutput, LaunchctlProcess};
use rustix::io::Errno;

/// The attribute macOS sets on what came from the network, and a tool that unpacks it can
/// propagate to its output.
pub const QUARANTINE_ATTRIBUTE: &str = "com.apple.quarantine";
/// The tool that checks a code signature.
pub const CODESIGN: &str = "/usr/bin/codesign";
/// The tool that prints the I/O registry.
pub const IOREG: &str = "/usr/sbin/ioreg";
/// How long `codesign` or `ioreg` may run before it is killed and the check is not made.
pub const TOOL_DEADLINE: Duration = Duration::from_secs(30);
/// What `codesign` writes for a file that has no signature at all.
pub const NOT_SIGNED: &str = "code object is not signed at all";
/// The name the attribute read is reported under.
const GETXATTR: &str = "getxattr";
/// The name the signature check is reported under.
const CODESIGN_TOOL: &str = "codesign";

/// The error `getxattr` gives for an attribute a file does not have: `ENOATTR` on macOS,
/// `ENODATA` (its Linux spelling) elsewhere.
#[cfg(target_os = "macos")]
const NO_ATTRIBUTE: Errno = Errno::NOATTR;
#[cfg(not(target_os = "macos"))]
const NO_ATTRIBUTE: Errno = Errno::NODATA;

/// The arguments of `codesign` for `binary`.
pub fn codesign_args(binary: &str) -> Vec<String> {
    vec![
        "--verify".to_owned(),
        "--strict".to_owned(),
        binary.to_owned(),
    ]
}

/// The arguments of `ioreg` for the platform expert device's own properties.
pub fn ioreg_args() -> Vec<String> {
    vec![
        "-rd1".to_owned(),
        "-c".to_owned(),
        "IOPlatformExpertDevice".to_owned(),
    ]
}

/// Whether `path` itself (a link is not followed) carries the attribute `name`. An attribute
/// that cannot be read for another reason than its absence is an error: the check was not made.
pub fn has_attribute(path: &Path, name: &str) -> Result<bool, VerifyIoError> {
    let mut nothing: [u8; 0] = [];
    match rustix::fs::lgetxattr(path, name, &mut nothing[..]) {
        Ok(_) => Ok(true),
        Err(errno) if errno == NO_ATTRIBUTE => Ok(false),
        Err(errno) => Err(VerifyIoError::Spawn {
            tool: GETXATTR,
            source: io::Error::from(errno),
        }),
    }
}

/// A tool that did not run to an exit, as the check reports it.
fn not_run(tool: &'static str, error: LaunchctlError) -> VerifyIoError {
    match error {
        LaunchctlError::Spawn { why, .. } => VerifyIoError::Spawn {
            tool,
            source: io::Error::other(why),
        },
        LaunchctlError::TimedOut { .. } => VerifyIoError::Unfinished {
            tool,
            why: format!(
                "killed at its deadline of {} seconds",
                TOOL_DEADLINE.as_secs()
            ),
        },
    }
}

/// What `codesign --verify --strict` said, as a verdict: exit 0 is a valid signature; exit 1 (a
/// failed verification) or 3 (code or signature modified) is a refusal, `Unsigned` when it says
/// the file is [`NOT_SIGNED`] and `Invalid` with its words otherwise; any other end (a usage
/// error, a signal) means the check was not made.
pub fn codesign_verdict(output: &LaunchctlOutput) -> Result<SignatureVerdict, VerifyIoError> {
    let said = output.stderr_text().trim().to_owned();
    match output.code {
        Some(0) => Ok(SignatureVerdict::Valid),
        Some(1 | 3) if said.contains(NOT_SIGNED) => Ok(SignatureVerdict::Unsigned),
        Some(1 | 3) => Ok(SignatureVerdict::Invalid { note: said }),
        code => Err(VerifyIoError::Unfinished {
            tool: CODESIGN_TOOL,
            why: format!("it ended with {code:?}: {said}"),
        }),
    }
}

/// The Mac's [`MacVerify`]: the quarantine attribute by `getxattr`, the signature by `codesign`
/// run through `codesign`, a [`Launchctl`] port.
#[derive(Debug)]
pub struct HostMacVerify<L> {
    codesign: L,
    attribute: String,
}

impl HostMacVerify<LaunchctlProcess> {
    /// The host's: [`CODESIGN`] with [`TOOL_DEADLINE`], and [`QUARANTINE_ATTRIBUTE`].
    pub fn system() -> HostMacVerify<LaunchctlProcess> {
        HostMacVerify::new(
            LaunchctlProcess::new(Path::new(CODESIGN), TOOL_DEADLINE),
            QUARANTINE_ATTRIBUTE,
        )
    }
}

impl<L: Launchctl> HostMacVerify<L> {
    /// A verifier that runs `codesign` (given the arguments of [`codesign_args`]) and reads the
    /// attribute `attribute` as the quarantine mark.
    pub fn new(codesign: L, attribute: &str) -> HostMacVerify<L> {
        HostMacVerify {
            codesign,
            attribute: attribute.to_owned(),
        }
    }
}

impl<L: Launchctl> MacVerify for HostMacVerify<L> {
    fn quarantine(&self, path: &Path) -> Result<bool, VerifyIoError> {
        has_attribute(path, &self.attribute)
    }

    fn signature(&self, binary: &Path) -> Result<SignatureVerdict, VerifyIoError> {
        let Some(text) = binary.to_str() else {
            return Err(VerifyIoError::Unfinished {
                tool: CODESIGN_TOOL,
                why: format!("the path {} is not UTF-8", binary.display()),
            });
        };
        let output = self
            .codesign
            .run(&codesign_args(text))
            .map_err(|error| not_run(CODESIGN_TOOL, error))?;
        codesign_verdict(&output)
    }
}

/// The Mac's [`Ioreg`]: `ioreg -rd1 -c IOPlatformExpertDevice`, run through `ioreg`, a
/// [`Launchctl`] port. Anything but exit 0 with UTF-8 output is an error, which stops the
/// enrollment before a request is made.
#[derive(Debug)]
pub struct HostIoreg<L> {
    ioreg: L,
}

impl HostIoreg<LaunchctlProcess> {
    /// The host's: [`IOREG`] with [`TOOL_DEADLINE`].
    pub fn system() -> HostIoreg<LaunchctlProcess> {
        HostIoreg::new(LaunchctlProcess::new(Path::new(IOREG), TOOL_DEADLINE))
    }
}

impl<L: Launchctl> HostIoreg<L> {
    /// An `ioreg` run through `ioreg`.
    pub fn new(ioreg: L) -> HostIoreg<L> {
        HostIoreg { ioreg }
    }
}

impl<L: Launchctl> Ioreg for HostIoreg<L> {
    fn platform_expert(&self) -> io::Result<String> {
        let output = self
            .ioreg
            .run(&ioreg_args())
            .map_err(|error| io::Error::other(not_run("ioreg", error).to_string()))?;
        if output.code != Some(0) {
            return Err(io::Error::other(format!(
                "ioreg ended with {:?}: {}",
                output.code,
                output.stderr_text().trim()
            )));
        }
        String::from_utf8(output.stdout).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "ioreg wrote output that is not UTF-8",
            )
        })
    }
}
