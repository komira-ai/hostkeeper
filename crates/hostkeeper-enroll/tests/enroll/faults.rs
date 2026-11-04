//! A failed fsync, or a crash, at each point of the store's writes: the key is never posted
//! before it is durable, a generation that was not published leaves nothing behind, and one that
//! was published is reported even when what follows fails.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hostkeeper_enroll::{EnrollRequest, Error, Fault, FaultGate, HostId, Outcome, Refusal};

use crate::support::{HOST, T0, WEEK, World};

/// What the gate and the coordinator saw, in order.
type Seen = Arc<Mutex<Vec<String>>>;

/// A gate that fails at `point` the first `times` times it is crossed, and records every
/// crossing in `seen`.
fn gate(point: Fault, times: usize, seen: &Seen) -> FaultGate {
    let seen = Arc::clone(seen);
    let left = Mutex::new(times);
    Arc::new(move |fault| {
        seen.lock().expect("seen").push(format!("{fault:?}"));
        let mut left = left.lock().expect("left");
        let fail = (fault == point) & (*left > 0);
        if fail {
            *left -= 1;
        }
        fail
    })
}

/// Records each `Enroll` the coordinator receives in `seen`.
fn record_enrolls(world: &World, seen: &Seen) {
    let seen = Arc::clone(seen);
    world.coordinator().on_enroll = Some(Box::new(move |_: &EnrollRequest| {
        seen.lock().expect("seen").push("Enroll".to_owned());
    }));
}

fn seen() -> Seen {
    Arc::new(Mutex::new(Vec::new()))
}

/// Catches a pending key whose directory fsync failed after its rename being posted on the
/// next step as it is: it is fsynced first, and only then posted.
#[test]
fn a_pending_key_whose_directory_fsync_failed_is_fsynced_before_it_is_posted() {
    let world = World::new("key-dir-fsync");
    let seen = seen();
    record_enrolls(&world, &seen);
    let mut enroller = world.open_with_faults(gate(Fault::PendingKeyRenamed, 1, &seen));
    let refused = enroller.run_once().expect_err("the fsync failed");
    assert_eq!(
        refused.to_string(),
        "cannot use the pki directory: PendingKeyRenamed failed"
    );
    assert!(world.coordinator().enrolls.is_empty());
    assert!(
        world.pki().join("pending/key").is_file(),
        "renamed into place"
    );

    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    assert_eq!(
        *seen.lock().expect("seen"),
        ["PendingKeyRenamed", "PendingKeyReused", "Enroll"]
    );
}

/// Catches a reused pending key posted although making it durable failed.
#[test]
fn a_reused_pending_key_that_cannot_be_made_durable_is_not_posted() {
    let world = World::new("key-reuse-fsync");
    let mut enroller = world.open();
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    drop(enroller);

    let mut enroller = world.open_with_faults(gate(Fault::PendingKeyReused, 1, &seen()));
    let refused = enroller.run_once().expect_err("not durable");
    assert_eq!(
        refused.to_string(),
        "cannot use the pki directory: PendingKeyReused failed"
    );
    assert_eq!(world.coordinator().enrolls.len(), 1, "not posted again");
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    assert_eq!(world.coordinator().enrolls.len(), 2);
}

/// Catches a generation that failed before `COMPLETE` being left behind, so that failed attempts
/// pile up, or taking the pending key with it: the next attempt writes generation 1 again.
#[test]
fn a_failure_before_completion_removes_the_partial_generation_and_keeps_the_key() {
    let world = World::new("partial-generation");
    let mut enroller = world.open_with_faults(gate(Fault::GenerationFiles, 2, &seen()));
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    world.coordinator().approve(HOST);
    for _ in 0..2 {
        let refused = enroller.run_once().expect_err("not written");
        assert_eq!(
            refused.to_string(),
            "cannot use the pki directory: GenerationFiles failed"
        );
        assert_eq!(world.names(), [".lock", "SCHEMA", "pending"]);
        assert!(world.pki().join("pending/key").is_file());
    }
    let Outcome::Enrolled(identity) = enroller.run_once().expect("enroll") else {
        panic!("not enrolled");
    };
    assert_eq!(identity.generation, 1);
    assert_eq!(
        world.names(),
        [".lock", "SCHEMA", "current", "gen-1", "pending"]
    );
}

