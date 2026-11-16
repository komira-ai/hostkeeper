//! The files of the sshd setup and the record they are made from
//! (docs/design/ssh-through-coordinator.md section 3): the drop-in, the CA keys, the principals
//! file of the login user and the revoked serials, and the record's checks before any of them is
//! written.
//!
//! The record is checked first, because what it says goes into files sshd reads as root: the login
//! user and the principal are words of a short alphabet, the CA keys are `type base64` pairs with
//! the comment dropped, so a record cannot carry a second line or an option into a file. Writing
//! goes through [`crate::mac_files`]'s replace (a temporary file beside the target, renamed over
//! it, written only when the bytes differ), and [`Snapshot`] keeps what was there so a failed
//! step puts it back.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use hostkeeper_link::{PASSWORD_OFF, PushedSsh};

use crate::mac_files::{Rendered, Replace, replace};

/// The state a setup ends in: the closed set of the design's section 9 (`SshReport.state`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SshState {
    Applied,
    Disabled,
    BlockedByAllowList,
    SshdMissing,
    ConfigInvalid,
    ReloadFailed,
    WouldLockOut,
    NoLoginUser,
    CertLoginNotPossible,
    ShadowedByStock,
    Unsupported,
}

impl SshState {
    /// Its wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            SshState::Applied => "applied",
            SshState::Disabled => "disabled",
            SshState::BlockedByAllowList => "blocked_by_allow_list",
            SshState::SshdMissing => "sshd_missing",
            SshState::ConfigInvalid => "config_invalid",
            SshState::ReloadFailed => "reload_failed",
            SshState::WouldLockOut => "would_lock_out",
            SshState::NoLoginUser => "no_login_user",
            SshState::CertLoginNotPossible => "cert_login_not_possible",
            SshState::ShadowedByStock => "shadowed_by_stock",
            SshState::Unsupported => "unsupported",
        }
    }
}

/// A setup that stopped, with the state it ends in and the keyword or class that says why: never
/// an account name or a secret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub state: SshState,
    pub detail: &'static str,
}

impl Refused {
    #[must_use]
    pub const fn new(state: SshState, detail: &'static str) -> Refused {
        Refused { state, detail }
    }
}

/// Where the files go.
#[derive(Clone, Debug)]
pub struct SshPaths {
    /// The directory sshd's stock configuration includes: `/etc/ssh/sshd_config.d`.
    pub config_dir: PathBuf,
    /// The directory of the files the drop-in names: `/etc/ssh/hostkeeper`.
    pub own_dir: PathBuf,
}

/// The name of the drop-in; `10-` so that it is read early where the directory is included early.
const DROP_IN: &str = "10-hostkeeper.conf";

impl SshPaths {
    /// The places of a Linux host.
    #[must_use]
    pub fn linux() -> SshPaths {
        SshPaths {
            config_dir: PathBuf::from("/etc/ssh/sshd_config.d"),
            own_dir: PathBuf::from("/etc/ssh/hostkeeper"),
        }
    }

    #[must_use]
    pub fn drop_in(&self) -> PathBuf {
        self.config_dir.join(DROP_IN)
    }

    #[must_use]
    pub fn ca_keys(&self) -> PathBuf {
        self.own_dir.join("user_ca.pub")
    }

    #[must_use]
    pub fn revoked(&self) -> PathBuf {
        self.own_dir.join("revoked")
    }

    /// The principals file of `user`.
    #[must_use]
    pub fn principals(&self, user: &str) -> PathBuf {
        self.own_dir.join(format!("principals.{user}"))
    }

    /// Where the drop-in is written to be checked, in a place no configuration includes.
    #[must_use]
    pub fn candidate(&self) -> PathBuf {
        self.own_dir.join("candidate.conf")
    }

    /// The principals file name with `%u`, as the drop-in writes it.
    #[must_use]
    pub fn principals_template(&self) -> String {
        format!("{}/principals.%u", self.own_dir.display())
    }
}

/// What a record asks for, once checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// `type base64` lines.
    pub ca_keys: Vec<String>,
    pub login_user: String,
    pub principal: String,
    pub passwords_off: bool,
    pub serials: Vec<u64>,
}

