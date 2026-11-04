//! Enrollment, renewal and return after expiry, as design section 5.7 (steps 1 to 4) states them.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hostkeeper_enroll::{
    EnrollRequest, Error, HostId, Outcome, PkiDir, Refusal, RequestId, SCHEMA, Store, StoreError,
};
use p256::ecdsa::SigningKey;
use p256::pkcs8::{DecodePrivateKey, EncodePublicKey};
use x509_cert::der::Encode;

use crate::support::{
    Args, Ca, HOST, KeyWriteFails, RowState, T0, WEEK, World, csr_spki, pkcs8, proper, signing_key,
    spki_of,
};

/// The DER SubjectPublicKeyInfo of the key in a PKCS#8 file.
fn spki_of_pkcs8(der: &[u8]) -> Vec<u8> {
    let key = SigningKey::from_pkcs8_der(der).expect("a PKCS#8 P-256 key");
    key.verifying_key()
        .to_public_key_der()
        .expect("SPKI")
        .as_bytes()
        .to_vec()
}

/// Catches a key made again on restart, or a request that varies between two posts of one key:
/// either gives the coordinator a second request id and a second pending row.
#[test]
fn a_restarted_enroller_reposts_the_same_request() {
    let world = World::new("restart");
    let mut first = world.open();
    assert_eq!(first.run_once().expect("post"), Outcome::Pending);
    // A second step of the same process polls without posting again.
    assert_eq!(first.run_once().expect("poll"), Outcome::Pending);
    assert_eq!(world.coordinator().enrolls.len(), 1);
    drop(first);

    let mut second = world.open();
    assert_eq!(second.run_once().expect("repost"), Outcome::Pending);
    let c = world.coordinator();
    assert_eq!(c.enrolls.len(), 2);
    let (a, b) = (&c.enrolls[0], &c.enrolls[1]);
    assert_eq!(a.csr_der, b.csr_der, "the same request bytes");
    assert_eq!(csr_spki(&a.csr_der), csr_spki(&b.csr_der));
    assert_eq!(a.claims, b.claims);
    assert_eq!(a.image, "image-1");
    assert_eq!(a.renewal_of, None);
    assert_eq!(a, b);
    let ids: Vec<&RequestId> = c.rows.keys().collect();
    assert_eq!(ids, [&RequestId::of_spki(&csr_spki(&a.csr_der))]);
    assert_eq!(c.rows[ids[0]].state, RowState::Pending);
}

/// Catches a request posted before its key is on disk, or posted although the key's write
/// failed: a crash after such a post would leave an approval for a key the host never kept.
#[test]
fn the_key_is_durable_before_the_request_is_posted() {
    let world = World::new("durable");
    let pki = world.pki();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    world.coordinator().on_enroll = Some(Box::new(move |request: &EnrollRequest| {
        let key = pki.join("pending/key");
        let mode = fs::metadata(&key)
            .expect("the key exists")
            .permissions()
            .mode();
        let dir_mode = fs::metadata(pki.join("pending"))
            .expect("dir")
            .permissions()
            .mode();
        let on_disk = spki_of_pkcs8(&fs::read(&key).expect("read the key"));
        record.lock().expect("seen").push((
            mode & 0o777,
            dir_mode & 0o777,
            on_disk == csr_spki(&request.csr_der),
        ));
    }));
    let mut enroller = world.open();
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    assert_eq!(*seen.lock().expect("seen"), [(0o600, 0o700, true)]);

    // The key's write fails: nothing reaches the coordinator.
    let failing = World::new("durable-fails");
    let store = Box::new(KeyWriteFails(PkiDir::new(&failing.pki())));
    let ports = failing.ports_with(Some(store), None);
    let mut enroller = hostkeeper_enroll::Enroller::open(failing.settings(), ports).expect("open");
    let refused = enroller.run_once().expect_err("the key write failed");
    assert!(
        matches!(refused, Error::Store(StoreError::Io(_))),
        "{refused:?}"
    );
    assert_eq!(
        refused.to_string(),
        "cannot use the pki directory: fsync failed"
    );
    let c = failing.coordinator();
    assert!(c.enrolls.is_empty(), "{:?}", c.enrolls);
    assert!(c.polls.is_empty());
}

