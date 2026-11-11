//! What the `apt` backend reads of apt's, dpkg's and dpkg-query's output, and how a failed run
//! maps onto section 4.5's reasons. The daemon classifies by the step first; apt's messages are
//! read only where apt has no other signal, in the forms each supported release prints (named
//! below where they differ: apt before 3.0 verifies with gpgv and names gpg's status tokens,
//! apt 3.0 and later with sqv, whose words differ; both end in the same `E:` line).

use crate::repository::backend::{Fields, Installed, Stanza};
use crate::repository::outcome::{Failure, Reason};
use crate::repository::tools::ToolOutput;

/// What apt prints for a suite whose signature did not verify. `E: The repository '…' is not
/// signed.` and `… is no longer signed.` close every signature failure on apt 2.x and 3.x
/// (gpgv's `NO_PUBKEY`, `BADSIG`, `EXPKEYSIG`, `REVKEYSIG` before it on 2.x; sqv's `Missing key`
/// and `Message has been manipulated` on 3.x); a `Valid-Until` passed is `E: Release file for …
/// is expired`.
pub const SIGNATURE: [&str; 9] = [
    "is not signed",
    "is no longer signed",
    "is expired",
    "has expired",
    "NO_PUBKEY",
    "BADSIG",
    "EXPKEYSIG",
    "REVKEYSIG",
    "Missing key",
];

/// What apt and dpkg print when the dpkg lock is held: apt's `Could not get lock`, dpkg's
/// `dpkg frontend lock was locked by another process` (2.x) and `… is locked by another
/// process`.
pub const BUSY: [&str; 4] = [
    "Could not get lock",
    "was locked by another process",
    "is locked by another process",
    "Unable to acquire the dpkg frontend lock",
];

/// What apt and dpkg print when an earlier dpkg run was interrupted.
pub const INTERRUPTED: [&str; 1] = ["dpkg was interrupted"];

/// What apt prints when a dependency cannot be met.
pub const UNMET: [&str; 3] = [
    "unmet dependencies",
    "Unable to correct problems",
    "but it is not installable",
];

/// What apt prints when a downloaded file is not the bytes its index names.
pub const HASH: [&str; 3] = [
    "Hash Sum mismatch",
    "File has unexpected size",
    "Hashes of expected file",
];

/// What apt-cache and dpkg-query print for a package or version they do not know.
pub const UNKNOWN: [&str; 4] = [
    "No packages found",
    "Unable to locate package",
    "Can't select",
    "no packages found matching",
];

/// What dpkg prints when another installed package depends on the one being purged.
pub const DEPENDENTS: [&str; 2] = ["dependency problems", "depends on"];

/// Whether `text` holds any of `needles`.
fn holds(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

/// A failure of `reason` quoting the run's last line.
fn failed(reason: Reason, what: &str, output: &ToolOutput) -> Failure {
    Failure::new(reason, format!("{what}: {}", output.last_line()))
}

/// A run stopped at its bound (no exit code): the host's, not the release's (packages-tests.md:
/// "a tool past its bound is stopped and the step fails `environment`").
fn stopped(what: &str, output: &ToolOutput) -> Option<Failure> {
    output.code.is_none().then(|| {
        Failure::new(
            Reason::IoError,
            format!("{what} was stopped at its bound: {}", output.last_line()),
        )
    })
}

/// The reason a run that ended badly gives in any step: the lock and an interrupted dpkg
/// first, which no step's own check would explain.
fn host_state(text: &str) -> Option<Reason> {
    if holds(text, &INTERRUPTED) {
        return Some(Reason::DpkgInterrupted);
    }
    holds(text, &BUSY).then_some(Reason::PackageManagerBusy)
}

/// `apt-get update`'s outcome: a signature failure, else the repository could not be read.
pub fn update(output: &ToolOutput) -> Result<(), Failure> {
    if output.succeeded() {
        return Ok(());
    }
    let reason = match holds(&output.text(), &SIGNATURE) {
        true => Reason::Signature,
        false => Reason::FetchFailed,
    };
    Err(failed(reason, "apt-get update", output))
}

/// The value of `key` in a stanza's lines.
fn field<'a>(stanza: &'a str, key: &str) -> Option<&'a str> {
    stanza.lines().find_map(|line| {
        line.split_once(':')
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v.trim())
    })
}

/// The stanza of `package` at `version` for `architecture` or `all` in `apt-cache show`'s
/// output; `None` when it lists none.
pub fn stanza(
    output: &ToolOutput,
    package: &str,
    version: &str,
    architecture: &str,
) -> Result<Option<Stanza>, Failure> {
    let text = String::from_utf8_lossy(&output.stdout);
    if !output.succeeded() {
        return match holds(&output.text(), &UNKNOWN) {
            true => Ok(None),
            false => Err(failed(Reason::IoError, "apt-cache show", output)),
        };
    }
    if output.cut {
        return Err(Failure::new(
            Reason::IoError,
            "apt-cache show printed more than is kept",
        ));
    }
    let found = text.split("\n\n").find(|s| {
        (field(s, "Package") == Some(package))
            & (field(s, "Version") == Some(version))
            & field(s, "Architecture").is_some_and(|a| (a == architecture) | (a == "all"))
    });
    let Some(found) = found else {
        return Ok(None);
    };
    let sha256 = field(found, "SHA256")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let size = field(found, "Size").and_then(|s| s.parse::<u64>().ok());
    match (sha256.len() == 64, size) {
        (true, Some(size)) => Ok(Some(Stanza {
            sha256,
            size,
            filename: field(found, "Filename").map(str::to_owned),
        })),
        _ => Err(Failure::new(
            Reason::ManifestInvalid,
            format!("the index's stanza of {package} {version} has no SHA256 and Size"),
        )),
    }
}

