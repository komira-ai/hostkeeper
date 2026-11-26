//! A renewal asked on the host (`hostkeeper renew` writes the pki directory's `RENEW`,
//! [`PkiDir::ask_renewal`]): the next step renews as the middle of the validity does, for the same
//! key, and the ask goes once a generation is stored or the renewal is refused; any other failed
//! step keeps it for the next.

use std::fs;
use std::io;
use std::os::unix::fs::symlink;

use hostkeeper_enroll::{
    Enroller, Error, KeyFrom, Outcome, PkiDir, Reason, RenewRequest, Store, StoreError, StoreLock,
    Stored,
};

use crate::support::{Args, Ca, HOST, T0, WEEK, World, csr_spki, proper};

/// The ask's file in the pki directory.
const RENEW: &str = "RENEW";

/// The pki directory of `world`, as `hostkeeper renew` opens it.
fn pki_of(world: &World) -> PkiDir {
    PkiDir::new(&world.pki())
}

/// Whether the pki directory of `world` holds the ask.
fn asked(world: &World) -> bool {
    world.pki().join(RENEW).exists()
}

/// The generation `outcome` renewed to, with the validity of its certificate.
fn renewed(outcome: Result<Outcome, Error>) -> (u64, u64, u64) {
    match outcome {
        Ok(Outcome::Renewed(identity)) => {
            (identity.generation, identity.not_before, identity.not_after)
        }
        other => panic!("not renewed: {other:?}"),
    }
}

/// The file `name` of generation `generation`.
fn read(world: &World, generation: u64, name: &str) -> Vec<u8> {
    fs::read(world.pki().join(format!("gen-{generation}/{name}"))).expect("a generation's file")
}

/// The host id and the key a renewal request asks for: what it asks the coordinator to certify.
fn asks_for(request: &RenewRequest) -> (String, Vec<u8>) {
    (
        request.host_id.as_str().to_owned(),
        csr_spki(&request.csr_der),
    )
}

/// Catches an ask that waits for the middle of the validity, renews with a new key, or is left
/// behind once answered; and an asked renewal that is not the renewal of the middle: a host
/// enrolled at `T0` and asked one second later renews at that step, signed by the key it holds,
/// into generation 2, whose certificate is new (valid from the ask for a week) and whose key is
/// generation 1's; its request names the same host and key as the renewal the same host makes
/// unasked at the middle of its validity; the ask is gone, and the next step does nothing.
#[test]
fn an_asked_renewal_is_the_renewal_of_the_middle_now() {
    let world = World::new("ask-renews");
    let mut enroller = world.enrolled();
    world.set_now(T0 + 1);
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    assert_eq!(pki_of(&world).ask_renewal().expect("ask"), Some(1));
    assert!(asked(&world));

    assert_eq!(renewed(enroller.run_once()), (2, T0 + 1, T0 + 1 + WEEK));
    assert!(!asked(&world), "the ask is answered");
    assert_eq!(
        read(&world, 2, "key"),
        read(&world, 1, "key"),
        "the key is kept"
    );
    assert_ne!(read(&world, 2, "cert.pem"), read(&world, 1, "cert.pem"));
    let asked_request = world.coordinator().renews.clone();
    assert_eq!(asked_request.len(), 1);
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    assert_eq!(world.coordinator().renews.len(), 1);

    let unasked = World::new("ask-middle");
    let mut enroller = unasked.enrolled();
    unasked.set_now(T0 + WEEK / 2);
    assert_eq!(renewed(enroller.run_once()).0, 2);
    let middle = unasked.coordinator().renews.clone();
    assert_eq!(asks_for(&asked_request[0]), asks_for(&middle[0]));
    assert_eq!(asks_for(&middle[0]).0, HOST);
}

/// Catches a refused renewal that leaves the ask, so that the host asks the coordinator every
/// five seconds for an answer it already has, and an ask that the coordinator's answer for the
/// generation silences: the refusal is the step's error and removes the ask, the next step asks
/// nothing; a second ask, once the coordinator renews again, renews that same generation.
#[test]
fn a_refused_renewal_answers_the_ask() {
    let world = World::new("ask-refused");
    let mut enroller = world.enrolled();
    world.coordinator().renew_refusal = Reason::new("host_excluded");
    pki_of(&world).ask_renewal().expect("ask");
    let refused = enroller.run_once().expect_err("refused");
    assert_eq!(refused.to_string(), "renewal refused: host_excluded");
    assert!(!asked(&world));
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    assert_eq!(world.coordinator().renews.len(), 1);

    world.coordinator().renew_refusal = None;
    pki_of(&world).ask_renewal().expect("ask again");
    assert_eq!(renewed(enroller.run_once()).0, 2);
    assert!(!asked(&world));
}