/// Catches a host that tries to renew with an expired certificate, or that makes a new key to
/// return: it must post `renewal_of` with the key it holds, and write a new generation once an
/// operator approves it again.
#[test]
fn an_enroller_offline_past_expiry_posts_a_renewal_request() {
    let world = World::new("expiry");
    let mut enroller = world.enrolled();
    let first_key = fs::read(world.pki().join("gen-1/key")).expect("gen-1 key");
    let spki = world
        .coordinator()
        .rows
        .values()
        .next()
        .expect("row")
        .spki_der
        .clone();

    world.set_now(T0 + WEEK + 1);
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    {
        let c = world.coordinator();
        let last = c.enrolls.last().expect("an enroll");
        assert_eq!(last.renewal_of, HostId::new(HOST));
        assert_eq!(csr_spki(&last.csr_der), spki);
        assert!(c.renews.is_empty(), "{:?}", c.renews);
        let row = c.rows.values().next().expect("row");
        assert_eq!(row.renewal_of, HostId::new(HOST));
        assert_eq!(row.state, RowState::Pending);
    }
    assert_eq!(world.current(), Some(PathBuf::from("gen-1")));

    world.coordinator().approve(HOST);
    let Outcome::Enrolled(identity) = enroller.run_once().expect("return") else {
        panic!("not enrolled");
    };
    assert_eq!(identity.generation, 2);
    assert_eq!(identity.host_id.as_str(), HOST);
    assert_eq!(identity.not_before, T0 + WEEK + 1);
    assert_eq!(world.current(), Some(PathBuf::from("gen-2")));
    assert_eq!(
        fs::read(world.pki().join("gen-2/key")).expect("gen-2 key"),
        first_key
    );
    assert!(world.coordinator().renews.is_empty());
    assert_eq!(world.coordinator().rows.len(), 1, "no second key");
}

/// Catches a pin that is not compared, or compared after something was sent: a coordinator
/// presenting another CA sees nothing, and no key is made.
#[test]
fn an_untrusted_coordinator_certificate_is_refused() {
    let world = World::new("untrusted");
    let other = Ca::new(0x71);
    world.coordinator().presented = other.der.clone();
    let mut enroller = world.open();
    let refused = enroller.run_once().expect_err("untrusted");
    let presented = crate::support::sha256_hex(&other.der);
    assert!(
        matches!(&refused, Error::UntrustedCoordinator { presented: p } if *p == presented),
        "{refused:?}"
    );
    assert_eq!(
        refused.to_string(),
        format!(
            "the coordinator presents a CA whose SHA-256 is {presented}, not the pinned \
             ca_sha256; nothing was sent"
        )
    );
    {
        let c = world.coordinator();
        assert!(c.enrolls.is_empty() && c.polls.is_empty() && c.renews.is_empty());
    }
    assert!(!world.pki().join("pending").exists(), "no key was made");

    // The same enroller, once the coordinator presents the pinned CA, enrolls.
    let ca = world.coordinator().ca.der.clone();
    world.coordinator().presented = ca;
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    world.coordinator().approve(HOST);
    let outcome = enroller.run_once().expect("enroll");
    assert!(matches!(outcome, Outcome::Enrolled(_)), "{outcome:?}");
}

/// Catches renewal one second early or one second late, or a renewal not signed by the held
/// key (the coordinator checks the request against the enrolled key).
#[test]
fn renewal_happens_at_half_life_and_not_before() {
    let world = World::new("half-life");
    let mut enroller = world.enrolled();
    let midpoint = T0 + WEEK / 2;

    world.set_now(midpoint - 1);
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    assert!(world.coordinator().renews.is_empty());

    world.set_now(midpoint);
    let Outcome::Renewed(identity) = enroller.run_once().expect("renew") else {
        panic!("not renewed");
    };
    let c = world.coordinator();
    assert_eq!(c.renews.len(), 1);
    let enrolled = c.rows.values().next().expect("row").spki_der.clone();
    assert_eq!(csr_spki(&c.renews[0].csr_der), enrolled);
    assert_eq!(c.renews[0].host_id.as_str(), HOST);
    drop(c);
    assert_eq!(identity.generation, 2);
    assert_eq!(
        (identity.not_before, identity.not_after),
        (midpoint, midpoint + WEEK)
    );
    assert_eq!(identity.cert, world.pki().join("gen-2/cert.pem"));
    assert_eq!(identity.ca, world.pki().join("gen-2/ca.pem"));
    assert_eq!(identity.key, world.pki().join("gen-2/key"));
    assert_eq!(world.current(), Some(PathBuf::from("gen-2")));
    // The new certificate is not due until the middle of its own window.
    world.set_now(midpoint + WEEK / 2 - 1);
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
}

