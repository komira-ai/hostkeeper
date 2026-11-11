//! What the Mac device tests that start the daemon as a program share about `.pkg` files: how a
//! `.pkg` is served to the daemon, and how its signature is judged. The module is included by
//! path in `tests/bin/selfupdate_daemon.rs` (the daemon's side) and in the test files that start
//! it (the test's side), on a Mac only.
//!
//! **No package is signed on the runner.** A hosted macOS runner refuses an installer identity
//! whose root it does not trust, and the undo of a trust setting there makes no progress
//! (`tests/install_macos_pkg.rs` says what was met). So the program's `macos-pkg` installer runs
//! over the Mac's own tools but for one call, as the installer's own device test and its daemon
//! do: `pkgutil --check-signature` of a package the test served ([`SignedByStore`]) is answered
//! as a package signed by the leaf [`LEAF`], the one signer of the signers file the test writes
//! ([`signers_file_text`]) and the daemon's loader reads. `pkgutil --expand-full`, `lsbom`,
//! `installer`, the receipts and `launchctl` are the Mac's; what this does not show on real tools
//! is the form `pkgutil` prints of a trusted signature, which `install_macos_pkg.rs` says waits
//! for a signed run on an enrolled Mac.
//!
//! **The store.** The daemon fetches a `.pkg` by the pin of its entry. The program's package
//! port ([`PkgStore`]) serves each package from the directory `pkgs/` of the fixture's
//! directory, a file named `<sha256>.pkg` ([`served_file`]), which the test puts there
//! ([`serve`]); a digest with no such file is `package_not_found`. Nothing is fetched over a
//! network.

#![allow(
    dead_code,
    reason = "the program and each test file use part of this module"
)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::packages::{PackageError, Packages, Pinned};
use hostkeeper_install::tools::{ToolError, ToolOutput};
use hostkeeper_install::{HostTools, Tool, Tools};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use sha2::{Digest, Sha256};

/// The directory of the fixture's directory the served `.pkg` files are in.
pub const PKGS_DIR: &str = "pkgs";
/// The signers file's name in the fixture's directory.
pub const SIGNERS_FILE: &str = "macos-pkg.toml";
/// The leaf certificate's SHA-256 the answered `--check-signature` names, and the host's signer.
pub const LEAF: &str = "a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9a9";

/// The text of the signers file naming [`LEAF`] as the host's one signer.
pub fn signers_file_text() -> String {
    format!("signers = [\"{LEAF}\"]\n")
}

/// The SHA-256 of the file at `path`, in lower-case hex.
pub fn digest_of(path: &Path) -> std::io::Result<String> {
    let bytes = fs::read(path)?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// [`digest_of`] for a test, which fails when the file cannot be read.
pub fn sha256_of(path: &Path) -> String {
    digest_of(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Where the package of SHA-256 `digest` is served from in the fixture's directory `dir`.
pub fn served_file(dir: &Path, digest: &str) -> PathBuf {
    dir.join(PKGS_DIR).join(format!("{digest}.pkg"))
}

/// Serves the `.pkg` at `pkg` from the fixture's directory `dir`: a copy of its bytes (a new file,
/// root's, which a clone of the source would not be) named by its digest; the digest and the
/// size in bytes, which an entry pins.
pub fn serve(dir: &Path, pkg: &Path) -> (String, u64) {
    let bytes = fs::read(pkg).unwrap_or_else(|e| panic!("read {}: {e}", pkg.display()));
    let digest = sha256_of(pkg);
    let at = served_file(dir, &digest);
    fs::write(&at, &bytes).unwrap_or_else(|e| panic!("write {}: {e}", at.display()));
    fs::set_permissions(&at, fs::Permissions::from_mode(0o644)).expect("its mode");
    (digest, bytes.len() as u64)
}

/// `pkgutil --check-signature`'s answer for a package signed by [`LEAF`] whose chain the Mac
/// trusts, in the form `hostkeeper_install::macos_pkg::signature` documents.
pub fn signed_answer(package: &str) -> String {
    let hex_line = |hex: &str| {
        hex.as_bytes()
            .chunks(2)
            .map(|pair| String::from_utf8_lossy(pair).to_uppercase())
            .collect::<Vec<_>>()
            .join(" ")
    };
    [
        format!("Package \"{package}\":"),
        "   Status: signed by a certificate trusted by macOS".to_owned(),
        "   Certificate Chain:".to_owned(),
        "    1. hostkeeper device test installer".to_owned(),
        "       SHA256 Fingerprint:".to_owned(),
        format!("           {}", hex_line(&LEAF[..32])),
        format!("           {}", hex_line(&LEAF[32..])),
        format!("       {}", "-".repeat(72)),
        String::new(),
    ]
    .join("\n")
}

/// The Mac's tools, but for `pkgutil --check-signature` of a file whose bytes the test served
/// (a file of `dir`'s [`PKGS_DIR`] has their digest as its name), answered as signed by [`LEAF`]
/// with a chain the Mac trusts. The daemon names the `.pkg` by the path of its own copy, so the
/// answer goes by the bytes. Every other call is the Mac's.
pub struct SignedByStore {
    /// The fixture's directory.
    pub dir: PathBuf,
}

impl Tools for SignedByStore {
    fn run(
        &self,
        tool: Tool,
        args: &[String],
        deadline: Duration,
    ) -> Result<ToolOutput, ToolError> {
        if tool == Tool::Pkgutil && args.first().map(String::as_str) == Some("--check-signature") {
            let package = args.get(1).cloned().unwrap_or_default();
            let served = fs::metadata(&package).is_ok_and(|meta| meta.is_file())
                && digest_of(Path::new(&package))
                    .is_ok_and(|digest| served_file(&self.dir, &digest).is_file());
            if served {
                return Ok(ToolOutput {
                    code: Some(0),
                    stdout: signed_answer(&package).into_bytes(),
                    stderr: Vec::new(),
                });
            }
        }
        HostTools.run(tool, args, deadline)
    }
}

/// A file the store wrote, removed when dropped.
struct Copy(PathBuf);

impl Drop for Copy {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// The package port of the program: each `.pkg` served from the fixture's directory by its pin,
/// copied into the service's directory of the packages directory for the installer's `stage` to
/// read, and removed when the daemon drops it. It stages no artifact.
pub struct PkgStore {
    /// The fixture's directory.
    pub dir: PathBuf,
    /// The daemon's packages directory.
    pub packages: PathBuf,
}

impl Packages for PkgStore {
    fn pinned(&self, service: &ServiceName, entry: &Target) -> Result<Pinned, PackageError> {
        let from = served_file(&self.dir, &entry.digest);
        if !from.is_file() {
            return Err(PackageError::NotFound(entry.digest.clone()));
        }
        let name = format!(
            ".fetch-test-{}",
            &entry.digest[..entry.digest.len().min(12)]
        );
        let parent = self.packages.join(service.as_str());
        let copy = Copy(parent.join(name));
        fs::create_dir_all(&parent)
            .map_err(|e| PackageError::Other(format!("{}: {e}", parent.display())))?;
        fs::copy(&from, &copy.0)
            .map_err(|e| PackageError::Other(format!("{}: {e}", copy.0.display())))?;
        let path = copy.0.clone();
        Ok(Pinned::new(&path, Box::new(copy)))
    }
}
