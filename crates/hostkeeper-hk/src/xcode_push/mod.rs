//! The Mac's programs and the steps `hk xcode add` ([`crate::xcode_add`]) and `hk xcode verify`
//! ([`crate::xcode_verify`]) share, each ending the run before anything is uploaded when it
//! fails:
//!
//! - [`archive`]: the archive's SHA-256 and size, and its version and build, read from the app
//!   that `xip --expand` makes in a directory beside the archive, which `codesign --verify
//!   --deep --strict` must pass and which is removed afterwards;
//! - [`signature`]: Apple's signature, `pkgutil --check-signature` must exit 0, say the status
//!   `signed Apple Software` and name a chain that ends at Apple Root CA, by its SHA-256;
//! - [`upload`]: the archive, unmodified and streamed, with the operator's token in the request's
//!   header and nowhere else; it resumes after a cut ([`upload::put`]).
//!
//! The programs are the [`Mac`]'s. [`target`] reads the coordinator's address, the CA and the token
//! before anything else, so that a mistake in any ends the run at once. The token is never an
//! argument, an environment variable or an output line of a program `hk` runs.
//!
//! Off macOS none of the programs is run: only [`upload`] and [`target`] are used there, and a
//! farm Mac does the checking of [`archive`] and [`signature`] ([`crate::xcode_farm`]).

pub(crate) mod archive;
mod resume;
pub(crate) mod signature;
#[cfg(test)]
mod tests;
pub(crate) mod upload;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::args::Globals;
use crate::body::{read_file, read_path};
use crate::call::{is_endpoint, private};
use crate::encode::visible;
use crate::send::{agent, certificates};
use crate::{Flow, PROGRAM, Stop, World};

/// The system `hk xcode add` and `hk xcode verify` run on, the programs of the Mac they run, and
/// the two things `verify` reads from outside: Apple's root certificate and a clock.
#[derive(Debug, Clone)]
pub struct Mac {
    /// The operating system, as `std::env::consts::OS` names it: both run the programs below only
    /// on `macos`; anywhere else a farm Mac checks the archive.
    pub os: &'static str,
    /// `xcodes`, which downloads an Xcode in the operator's session.
    pub xcodes: PathBuf,
    /// `pkgutil`, which checks the archive's signature.
    pub pkgutil: PathBuf,
    /// `xip`, which expands the archive.
    pub xip: PathBuf,
    /// `plutil`, which reads the version and the build of the expanded app.
    pub plutil: PathBuf,
    /// `codesign`, which verifies the expanded app.
    pub codesign: PathBuf,
    /// `spctl`, which assesses the expanded app against Gatekeeper's policy.
    pub spctl: PathBuf,
    /// `xar`, which lists and extracts the members of an archive.
    pub xar: PathBuf,
    /// `sw_vers`, which names the macOS version.
    pub sw_vers: PathBuf,
    /// `uname`, which names the kernel and the machine.
    pub uname: PathBuf,
    /// `cp`, which makes the copy-on-write clone of an archive (`-c`).
    pub cp: PathBuf,
    /// `df`, which says how much space a volume has free.
    pub df: PathBuf,
    /// `archiveutil`, which Apple's other archive formats are handled with; it may be absent.
    pub archiveutil: PathBuf,
    /// `aa`, the Apple Archive tool; it may be absent.
    pub aa: PathBuf,
    /// `aea`, the Apple Encrypted Archive tool; it may be absent.
    pub aea: PathBuf,
    /// The directory of the Command Line Tools' packages, whose `.pkg` is a package Apple did not
    /// sign as Apple Software.
    pub command_line_tools: PathBuf,
    /// Where Apple's root certificate is fetched from, by HTTPS.
    pub apple_root_url: String,
    /// The SHA-256 fingerprint (upper-case hex) that certificate must have: Apple's own, as
    /// `signature::APPLE_ROOT_CA_SHA256` holds it. A field so that a test can serve a root of its
    /// own making.
    pub apple_root_sha256: String,
    /// The time since an origin that does not change in the run: the seconds `xip --expand` took
    /// are the difference of two readings.
    pub clock: fn() -> Duration,
}

/// The reading of the system's monotonic clock.
pub(crate) fn monotonic() -> Duration {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    Duration::new(
        u64::try_from(now.tv_sec).unwrap_or_default(),
        u32::try_from(now.tv_nsec).unwrap_or_default(),
    )
}

impl Mac {
    /// The system this program runs on, `xcodes` from the `PATH` (it is not Apple's), Apple's
    /// programs by their paths, Apple's root certificate from Apple's certificate authority page,
    /// and the system's monotonic clock.
    pub fn system() -> Mac {
        Mac {
            os: std::env::consts::OS,
            xcodes: PathBuf::from("xcodes"),
            pkgutil: PathBuf::from("/usr/sbin/pkgutil"),
            xip: PathBuf::from("/usr/bin/xip"),
            plutil: PathBuf::from("/usr/bin/plutil"),
            codesign: PathBuf::from("/usr/bin/codesign"),
            spctl: PathBuf::from("/usr/sbin/spctl"),
            xar: PathBuf::from("/usr/bin/xar"),
            sw_vers: PathBuf::from("/usr/bin/sw_vers"),
            uname: PathBuf::from("/usr/bin/uname"),
            cp: PathBuf::from("/bin/cp"),
            df: PathBuf::from("/bin/df"),
            archiveutil: PathBuf::from("/usr/bin/archiveutil"),
            aa: PathBuf::from("/usr/bin/aa"),
            aea: PathBuf::from("/usr/bin/aea"),
            command_line_tools: PathBuf::from("/Library/Developer/CommandLineTools/Packages"),
            apple_root_url: "https://www.apple.com/appleca/AppleIncRootCertificate.cer".to_owned(),
            apple_root_sha256: signature::APPLE_ROOT_CA_SHA256.to_owned(),
            clock: monotonic,
        }
    }
}

