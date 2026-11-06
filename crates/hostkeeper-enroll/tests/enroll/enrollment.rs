//! What the enroller says of the host's enrollment (`Enroller::enrollment`), the view the
//! daemon's admin `Status` reports: read from the pki directory as a step reads it.

use std::fs;

use hostkeeper_enroll::{Enrollment, HostId, RequestId};

use crate::support::{HOST, T0, WEEK, World, csr_spki};

/// Catches a host reported pending before it has a key, or a key hash that is not the one the
/// coordinator lists: a pki directory with nothing in it is unenrolled; once the first step
/// posted, the host is pending with the request id of the key it posted, which is the
/// `key_sha256` the coordinator shows.
#[test]
fn a_new_host_is_unenrolled_then_pending_with_the_key_it_posted() {
    let world = World::new("view-pending");
    let mut enroller = world.open();
    assert_eq!(enroller.enrollment(), Enrollment::Unenrolled);
    enroller.run_once().expect("post");
    let posted = RequestId::of_spki(&csr_spki(&world.coordinator().enrolls[0].csr_der));
    assert_eq!(
        enroller.enrollment(),
        Enrollment::Pending {
            key_sha256: posted.clone()
        }
    );
    assert_eq!(posted.as_str().len(), 64);
}

/// Catches a host whose certificate is not reported, or reported with another validity than the
/// one stored, or one that expired reported as no longer enrolled: once approved, the host is
/// enrolled as its host id until the end of the seven days it was issued for, and past that end
/// still enrolled with that end, which is now in the past.
#[test]
fn an_enrolled_host_reports_its_host_id_and_not_after_even_once_expired() {
    let world = World::new("view-enrolled");
    let enroller = world.enrolled();
    let enrolled = Enrollment::Enrolled {
        host_id: HostId::new(HOST).expect("a host id"),
        not_after: T0 + WEEK,
    };
    let sites = world.sites();
    assert_eq!(enroller.enrollment(), enrolled);
    world.set_now(T0 + 2 * WEEK);
    assert_eq!(enroller.enrollment(), enrolled);
    assert_eq!(world.sites(), sites, "the view reads no clock");
}

/// Catches a corrupt certificate or pending key reported as a state it is not: a current
/// generation whose certificate does not parse, and a pending key that is not a key, are
/// unreadable, with the reason a step gives.
#[test]
fn a_corrupt_generation_or_pending_key_is_unreadable_with_the_steps_reason() {
    let world = World::new("view-corrupt-cert");
    let enroller = world.enrolled();
    fs::write(world.pki().join("gen-1/cert.pem"), "not a certificate").expect("cert");
    assert_eq!(
        enroller.enrollment(),
        Enrollment::Unreadable(
            "the pki directory's generation 1 is not a certificate and key of this host".to_owned()
        )
    );

    let world = World::new("view-corrupt-key");
    let mut enroller = world.open();
    enroller.run_once().expect("post");
    let key = world.pki().join("pending/key");
    fs::remove_file(&key).expect("unlink");
    crate::support::write_private(&key, b"not a key");
    let Enrollment::Unreadable(why) = enroller.enrollment() else {
        panic!("a pending key that is not a key is unreadable");
    };
    assert!(why.starts_with("the host key: "), "{why}");
}

/// Catches a pki directory that cannot be read reported as unenrolled: a pending directory that
/// is a link, not the daemon's own directory, is unreadable, naming it.
#[test]
fn a_pending_directory_that_is_a_link_is_unreadable() {
    let world = World::new("view-not-private");
    let mut enroller = world.open();
    enroller.run_once().expect("post");
    let pending = world.pki().join("pending");
    let moved = world.pki().join("pending.moved");
    fs::rename(&pending, &moved).expect("move it");
    std::os::unix::fs::symlink(&moved, &pending).expect("link it");
    assert_eq!(
        enroller.enrollment(),
        Enrollment::Unreadable(format!(
            "{pending:?} is not the daemon's own: a directory or key must be owned by it, not a \
             link, and closed to group and others"
        ))
    );
}