/// Catches an ask dropped by a step that did not reach the coordinator: with the coordinator
/// out of reach the step fails and the ask stays; once it is back, the next step renews and
/// removes it.
#[test]
fn an_ask_outlives_a_coordinator_out_of_reach() {
    let world = World::new("ask-down");
    let mut enroller = world.enrolled();
    world.coordinator().down = Some("connection refused".to_owned());
    pki_of(&world).ask_renewal().expect("ask");
    let failed = enroller.run_once().expect_err("out of reach");
    assert_eq!(
        failed.to_string(),
        "the coordinator could not be reached: connection refused"
    );
    assert!(asked(&world));
    world.coordinator().down = None;
    assert_eq!(renewed(enroller.run_once()).0, 2);
    assert!(!asked(&world));
}

/// Catches an ask dropped by a certificate the host refused to store: a coordinator that issues
/// one that ended a week ago fails the step, stores nothing and keeps the ask; once it issues
/// as its profile says, the next step stores generation 2 and removes the ask.
#[test]
fn an_ask_outlives_an_issued_certificate_refused() {
    let world = World::new("ask-stale");
    let mut enroller = world.enrolled();
    world.coordinator().issue = Some(Box::new(|ca: &Ca, args: &Args| {
        let past = Args {
            spki: args.spki.clone(),
            host_id: args.host_id.clone(),
            now: args.now - 2 * WEEK,
        };
        proper(ca, &past)
    }));
    pki_of(&world).ask_renewal().expect("ask");
    let refused = enroller.run_once().expect_err("refused");
    assert!(matches!(refused, Error::Refused(_)), "{refused:?}");
    assert!(asked(&world));
    assert_eq!(world.current(), Some("gen-1".into()));
    world.coordinator().issue = None;
    assert_eq!(renewed(enroller.run_once()).0, 2);
    assert!(!asked(&world));
}

/// Catches an ask written for a host with no certificate, which would renew the first
/// generation the moment it is stored: a pki directory with no generation, before the request
/// and while it waits for approval, answers `None` and gets no `RENEW`.
#[test]
fn an_ask_without_a_certificate_writes_nothing() {
    let world = World::new("ask-unenrolled");
    let mut enroller = world.open();
    assert_eq!(pki_of(&world).ask_renewal().expect("ask"), None);
    assert_eq!(enroller.run_once().expect("post"), Outcome::Pending);
    assert_eq!(pki_of(&world).ask_renewal().expect("ask"), None);
    assert!(!asked(&world));
    assert_eq!(world.names(), [".lock", "SCHEMA", "pending"]);
}

/// Catches an ask written where the daemon would not read it, or into a layout this release
/// does not know: a pki directory that is not there, one that is a link, one of a newer schema,
/// and a path that is a file are each refused, by their error, and nothing is written.
#[test]
fn an_ask_on_a_directory_the_daemon_would_refuse_is_refused() {
    let world = World::new("ask-refusals");
    let missing = world.dir.path().join("missing");
    let error = PkiDir::new(&missing).ask_renewal().expect_err("missing");
    assert!(
        matches!(&error, StoreError::Io(e) if e.kind() == io::ErrorKind::NotFound),
        "{error:?}"
    );
    assert!(!missing.exists());

    drop(world.enrolled());
    let link = world.dir.path().join("link");
    symlink(world.pki(), &link).expect("a link");
    let error = PkiDir::new(&link).ask_renewal().expect_err("a link");
    assert_eq!(
        error.to_string(),
        format!(
            "{link:?} is not the daemon's own: a directory or key must be owned by it, not a link, and closed to group and others"
        )
    );

    let file = world.dir.path().join("file");
    fs::write(&file, "").expect("a file");
    let error = PkiDir::new(&file).ask_renewal().expect_err("a file");
    assert!(matches!(error, StoreError::Io(_)), "{error:?}");

    fs::write(world.pki().join("SCHEMA"), "2\n").expect("a newer schema");
    let error = pki_of(&world).ask_renewal().expect_err("newer");
    assert_eq!(
        error.to_string(),
        "the pki directory has schema 2, newer than schema 1 this release reads; it is left as it is"
    );
    assert!(!asked(&world));
}

/// Catches an ask that a host which cannot renew drops before it is answered: a host whose
/// certificate expired returns with its key (`renewal_of`), keeps the ask while that request
/// waits for approval, and removes it when the returned generation is stored.
#[test]
fn an_expired_host_keeps_the_ask_until_its_return_is_stored() {
    let world = World::new("ask-expired");
    let mut enroller = world.enrolled();
    world.set_now(T0 + WEEK + 1);
    assert_eq!(pki_of(&world).ask_renewal().expect("ask"), Some(1));
    assert_eq!(enroller.run_once().expect("returns"), Outcome::Pending);
    assert!(world.coordinator().renews.is_empty());
    assert!(asked(&world));
    world.coordinator().approve(HOST);
    let outcome = enroller.run_once().expect("returned");
    assert!(
        matches!(&outcome, Outcome::Enrolled(identity) if identity.generation == 2),
        "{outcome:?}"
    );
    assert!(!asked(&world));
}