/// Catches a published generation hidden by a failure of the fsync that follows the switch of
/// `current`: the enrollment is reported, and the pending key left behind is removed by the
/// next open.
#[test]
fn a_publication_whose_fsync_failed_still_reports_the_enrollment() {
    let world = World::new("current-fsync");
    let mut enroller = world.open_with_faults(gate(Fault::CurrentRenamed, 1, &seen()));
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    world.coordinator().approve(HOST);
    let Outcome::Enrolled(identity) = enroller.run_once().expect("enrolled") else {
        panic!("not enrolled");
    };
    assert_eq!(identity.generation, 1);
    assert_eq!(world.current(), Some(PathBuf::from("gen-1")));
    assert!(
        world.pki().join("pending/key").is_file(),
        "cleanup did not run"
    );
    drop(enroller);
    drop(world.open());
    assert!(
        !world.pki().join("pending/key").exists(),
        "the open finished it"
    );
}

/// Catches a failure to remove the pending key after publication being reported as a failed
/// enrollment.
#[test]
fn a_cleanup_failure_after_publication_is_not_an_error() {
    let world = World::new("cleanup");
    let mut enroller = world.open_with_faults(gate(Fault::Published, 1, &seen()));
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    world.coordinator().approve(HOST);
    let outcome = enroller.run_once().expect("enrolled");
    assert!(matches!(outcome, Outcome::Enrolled(_)), "{outcome:?}");
    assert!(world.pki().join("pending/key").is_file());
    drop(enroller);
    drop(world.open());
    assert!(!world.pki().join("pending/key").exists());
}

/// Catches a renewal whose publication fsync failed being reported as failed although the new
/// generation is current.
#[test]
fn a_renewal_whose_publication_fsync_failed_reports_the_renewal() {
    let world = World::new("renew-fsync");
    drop(world.enrolled());
    let mut enroller = world.open_with_faults(gate(Fault::CurrentRenamed, 1, &seen()));
    world.set_now(T0 + WEEK / 2);
    let Outcome::Renewed(identity) = enroller.run_once().expect("renewed") else {
        panic!("not renewed");
    };
    assert_eq!(identity.generation, 2);
    assert_eq!(world.current(), Some(PathBuf::from("gen-2")));
}

/// Catches a host stuck on a certificate that was approved and issued but expired before it
/// could be installed: the step refuses it, then posts the same key as `renewal_of` the host it
/// named, and one approval installs a new one.
#[test]
fn an_issued_certificate_that_expired_before_it_was_installed_returns_through_renewal_of() {
    let world = World::new("expired-before-install");
    let mut enroller = world.open_with_faults(gate(Fault::GenerationFiles, 1, &seen()));
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    let key = fs::read(world.pki().join("pending/key")).expect("key");
    world.coordinator().approve(HOST);
    assert!(enroller.run_once().is_err(), "the install failed");

    world.set_now(T0 + WEEK + 1);
    let refused = enroller.run_once().expect_err("expired");
    assert!(
        matches!(refused, Error::Refused(Refusal::NotValidNow { .. })),
        "{refused:?}"
    );
    assert_eq!(enroller.run_once().expect("posted again"), Outcome::Pending);
    {
        let c = world.coordinator();
        assert_eq!(c.enrolls.len(), 2);
        assert_eq!(c.enrolls[1].renewal_of, HostId::new(HOST));
        assert_eq!(c.enrolls[1].csr_der, c.enrolls[0].csr_der, "the same key");
    }
    world.coordinator().approve(HOST);
    let Outcome::Enrolled(identity) = enroller.run_once().expect("enrolled") else {
        panic!("not enrolled");
    };
    assert_eq!(identity.not_before, T0 + WEEK + 1);
    assert_eq!(fs::read(world.pki().join("gen-1/key")).expect("key"), key);
}
