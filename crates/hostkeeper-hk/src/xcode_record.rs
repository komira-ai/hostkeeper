//! The record of what `hk` found out about an Xcode archive: `~/.hostkeeper/xcode/<sha256>.json`,
//! mode 0600, named by the SHA-256 of the archive's bytes.
//!
//! The record of `hk xcode verify` is `"record": "hk-xcode-verify-v1"` and holds every check it
//! made ([`Verify`]). It is written whatever the verdict is, replacing the file that is there,
//! and printed on stdout as it is written. A section a check did not reach is `null`.
//!
//! A reader ignores the keys it does not know, so a record grows only by new keys (AGENTS.md B8).
//! `hk xcode add` will keep a record of its own kind at the same path, `"record":
//! "hk-xcode-add-v1"`, written through [`write`] too; it reads only `archive`, `verdict` and the
//! `version` and `build` of `expansion`, and uses a record only when `verdict` is `pass`.

use serde::Serialize;

use crate::home::Home;
use crate::{Flow, Stop};

/// The `record` key of the record `hk xcode verify` writes.
pub(crate) const VERIFY: &str = "hk-xcode-verify-v1";

/// The verdict of a record whose checks all passed.
pub(crate) const PASS: &str = "pass";

/// What a program did: its exit code (`null` when it could not be run, or was ended by a signal),
/// and what it wrote to stdout and to stderr, apart. A program that could not be run says why on
/// stderr.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct Output {
    pub(crate) exit: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

/// The archive: its file name (no directory), size, SHA-256 and kind (`xar!`, `AA01`, `AEA1` or
/// `unknown`, from its first four bytes).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Archive {
    pub(crate) name: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
    pub(crate) kind: String,
}

/// The macOS the checks ran on: `sw_vers` and `uname -sm`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct MacOs {
    pub(crate) sw_vers: Output,
    pub(crate) uname: Output,
}

/// A program of the Mac and whether its path is a file.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Program {
    pub(crate) name: &'static str,
    pub(crate) path: String,
    pub(crate) file: bool,
}

/// The programs: every one `hk` needs (a missing one fails the verdict), the three of Apple's
/// other archive formats (recorded only), `xcodes version` and the first lines of `xip --help`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Programs {
    pub(crate) required: Vec<Program>,
    pub(crate) optional: Vec<Program>,
    pub(crate) xcodes: Output,
    pub(crate) xip_help: String,
}

/// One certificate of the signature's chain.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Link {
    pub(crate) name: String,
    pub(crate) sha256: String,
}

/// Apple's published root certificate beside the constant of the code: where it came from, its
/// SHA-256 fingerprint (upper-case hex) and subject, and whether the fingerprint is the
/// constant's (`signature::APPLE_ROOT_CA_SHA256`). `error` says why the certificate could not be had.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AppleRoot {
    pub(crate) source: String,
    pub(crate) sha256: Option<String>,
    pub(crate) subject: Option<String>,
    pub(crate) constant: String,
    pub(crate) equal: bool,
    pub(crate) error: Option<String>,
}

/// `pkgutil --check-signature` on the archive, as it printed and as `signature.rs` read it.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Signature {
    #[serde(flatten)]
    pub(crate) output: Output,
    pub(crate) status: Option<String>,
    pub(crate) chain: Vec<Link>,
    pub(crate) apple_root: AppleRoot,
}

/// `pkgutil --check-signature` and `xip --expand` on one damaged file.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Refusals {
    pub(crate) pkgutil: Output,
    pub(crate) xip: Output,
}

/// `pkgutil --check-signature` on a package of the Command Line Tools.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CommandLineTools {
    pub(crate) package: String,
    pub(crate) pkgutil: Output,
    pub(crate) status: Option<String>,
}

/// The damaged archives: `damaged_clone` is `run`, or says why it was not; `clone` is the
/// `cp -c` that made it; `non_archive` is the 16-byte file that is no archive.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Tamper {
    pub(crate) damaged_clone: String,
    pub(crate) clone: Output,
    pub(crate) damaged: Option<Refusals>,
    pub(crate) non_archive: Option<Refusals>,
    pub(crate) command_line_tools: Option<CommandLineTools>,
}

/// The `Metadata` member of the archive, extracted: what it is (`file`, `directory`, `link` or
/// `missing`) and its text.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Metadata {
    pub(crate) extract: Output,
    pub(crate) kind: String,
    pub(crate) text: Option<String>,
}

/// What the archive says without being expanded: `xip --list`, `xar -tf`, the table of contents
/// (`xar --dump-toc`), the `Metadata` member, and whether a member name or the `Metadata` text
/// names a version or a build (`version` or `build`, in any case). For the record only.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Toc {
    pub(crate) xip_list: Output,
    pub(crate) xar_list: Output,
    pub(crate) dump_toc: Output,
    pub(crate) toc: Option<String>,
    pub(crate) metadata: Metadata,
    pub(crate) names_version: bool,
}

/// The value `plutil` read of the version and of the build.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Plutil {
    pub(crate) version: Output,
    pub(crate) build: Output,
}

/// `xip --expand` of the archive and the app it made: the free space (bytes) before and after,
/// the exit and everything the program printed (stdout, then stderr), the seconds it took (read
/// from the clock, recorded and never judged), the names of the `.app` entries, the names in the
/// `Contents` of the app when there is one, whether its `Contents/version.plist` exists, the
/// `plutil` runs, the version and the build read, and `codesign` and `spctl` on the app.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct Expansion {
    pub(crate) free_before: Option<u64>,
    pub(crate) free_after: Option<u64>,
    pub(crate) exit: Option<i32>,
    pub(crate) output: Option<String>,
    pub(crate) seconds: Option<f64>,
    pub(crate) apps: Vec<String>,
    pub(crate) contents: Vec<String>,
    pub(crate) version_plist: bool,
    pub(crate) plutil: Option<Plutil>,
    pub(crate) version: Option<String>,
    pub(crate) build: Option<String>,
    pub(crate) codesign: Option<Output>,
    pub(crate) spctl: Option<Output>,
}

/// The record of `hk xcode verify`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Verify {
    pub(crate) record: &'static str,
    pub(crate) hk: String,
    pub(crate) macos: MacOs,
    pub(crate) programs: Option<Programs>,
    pub(crate) archive: Archive,
    pub(crate) signature: Option<Signature>,
    pub(crate) tamper: Option<Tamper>,
    pub(crate) toc: Option<Toc>,
    pub(crate) expansion: Option<Expansion>,
    pub(crate) verdict: String,
}

/// The JSON of `record`, indented, and one newline.
pub(crate) fn json(record: &impl Serialize) -> Flow<String> {
    serde_json::to_string_pretty(record)
        .map(|text| text + "\n")
        .map_err(|error| Stop::fail(format!("cannot write the record as JSON: {error}")))
}

/// Writes `text`, the JSON of a record of the archive whose SHA-256 is `sha256`, as
/// `<xcode directory>/<sha256>.json` with mode 0600, replacing the file that is there; its path.
pub(crate) fn write(home: &Home, sha256: &str, text: &str) -> Flow<std::path::PathBuf> {
    let path = home.ensure()?.join(format!("{sha256}.json"));
    home.replace_file(&path, text.as_bytes())?;
    Ok(path)
}

#[cfg(test)]
#[path = "xcode_record_tests.rs"]
mod tests;
