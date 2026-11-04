//! The pki directory: what opening it creates, the schema check, what recovery does with each
//! thing a crash can leave, how strictly `current` is read, and pruning.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hostkeeper_enroll::{CurrentPaths, Error, KeyFrom, Outcome, PkiDir, Store, StoreError};
use p256::ecdsa::SigningKey;
use p256::pkcs8::DecodePrivateKey;

use crate::support::{Args, HOST, T0, WEEK, World, pkcs8, proper, signing_key, spki_of};

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

/// Copies the files of one generation directory into a new one.
fn copy_generation(pki: &Path, from: &str, to: &str) {
    fs::create_dir(pki.join(to)).expect("create");
    for name in ["cert.pem", "ca.pem", "key", "COMPLETE"] {
        fs::copy(pki.join(from).join(name), pki.join(to).join(name)).expect("copy");
    }
}

/// Points `current` at `target`.
fn relink(pki: &Path, target: &str) {
    fs::remove_file(pki.join("current")).expect("unlink");
    symlink(target, pki.join("current")).expect("link");
}

/// Catches a pki directory opened to group or others, a lock file anyone can take, or a new
/// directory without its schema.
#[test]
fn a_new_pki_directory_is_private_and_carries_its_schema() {
    let world = World::new("new-dir");
    let enroller = world.open();
    let pki = world.pki();
    assert_eq!(world.names(), [".lock", "SCHEMA"]);
    assert_eq!(
        fs::read_to_string(pki.join("SCHEMA")).expect("SCHEMA"),
        "1\n"
    );
    assert_eq!(mode(&pki), 0o700);
    assert_eq!(mode(&pki.join(".lock")), 0o600);
    drop(enroller);
    // A directory made open by hand is closed again.
    fs::set_permissions(&pki, fs::Permissions::from_mode(0o755)).expect("chmod");
    drop(world.open());
    assert_eq!(mode(&pki), 0o700);
}

/// Catches a SCHEMA that is not a schema number being read as schema 1, or a SCHEMA that
/// cannot be read being taken as missing and overwritten.
#[test]
fn a_schema_that_is_not_a_number_is_refused() {
    for (n, text) in ["x\n", "0\n", ""].into_iter().enumerate() {
        let world = World::new(&format!("bad-schema-{n}"));
        fs::create_dir_all(world.pki()).expect("pki");
        fs::write(world.pki().join("SCHEMA"), text).expect("SCHEMA");
        let refused = world.try_open().err().expect("refused");
        assert!(
            matches!(&refused, Error::Store(StoreError::BadSchema(t)) if t == text),
            "{refused:?}"
        );
        assert_eq!(
            refused.to_string(),
            format!("the pki directory's SCHEMA file does not hold a schema number: {text:?}")
        );
    }
    let world = World::new("schema-dir");
    fs::create_dir_all(world.pki().join("SCHEMA")).expect("a directory named SCHEMA");
    let refused = world.try_open().err().expect("refused");
    assert!(
        matches!(refused, Error::Store(StoreError::Io(_))),
        "{refused:?}"
    );
    assert!(world.pki().join("SCHEMA").is_dir(), "left as it was");
}

/// Catches `current` left on a generation that is gone, or on an older generation when a newer
/// one was completed just before a crash, or kept when no complete generation is left.
#[test]
fn current_is_repaired_to_the_newest_complete_generation() {
    let world = World::new("repair");
    drop(world.enrolled());
    let pki = world.pki();

    relink(&pki, "gen-5");
    drop(world.open());
    assert_eq!(world.current(), Some(PathBuf::from("gen-1")));

    copy_generation(&pki, "gen-1", "gen-2");
    drop(world.open());
    assert_eq!(world.current(), Some(PathBuf::from("gen-2")));

    // No complete generation: the link goes.
    fs::remove_file(pki.join("gen-1/COMPLETE")).expect("unmark gen-1");
    fs::remove_file(pki.join("gen-2/COMPLETE")).expect("unmark gen-2");
    drop(world.open());
    assert_eq!(world.current(), None);
    assert_eq!(world.names(), [".lock", "SCHEMA", "pending"]);
}

