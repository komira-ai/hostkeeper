//! `backup` and `restore` over the files a coordinator wrote: the round trip keeps every file
//! byte for byte and what the stores read back from them, the CA's key is never copied, and a
//! backup taken while the lock is held is refused. The refusals of a restore are in
//! `backup_refusal_tests`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_enrollment::enroll::EnrollmentStore;
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::names::Role;
use hostkeeper_oidc::testkit::{
    STATIC_ISSUER, StaticKeys, clock, config_text, good_claims, jwks_of, keys, mint,
};
use hostkeeper_oidc::{Clock, OidcConfig, Refusal, Verifier};
use hostkeeper_testkit::Scratch;
use serde_json::json;

use crate::assign_support::assign;
use crate::backup::{Backup, BackupError, MANIFEST, Restore, StatePaths, backup, restore};
use crate::desired::DesiredStore;
use crate::enrollment::os_random;
use crate::enrollment::tests::support::{
    FixedClock, HostKey, NOW, Rig as EnrollRig, approve, enroll_request, host_id,
};
use crate::jti_file::{JtiFile, jti_path};
use crate::lock::{FileLock, LockError, StateLock, lock_path};
use crate::marks_support::{INSTALLER, WEB, ask_typed, beat_of, hold, rig, scope_host, set};
use crate::ops::OpsStore;
use crate::testutil::{LEGACY, scratch};

/// A verifier for the static issuer whose spent ids are kept in the file at `path`.
fn verifier(path: &Path, clock: &Clock) -> Verifier {
    Verifier::new(
        OidcConfig::parse(&config_text(STATIC_ISSUER)).expect("a config"),
        Arc::new(StaticKeys(jwks_of(&[&keys()[0]]))),
        Duration::from_secs(30),
        Arc::clone(clock),
        Arc::new(JtiFile::open(path, Arc::clone(clock)).expect("the file of spent ids")),
    )
}

/// A token of the static issuer whose `jti` is `jti`.
fn token(jti: &str) -> String {
    let mut claims = good_claims(STATIC_ISSUER);
    claims["jti"] = json!(jti);
    mint(&keys()[0], &claims)
}

/// Every file under `dir`, by its path under it, with its bytes.
pub(crate) fn files_under(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        for entry in fs::read_dir(&next).expect("a directory") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                todo.push(path);
            } else {
                let name = path.strip_prefix(dir).expect("under it");
                found.insert(
                    name.to_string_lossy().into_owned(),
                    fs::read(&path).expect("a file"),
                );
            }
        }
    }
    found
}

/// The flags of a backup of `paths` into `out`.
pub(crate) fn backup_flags(paths: &StatePaths, out: &Path) -> Backup {
    Backup {
        paths: paths.clone(),
        out: out.to_path_buf(),
    }
}

/// The flags of a restore of `from` into `paths`.
pub(crate) fn restore_flags(from: &Path, paths: &StatePaths) -> Restore {
    Restore {
        from: from.to_path_buf(),
        paths: paths.clone(),
        into_empty: true,
    }
}

/// Targets in `dir`: the state file in a directory of its own that is not there yet, and the
/// enrollment store beside it.
pub(crate) fn targets_in(dir: &Scratch) -> StatePaths {
    StatePaths {
        state_file: dir.path().join("state").join("desired.json"),
        ops_file: None,
        enrollment_dir: Some(dir.path().join("enrollment")),
    }
}

/// A coordinator's state, made through its calls: the state files of a rig, a spent jti, and
/// an enrollment store with an approved host and a denied one.
struct Populated {
    coordinator: crate::testutil::Rig,
    enrollment: EnrollRig,
    clock: Clock,
}

impl Populated {
    async fn new(test: &str) -> Populated {
        let coordinator = rig(test, "");
        set(&coordinator, WEB, "2.0", 1).await;
        hold(&coordinator, WEB, scope_host("n1")).expect("held");
        ask_typed(&coordinator, "n2", "daemon", beat_of(INSTALLER, vec![]));
        assign(&coordinator, WEB, "n2", "").expect("assigned");
        let (clock, _) = clock();
        let state = coordinator.dir.path().join("desired.json");
        verifier(&jti_path(&state), &clock)
            .verify(&token("spent-before-the-backup"))
            .await
            .expect("the first use");
        let enrollment = EnrollRig::new(&format!("{test}-enroll"));
        for (machine, host) in [("m1", "s1"), ("m2", "s2")] {
            let key = HostKey::new();
            enrollment
                .enrollment
                .enroll(enroll_request(&key, machine))
                .expect("posted");
            approve(&enrollment.enrollment, &key, host, false);
        }
        enrollment
            .enrollment
            .store()
            .deny(&host_id("s2"))
            .expect("denied");
        Populated {
            coordinator,
            enrollment,
            clock,
        }
    }

