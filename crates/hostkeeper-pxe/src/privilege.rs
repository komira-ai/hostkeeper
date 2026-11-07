//! The privilege drop of `hostkeeper-pxe serve` (docs/design/network-install.md, section 3,
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
    // there, and cannot change them.
    if geteuid().is_root() {
        set_thread_groups(&[gid]).map_err(set("groups", to.gid))?;
    }
    set_thread_res_gid(gid, gid, gid).map_err(set("group", to.gid))?;
    set_thread_res_uid(uid, uid, uid).map_err(set("user", to.uid))?;
    let now = (getuid(), geteuid(), getgid(), getegid());
    if now != (uid, uid, gid, gid) {
        return Err(PrivilegeError::NotDropped {
            uid: now.1.as_raw(),
            gid: now.3.as_raw(),
            want_uid: to.uid,
            want_gid: to.gid,
        });
    }
    let root = Uid::ROOT;
    match set_thread_res_uid(root, root, root) {
        Ok(()) => Err(PrivilegeError::Regained(to.uid)),
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test runs as an unprivileged user and drops to itself: the drop succeeds, and the
    /// thread is that user and group. Root is refused as a target before any call, and a drop
    /// to another user, which an unprivileged process cannot make, is refused naming the call.
    ///
    /// Catches a drop that leaves the process able to become root, one that accepts root as
    /// the user to drop to, and one that reports success when a call failed.
    #[test]
    fn an_unprivileged_process_drops_to_itself_and_refuses_root_or_another_user() {
        let me = DropTo {
            uid: getuid().as_raw(),
            gid: getgid().as_raw(),
        };
        assert_ne!(me.uid, 0, "the tests run unprivileged");
        assert_eq!(drop_to(me), Ok(()));
        assert_eq!((getuid().as_raw(), getgid().as_raw()), (me.uid, me.gid));
        assert_eq!(
            drop_to(DropTo {
                uid: 0,
                gid: me.gid
            }),
            Err(PrivilegeError::Root)
        );
        assert_eq!(
            drop_to(DropTo {
                uid: me.uid,
                gid: 0
            }),
            Err(PrivilegeError::Root)
        );
        let other = DropTo {
            uid: me.uid,
            gid: me.gid.wrapping_add(1).max(1),
        };
        let refused = drop_to(other).expect_err("another group");
        assert!(
            matches!(&refused, PrivilegeError::Set { what: "group", id, .. } if *id == other.gid),
            "{refused:?}"
        );
        assert_eq!(
            refused.to_string(),
            format!(
                "cannot set the group to {}: {}",
                other.gid,
                rustix::io::Errno::PERM
            )
        );
        assert_eq!(
            PrivilegeError::Root.to_string(),
            "--user-id and --group-id must name an unprivileged user and group, not root (0)"
        );
        assert_eq!(
            PrivilegeError::NotDropped {
                uid: 5,
                gid: 6,
                want_uid: 7,
                want_gid: 8
            }
            .to_string(),
            "after the drop the process is uid 5 gid 6, not uid 7 gid 8"
        );
        assert_eq!(
            PrivilegeError::Regained(7).to_string(),
            "after the drop to uid 7 the process could become root again"
        );
    }
}
