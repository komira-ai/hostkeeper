//! The privilege drop of `pxe serve` (docs/design/network-install.md, section 3,
//! "Where it runs, and as whom"): it starts as root only to bind its ports, then becomes the
//! unprivileged user `--user-id` and `--group-id` name before it reads the token file (opened
//! before the drop and kept as a descriptor) or parses a packet.
//!
//! The drop sets the thread's supplementary groups (to the one group, when the thread is root),
//! then its real, effective and saved group, then its real, effective and saved user, and then
//! checks the result: the thread is that user and group, and taking root back is refused. Linux
//! keeps credentials per thread and a thread inherits them from the one that makes it, so the
//! drop is made on the one thread there is, before the runtime makes any other.
//!
//! The ids are numbers, not names: a name would need the account database, which the tests of
//! the command do not read (they pass their own user's ids).

use rustix::process::{Gid, Uid, getegid, geteuid, getgid, getuid};
use rustix::thread::{set_thread_groups, set_thread_res_gid, set_thread_res_uid};

/// The user and group to drop to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropTo {
    /// `--user-id`.
    pub uid: u32,
    /// `--group-id`.
    pub gid: u32,
}

/// Why the drop failed. Nothing is served after it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrivilegeError {
    /// `--user-id 0` or `--group-id 0`: root is not a user to drop to.
    #[error("--user-id and --group-id must name an unprivileged user and group, not root (0)")]
    Root,
    /// A system call refused.
    #[error("cannot set the {what} to {id}: {errno}")]
    Set {
        /// What was set: the groups, the group or the user.
        what: &'static str,
        /// The id it was set to.
        id: u32,
        /// What the system answered.
        errno: String,
    },
    /// The thread is not the user and group it dropped to.
    #[error("after the drop the process is uid {uid} gid {gid}, not uid {want_uid} gid {want_gid}")]
    NotDropped {
        uid: u32,
        gid: u32,
        want_uid: u32,
        want_gid: u32,
    },
    /// Root could be taken back.
    #[error("after the drop to uid {0} the process could become root again")]
    Regained(u32),
}

/// Drops the calling thread to `to`, as the module documentation says.
///
/// # Errors
/// [`PrivilegeError`]: `to` is root, a call refused, or the result is not the drop asked for.
pub fn drop_to(to: DropTo) -> Result<(), PrivilegeError> {
    if to.uid == 0 || to.gid == 0 {
        return Err(PrivilegeError::Root);
    }
    let set = |what: &'static str, id: u32| {
        move |errno: rustix::io::Errno| PrivilegeError::Set {
            what,
            id,
            errno: errno.to_string(),
        }
    };
    let gid = Gid::from_raw(to.gid);
    let uid = Uid::from_raw(to.uid);
    // Only root may set the supplementary groups; an unprivileged start has nothing to drop
    // there, and cannot change them, so its refusal there is no error.
    let root = geteuid().is_root();
    let groups = set_thread_groups(&[gid]);
    (groups.is_ok() | !root)
        .then_some(())
        .ok_or_else(|| set("groups", to.gid)(groups.err().unwrap_or(rustix::io::Errno::PERM)))?;
    set_thread_res_gid(gid, gid, gid).map_err(set("group", to.gid))?;
    set_thread_res_uid(uid, uid, uid).map_err(set("user", to.uid))?;
    let now = (getuid(), geteuid(), getgid(), getegid());
    (now == (uid, uid, gid, gid))
        .then_some(())
        .ok_or(PrivilegeError::NotDropped {
            uid: now.1.as_raw(),
            gid: now.3.as_raw(),
            want_uid: to.uid,
            want_gid: to.gid,
        })?;
    let root = Uid::ROOT;
    match set_thread_res_uid(root, root, root) {
        Ok(()) => Err(PrivilegeError::Regained(to.uid)),
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
#[path = "privilege_tests.rs"]
mod tests;