    fn paths(&self) -> StatePaths {
        StatePaths {
            state_file: self.coordinator.dir.path().join("desired.json"),
            ops_file: None,
            enrollment_dir: Some(self.enrollment.files.store.clone()),
        }
    }
}

/// The files of the state at `paths`: the ones beside the state file (but the lock and the
/// leftovers of a write) and those of the enrollment store (but its lock), by name.
fn state_of(paths: &StatePaths) -> BTreeMap<String, Vec<u8>> {
    let dir = paths.state_file.parent().expect("a directory");
    let mut found: BTreeMap<String, Vec<u8>> = files_under(dir)
        .into_iter()
        .filter(|(name, _)| name.starts_with("desired.json"))
        .filter(|(name, _)| !name.ends_with(".lock") && !name.ends_with(".new"))
        .collect();
    let store = paths.enrollment_dir.as_ref().expect("an enrollment dir");
    for (name, bytes) in files_under(store) {
        if name != "lock" {
            found.insert(format!("enrollment/{name}"), bytes);
        }
    }
    found
}

/// Catches: a file of the state left out of a backup or changed by the round trip, or a store
/// that does not read back from a restore what it held (desired versions, holds, assignments,
/// enrollment, deny rows, the spent ids of CI's tokens): a backup restored into empty targets
/// holds every file byte for byte, the audit file among them, and each store opened over them
/// reads what it read before.
#[tokio::test]
async fn a_backup_restored_into_empty_targets_reads_back_the_same_state() {
    let before = Populated::new("backup-round").await;
    let paths = before.paths();
    let out = scratch("backup-round-out");
    let snapshot = out.path().join("snapshot");
    backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");
    let target = scratch("backup-round-target");
    let restored = targets_in(&target);
    let report = restore(&restore_flags(&snapshot, &restored), &FileLock).expect("restored");

    let original = state_of(&paths);
    assert!(
        original.contains_key("desired.json.ops.audit"),
        "{original:?}"
    );
    assert!(original.contains_key("desired.json.jti"), "{original:?}");
    assert_eq!(state_of(&restored), original, "every file, byte for byte");
    assert_eq!(
        report.to_string().lines().count(),
        original.len() + 1,
        "{report}"
    );

    let state = |p: &StatePaths| DesiredStore::open(&p.state_file, Some(LEGACY)).expect("a store");
    assert_eq!(state(&restored).all(), state(&paths).all());
    assert_eq!(
        state(&restored).get(WEB).map(|d| d.version.to_string()),
        Some("2.0".to_owned())
    );
    let ops = |p: &StatePaths| OpsStore::open(&p.ops_file()).expect("ops").state();
    let (was, now) = (ops(&paths), ops(&restored));
    assert_eq!(now, was);
    assert_eq!(now.holds.len(), 1, "{now:?}");
    assert_eq!(now.assignments.len(), 1, "{now:?}");

    let store = EnrollmentStore::open(
        Arc::new(RealFs),
        restored.enrollment_dir.as_ref().expect("a dir"),
    )
    .expect("the store");
    assert_eq!(
        store.host_role(&host_id("s1")).expect("read"),
        Some(Role::parse("storage").expect("a role"))
    );
    assert!(!store.is_denied(&host_id("s1")).expect("read"));
    assert!(store.is_denied(&host_id("s2")).expect("read"));

    let spent = verifier(&jti_path(&restored.state_file), &before.clock);
    assert_eq!(
        spent.verify(&token("spent-before-the-backup")).await,
        Err(Refusal::Replayed)
    );
    spent
        .verify(&token("never-spent"))
        .await
        .expect("an id not spent is not refused");
}

/// Catches: the CA's key, or the server certificate file with its key, copied because they
/// were kept inside the enrollment directory: no byte sequence of either file is in any file
/// of the snapshot, the backup names each one it passed over, and it ends saying the CA's key
/// is not in it.
#[test]
fn a_backup_holds_no_byte_of_the_ca_key_even_when_it_is_in_the_enrollment_dir() {
    let dir = scratch("backup-ca");
    let store = dir.path().join("enrollment");
    fs::create_dir(&store).expect("the store's directory");
    let files = crate::enrollment::tests::support::Files {
        store: store.clone(),
        ca_key: store.join("ca.key"),
        server_cert: store.join("server.pem"),
        names: vec!["coordinator.example".to_owned()],
    };
    files
        .start(Arc::new(RealFs), FixedClock::at(NOW), os_random())
        .expect("enrollment starts");
    let secrets = [
        fs::read(&files.ca_key).expect("the CA key"),
        fs::read(&files.server_cert).expect("the server certificate file"),
    ];
    let state = dir.path().join("desired.json");
    fs::write(&state, b"{}\n").expect("a state file");
    let paths = StatePaths {
        state_file: state,
        ops_file: None,
        enrollment_dir: Some(store.clone()),
    };
    let snapshot = dir.path().join("snapshot");
    let report = backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");

    let held = files_under(&snapshot);
    assert!(held.contains_key("enrollment/serial.pb"), "{held:?}");
    for (name, bytes) in &held {
        for secret in &secrets {
            assert!(
                !bytes.windows(secret.len()).any(|w| w == secret.as_slice()),
                "{name} holds a key file"
            );
        }
    }
    let text = report.to_string();
    let tail: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("not copied") || line.starts_with("not in"))
        .collect();
    assert_eq!(
        tail,
        [
            format!(
                "not copied, no file of the enrollment store: {}",
                store.join("ca.key").display()
            )
            .as_str(),
            &format!(
                "not copied, no file of the enrollment store: {}",
                store.join("ca.key.cert").display()
            ),
            &format!(
                "not copied, no file of the enrollment store: {}",
                store.join("server.pem").display()
            ),
            "not in this backup: the CA's key and certificate (--ca-key-file); back them up once, sealed and offline, as docs/operations/ca-backup.md says",
        ]
    );
}

