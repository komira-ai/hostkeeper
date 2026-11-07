//! The privilege drop, as an unprivileged test can make it.

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