/// Catches anything at `RENEW` taken for an ask, which a removal could never clear (a
/// directory) or which only leads elsewhere (a link): neither renews, and the step is idle.
#[test]
fn only_a_regular_file_is_an_ask() {
    let world = World::new("ask-not-file");
    let mut enroller = world.enrolled();
    fs::create_dir(world.pki().join(RENEW)).expect("a directory");
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    fs::remove_dir(world.pki().join(RENEW)).expect("remove");
    let elsewhere = world.dir.path().join("elsewhere");
    fs::write(&elsewhere, "").expect("a file");
    symlink(&elsewhere, world.pki().join(RENEW)).expect("a link");
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
    assert!(world.coordinator().renews.is_empty());
}

/// Catches an ask lost across a daemon restart, or a torn one left for ever: with the daemon
/// stopped, an ask and a temporary file of one are in the pki directory; the next enroller's open
/// keeps the ask and removes the temporary file, and its first step renews.
#[test]
fn an_ask_made_while_the_daemon_is_stopped_is_answered_at_its_start() {
    let world = World::new("ask-stopped");
    drop(world.enrolled());
    assert_eq!(pki_of(&world).ask_renewal().expect("ask"), Some(1));
    fs::write(world.pki().join("RENEW.tmp"), "").expect("a torn ask");
    let mut enroller = world.open();
    assert!(asked(&world));
    assert!(!world.pki().join("RENEW.tmp").exists());
    assert_eq!(renewed(enroller.run_once()).0, 2);
    assert!(!asked(&world));
}

/// The pki directory, except that the ask cannot be read, or cannot be removed.
struct Faulty {
    pki: PkiDir,
    read_fails: bool,
}

impl Store for Faulty {
    fn open(&self) -> Result<StoreLock, StoreError> {
        self.pki.open()
    }

    fn pending_key(&self) -> Result<Option<Vec<u8>>, StoreError> {
        self.pki.pending_key()
    }

    fn write_pending_key(&self, pkcs8_der: &[u8]) -> Result<(), StoreError> {
        self.pki.write_pending_key(pkcs8_der)
    }

    fn current(&self) -> Result<Option<Stored>, StoreError> {
        self.pki.current()
    }

    fn write_generation(
        &self,
        cert_pem: &str,
        ca_pem: &str,
        key: KeyFrom,
    ) -> Result<Stored, StoreError> {
        self.pki.write_generation(cert_pem, ca_pem, key)
    }

    fn prune(&self, used: u64) -> Result<(), StoreError> {
        self.pki.prune(used)
    }

    fn renewal_asked(&self) -> Result<bool, StoreError> {
        match self.read_fails {
            true => Err(StoreError::Io(io::Error::other("read failed"))),
            false => self.pki.renewal_asked(),
        }
    }

    fn clear_renewal(&self) -> Result<(), StoreError> {
        Err(StoreError::Io(io::Error::other("unlink failed")))
    }
}

/// An enroller of `world`'s enrolled host over its pki directory made [`Faulty`].
fn faulty(world: &World, read_fails: bool) -> Enroller {
    drop(world.enrolled());
    let store = Faulty {
        pki: pki_of(world),
        read_fails,
    };
    let ports = world.ports_with(Some(Box::new(store)), None);
    Enroller::open(world.settings(), ports).expect("open")
}

/// Catches a renewal reported failed because its ask could not be removed, and an ask that
/// cannot be read taken as none: a removal that fails leaves the renewal stored and reported,
/// and the step after it, finding the ask, renews once more; an ask that cannot be read fails
/// the step with the store's error and renews nothing.
#[test]
fn a_failed_removal_renews_again_and_an_unreadable_ask_fails_the_step() {
    let world = World::new("ask-unlink");
    let mut enroller = faulty(&world, false);
    pki_of(&world).ask_renewal().expect("ask");
    assert_eq!(renewed(enroller.run_once()).0, 2);
    assert!(asked(&world));
    assert_eq!(renewed(enroller.run_once()).0, 3);

    let world = World::new("ask-unreadable");
    let mut enroller = faulty(&world, true);
    let failed = enroller.run_once().expect_err("unreadable");
    assert_eq!(
        failed.to_string(),
        "cannot use the pki directory: read failed"
    );
    assert!(world.coordinator().renews.is_empty());
}

/// Catches the store's own reads and removal of the ask failing silently: through a path that
/// is a file both fail with the system's error, and removing an ask that is a directory fails;
/// removing none is no error.
#[test]
fn the_store_reports_what_it_cannot_read_or_remove() {
    let world = World::new("ask-store");
    let file = world.dir.path().join("file");
    fs::write(&file, "").expect("a file");
    let store = PkiDir::new(&file);
    assert!(matches!(store.renewal_asked(), Err(StoreError::Io(_))));
    drop(world.enrolled());
    let store = pki_of(&world);
    store.clear_renewal().expect("no ask is no error");
    fs::create_dir(world.pki().join(RENEW)).expect("a directory");
    assert!(matches!(store.clear_renewal(), Err(StoreError::Io(_))));
}