/// Catches: a backup that reads the state while a coordinator (here the test) holds the lock,
/// and so may copy a file half written: it is refused with the lock's path, and neither the
/// snapshot nor its partial directory is made. Once the lock is let go, the same backup is made.
#[test]
fn a_backup_is_refused_while_the_lock_is_held() {
    let dir = scratch("backup-locked");
    let state = dir.path().join("desired.json");
    fs::write(&state, b"{}\n").expect("a state file");
    let paths = StatePaths {
        state_file: state.clone(),
        ops_file: None,
        enrollment_dir: None,
    };
    let held = FileLock
        .acquire(&lock_path(&state))
        .expect("the test holds the lock");
    let snapshot = dir.path().join("snapshot");
    let refused = backup(&backup_flags(&paths, &snapshot), &FileLock).expect_err("held");
    assert!(
        matches!(&refused, BackupError::Locked { path } if *path == lock_path(&state)),
        "{refused:?}"
    );
    assert_eq!(
        refused.to_string(),
        format!(
            "{}: a coordinator holds the lock of these state files; stop it first, so that no file changes while it is copied",
            lock_path(&state).display()
        )
    );
    assert!(!snapshot.exists());
    assert!(!dir.path().join("snapshot.partial").exists());
    drop(held);
    backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup once let go");
    assert_eq!(
        files_under(&snapshot).keys().collect::<Vec<_>>(),
        [MANIFEST, "state"]
    );
}

/// A lock that cannot be taken: its file could not be opened.
pub(crate) struct Unopenable;

impl StateLock for Unopenable {
    fn acquire(&self, _path: &Path) -> Result<crate::lock::LockGuard, LockError> {
        Err(LockError::Io(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied,
        )))
    }
}

/// Catches: a lock that could not be taken read as one taken: the backup is refused naming the
/// lock file and why.
#[test]
fn a_backup_is_refused_when_the_lock_cannot_be_taken() {
    let dir = scratch("backup-lock-io");
    let state = dir.path().join("desired.json");
    fs::write(&state, b"{}\n").expect("a state file");
    let paths = StatePaths {
        state_file: state.clone(),
        ops_file: None,
        enrollment_dir: None,
    };
    let refused = backup(
        &backup_flags(&paths, &dir.path().join("snapshot")),
        &Unopenable,
    )
    .expect_err("io");
    assert_eq!(
        refused.to_string(),
        format!(
            "{}: the lock of the state files could not be taken: permission denied",
            lock_path(&state).display()
        )
    );
}

/// The state files in `dir` of a coordinator that wrote little, as raw bytes, with an
/// enrollment store of one host row: enough for the refusals, which read no file's contents.
pub(crate) fn small_state(dir: &Scratch) -> StatePaths {
    let state = dir.path().join("desired.json");
    fs::write(&state, b"{\"services\":{}}\n").expect("a state file");
    fs::write(jti_path(&state), b"spent 1900000000\n").expect("spent ids");
    let store = dir.path().join("enrollment");
    fs::create_dir_all(store.join("hosts")).expect("a store");
    fs::write(store.join("serial.pb"), b"\x08\x01\x10\x02").expect("a serial");
    fs::write(store.join("hosts").join("s1.pb"), b"\x08\x01").expect("a host row");
    StatePaths {
        state_file: state,
        ops_file: None,
        enrollment_dir: Some(store),
    }
}

/// A snapshot of [`small_state`] in a directory of its own: the directory and the snapshot.
pub(crate) fn small_snapshot(test: &str) -> (Scratch, PathBuf) {
    let dir = scratch(test);
    let paths = small_state(&dir);
    let snapshot = dir.path().join("snapshot");
    backup(&backup_flags(&paths, &snapshot), &FileLock).expect("a backup");
    (dir, snapshot)
}