/// The key types a CA public key may have.
const KEY_TYPES: [&str; 6] = [
    "ssh-ed25519",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "ssh-rsa",
    "sk-ssh-ed25519@openssh.com",
];

/// The most CA keys a record carries: the one in use and the next, during a rotation.
const MAX_CA_KEYS: usize = 2;

/// Whether `word` is made of the characters of an account name or a principal.
fn plain(word: &str, longest: usize) -> bool {
    !word.is_empty()
        && word.len() <= longest
        && !word.starts_with('-')
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The `type base64` pair of an `authorized_keys` line, with the comment dropped; none when the
/// line is anything else (an option, a second line, a key of another kind).
fn ca_key(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let (kind, body) = (words.next()?, words.next()?);
    let clean = KEY_TYPES.contains(&kind)
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='));
    clean.then(|| format!("{kind} {body}"))
}

/// The plan of an enabled `access`, or what is wrong with it.
///
/// # Errors
/// `no_login_user` for an empty login user; `config_invalid` with the field for any other record
/// that cannot be written safely.
pub fn plan(access: &PushedSsh) -> Result<Plan, Refused> {
    if access.login_user.is_empty() {
        return Err(Refused::new(SshState::NoLoginUser, "login_user"));
    }
    if !plain(&access.login_user, 32) {
        return Err(Refused::new(SshState::ConfigInvalid, "login_user"));
    }
    let principal_ok = access
        .principal
        .strip_prefix("hk-host-")
        .is_some_and(|id| plain(id, 120));
    if !principal_ok {
        return Err(Refused::new(SshState::ConfigInvalid, "principal"));
    }
    let keys: Option<Vec<String>> = access.ca_public_keys.iter().map(|k| ca_key(k)).collect();
    let keys = keys
        .filter(|keys| (1..=MAX_CA_KEYS).contains(&keys.len()))
        .ok_or(Refused::new(SshState::ConfigInvalid, "ca_public_keys"))?;
    let mut serials = access.revoked_serials.clone();
    serials.sort_unstable();
    serials.dedup();
    Ok(Plan {
        ca_keys: keys,
        login_user: access.login_user.clone(),
        principal: access.principal.clone(),
        passwords_off: access.password_auth == PASSWORD_OFF,
        serials,
    })
}

/// The bytes of the drop-in.
#[must_use]
pub fn render_drop_in(paths: &SshPaths, passwords_off: bool) -> Vec<u8> {
    let mut text = format!(
        "# Written by hostkeeper (ssh-v1); the coordinator's record replaces it. Do not edit.\n\
         TrustedUserCAKeys {}\n\
         AuthorizedPrincipalsFile {}\n\
         RevokedKeys {}\n",
        paths.ca_keys().display(),
        paths.principals_template(),
        paths.revoked().display(),
    );
    if passwords_off {
        text.push_str("PasswordAuthentication no\nKbdInteractiveAuthentication no\n");
    }
    text.into_bytes()
}

/// The bytes of the CA keys file.
#[must_use]
pub fn render_ca_keys(keys: &[String]) -> Vec<u8> {
    keys.iter().map(|key| format!("{key}\n")).collect::<String>().into_bytes()
}

/// The files a plan wants, by path.
pub struct Wanted {
    pub files: Vec<(PathBuf, Vec<u8>)>,
}

/// What `paths` and `plan` want written, the list of revoked serials as `revoked` when it is to
/// be written (none: the file is left as it is).
#[must_use]
pub fn wanted(paths: &SshPaths, plan: &Plan, revoked: Option<Vec<u8>>) -> Wanted {
    let mut files = vec![
        (paths.drop_in(), render_drop_in(paths, plan.passwords_off)),
        (paths.ca_keys(), render_ca_keys(&plan.ca_keys)),
        (
            paths.principals(&plan.login_user),
            format!("{}\n", plan.principal).into_bytes(),
        ),
    ];
    if let Some(bytes) = revoked {
        files.push((paths.revoked(), bytes));
    }
    Wanted { files }
}

/// What a set of paths held, to put back.
pub struct Snapshot(Vec<(PathBuf, Option<Vec<u8>>)>);

/// Every `principals.*` file in the directory of the setup's own files, but `keep`.
fn other_principals(paths: &SshPaths, keep: Option<&Path>) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(&paths.own_dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("principals."))
        })
        .filter(|path| Some(path.as_path()) != keep)
        .collect();
    found.sort();
    found
}

