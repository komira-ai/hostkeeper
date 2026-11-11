//! The updater's [`Verifier`]: the pinned `gh attestation verify`, run offline, as the daemon's
//! verifier runs it (`hostkeeper_package::gh`, which `pxe update-hook prepare` runs):
//!
//! - `gh` is the pinned one (`--gh`), started by absolute path with a fixed environment: no
//!   token, no prompts, a scratch home in the download directory, and a proxy that refuses every
//!   connection ([`DEAD_PROXY`]), so the check is offline whatever the machine's network does and
//!   needs no login;
//! - the Sigstore trusted root is the file `--trusted-root`, passed as `--custom-trusted-root`;
//! - `gh` and the trusted root are checked before each run ([`FileTrust`]): a regular file, not a
//!   link, owned by root or by the user the updater runs as, writable by neither its group nor
//!   others, in a directory that is the same; an untrusted one fails the check;
//! - the attestation comes from the release's bundle (`<asset>.sigstore.json`), never fetched.
//!
//! The identity is the release signer's ([`crate::release::contract::Signer::hostkeeper`]), held
//! by `gh` itself: `--repo komira-ai/hostkeeper --signer-workflow
//! komira-ai/hostkeeper/.github/workflows/artifacts.yml --source-ref refs/heads/main
//! --source-digest <the release's commit> --deny-self-hosted-runners`, with the daemon's
//! `--cert-oidc-issuer` and `--predicate-type`. `gh` prints what it verified (`--format json`),
//! which the caller checks again (`check::judge`): signer workflow, ref, commit and the file's
//! SHA-256.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::release::contract::{Commit, Signer};
use crate::release::github::{attestations_of, verify_args};
use crate::release::ports::{Attestation, ExecSealed, PortError, Sealed, Verifier};

/// The OIDC issuer of GitHub Actions, which signs the attestation's certificate.
pub(crate) const ISSUER: &str = "https://token.actions.githubusercontent.com";
/// The kind of attestation `gh` is asked for: SLSA build provenance.
pub(crate) const PREDICATE: &str = "https://slsa.dev/provenance/v1";
/// A proxy that refuses every connection (the discard port on loopback).
pub(crate) const DEAD_PROXY: &str = "http://127.0.0.1:9";
/// The most characters of `gh`'s complaint that are kept.
const COMPLAINT_CHARS: usize = 300;
/// What the errors of the check name.
const WHAT: &str = "gh attestation verify (offline)";

/// Whether a file may be believed without reading its content.
pub(crate) trait FileTrust {
    /// Why `path` is not trusted, when it is not.
    fn check(&self, path: &Path) -> Result<(), String>;
}

/// The [`FileTrust`] of a host: the file and its directory are owned by root or by `uid` (the
/// user the updater runs as) and writable by neither the group nor others; the file is regular
/// and not a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OwnedByRootOr {
    pub(crate) uid: u32,
}

impl OwnedByRootOr {
    /// Whether `path`, owned by `owner` with `mode`, may be believed.
    pub(crate) fn judge(&self, path: &Path, owner: u32, mode: u32) -> Result<(), String> {
        if (owner != 0) & (owner != self.uid) {
            return Err(format!(
                "{} is owned by uid {owner}, not by root or uid {}",
                path.display(),
                self.uid
            ));
        }
        if mode & 0o022 != 0 {
            return Err(format!(
                "{} is writable by its group or others (mode {:o})",
                path.display(),
                mode & 0o7777
            ));
        }
        Ok(())
    }
}

impl FileTrust for OwnedByRootOr {
    fn check(&self, path: &Path) -> Result<(), String> {
        let meta =
            std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if !meta.is_file() {
            return Err(format!("{} is not a regular file", path.display()));
        }
        self.judge(path, meta.uid(), meta.mode())?;
        // The caller passes an absolute path, so a file has a parent.
        let dir = path.parent().unwrap_or(path);
        let meta = std::fs::metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        self.judge(dir, meta.uid(), meta.mode())
    }
}