/// Catches the leftovers of a crash being kept: a temporary link, a temporary key, and a pending
/// key whose enrollment finished. Names that are not the store's are left alone.
#[test]
fn leftovers_of_a_crash_are_removed() {
    let world = World::new("leftovers");
    drop(world.enrolled());
    let pki = world.pki();
    symlink("gen-9", pki.join("current.tmp")).expect("current.tmp");
    fs::write(pki.join("pending/key.tmp"), "half a key").expect("key.tmp");
    fs::copy(pki.join("gen-1/key"), pki.join("pending/key")).expect("pending key");
    fs::create_dir(pki.join("gen-01")).expect("gen-01");
    fs::write(pki.join("notes"), "kept").expect("notes");
    drop(world.open());
    assert_eq!(
        world.names(),
        [
            ".lock", "SCHEMA", "current", "gen-01", "gen-1", "notes", "pending"
        ]
    );
    assert!(crate::support::names(&pki.join("pending")).is_empty());
    assert_eq!(world.current(), Some(PathBuf::from("gen-1")));
}

/// Catches a leftover that cannot be removed being ignored: the open fails and says why.
#[test]
fn a_leftover_that_cannot_be_removed_fails_the_open() {
    let world = World::new("stuck-leftover");
    fs::create_dir_all(world.pki().join("current.tmp/inside")).expect("a directory");
    let refused = world.try_open().err().expect("refused");
    assert!(
        matches!(refused, Error::Store(StoreError::Io(_))),
        "{refused:?}"
    );
}

/// Catches a `current` that is not a link to a generation being followed anyway.
#[test]
fn the_current_link_is_read_strictly() {
    let world = World::new("strict-current");
    let mut enroller = world.enrolled();
    let pki = world.pki();
    let store = PkiDir::new(&pki);

    relink(&pki, "elsewhere");
    let refused = store.current().expect_err("not a generation");
    assert!(matches!(&refused, StoreError::BadLink(target) if target == Path::new("elsewhere")));
    assert_eq!(
        refused.to_string(),
        "the pki directory's current link points to \"elsewhere\", which is not a generation"
    );
    let refused = enroller.run_once().expect_err("not a generation");
    assert!(matches!(refused, Error::Store(StoreError::BadLink(_))));

    fs::remove_file(pki.join("current")).expect("unlink");
    fs::write(pki.join("current"), "gen-1").expect("a file");
    assert!(matches!(store.current(), Err(StoreError::Io(_))));
    assert!(matches!(store.prune(), Err(StoreError::Io(_))));
}

/// Catches a pending key that cannot be read being taken as absent, which would make a second
/// key and a second request.
#[test]
fn a_pending_key_that_cannot_be_read_stops_the_step() {
    let world = World::new("pending-dir");
    let mut enroller = world.open();
    fs::create_dir_all(world.pki().join("pending/key")).expect("a directory");
    let refused = enroller.run_once().expect_err("unreadable");
    assert!(
        matches!(refused, Error::Store(StoreError::Io(_))),
        "{refused:?}"
    );
    assert!(world.coordinator().enrolls.is_empty());
}

/// Catches a generation whose certificate or key is not this host's being used, or a missing
/// file being taken as no generation.
#[test]
fn a_corrupt_generation_stops_the_step() {
    let world = World::new("corrupt-cert");
    let mut enroller = world.enrolled();
    let pki = world.pki();
    fs::write(pki.join("gen-1/cert.pem"), "not a certificate").expect("cert");
    let refused = enroller.run_once().expect_err("corrupt");
    assert!(matches!(refused, Error::Corrupt(1)), "{refused:?}");
    assert_eq!(
        refused.to_string(),
        "the pki directory's generation 1 is not a certificate and key of this host"
    );

    let world = World::new("corrupt-key");
    let mut enroller = world.enrolled();
    fs::remove_file(world.pki().join("gen-1/key")).expect("unlink");
    fs::write(world.pki().join("gen-1/key"), "not a key").expect("key");
    assert!(matches!(enroller.run_once(), Err(Error::Corrupt(1))));

    // A certificate whose subject does not name a host.
    let world = World::new("corrupt-subject");
    let mut enroller = world.enrolled();
    let held = SigningKey::from_pkcs8_der(&fs::read(world.pki().join("gen-1/key")).expect("key"))
        .expect("the held key");
    let cert =
        world
            .coordinator()
            .ca
            .leaf(&spki_of(&held), "CN=web-1", Vec::new(), (T0, T0 + WEEK));
    fs::write(world.pki().join("gen-1/cert.pem"), cert).expect("cert");
    assert!(matches!(enroller.run_once(), Err(Error::Corrupt(1))));

    let world = World::new("missing-cert");
    let mut enroller = world.enrolled();
    fs::remove_file(world.pki().join("gen-1/cert.pem")).expect("unlink");
    assert!(matches!(
        enroller.run_once(),
        Err(Error::Store(StoreError::Io(_)))
    ));
}