/// The variables the GitHub Actions runner sets to request an ID token: secret material that no
/// program `hk` starts is given.
const RUNNER_TOKEN_VARIABLES: [&str; 2] = [
    "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
    "ACTIONS_ID_TOKEN_REQUEST_URL",
];

/// A command of `program` that inherits this process's environment but the runner's token
/// variables. Nothing is added to it: the token is never in it.
pub(crate) fn command(program: &Path) -> Command {
    let mut command = Command::new(program);
    for name in RUNNER_TOKEN_VARIABLES {
        command.env_remove(name);
    }
    command
}

/// A failure that names what failed, and that nothing was uploaded.
pub(crate) fn failed(what: &str) -> Stop {
    Stop::fail(format!("{what}; nothing was uploaded"))
}

/// Runs `program` to its end: what it wrote to stdout, when that was piped. A program that cannot
/// be started, or that exits other than 0, is a failure naming `what` it was asked.
pub(crate) fn finish(mut program: Command, what: &str) -> Flow<Vec<u8>> {
    match program.output() {
        Ok(output) if output.status.success() => Ok(output.stdout),
        Ok(output) => Err(failed(&format!("{what} failed ({})", output.status))),
        Err(error) => Err(failed(&format!(
            "cannot run {} ({error})",
            Path::new(program.get_program()).display()
        ))),
    }
}

/// Writes `hk: <text>` and a newline to stderr: what the run is doing.
pub(crate) fn say(w: &mut World<'_>, text: &str) {
    // A stderr that cannot be written to does not stop the run.
    let _ = writeln!(w.stderr, "{PROGRAM}: {text}");
}

/// What the coordinator and the token are, read before the archive.
pub(crate) struct Target {
    /// `https://<host>[:<port>]`, with no `/` at the end.
    pub(crate) endpoint: String,
    /// The HTTPS agent that trusts the certificates of `--ca-file` alone.
    pub(crate) agent: ureq::Agent,
    /// The operator's token, visible ASCII.
    pub(crate) token: String,
    /// `--timeout-secs`: to connect, and to wait for the coordinator's `100 Continue`.
    pub(crate) timeout_secs: u32,
}

/// The coordinator and the token of `globals` for the verb `word` (`xcode add`, `xcode-push`,
/// `xcode verify` off macOS): the usage error when a flag is missing, refused or unreadable.
pub(crate) fn target(globals: &Globals, word: &str) -> Flow<Target> {
    if globals.token_stdin || globals.audience.is_some() || globals.response_file.is_some() {
        // `hk xcode verify` runs no `xcodes`, so there is no two-factor prompt to leave stdin for.
        let why = if word == "xcode verify" {
            ""
        } else {
            " (stdin is the terminal xcodes asks a two-factor code on)"
        };
        return Err(Stop::usage(format!(
            "{PROGRAM} {word} takes --token-file only: --token-stdin, --github-oidc-audience \
             and --response-file are not for it{why}"
        )));
    }
    let (Some(endpoint), Some(ca_file), Some(token_file)) =
        (&globals.endpoint, &globals.ca_file, &globals.token_file)
    else {
        return Err(Stop::usage(format!(
            "{PROGRAM} {word} needs --endpoint, --ca-file and --token-file"
        )));
    };
    if !is_endpoint(endpoint) {
        return Err(Stop::usage("--endpoint takes https://<host>[:<port>]"));
    }
    let Some(ca) = read_path(ca_file) else {
        return Err(Stop::usage(
            [b"cannot read --ca-file ".as_slice(), ca_file].concat(),
        ));
    };
    let connect = Duration::from_secs(u64::from(globals.timeout_secs()));
    // The only error is that there is no certificate to trust.
    let agent = agent(&certificates(&ca), connect, None).map_err(|_| {
        Stop::usage([b"--ca-file ".as_slice(), ca_file, b" holds no certificate"].concat())
    })?;
    private(token_file)?;
    let token = read_file(token_file, b"the token")?;
    if !visible(&token) {
        return Err(Stop::usage(
            "the token is empty or holds a character that is not visible ASCII",
        ));
    }
    Ok(Target {
        endpoint: String::from_utf8_lossy(endpoint.strip_suffix(b"/").unwrap_or(endpoint))
            .into_owned(),
        agent,
        token: String::from_utf8_lossy(&token).into_owned(),
        timeout_secs: globals.timeout_secs(),
    })
}

/// The failure that the volume of `dir` has less than three times the archive's `size` free, the
/// margin the farm Mac's stage uses; the expansion of the archive makes this much.
pub(crate) fn free_check(mac: &Mac, dir: &Path, size: u64) -> Flow<()> {
    use crate::xcode_verify::free::{enough, free_bytes};
    let free = free_bytes(mac, dir).map_err(|why| failed(&why))?;
    enough(free, size, dir).map_err(|why| failed(&why))
}
