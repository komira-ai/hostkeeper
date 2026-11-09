//! `channels apt sign --dir <dir> --work <dir> --fingerprint <fpr>[,<fpr>...] [--gpg <program>]
//! [--output <file>]`: `InRelease` (clear signed) and `Release.gpg` (detached) of
//! `<dir>/Release`, verified.
//!
//! The index is signed with the signing subkey of the archive key: the key spec is the subkey's
//! fingerprint followed by `!`, which makes gpg use exactly that key and no other. Several
//! fingerprints (a key rotation) give one signature per key, in both files. The passphrase is
//! never an argument (arguments are readable by every process of the machine): it is a file, with
//! the loopback pinentry. The key and the passphrase come from the step's environment
//! (`APT_GPG_PRIVATE_KEY`, `APT_GPG_PASSPHRASE`: secret material, read in [`secrets_from_env`]
//! and nowhere else); the verb writes them to files (0600) below `--work`, and they and the
//! keyring in a private directory (0700) below `--work` are removed when the verb ends, whatever
//! happened. A fingerprint that is not among the secret keys imported fails before
//! anything is signed. `signed=true` is appended to `--output` when both files verify.
//!
//! Copied from komira-ai/tierstore at ff4777831c08fc13aa025b54e6e0531c2f350f4f
//! (`crates/xtask/src/channels/apt/sign.rs`), adapted to this repository (see [`crate::channels`]).

use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::channels::Error;
use crate::channels::aws::{Answer, run_program};
use crate::release::ports::Exec;

/// The `gpg` program, behind a trait so a test answers in place of it.
pub trait Gpg {
    /// `gpg <args>`. A refusal is an [`Answer`] that is not `ok`.
    ///
    /// # Errors
    /// The program could not be started.
    fn run(&self, args: &[String]) -> Result<Answer, Error>;
}

/// The `gpg` program at a path, run through an [`Exec`].
pub struct ExecGpg<'a> {
    exec: &'a dyn Exec,
    program: PathBuf,
}

impl<'a> ExecGpg<'a> {
    pub fn new(exec: &'a dyn Exec, program: PathBuf) -> ExecGpg<'a> {
        ExecGpg { exec, program }
    }
}

impl Gpg for ExecGpg<'_> {
    fn run(&self, args: &[String]) -> Result<Answer, Error> {
        run_program(self.exec, &self.program, args)
    }
}

/// The environment variable holding the armored private key.
pub(crate) const KEY_VAR: &str = "APT_GPG_PRIVATE_KEY";

/// The environment variable holding the key's passphrase.
pub(crate) const PASSPHRASE_VAR: &str = "APT_GPG_PASSPHRASE";

/// The files below `--work` the key and the passphrase are written to; the sign verbs remove them.
pub(crate) struct Secrets {
    pub(crate) key_file: PathBuf,
    pub(crate) passphrase_file: PathBuf,
}

/// The value of the secret variable `name`.
///
/// # Errors
/// The variable is unset or empty (a usage error naming it).
#[expect(
    clippy::disallowed_methods,
    reason = "the signing key and its passphrase are secret material the job passes in its environment, never as arguments (AGENTS.md S2: secret material)"
)]
fn secret_var(name: &str) -> Result<String, Error> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| Error::Usage(format!("{name} is not set: the signing key comes from it")))
}

/// [`write_secrets`] of the key and passphrase in the environment, for the `sign` verbs.
///
/// # Errors
/// A variable that is not set (usage), or a file that cannot be written.
pub(crate) fn secrets_from_env(work: &Path) -> Result<Secrets, Error> {
    write_secrets(work, &secret_var(KEY_VAR)?, &secret_var(PASSPHRASE_VAR)?)
}

/// Writes the key and the passphrase, mode 0600, to new files below `work` (which is made when
/// missing). Neither value is ever in an error. A failure removes what was written.
///
/// # Errors
/// A directory or a file that cannot be written.
pub(crate) fn write_secrets(work: &Path, key: &str, passphrase: &str) -> Result<Secrets, Error> {
    let secrets = Secrets {
        key_file: work.join("signing-key.asc"),
        passphrase_file: work.join("signing-passphrase"),
    };
    std::fs::create_dir_all(work).map_err(Error::spawn(format!("create {}", work.display())))?;
    for (path, text) in [
        (&secrets.key_file, key),
        (&secrets.passphrase_file, passphrase),
    ] {
        let _ = std::fs::remove_file(path);
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .and_then(|mut f| f.write_all(text.as_bytes()));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&secrets.key_file);
            let _ = std::fs::remove_file(&secrets.passphrase_file);
            return Err(Error::spawn(format!("write {}", path.display()))(e));
        }
    }
    Ok(secrets)
}

/// What `apt sign` is asked.
pub struct Request<'a> {
    pub dir: &'a Path,
    pub work: &'a Path,
    pub fingerprints: &'a str,
    pub key_file: &'a Path,
    pub passphrase_file: &'a Path,
}

