//! The real [`Verifier`]: the pinned `gh attestation verify`, run offline. Ported from kbf's
//! `GhCheck` (kbf-installer, `attest.rs`), with these differences:
//!
//! - the signer is the daemon config's one `[trusted_signers]` entry, passed in each
//!   [`Attest`]; the crate holds no organisation, repository or workflow of its own;
//! - the attested source repository must belong to the organisation (`--owner`), and any
//!   repository of it may sign (design Q12, option (a)): nothing compares the repository with
//!   the service;
//! - the signing workflow is the config's `workflow` path in any repository of the
//!   organisation. `gh` quotes `--signer-workflow` as one exact workflow URL, which cannot say
//!   "any repository", so the check is `--cert-identity-regex` on the certificate's signer URI,
//!   `^https://github\.com/<organisation>/[^/]+/<workflow>@refs/[^@]+$`, with both config values
//!   escaped. The whole URI must match: after the workflow path comes `@` and a `refs/...` ref
//!   with no further `@`, so a workflow whose file name merely starts with the configured one
//!   and an `@` (`artifacts.yml@other.example`) is a different workflow and is refused;
//! - `--deny-self-hosted-runners` is passed only when the config's `hosted_runner` is true;
//! - the config's refs are globs. The ref in the bundle's certificate is matched against them
//!   ([`TrustedSigners::allows_ref`], which channel-sync's check uses too), and a ref no glob
//!   matches is rejected without running `gh`; otherwise `gh` is run once, with that ref as
//!   `--source-ref`;
//! - the commit is the one the bundle's provenance names (`--source-digest`); `gh` checks it
//!   against the certificate, so a bundle that lies about it is rejected.
//!
//! As in kbf, `gh` is started by absolute path with a fixed environment (no token, no prompts, a
//! scratch home) and a proxy that refuses every connection, so the check is offline whatever the
//! host's network does; the Sigstore trusted root is passed as `--custom-trusted-root`. The paths
//! of `gh`, of the trusted root and of the scratch home come from the daemon, not from the
//! config. `gh` and the trusted root are checked before each run ([`FileTrust`]): an untrusted
//! one is no verdict on the package and makes the verifier [`VerifyError::Unavailable`].
//!
//! `gh` runs with the scratch home as its working directory, so every path it is given (the
//! program, the archive, the bundle, the trusted root and the home) is made absolute first,
//! against the daemon's own working directory; one that cannot be made absolute makes the
//! verifier [`VerifyError::Unavailable`]. The daemon installs `gh` and the trusted root under
//! root-owned directories nobody else can write; [`OwnedBy`] checks the file and its directory.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use hostkeeper_config::daemon::TrustedSigners;
use hostkeeper_config::trust::TrustError;
use hostkeeper_steps::{Clock, Stamp};

use crate::ports::{Attest, Attested, Verifier, VerifyError, remaining_secs};
use crate::stage;

/// The OIDC issuer of GitHub Actions, which signs the attestation's certificate.
pub const ISSUER: &str = "https://token.actions.githubusercontent.com";
/// The kind of attestation `gh` is asked for: SLSA build provenance.
pub const PREDICATE: &str = "https://slsa.dev/provenance/v1";
/// A proxy that refuses every connection (the discard port on loopback).
pub const DEAD_PROXY: &str = "http://127.0.0.1:9";
/// The URL every signer URI of a GitHub Actions certificate starts with, as a regular
/// expression.
const SIGNER_URI_START: &str = r"^https://github\.com/";
/// What ends a signer URI, as a regular expression: `@`, a `refs/...` ref with no further `@`,
/// and the end of the text.
const SIGNER_URI_END: &str = "@refs/[^@]+$";
/// The characters that mean something in a Go regular expression (`regexp.QuoteMeta`'s set).
const REGEX_META: &str = r"\.+*?()|[]{}^$";
/// The most bytes of `gh`'s standard error that are kept.
pub const STDERR_BYTES: u64 = 64 * 1024;

/// `text` with every character a Go regular expression gives a meaning escaped, as
/// `regexp.QuoteMeta` does.
pub(crate) fn quote_meta(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len());
    for c in text.chars() {
        if REGEX_META.contains(c) {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted
}

/// The regular expression the certificate's signer URI must match in full: the config's
/// workflow, in any repository of the config's organisation, then `@` and a ref with no further
/// `@`.
pub(crate) fn signer_identity(signers: &TrustedSigners) -> String {
    format!(
        "{SIGNER_URI_START}{}/[^/]+/{}{SIGNER_URI_END}",
        quote_meta(&signers.organisation),
        quote_meta(&signers.workflow)
    )
}

/// One run of a program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spawn {
    /// An absolute path.
    pub program: PathBuf,
    pub args: Vec<OsString>,
    /// The whole environment; nothing is inherited.
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    /// When the program is killed, on the injected clock.
    pub deadline: Stamp,
}