/// Catches a daemon that stores whatever certificate it is given: one for another key is
/// refused, at first enrollment and at renewal, and the pki directory is left as it was.
#[test]
fn a_leaf_for_another_key_is_refused() {
    let world = World::new("other-key");
    let other = spki_of(&signing_key(0x33));
    world.coordinator().issue = Some(Box::new(move |ca: &Ca, args: &Args| {
        let mut issued = proper(ca, args);
        issued.cert_pem = proper(
            ca,
            &Args {
                spki: other.clone(),
                host_id: args.host_id.clone(),
                now: args.now,
            },
        )
        .cert_pem;
        issued
    }));
    let mut enroller = world.open();
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    world.coordinator().approve(HOST);
    let refused = enroller.run_once().expect_err("another key");
    assert!(
        matches!(refused, Error::Refused(Refusal::OtherKey)),
        "{refused:?}"
    );
    assert_eq!(world.current(), None);
    assert_eq!(world.names(), [".lock", "SCHEMA", "pending"]);

    // At renewal: generation 1 stays current and no generation 2 is made.
    let world = World::new("other-key-renewal");
    let mut enroller = world.enrolled();
    let other = spki_of(&signing_key(0x34));
    world.coordinator().issue = Some(Box::new(move |ca: &Ca, args: &Args| {
        proper(
            ca,
            &Args {
                spki: other.clone(),
                host_id: args.host_id.clone(),
                now: args.now,
            },
        )
    }));
    world.set_now(T0 + WEEK / 2);
    let refused = enroller.run_once().expect_err("another key");
    assert_eq!(
        refused.to_string(),
        "the issued certificate was refused: the certificate is for another key than the one \
         this host holds"
    );
    assert_eq!(world.current(), Some(PathBuf::from("gen-1")));
    assert_eq!(
        world.names(),
        [".lock", "SCHEMA", "current", "gen-1", "pending"]
    );
}

/// Catches recovery that ignores the `COMPLETE` marker or the store lock: a generation without
/// the marker is removed and `current` stays on the complete one; a pending key whose request
/// was approved while the daemon was down ends enrolled; and a second enroller on the same
/// directory is refused until the first is dropped.
#[test]
fn a_half_written_generation_is_removed_and_the_poll_finishes_under_the_lock() {
    // A crash while generation 2 was written.
    let world = World::new("half-written");
    drop(world.enrolled());
    let half = world.pki().join("gen-2");
    fs::create_dir(&half).expect("gen-2");
    fs::write(half.join("cert.pem"), "half").expect("cert");
    let enroller = world.open();
    assert!(!half.exists(), "gen-2 was removed");
    assert_eq!(world.current(), Some(PathBuf::from("gen-1")));
    drop(enroller);

    // A crash after the request was posted; the operator approved it meanwhile.
    let world = World::new("approved-while-down");
    let mut enroller = world.open();
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    drop(enroller);
    world.coordinator().approve(HOST);
    let mut enroller = world.open();
    let Outcome::Enrolled(identity) = enroller.run_once().expect("finish") else {
        panic!("not enrolled");
    };
    assert_eq!(identity.generation, 1);
    assert!(
        !world.pki().join("pending/key").exists(),
        "the pending key moved"
    );

    // Two enrollers on one directory: the second is refused while the first lives.
    let refused = world.try_open().err().expect("locked");
    assert!(
        matches!(refused, Error::Store(StoreError::Locked(_))),
        "{refused:?}"
    );
    assert!(
        refused
            .to_string()
            .starts_with("the pki directory is locked by another enroller: "),
        "{refused}"
    );
    drop(enroller);
    let mut second = world.open();
    assert_eq!(second.run_once().expect("idle"), Outcome::Idle);
}

/// Catches a reader that takes a pki directory of a newer layout for its own, or that changes
/// it before refusing it: it is refused with a message naming both schemas, and its names and
/// mode are exactly as they were.
#[test]
fn unknown_schema_in_the_pki_directory_is_refused() {
    let world = World::new("newer-schema");
    let pki = world.pki();
    fs::create_dir_all(pki.join("gen-3")).expect("gen-3");
    fs::write(pki.join("SCHEMA"), "2\n").expect("SCHEMA");
    fs::set_permissions(&pki, fs::Permissions::from_mode(0o755)).expect("chmod");
    let before = (
        world.names(),
        fs::metadata(&pki).expect("pki").permissions().mode(),
    );
    let refused = world.try_open().err().expect("a newer schema");
    assert!(
        matches!(refused, Error::Store(StoreError::NewerSchema { found: 2 })),
        "{refused:?}"
    );
    assert_eq!(
        refused.to_string(),
        format!(
            "the pki directory has schema 2, newer than schema {SCHEMA} this release reads; it \
             is left as it is"
        )
    );
    // Not even the lock file is made, and the directory's mode is as it was.
    let after = (
        world.names(),
        fs::metadata(&pki).expect("pki").permissions().mode(),
    );
    assert_eq!(after, before);
    assert_eq!(before.0, ["SCHEMA", "gen-3"]);
    assert_eq!(
        fs::read_to_string(world.pki().join("SCHEMA")).expect("SCHEMA"),
        "2\n"
    );
}