/// A fingerprint as hex digits (the spaces gpg prints between groups are tolerated), at least 16
/// of them: upper case, without spaces.
pub(super) fn clean_fingerprint(raw: &str) -> Option<String> {
    let clean: String = raw.chars().filter(|c| *c != ' ').collect();
    (clean.len() >= 16 && clean.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| clean.to_ascii_uppercase())
}

/// The fingerprints of the signing subkeys: one, or several separated by commas. A list that is
/// empty, has an empty element or names a key twice is refused.
pub(crate) fn split_fingerprints(raw: &str) -> Result<Vec<String>, Error> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split(',') {
        match clean_fingerprint(part) {
            Some(fpr) if !out.contains(&fpr) => out.push(fpr),
            _ => {
                return Err(Error::Usage(
                    "--fingerprint is not a fingerprint, or a comma-separated list of different ones"
                        .to_owned(),
                ));
            }
        }
    }
    Ok(out)
}

pub(crate) fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

/// The gpg half: imports the key, checks every fingerprint is a secret key, signs both files and
/// verifies them.
fn gpg_steps(
    gpg: &dyn Gpg,
    home: &Path,
    fingerprints: &[String],
    r: &Request,
) -> Result<(), Error> {
    let text = |p: &Path| p.display().to_string();
    let home_arg = text(home);
    let base = |extra: &[&str]| {
        let mut v = args(&["--homedir", &home_arg, "--batch", "--no-tty", "--yes"]);
        v.extend(args(extra));
        v
    };
    let (release, in_release, detached) = (
        text(&r.dir.join("Release")),
        text(&r.dir.join("InRelease")),
        text(&r.dir.join("Release.gpg")),
    );
    let run = |what: &str, argv: Vec<String>| -> Result<(), Error> {
        let answer = gpg.run(&argv)?;
        if answer.ok {
            Ok(())
        } else {
            Err(Error::Failed(format!("{what}: {}", answer.stderr.trim())))
        }
    };
    run(
        "gpg could not import the key",
        base(&["--import", &text(r.key_file)]),
    )?;
    for fpr in fingerprints {
        run(
            &format!("the signing key is not among the secret keys that were imported ({fpr})"),
            base(&["--list-secret-keys", "--with-colons", fpr]),
        )?;
    }
    let signing = |mode: &str, input: &str, output: &str| {
        let mut v = base(&[
            "--pinentry-mode",
            "loopback",
            "--passphrase-file",
            &text(r.passphrase_file),
            "--digest-algo",
            "SHA512",
        ]);
        for fpr in fingerprints {
            v.extend(args(&["--local-user", &format!("{fpr}!")]));
        }
        v.extend(args(&["--armor", mode, "--output", output, input]));
        v
    };
    run(
        "gpg could not sign InRelease",
        signing("--clearsign", &release, &in_release),
    )?;
    run(
        "gpg could not sign Release.gpg",
        signing("--detach-sign", &release, &detached),
    )?;
    run(
        "InRelease does not verify",
        base(&["--verify", &in_release]),
    )?;
    run(
        "Release.gpg does not verify",
        base(&["--verify", &detached, &release]),
    )
}

/// An empty directory `home` (mode 0700) below `work`, which is made when missing.
pub(crate) fn private_dir(work: &Path, home: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(home) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    std::fs::create_dir_all(work)?;
    std::fs::DirBuilder::new().mode(0o700).create(home)
}

/// The `apt-sign` verb: the lines to print.
///
/// # Errors
/// A fingerprint that is not one, a `Release` that is not there, a private directory that cannot
/// be made, or a gpg step that fails.
pub fn sign(gpg: &dyn Gpg, r: &Request, output: Option<&Path>) -> Result<Vec<String>, Error> {
    let done = sign_files(gpg, r);
    // The key never outlives the verb, whatever the verb came to (a flag out of its form
    // included). A runner is discarded after its job all the same, so a file that is gone already
    // is not a failure.
    let _ = std::fs::remove_dir_all(r.work.join("gnupg"));
    let _ = std::fs::remove_file(r.key_file);
    let _ = std::fs::remove_file(r.passphrase_file);
    let fingerprints = done?;
    if let Some(output) = output {
        crate::channels::append_output(output, &["signed=true".to_owned()])?;
    }
    Ok(vec![format!(
        "signed Release with {} subkey(s), {}: InRelease and Release.gpg verify",
        fingerprints.len(),
        fingerprints.join(", ")
    )])
}

/// The signing itself: the fingerprints that signed.
fn sign_files(gpg: &dyn Gpg, r: &Request) -> Result<Vec<String>, Error> {
    let fingerprints = split_fingerprints(r.fingerprints)?;
    let release = r.dir.join("Release");
    if !release.is_file() {
        return Err(Error::Failed(format!(
            "{} does not exist: nothing to sign",
            release.display()
        )));
    }
    let home = r.work.join("gnupg");
    private_dir(r.work, &home).map_err(Error::spawn(format!("create {}", home.display())))?;
    gpg_steps(gpg, &home, &fingerprints, r)?;
    Ok(fingerprints)
}

#[cfg(test)]
mod tests;