/// How a run ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ran {
    /// The exit code; `None` when a signal ended it.
    pub code: Option<i32>,
    /// It was killed at the deadline.
    pub timed_out: bool,
    /// The first [`STDERR_BYTES`] of its standard error.
    pub stderr: Vec<u8>,
}

/// Runs a program to its end or its deadline.
pub trait Runner {
    /// An error is a program that could not be started.
    fn run(&self, spawn: &Spawn) -> io::Result<Ran>;
}

/// The [`Runner`] of a host: a child process with no input, its output dropped and its
/// standard error kept in a staging file in `cwd`, polled once a second on `clock` and killed at
/// the deadline. A deadline that has passed before the start starts nothing, and one that passes
/// while the child runs wins over the child's exit seen at the same poll.
pub struct SystemRunner {
    clock: Arc<dyn Clock>,
}

impl SystemRunner {
    pub fn new(clock: Arc<dyn Clock>) -> SystemRunner {
        SystemRunner { clock }
    }
}

impl Runner for SystemRunner {
    fn run(&self, spawn: &Spawn) -> io::Result<Ran> {
        if remaining_secs(&spawn.deadline, &*self.clock) == 0 {
            return Ok(Ran {
                timed_out: true,
                ..Ran::default()
            });
        }
        let (stderr, kept) = stage::create(&spawn.cwd, stage::names())?;
        let mut child = Command::new(&spawn.program)
            .args(&spawn.args)
            .env_clear()
            .envs(spawn.env.iter().map(|pair| (&pair.0, &pair.1)))
            .current_dir(&spawn.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()?;
        let (code, timed_out) = loop {
            if remaining_secs(&spawn.deadline, &*self.clock) == 0 {
                // An error here is a child that has just exited; `wait` reaps it either way.
                let _ = child.kill();
                child.wait()?;
                break (None, true);
            }
            if let Some(status) = child.try_wait()? {
                break (status.code(), false);
            }
            self.clock.sleep_secs(1);
        };
        let mut text = Vec::new();
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(kept.path())?
            .take(STDERR_BYTES)
            .read_to_end(&mut text)?;
        Ok(Ran {
            code,
            timed_out,
            stderr: text,
        })
    }
}

/// Whether a file may be believed without reading its content.
pub trait FileTrust {
    fn check(&self, path: &Path) -> Result<(), TrustError>;
}

/// The [`FileTrust`] of a host: a regular file, not a link, owned by `uid` (root, 0, on a host)
/// and writable by neither its group nor others, checked on the open file; and the directory
/// that holds it, owned by `uid` or by root and writable by neither its group nor others, so that
/// nobody else can swap the file for another between the check and its use. The directories above
/// that one are not checked: the daemon installs the file under root-owned directories all the
/// way up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OwnedBy {
    pub uid: u32,
}

/// Whether an owner and a mode are trusted: the owner is `wanted` or one of `also`, and neither
/// the group nor others can write.
fn judge(found: u32, mode: u32, wanted: u32, also: &[u32]) -> Result<(), TrustError> {
    if (found != wanted) & !also.contains(&found) {
        return Err(TrustError::Owner { found, wanted });
    }
    if mode & 0o022 != 0 {
        return Err(TrustError::Writable {
            mode: mode & 0o7777,
        });
    }
    Ok(())
}

impl FileTrust for OwnedBy {
    fn check(&self, path: &Path) -> Result<(), TrustError> {
        let meta = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?
            .metadata()?;
        if !meta.is_file() {
            return Err(TrustError::NotRegular);
        }
        judge(meta.uid(), meta.mode(), self.uid, &[])?;
        let whole = std::path::absolute(path)?;
        // The parent of a path that opened as a file is a directory.
        let parent = std::fs::metadata(whole.parent().unwrap_or(&whole))?;
        judge(parent.uid(), parent.mode(), self.uid, &[0])
    }
}

/// `gh attestation verify`, as the [`Verifier`] of a host.
pub struct GhVerifier {
    gh: PathBuf,
    trusted_root: PathBuf,
    home: PathBuf,
    runner: Box<dyn Runner>,
    trust: Box<dyn FileTrust>,
}

impl GhVerifier {
    /// `gh` is the pinned `gh`, `trusted_root` the Sigstore trusted root as `gh attestation
    /// trusted-root` prints it, and `home` a directory only root can write, which `gh` runs in.
    pub fn new(
        gh: PathBuf,
        trusted_root: PathBuf,
        home: PathBuf,
        runner: Box<dyn Runner>,
        trust: Box<dyn FileTrust>,
    ) -> GhVerifier {
        GhVerifier {
            gh,
            trusted_root,
            home,
            runner,
            trust,
        }
    }