/// Catches pruning that removes the current generation, or that keeps the older ones once the
/// current one was used.
#[test]
fn confirming_the_current_generation_removes_the_older_ones() {
    let world = World::new("prune");
    let mut enroller = world.open();
    enroller.confirm_current().expect("nothing to prune");
    drop(enroller);

    let mut enroller = world.enrolled();
    enroller.confirm_current().expect("one generation");
    world.set_now(T0 + WEEK / 2);
    assert!(matches!(enroller.run_once(), Ok(Outcome::Renewed(_))));
    assert_eq!(
        world.names(),
        [".lock", "SCHEMA", "current", "gen-1", "gen-2", "pending"]
    );
    enroller.confirm_current().expect("prune");
    assert_eq!(
        world.names(),
        [".lock", "SCHEMA", "current", "gen-2", "pending"]
    );
    assert_eq!(world.current(), Some(PathBuf::from("gen-2")));
}

/// Catches the transport being given paths that do not follow `current`, and the host's store
/// being anywhere but `/etc/updater/pki`.
#[test]
fn the_current_paths_follow_the_link() {
    let root = Path::new("/srv/pki");
    assert_eq!(
        PkiDir::new(root).current_paths(),
        CurrentPaths {
            cert: root.join("current/cert.pem"),
            ca: root.join("current/ca.pem"),
            key: root.join("current/key"),
        }
    );
    assert_eq!(PkiDir::host(), PkiDir::new(Path::new("/etc/updater/pki")));
}

/// Catches a generation written over an existing one, a key that is copied rather than linked
/// from the generation it comes from, and a pending key left after the first generation.
#[test]
fn a_generation_links_its_key_and_numbers_up() {
    let world = World::new("write-generation");
    drop(world.open());
    let store = PkiDir::new(&world.pki());
    let key = signing_key(0x55);
    store.write_pending_key(&pkcs8(&key)).expect("pending key");
    assert_eq!(mode(&world.pki().join("pending/key")), 0o600);
    assert_eq!(mode(&world.pki().join("pending")), 0o700);
    let issued = proper(
        &world.coordinator().ca,
        &Args {
            spki: spki_of(&key),
            host_id: HOST.into(),
            now: T0,
        },
    );
    let first = store
        .write_generation(&issued.cert_pem, &issued.ca_bundle_pem, KeyFrom::Pending)
        .expect("gen-1");
    assert_eq!(first.number, 1);
    assert_eq!(store.pending_key().expect("read"), None);
    assert_eq!(mode(&first.dir), 0o700);
    let second = store
        .write_generation(
            &issued.cert_pem,
            &issued.ca_bundle_pem,
            KeyFrom::Generation(1),
        )
        .expect("gen-2");
    assert_eq!(second.number, 2);
    assert_eq!(second.key_der, pkcs8(&key));
    assert_eq!(
        fs::metadata(second.key_path()).expect("key").ino(),
        fs::metadata(first.key_path()).expect("key").ino(),
        "one key, linked"
    );
    assert_eq!(store.current().expect("read"), Some(second));
}

/// Catches recovery that removes a pending key only because some complete generation exists:
/// a pending key that is not the newest generation's key belongs to an open request and stays.
#[test]
fn a_pending_key_of_another_request_is_kept() {
    let world = World::new("other-pending-key");
    drop(world.enrolled());
    let pending = world.pki().join("pending/key");
    let other = pkcs8(&signing_key(0x66));
    fs::write(&pending, &other).expect("another pending key");
    drop(world.open());
    assert_eq!(fs::read(&pending).expect("kept"), other);
    assert_eq!(world.current(), Some(PathBuf::from("gen-1")));
}

/// Catches a stored generation printing the host's private key when it is debugged or when an
/// assertion on it fails.
#[test]
fn a_stored_generation_does_not_print_its_key() {
    let world = World::new("redacted");
    drop(world.enrolled());
    let stored = PkiDir::new(&world.pki())
        .current()
        .expect("read")
        .expect("current");
    assert_eq!(
        format!("{stored:?}"),
        format!(
            "Stored {{ number: 1, dir: {:?}, key_der: \"<redacted>\", .. }}",
            world.pki().join("gen-1")
        )
    );
}