/// The offline `gh` (see the module).
pub(crate) struct OfflineGh<'a> {
    /// The pinned `gh`.
    pub(crate) gh: &'a Path,
    /// The Sigstore trusted root, as `gh attestation trusted-root` prints it.
    pub(crate) trusted_root: &'a Path,
    /// The directory `gh` runs in and keeps its state in (made when missing).
    pub(crate) home: &'a Path,
    pub(crate) exec: &'a dyn ExecSealed,
    pub(crate) trust: &'a dyn FileTrust,
}

/// The whole environment `gh` gets, the daemon's: a fixed `PATH`, the scratch home and a
/// configuration directory in it, no prompts, update notices or colour, and a proxy that refuses
/// every connection. Nothing of the updater's environment, a token included, is in it.
pub(crate) fn env(home: &Path) -> Vec<(String, String)> {
    let home = home.display().to_string();
    let mut env = vec![
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ("HOME".to_owned(), home.clone()),
        ("GH_CONFIG_DIR".to_owned(), format!("{home}/config")),
        ("GH_NO_UPDATE_NOTIFIER".to_owned(), "1".to_owned()),
        ("GH_PROMPT_DISABLED".to_owned(), "1".to_owned()),
        ("NO_COLOR".to_owned(), "1".to_owned()),
    ];
    for proxy in ["HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY"] {
        env.push((proxy.to_owned(), DEAD_PROXY.to_owned()));
    }
    env
}

/// `path` made absolute against the working directory, without touching the filesystem.
fn absolute(path: &Path) -> Result<PathBuf, PortError> {
    std::path::absolute(path).map_err(|e| PortError::new(WHAT, format!("{}: {e}", path.display())))
}

impl Verifier for OfflineGh<'_> {
    /// Checks `file` against `bundle` (see the module). There is no online check: a request
    /// without a bundle is an error, and nothing is run.
    fn verify(
        &self,
        signer: &Signer,
        file: &Path,
        bundle: Option<&Path>,
        commit: &Commit,
    ) -> Result<Vec<Attestation>, PortError> {
        let Some(bundle) = bundle else {
            return Err(PortError::new(
                WHAT,
                "the check is offline: it needs the bundle",
            ));
        };
        let gh = absolute(self.gh)?;
        let root = absolute(self.trusted_root)?;
        let home = absolute(self.home)?;
        for (what, path) in [("--gh", &gh), ("--trusted-root", &root)] {
            self.trust
                .check(path)
                .map_err(|why| PortError::new(WHAT, format!("{what} is not trusted: {why}")))?;
        }
        std::fs::create_dir_all(&home)
            .map_err(|e| PortError::new(WHAT, format!("make {}: {e}", home.display())))?;
        let root_text = root.to_str().ok_or_else(|| {
            PortError::new(WHAT, format!("the path {} is not UTF-8", root.display()))
        })?;
        let extra = [
            "--custom-trusted-root",
            root_text,
            "--cert-oidc-issuer",
            ISSUER,
            "--predicate-type",
            PREDICATE,
        ];
        let args = verify_args(
            signer,
            &absolute(file)?,
            Some(&absolute(bundle)?),
            &extra,
            commit,
        )?;
        let run = Sealed {
            program: gh.clone(),
            args,
            env: env(&home),
            cwd: home,
        };
        let ran = self.exec.spawn_sealed(&run).map_err(|e| {
            PortError::new(WHAT, format!("{} could not be started: {e}", gh.display()))
        })?;
        if !ran.success {
            return Err(PortError::new(WHAT, complaint(&ran.stderr)));
        }
        let value: Value = serde_json::from_slice(&ran.stdout)
            .map_err(|e| PortError::new(WHAT, format!("not JSON: {e}")))?;
        attestations_of(&value, WHAT)
    }
}

/// What `gh` printed on its standard error: its lines that are not blank, joined by `; `, cut to
/// [`COMPLAINT_CHARS`] characters, with anything but printable ASCII made `?`. For the report,
/// never for a decision.
fn complaint(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return "it failed and said nothing".to_owned();
    }
    lines
        .join("; ")
        .chars()
        .take(COMPLAINT_CHARS)
        .map(|c| {
            if c.is_ascii_graphic() | (c == ' ') {
                c
            } else {
                '?'
            }
        })
        .collect()
}
