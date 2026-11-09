//! The policy file hostkeeper holds in place while the package manager runs a package's
//! maintainer scripts (packages-native.md section 10.2): `usr/sbin/policy-rc.d` under the host's
//! root answers 101 ("action not allowed") to every `invoke-rc.d` and `deb-systemd-invoke` call,
//! so a script that starts or stops a unit through them (as debhelper's do) does nothing;
//! hostkeeper does that itself, in the order of the update. A script that calls `systemctl`
//! itself is not held back.
//!
//! A [`Guard`] writes the file before a run and takes it away when dropped. A file already there
//! that is not byte-equal to [`TEXT`] is an operator's own: it is left as it is, the host's own
//! policy applies, and nothing is removed afterwards. A file byte-equal to [`TEXT`] is one an
//! earlier run left (it was killed): it is reused and removed at the end. [`clear_stale`] takes
//! such a file away.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// The policy file's path, relative to the host's root.
pub const POLICY_RC_D: &str = "usr/sbin/policy-rc.d";

/// The policy file's text.
pub const TEXT: &str = "#!/bin/sh\n# Written by hostkeeper while it runs the package manager; removed when it ends.\nexit 101\n";

/// The policy file of `root`.
fn path_of(root: &Path) -> PathBuf {
    root.join(POLICY_RC_D)
}

/// Whether the entry at `path` is a regular file of exactly [`TEXT`]; `None`: nothing there.
fn is_ours(path: &Path) -> io::Result<Option<bool>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(Some(fs::read(path)? == TEXT.as_bytes())),
        Ok(_) => Ok(Some(false)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The policy file in place; dropping it takes the file away if it is hostkeeper's.
#[derive(Debug)]
pub struct Guard {
    path: PathBuf,
    ours: bool,
}

impl Guard {
    /// Puts the policy file in place under `root`, unless an operator's own is there.
    pub fn hold(root: &Path) -> io::Result<Guard> {
        let path = path_of(root);
        let ours = match is_ours(&path)? {
            Some(ours) => ours,
            None => {
                if let Some(dir) = path.parent() {
                    fs::create_dir_all(dir)?;
                }
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o755)
                    .open(&path)?;
                file.write_all(TEXT.as_bytes())?;
                file.set_permissions(fs::Permissions::from_mode(0o755))?;
                true
            }
        };
        Ok(Guard { path, ours })
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Removed only while it is still ours: a package the run installed may have put its own
        // policy file there, which stays. A file that cannot be removed is found again by
        // `clear_stale` at the next start.
        if self.ours && matches!(is_ours(&self.path), Ok(Some(true))) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Removes the policy file under `root` if it is byte-equal to [`TEXT`], and nothing else;
/// answers whether it removed one.
pub fn clear_stale(root: &Path) -> io::Result<bool> {
    let path = path_of(root);
    match is_ours(&path)? {
        Some(true) => {
            fs::remove_file(&path)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}
