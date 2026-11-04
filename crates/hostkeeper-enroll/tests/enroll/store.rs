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
    assert!(matches!(store.prune(1), Err(StoreError::Io(_))));
}

/// Catches a pending key that is not a key file being taken as absent, which would make a
/// second key and a second request.
#[test]
fn a_pending_key_that_cannot_be_read_stops_the_step() {
    let world = World::new("pending-dir");
    let mut enroller = world.open();
    let key = world.pki().join("pending/key");
    fs::create_dir_all(&key).expect("a directory");
    let refused = enroller.run_once().expect_err("unreadable");
    assert!(
        matches!(&refused, Error::Store(StoreError::NotPrivate(path)) if *path == key),
        "{refused:?}"
    );
    assert!(key.is_dir(), "left as it was");
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
    crate::support::write_private(&world.pki().join("gen-1/key"), b"not a key");
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
    enroller.confirm_generation(1).expect("nothing to prune");
    drop(enroller);

    let mut enroller = world.enrolled();
    enroller.confirm_generation(1).expect("one generation");
    world.set_now(T0 + WEEK / 2);
    assert!(matches!(enroller.run_once(), Ok(Outcome::Renewed(_))));
    assert_eq!(
        world.names(),
        [".lock", "SCHEMA", "current", "gen-1", "gen-2", "pending"]
    );
    enroller.confirm_generation(2).expect("prune");
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
    assert_eq!(
        PkiDir::host().current_paths(),
        PkiDir::new(Path::new("/etc/updater/pki")).current_paths()
    );
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

/// Catches a pki directory, `pending/` or key that is not the daemon's own being used: a link,
/// another owner, or a key open to others.
#[test]
fn a_tree_that_is_not_the_daemons_own_is_refused() {
    // The root is a link to a directory.
    let world = World::new("root-link");
    let real = world.dir.path().join("real");
    fs::create_dir(&real).expect("real");
    symlink(&real, world.pki()).expect("link");
    let refused = world.try_open().err().expect("a link");
    assert!(
        matches!(&refused, Error::Store(StoreError::NotPrivate(p)) if *p == world.pki()),
        "{refused:?}"
    );
    assert_eq!(
        refused.to_string(),
        format!(
            "{:?} is not the daemon's own: a directory or key must be owned by it, not a link, \
             and closed to group and others",
            world.pki()
        )
    );

    // The root belongs to another user than the daemon's.
    let world = World::new("other-owner");
    fs::create_dir(world.pki()).expect("pki");
    let mine = fs::metadata(world.pki()).expect("pki").uid();
    let other = PkiDir::new(&world.pki()).with_owner(mine.wrapping_add(1));
    let refused = other.open().expect_err("another owner");
    assert!(matches!(refused, StoreError::NotPrivate(_)), "{refused:?}");
    assert!(!world.pki().join(".lock").exists(), "nothing made in it");

    // `pending/` is a link.
    let world = World::new("pending-link");
    let mut enroller = world.open();
    let elsewhere = world.dir.path().join("elsewhere");
    fs::create_dir(&elsewhere).expect("elsewhere");
    symlink(&elsewhere, world.pki().join("pending")).expect("link");
    let refused = enroller.run_once().expect_err("a link");
    assert!(
        matches!(refused, Error::Store(StoreError::NotPrivate(_))),
        "{refused:?}"
    );
    assert!(
        crate::support::names(&elsewhere).is_empty(),
        "no key written through it"
    );
    assert!(world.coordinator().enrolls.is_empty());

    // A generation's key is open to others.
    let world = World::new("open-key");
    let mut enroller = world.enrolled();
    let key = world.pki().join("gen-1/key");
    fs::set_permissions(&key, fs::Permissions::from_mode(0o644)).expect("chmod");
    let refused = enroller.run_once().expect_err("an open key");
    assert!(
        matches!(&refused, Error::Store(StoreError::NotPrivate(p)) if *p == key),
        "{refused:?}"
    );
}

/// Catches recovery removing a pending key it could not read: when neither key reads back, the
/// pending key is kept.
#[test]
fn a_pending_key_that_cannot_be_read_back_is_kept_by_recovery() {
    let world = World::new("unreadable-keys");
    drop(world.enrolled());
    let pki = world.pki();
    fs::remove_file(pki.join("gen-1/key")).expect("unlink");
    fs::create_dir(pki.join("gen-1/key")).expect("a directory");
    fs::create_dir(pki.join("pending/key")).expect("a directory");
    drop(world.open());
    assert!(pki.join("pending/key").is_dir(), "kept");
}

/// Catches a generation whose key is another valid key, whose CA bundle is not the pinned CA's
/// or is missing, or whose `COMPLETE` marker is gone, being used as the host's identity.
#[test]
fn a_generation_that_does_not_hold_together_stops_the_step() {
    let world = World::new("other-valid-key");
    let mut enroller = world.enrolled();
    let key = world.pki().join("gen-1/key");
    fs::remove_file(&key).expect("unlink");
    crate::support::write_private(&key, &pkcs8(&signing_key(0x5a)));
    assert!(matches!(enroller.run_once(), Err(Error::Corrupt(1))));

    let world = World::new("other-ca");
    let mut enroller = world.enrolled();
    let other = crate::support::Ca::new(0x5b);
    fs::write(world.pki().join("gen-1/ca.pem"), &other.pem).expect("ca");
    assert!(matches!(enroller.run_once(), Err(Error::Corrupt(1))));

    let world = World::new("missing-ca");
    let mut enroller = world.enrolled();
    fs::remove_file(world.pki().join("gen-1/ca.pem")).expect("unlink");
    assert!(matches!(
        enroller.run_once(),
        Err(Error::Store(StoreError::Io(_)))
    ));

    let world = World::new("not-complete");
    let mut enroller = world.enrolled();
    fs::remove_file(world.pki().join("gen-1/COMPLETE")).expect("unlink");
    let refused = enroller.run_once().expect_err("incomplete");
    assert!(
        matches!(refused, Error::Store(StoreError::Incomplete(1))),
        "{refused:?}"
    );
    assert_eq!(
        refused.to_string(),
        "generation 1 of the pki directory has no COMPLETE marker"
    );
}

/// Catches a late confirmation of an older generation removing generations: a heartbeat that
/// started on generation 1 and ends after generation 2 became current removes nothing.
#[test]
fn a_stale_confirmation_removes_nothing() {
    let world = World::new("stale-confirm");
    let mut enroller = world.enrolled();
    world.set_now(T0 + WEEK / 2);
    assert!(matches!(enroller.run_once(), Ok(Outcome::Renewed(_))));
    enroller.confirm_generation(1).expect("stale");
    assert_eq!(
        world.names(),
        [".lock", "SCHEMA", "current", "gen-1", "gen-2", "pending"]
    );
    enroller.confirm_generation(2).expect("prune");
    assert_eq!(
        world.names(),
        [".lock", "SCHEMA", "current", "gen-2", "pending"]
    );
}

/// Catches the generation number wrapping or saturating onto an existing generation: past the
/// last number a renewal is refused by name, and the current generation and its key stay.
#[test]
fn the_last_generation_number_is_refused_by_name() {
    let world = World::new("exhausted");
    drop(world.enrolled());
    let pki = world.pki();
    let last = format!("gen-{}", u64::MAX);
    fs::rename(pki.join("gen-1"), pki.join(&last)).expect("rename");
    let key = fs::read(pki.join(&last).join("key")).expect("key");
    let mut enroller = world.open();
    assert_eq!(world.current(), Some(PathBuf::from(&last)));
    world.set_now(T0 + WEEK / 2);
    let refused = enroller.run_once().expect_err("exhausted");
    assert!(
        matches!(refused, Error::Store(StoreError::GenerationsExhausted)),
        "{refused:?}"
    );
    assert_eq!(
        refused.to_string(),
        "the pki directory has used every generation number"
    );
    assert_eq!(world.current(), Some(PathBuf::from(&last)));
    assert_eq!(fs::read(pki.join(&last).join("key")).expect("key"), key);
}

/// Catches a pki directory whose parent does not exist yet failing to open.
#[test]
fn a_pki_directory_is_made_with_its_parents() {
    let world = World::new("nested");
    let root = world.dir.path().join("etc/updater/pki");
    let store = PkiDir::new(&root);
    drop(store.open().expect("open"));
    assert_eq!(mode(&root), 0o700);
    assert_eq!(
        fs::read_to_string(root.join("SCHEMA")).expect("SCHEMA"),
        "1\n"
    );
}

/// Catches the store's `Debug` hiding where it is, or trying to print its fault gate.
#[test]
fn a_pki_directory_debug_shows_its_root_and_owner() {
    assert_eq!(
        format!("{:?}", PkiDir::new(Path::new("/srv/pki")).with_owner(7)),
        "PkiDir { root: \"/srv/pki\", owner: 7, .. }"
    );
}

/// Catches recovery following a link out of the pki directory: with `pending/` or a generation
/// already a link when the store opens, the open is refused and nothing outside is removed,
/// neither a leftover temporary key nor a key equal to the current generation's.
#[test]
fn recovery_does_not_follow_a_link_out_of_the_tree() {
    // `pending/` links to a directory holding a temporary key.
    let world = World::new("pending-link-tmp");
    drop(world.open());
    let outside = world.dir.path().join("outside");
    fs::create_dir(&outside).expect("outside");
    fs::write(outside.join("key.tmp"), "theirs").expect("key.tmp");
    symlink(&outside, world.pki().join("pending")).expect("link");
    let refused = world.try_open().err().expect("a link");
    assert!(
        matches!(&refused, Error::Store(StoreError::NotPrivate(p)) if *p == world.pki().join("pending")),
        "{refused:?}"
    );
    assert_eq!(fs::read(outside.join("key.tmp")).expect("kept"), b"theirs");

    // `pending/` links to a directory holding a key equal to the current generation's.
    let world = World::new("pending-link-key");
    drop(world.enrolled());
    let pki = world.pki();
    let outside = world.dir.path().join("outside");
    fs::create_dir(&outside).expect("outside");
    fs::copy(pki.join("gen-1/key"), outside.join("key")).expect("copy");
    fs::remove_dir(pki.join("pending")).expect("rmdir");
    symlink(&outside, pki.join("pending")).expect("link");
    assert!(world.try_open().is_err(), "a link");
    assert_eq!(
        fs::read(outside.join("key")).expect("kept"),
        fs::read(pki.join("gen-1/key")).expect("key")
    );

    // A generation links to a directory outside, without `COMPLETE`.
    let world = World::new("generation-link");
    drop(world.open());
    let outside = world.dir.path().join("outside");
    fs::create_dir(&outside).expect("outside");
    fs::write(outside.join("cert.pem"), "theirs").expect("cert");
    symlink(&outside, world.pki().join("gen-2")).expect("link");
    let refused = world.try_open().err().expect("a link");
    assert!(
        matches!(&refused, Error::Store(StoreError::NotPrivate(p)) if *p == world.pki().join("gen-2")),
        "{refused:?}"
    );
    assert_eq!(fs::read(outside.join("cert.pem")).expect("kept"), b"theirs");
}