/// The packages a simulated install would touch, from its `Inst`, `Conf`, `Remv` and `Purg`
/// lines, each without its `:<architecture>`.
fn touched(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let verb = words.next()?;
            let name = words.next()?;
            ["Inst", "Conf", "Remv", "Purg"]
                .contains(&verb)
                .then(|| name.split(':').next().unwrap_or(name).to_owned())
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The simulation's outcome: it may install, configure or downgrade `package` alone.
pub fn simulate(output: &ToolOutput, package: &str) -> Result<(), Failure> {
    let text = output.text();
    if let Some(reason) = host_state(&text) {
        return Err(failed(reason, "the dependency check", output));
    }
    if holds(&text, &UNMET) {
        return Err(failed(
            Reason::DependencyMissing,
            "the dependency check",
            output,
        ));
    }
    if !output.succeeded() || output.cut {
        return Err(failed(Reason::IoError, "the dependency check", output));
    }
    let others: Vec<String> = touched(&text)
        .into_iter()
        .filter(|p| p != package)
        .collect();
    match others.is_empty() {
        true => Ok(()),
        false => Err(Failure::new(
            Reason::DependencyChanges,
            format!(
                "the install of {package} would also change {}",
                others.join(", ")
            ),
        )),
    }
}

/// `apt-get download`'s outcome: other bytes than the index names, else a fetch failure.
pub fn download(output: &ToolOutput) -> Result<(), Failure> {
    if output.succeeded() {
        return Ok(());
    }
    let reason = match holds(&output.text(), &HASH) {
        true => Reason::StoreDigestMismatch,
        false => Reason::FetchFailed,
    };
    Err(failed(reason, "apt-get download", output))
}

/// The control fields `dpkg-deb --field <file> Package Version Architecture` prints.
pub fn fields(output: &ToolOutput) -> Result<Fields, Failure> {
    let text = String::from_utf8_lossy(&output.stdout);
    let get = |key| field(&text, key).map(str::to_owned);
    match (
        output.succeeded(),
        get("Package"),
        get("Version"),
        get("Architecture"),
    ) {
        (true, Some(package), Some(version), Some(architecture)) => Ok(Fields {
            package,
            version,
            architecture,
        }),
        _ => Err(failed(
            Reason::ManifestInvalid,
            "the package's control fields cannot be read",
            output,
        )),
    }
}

/// The installed package as `dpkg-query -W -f '${Version}\t${db:Status-Abbrev}\n'` prints it;
/// `None` for a package not installed, or of which only configuration files are left.
pub fn installed(output: &ToolOutput) -> Result<Option<Installed>, Failure> {
    if !output.succeeded() {
        return match holds(&output.text(), &UNKNOWN) {
            true => Ok(None),
            false => Err(failed(Reason::IoError, "dpkg-query", output)),
        };
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().next().unwrap_or_default();
    let (version, status) = line.split_once('\t').unwrap_or((line, ""));
    let state = status.as_bytes().get(1).copied();
    match state {
        None | Some(b'n') | Some(b'c') => Ok(None),
        Some(state) => Ok(Some(Installed {
            version: version.trim().to_owned(),
            complete: state == b'i',
        })),
    }
}

/// The install's outcome (`apt-get install --no-download`): its failure is the release's
/// (`dpkg_error`, a maintainer script or the package itself) but for the host's state.
pub fn install(output: &ToolOutput) -> Result<(), Failure> {
    let text = output.text();
    if output.succeeded() {
        return Ok(());
    }
    if let Some(failure) = stopped("apt-get install", output) {
        return Err(failure);
    }
    if let Some(reason) = host_state(&text) {
        return Err(failed(reason, "apt-get install", output));
    }
    let reason = match holds(&text, &UNMET) {
        true => Reason::DependencyMissing,
        false => Reason::DpkgError,
    };
    Err(failed(reason, "apt-get install", output))
}

/// `apt-mark hold` or `unhold`'s outcome.
pub fn hold(output: &ToolOutput) -> Result<(), Failure> {
    if output.succeeded() {
        return Ok(());
    }
    let reason = host_state(&output.text()).unwrap_or(Reason::IoError);
    Err(failed(reason, "apt-mark", output))
}

/// `dpkg --purge`'s outcome: done, or the lock held, which is tried again.
pub fn purge(output: &ToolOutput) -> Result<bool, Failure> {
    let text = output.text();
    if output.succeeded() {
        return Ok(true);
    }
    if let Some(failure) = stopped("dpkg --purge", output) {
        return Err(failure);
    }
    if holds(&text, &INTERRUPTED) {
        return Err(failed(Reason::DpkgInterrupted, "dpkg --purge", output));
    }
    if holds(&text, &BUSY) {
        return Ok(false);
    }
    let reason = match holds(&text, &DEPENDENTS) {
        true => Reason::DependencyChanges,
        false => Reason::DpkgError,
    };
    Err(failed(reason, "dpkg --purge", output))
}