fn held(path: &Path) -> Option<Vec<u8>> {
    fs::read(path).ok()
}

impl Snapshot {
    /// What every file of the setup holds now: the drop-in, the CA keys, the revoked list and
    /// each principals file.
    #[must_use]
    pub fn of(paths: &SshPaths) -> Snapshot {
        let mut every = vec![paths.drop_in(), paths.ca_keys(), paths.revoked()];
        every.extend(other_principals(paths, None));
        Snapshot(
            every
                .into_iter()
                .map(|path| {
                    let bytes = held(&path);
                    (path, bytes)
                })
                .collect(),
        )
    }

    /// Puts every file back as it was: written where it held bytes, removed where it held none.
    /// A file that cannot be put back is named in the error.
    ///
    /// # Errors
    /// The paths that could not be restored.
    pub fn restore(&self, owner: u32, paths: &SshPaths) -> Result<(), String> {
        let mut failed = Vec::new();
        for (path, bytes) in &self.0 {
            let failed_now = match bytes {
                Some(bytes) => write(path, bytes, owner).is_err(),
                None => remove(path).is_err(),
            };
            if failed_now {
                failed.push(path.display().to_string());
            }
        }
        // A principals file the failed run made, which the snapshot does not list.
        for path in other_principals(paths, None) {
            if !self.0.iter().any(|(held, _)| *held == path) && remove(&path).is_err() {
                failed.push(path.display().to_string());
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(format!("not restored: {}", failed.join(", ")))
        }
    }
}

/// Makes `path` hold `bytes`.
///
/// # Errors
/// What the write says.
pub fn write(path: &Path, bytes: &[u8], owner: u32) -> Result<Rendered, String> {
    replace(path, bytes, owner, "hostkeeper-ssh").map_err(|error| match error {
        Replace::NotAFile => format!("{}: not a file", path.display()),
        Replace::Io(e) => format!("{}: {e}", path.display()),
    })
}

/// Removes `path`; nothing there is removed already.
fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Writes every file of `wanted` and removes the principals files of any other account; true
/// when anything changed.
///
/// # Errors
/// The first path that could not be written or removed.
pub fn install(paths: &SshPaths, wanted: &Wanted, owner: u32) -> Result<bool, String> {
    let mut changed = false;
    let mut keep = None;
    for (path, bytes) in &wanted.files {
        changed |= write(path, bytes, owner)? == Rendered::Written;
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("principals."))
        {
            keep = Some(path.as_path());
        }
    }
    for stale in other_principals(paths, keep) {
        remove(&stale).map_err(|e| format!("{}: {e}", stale.display()))?;
        changed = true;
    }
    Ok(changed)
}

/// Removes every file of the setup and its directories when they are empty; true when anything
/// was there.
///
/// # Errors
/// The first path that could not be removed.
pub fn uninstall(paths: &SshPaths) -> Result<bool, String> {
    let mut every = vec![paths.drop_in(), paths.ca_keys(), paths.revoked(), paths.candidate()];
    every.extend(other_principals(paths, None));
    let mut changed = false;
    for path in every {
        if path.exists() {
            remove(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            changed = true;
        }
    }
    // Left in place when anything else is in it.
    let _ = fs::remove_dir(&paths.own_dir);
    Ok(changed)
}

/// Whether every file of `wanted` holds its bytes now, and the revoked list is there.
#[must_use]
pub fn current(paths: &SshPaths, wanted: &Wanted) -> bool {
    wanted
        .files
        .iter()
        .all(|(path, bytes)| held(path).as_deref() == Some(bytes.as_slice()))
        && paths.revoked().is_file()
}

#[cfg(test)]
#[path = "ssh_files_tests.rs"]
mod tests;