    /// The arguments of `gh` for `request`, the archive, the bundle and `trusted_root` as given:
    ///
    /// ```text
    /// attestation verify <archive> --bundle <bundle> --custom-trusted-root <root>
    ///   --owner <organisation> --cert-identity-regex <signer_identity>
    ///   --cert-oidc-issuer <ISSUER> --source-ref <ref> --source-digest <commit>
    ///   --predicate-type <PREDICATE> [--deny-self-hosted-runners]
    /// ```
    pub fn args(&self, request: &Attest<'_>, trusted_root: &Path) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "attestation".into(),
            "verify".into(),
            request.archive.into(),
            "--bundle".into(),
            request.bundle.into(),
            "--custom-trusted-root".into(),
            trusted_root.into(),
        ];
        let identity = signer_identity(request.signers);
        for (flag, value) in [
            ("--owner", request.signers.organisation.as_str()),
            ("--cert-identity-regex", identity.as_str()),
            ("--cert-oidc-issuer", ISSUER),
            ("--source-ref", request.source_ref.as_str()),
            ("--source-digest", request.commit.as_str()),
            ("--predicate-type", PREDICATE),
        ] {
            args.push(flag.into());
            args.push(value.into());
        }
        if request.signers.hosted_runner {
            args.push("--deny-self-hosted-runners".into());
        }
        args
    }

    /// The whole environment `gh` gets: a fixed `PATH`, the scratch home and a configuration
    /// directory in it, no prompts, update notices or colour, and a proxy that refuses every
    /// connection. Nothing of the daemon's environment, a token included, is in it.
    pub fn env(&self, home: &Path) -> Vec<(String, String)> {
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
}

impl Verifier for GhVerifier {
    /// Rejects a ref none of the signer's globs matches, without running `gh`; then makes the
    /// paths absolute, checks `gh`
    /// and the trusted root, runs `gh` once and reads its verdict: exit 0 is attested, another
    /// exit code is [`VerifyError::Rejected`] with `gh`'s first line of complaint, a `gh` killed
    /// at the deadline is [`VerifyError::TimedOut`], and a `gh` that cannot start, is ended by a
    /// signal, is not trusted or is given a path that cannot be made absolute is
    /// [`VerifyError::Unavailable`].
    fn verify(&self, request: &Attest<'_>) -> Result<Attested, VerifyError> {
        let source_ref = request.source_ref.as_str();
        if !request.signers.allows_ref(source_ref) {
            return Err(VerifyError::Rejected(format!(
                "the ref {source_ref} matches none of the trusted signer's refs"
            )));
        }
        let [gh, root, home, archive, bundle] = absolute_all([
            &self.gh,
            &self.trusted_root,
            &self.home,
            request.archive,
            request.bundle,
        ])
        .map_err(|_| VerifyError::Unavailable)?;
        self.trust
            .check(&gh)
            .and_then(|()| self.trust.check(&root))
            .map_err(|_| VerifyError::Unavailable)?;
        let request = Attest {
            archive: &archive,
            bundle: &bundle,
            ..*request
        };
        let spawn = Spawn {
            program: gh,
            args: self.args(&request, &root),
            env: self.env(&home),
            cwd: home,
            deadline: request.deadline.clone(),
        };
        let ran = self
            .runner
            .run(&spawn)
            .map_err(|_| VerifyError::Unavailable)?;
        match ran {
            Ran {
                timed_out: true, ..
            } => Err(VerifyError::TimedOut),
            Ran { code: Some(0), .. } => Ok(Attested {
                commit: request.commit.clone(),
                source_ref: request.source_ref.clone(),
            }),
            Ran { code: None, .. } => Err(VerifyError::Unavailable),
            Ran { stderr, .. } => Err(VerifyError::Rejected(complaint(&stderr))),
        }
    }
}

/// `paths`, each made absolute against the working directory without touching the filesystem.
/// An empty path cannot be made absolute and is an error.
fn absolute_all(paths: [&Path; 5]) -> io::Result<[PathBuf; 5]> {
    let mut out = [(); 5].map(|()| PathBuf::new());
    for (slot, path) in out.iter_mut().zip(paths) {
        *slot = std::path::absolute(path)?;
    }
    Ok(out)
}

/// The first line `gh` printed to its standard error, cut to 160 characters, with anything but
/// printable ASCII replaced by `?`: for the log, never for a decision.
fn complaint(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(160)
        .map(|c| {
            if c.is_ascii_graphic() | (c == ' ') {
                c
            } else {
                '?'
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "gh_tests.rs"]
mod tests;