/// Catches a change to the layout of schema 1 that this release would no longer read: a
/// directory built by hand to the layout of `store.rs` is read, its key renews, and the files
/// of the next generation follow the same layout.
#[test]
fn a_schema_1_directory_is_read_by_this_release() {
    let world = World::new("schema-1");
    let key = signing_key(0x44);
    let spki = spki_of(&key);
    let issued = {
        let c = world.coordinator();
        proper(
            &c.ca,
            &Args {
                spki: spki.clone(),
                host_id: HOST.to_owned(),
                now: T0,
            },
        )
    };
    let pki = world.pki();
    let gen1 = pki.join("gen-1");
    fs::create_dir_all(&gen1).expect("gen-1");
    fs::write(pki.join("SCHEMA"), "1\n").expect("SCHEMA");
    fs::write(gen1.join("cert.pem"), &issued.cert_pem).expect("cert");
    fs::write(gen1.join("ca.pem"), &issued.ca_bundle_pem).expect("ca");
    crate::support::write_private(&gen1.join("key"), &pkcs8(&key));
    fs::write(gen1.join("COMPLETE"), "").expect("COMPLETE");
    symlink("gen-1", pki.join("current")).expect("current");
    // The coordinator knows the host by that key.
    let spki_der = spki.to_der().expect("DER");
    world.coordinator().rows.insert(
        RequestId::of_spki(&spki_der),
        crate::support::Row {
            spki_der,
            state: RowState::Approved(HOST.to_owned()),
            renewal_of: None,
            issued: Some(issued),
        },
    );

    let mut enroller = world.open();
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    world.set_now(T0 + WEEK / 2);
    let Outcome::Renewed(identity) = enroller.run_once().expect("renew") else {
        panic!("not renewed");
    };
    assert_eq!(identity.generation, 2);
    assert_eq!(
        crate::support::names(&pki.join("gen-2")),
        ["COMPLETE", "ca.pem", "cert.pem", "key"]
    );
    assert_eq!(
        fs::read_to_string(pki.join("SCHEMA")).expect("SCHEMA"),
        "1\n"
    );
    assert_eq!(fs::read(pki.join("gen-2/key")).expect("key"), pkcs8(&key));
    // The store reads it back through `current`.
    let stored = PkiDir::new(&pki).current().expect("read").expect("current");
    assert_eq!(stored.number, 2);
    assert_eq!(stored.key_der, pkcs8(&key));
}

/// Catches a wall-clock read anywhere but the three validity checks, across enrollment, renewal
/// and the return after expiry.
#[test]
fn the_wall_clock_is_read_only_for_validity() {
    let world = World::new("wall-clock");
    let mut enroller = world.enrolled();
    assert_eq!(world.sites(), ["leaf_valid_now"]);
    world.set_now(T0 + WEEK / 2);
    assert!(matches!(enroller.run_once(), Ok(Outcome::Renewed(_))));
    world.set_now(T0 + WEEK / 2 + 1);
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    world.set_now(T0 + 3 * WEEK);
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    world.coordinator().approve(HOST);
    assert!(matches!(enroller.run_once(), Ok(Outcome::Enrolled(_))));

    let sites = world.sites();
    let allowed = BTreeSet::from(["renewal_due", "expired", "leaf_valid_now"]);
    let seen: BTreeSet<&str> = sites.iter().copied().collect();
    assert_eq!(seen, allowed, "{sites:?}");
    assert_eq!(
        sites,
        [
            // Enrollment: the issued certificate.
            "leaf_valid_now",
            // Renewal at the midpoint.
            "expired",
            "renewal_due",
            "leaf_valid_now",
            // Idle.
            "expired",
            "renewal_due",
            // Expired: the request, then the return.
            "expired",
            "expired",
            "leaf_valid_now",
        ]
    );
}
