//! The tests of `repository_push`: applying a pushed set to a scratch directory, owned by the
//! test's own user as the loader's rule asks, with no real `/etc`.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use hostkeeper_link::testing::RecordingLog;
use hostkeeper_testkit::Scratch;
use hostkeeper_unpack::Owner;
use sha2::{Digest, Sha256};

use super::*;

/// A scratch directory under the target directory's `tmp`.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// A binary keyring of one public key (a v4 key packet of 14 bytes of body, which `seed` makes
/// its own) and the key's fingerprint.
fn keyring(seed: u8) -> (Vec<u8>, String) {
    let body = [
        4, 0, 0, 0, seed, 1, 0, 8, 0xAA, 0xBB, 0xCC, seed, 0xDD, 0xEE,
    ];
    let mut bytes = vec![0x99, 0, body.len() as u8];
    bytes.extend_from_slice(&body);
    let found = registry::keys(&bytes).expect("a keyring");
    (bytes, found[0].fingerprint.clone())
}

fn entry(name: &str, suites: &[&str], seed: u8) -> Repository {
    let (keyring, fingerprint) = keyring(seed);
    Repository {
        name: name.to_owned(),
        kind: "apt".to_owned(),
        uri: "https://repo.example.com/debian".to_owned(),
        suites: suites.iter().map(|suite| (*suite).to_owned()).collect(),
        components: vec!["main".to_owned()],
        fingerprints: vec![fingerprint],
        keyring,
        ..Default::default()
    }
}

fn set_of(generation: u64, entries: Vec<Repository>) -> PushedSet {
    PushedSet {
        generation,
        digest: registry::digest(&entries),
        entries,
    }
}

/// The komira set: stable and testing of one repository.
fn komira(generation: u64, seed: u8) -> PushedSet {
    set_of(
        generation,
        vec![entry("komira", &["stable", "testing"], seed)],
    )
}

struct Rig {
    _scratch: Scratch,
    base: PathBuf,
    live: PathBuf,
    ports: Arc<RepositoryPorts>,
    log: Arc<RecordingLog>,
    push: RepositoryPush,
}

impl Rig {
    fn new(name: &str) -> Rig {
        Rig::with(name, Installer::Apt, |_| {})
    }

    /// A rig whose live directory is `<base>/etc/hostkeeper/repositories`, whose parent is made
    /// by `shape` (or not, as it does).
    fn with(name: &str, native: Installer, shape: impl FnOnce(&Path)) -> Rig {
        let scratch = scratch(name);
        let base = scratch.path().to_path_buf();
        fs::create_dir_all(base.join("state/packages")).expect("the state directories");
        let live = base.join("etc/hostkeeper/repositories");
        shape(&base);
        let owner = fs::metadata(&base)
            .map(|meta| meta.uid())
            .expect("an owner");
        let ports = Arc::new(
            RepositoryPorts::new(
                &base.join("state/packages"),
                &base,
                Repositories::default(),
                Owner { uid: owner, gid: 0 },
            )
            .with_managed(Managed {
                dir: live.clone(),
                owner_uid: owner,
                native: Some(native),
            }),
        );
        let log = Arc::new(RecordingLog::default());
        let push = RepositoryPush::new(
            Arc::clone(&ports),
            Announcement::default(),
            Arc::clone(&log) as Arc<dyn Log>,
        )
        .expect("a managed host");
        Rig {
            _scratch: scratch,
            base,
            live,
            ports,
            log,
            push,
        }
    }

    /// The mirrors' parent of the host, as the sets are rendered for it.
    fn mirror(&self) -> &str {
        self.ports.dirs.mirror.to_str().expect("utf-8")
    }

    fn staging(&self) -> PathBuf {
        self.live.with_file_name(STAGING)
    }

    /// Places `files` as host setup would: the directory at 0755, each file at 0644.
    fn place(&self, files: &[HostFile]) {
        fs::create_dir_all(&self.live).expect("the live directory");
        fs::set_permissions(&self.live, Permissions::from_mode(0o755)).expect("chmod");
        for file in files {
            let path = self.live.join(&file.name);
            fs::write(&path, &file.bytes).expect("a file");
            fs::set_permissions(&path, Permissions::from_mode(0o644)).expect("chmod");
        }
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.live)
            .expect("the live directory")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    fn in_use(&self) -> Vec<String> {
        let repositories = self.ports.repositories.snapshot();
        repositories
            .names()
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

const KOMIRA_FILES: [&str; 3] = [
    "komira-stable.sources",
    "komira-testing.sources",
    "komira.gpg",
];

/// Proves a live directory that already holds the set's files byte for byte (what the held
/// script wrote) is adopted: nothing is written (the directory and each file keep their inode
/// and modification time), the set is in force, and its entries are in use.
#[test]
fn files_already_in_place_are_adopted_without_a_write() {
    let rig = Rig::new("push-adopt");
    let set = komira(3, 1);
    rig.place(&registry::render(
        &set.entries,
        rig.live.to_str().expect("utf-8"),
        rig.mirror(),
    ));
    let before: Vec<(u64, i64, i64)> = KOMIRA_FILES
        .iter()
        .map(|name| {
            let meta = fs::metadata(rig.live.join(name)).expect("a file");
            (meta.ino(), meta.mtime(), meta.mtime_nsec())
        })
        .collect();
    let dir = fs::metadata(&rig.live).expect("the directory").ino();
    assert_eq!(rig.push.apply_set(&set), Applied::Adopted);
    let after: Vec<(u64, i64, i64)> = KOMIRA_FILES
        .iter()
        .map(|name| {
            let meta = fs::metadata(rig.live.join(name)).expect("a file");
            (meta.ino(), meta.mtime(), meta.mtime_nsec())
        })
        .collect();
    assert_eq!(before, after);
    assert_eq!(fs::metadata(&rig.live).expect("the directory").ino(), dir);
    assert!(!rig.staging().exists());
    assert_eq!(rig.in_use(), ["komira-stable", "komira-testing"]);
}

/// Proves the sink takes the files already in place as the set in force, says so in the log, and
/// reports the set's generation and digest.
#[test]
fn the_sink_reports_a_set_it_adopted() {
    let rig = Rig::new("push-sink-adopt");
    let set = komira(6, 1);
    rig.place(&registry::render(
        &set.entries,
        rig.live.to_str().expect("utf-8"),
        rig.mirror(),
    ));
    rig.push.apply(&set);
    assert_eq!(
        rig.push.report(),
        RepositoriesReport {
            generation: 6,
            digest: set.digest,
            refused: None,
        }
    );
    assert_eq!(
        rig.log.lines(),
        ["repositories generation 6: the files already in place are in force"]
    );
}

/// Proves the held script's own files are the files adopted: the rendering for komira, written
/// as the script does, is taken as it is.
#[test]
fn the_held_scripts_files_are_what_a_set_for_komira_renders() {
    let rig = Rig::new("push-script");
    let set = komira(1, 2);
    let live = rig.live.to_str().expect("utf-8");
    let files = registry::render(&set.entries, live, rig.mirror());
    let stanza = |suite: &str| {
        format!(
            "Types: deb\nURIs: https://repo.example.com/debian\nSuites: {suite}\nComponents: main\nSigned-By: {live}/komira.gpg\n"
        )
    };
    let named = |name: &str| {
        files
            .iter()
            .find(|file| file.name == name)
            .map(|file| file.bytes.clone())
    };
    assert_eq!(
        named("komira-stable.sources"),
        Some(stanza("stable").into_bytes())
    );
    assert_eq!(
        named("komira-testing.sources"),
        Some(stanza("testing").into_bytes())
    );
}

/// Proves a live directory that differs (other bytes, a stray file) is replaced whole: the new
/// files are there at 0644 in a directory at 0755, the stray is gone, no staging directory is
/// left, and the entries in use are the set's.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "replacing a live directory is renameat2 with RENAME_EXCHANGE, which only Linux has; a Mac is never sent a set"
)]
fn a_differing_directory_is_replaced_whole() {
    let rig = Rig::new("push-replace");
    let old = komira(1, 1);
    rig.place(&registry::render(
        &old.entries,
        rig.live.to_str().expect("utf-8"),
        rig.mirror(),
    ));
    fs::write(rig.live.join("stray.sources"), b"x").expect("a stray file");
    let set = komira(2, 2);
    assert_eq!(rig.push.apply_set(&set), Applied::Written);
    assert_eq!(rig.names(), KOMIRA_FILES);
    assert_eq!(
        fs::read(rig.live.join("komira.gpg")).expect("the keyring"),
        set.entries[0].keyring
    );
    for name in KOMIRA_FILES {
        let mode = fs::metadata(rig.live.join(name)).expect("a file").mode() & 0o7777;
        assert_eq!(mode, 0o644, "{name}");
    }
    let mode = fs::metadata(&rig.live).expect("the directory").mode() & 0o7777;
    assert_eq!(mode, 0o755);
    assert!(!rig.staging().exists());
    assert_eq!(rig.in_use(), ["komira-stable", "komira-testing"]);
}

/// Proves a live directory that is not there is made: its parent is made too, and the set is
/// written in place by a rename.
#[test]
fn a_missing_directory_and_parent_are_made() {
    let rig = Rig::new("push-absent");
    assert!(!rig.live.exists());
    assert_eq!(rig.push.apply_set(&komira(1, 1)), Applied::Written);
    assert_eq!(rig.names(), KOMIRA_FILES);
    assert!(!rig.staging().exists());
}

/// Proves a live path that is a file, not a directory, is replaced by the set, and the file is
/// removed.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "replacing a live directory is renameat2 with RENAME_EXCHANGE, which only Linux has; a Mac is never sent a set"
)]
fn a_file_where_the_directory_belongs_is_replaced() {
    let rig = Rig::with("push-file", Installer::Apt, |base| {
        fs::create_dir_all(base.join("etc/hostkeeper")).expect("the parent");
        fs::write(base.join("etc/hostkeeper/repositories"), b"x").expect("a file");
    });
    assert_eq!(rig.push.apply_set(&komira(1, 1)), Applied::Written);
    assert_eq!(rig.names(), KOMIRA_FILES);
    assert!(!rig.staging().exists());
}

/// Proves the same bytes with a file the loader refuses (writable by its group) are not adopted:
/// the directory is replaced by files the loader takes.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "replacing a live directory is renameat2 with RENAME_EXCHANGE, which only Linux has; a Mac is never sent a set"
)]
fn files_the_loader_refuses_are_not_adopted() {
    let rig = Rig::new("push-unsound");
    let set = komira(1, 1);
    rig.place(&registry::render(
        &set.entries,
        rig.live.to_str().expect("utf-8"),
        rig.mirror(),
    ));
    fs::set_permissions(rig.live.join("komira.gpg"), Permissions::from_mode(0o664)).expect("chmod");
    assert_eq!(rig.push.apply_set(&set), Applied::Written);
    let mode = fs::metadata(rig.live.join("komira.gpg"))
        .expect("a file")
        .mode()
        & 0o7777;
    assert_eq!(mode, 0o644);
}

/// Proves one file missing, or one file's bytes changed, is not adopted either.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "replacing a live directory is renameat2 with RENAME_EXCHANGE, which only Linux has; a Mac is never sent a set"
)]
fn a_directory_with_a_file_missing_or_changed_is_replaced() {
    for change in 0..2 {
        let rig = Rig::new(&format!("push-partial-{change}"));
        let set = komira(1, 1);
        rig.place(&registry::render(
            &set.entries,
            rig.live.to_str().expect("utf-8"),
            rig.mirror(),
        ));
        match change {
            0 => fs::remove_file(rig.live.join("komira-testing.sources")).expect("remove"),
            _ => fs::write(rig.live.join("komira-testing.sources"), b"changed").expect("write"),
        }
        assert_eq!(rig.push.apply_set(&set), Applied::Written);
        assert_eq!(rig.names(), KOMIRA_FILES);
    }
}

/// Proves a set whose entry lists a fingerprint the keyring lacks is refused, with the live
/// directory and the entries in use as they were, and the report says so with the set's
/// generation, digest and the reason.
#[test]
fn a_wrong_fingerprint_is_refused_and_the_last_good_set_kept() {
    let rig = Rig::new("push-wrong-fingerprint");
    let good = komira(1, 1);
    rig.push.apply(&good);
    let before = rig.names();
    let mut bad = komira(2, 2);
    bad.entries[0].fingerprints = vec![keyring(9).1];
    bad.digest = registry::digest(&bad.entries);
    rig.push.apply(&bad);
    let report = rig.push.report();
    assert_eq!(report.generation, 1);
    assert_eq!(report.digest, good.digest);
    let refused = report.refused.expect("a refusal");
    assert_eq!(refused.generation, 2);
    assert_eq!(refused.digest, bad.digest);
    assert!(refused.reason.contains("komira"), "{}", refused.reason);
    assert_eq!(rig.names(), before);
    assert_eq!(
        fs::read(rig.live.join("komira.gpg")).expect("the keyring"),
        good.entries[0].keyring
    );
    assert_eq!(rig.in_use(), ["komira-stable", "komira-testing"]);
}

/// Proves a keyring that is not the one the rules judged is refused: the keyring of the staged
/// file is changed, after it is written and before the loader reads it, to another valid
/// keyring, and the entry's fingerprint is no longer in it.
#[test]
fn a_keyring_that_is_not_the_one_judged_is_refused_and_the_live_directory_kept() {
    let rig = Rig::new("push-swapped-keyring");
    rig.push.apply(&komira(1, 1));
    let applied = rig.push.apply_with(&komira(2, 2), &|staged| {
        fs::write(staged.join("komira.gpg"), keyring(7).0).expect("a swapped keyring");
    });
    let Applied::Refused(reason) = applied else {
        panic!("{applied:?}");
    };
    assert!(reason.contains("keyring"), "{reason}");
    assert_eq!(
        fs::read(rig.live.join("komira.gpg")).expect("the keyring"),
        keyring(1).0
    );
    assert_eq!(rig.in_use(), ["komira-stable", "komira-testing"]);
}

/// Proves the loader's refusal of the staged files (a file that is not a repository file) is a
/// refusal that keeps the live directory, and the leftover staging directory is removed by the
/// next set.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "replacing a live directory is renameat2 with RENAME_EXCHANGE, which only Linux has; a Mac is never sent a set"
)]
fn a_staged_directory_the_loader_refuses_is_refused_and_the_live_directory_kept() {
    let rig = Rig::new("push-loader-refuses");
    rig.push.apply(&komira(1, 1));
    let applied = rig.push.apply_with(&komira(2, 2), &|staged| {
        fs::write(staged.join("notes.txt"), b"x").expect("a stray file");
    });
    assert!(matches!(applied, Applied::Refused(_)), "{applied:?}");
    assert_eq!(
        fs::read(rig.live.join("komira.gpg")).expect("the keyring"),
        keyring(1).0
    );
    assert!(rig.staging().exists());
    assert_eq!(rig.push.apply_set(&komira(3, 3)), Applied::Written);
    assert!(!rig.staging().exists());
}

/// Proves a daemon whose owner is not the files' owner refuses the set at the loader, whatever
/// the rules say of it, and keeps what it has.
#[test]
fn files_of_another_owner_are_refused_by_the_loader() {
    let rig = Rig::new("push-owner");
    let mut ports = (*rig.ports).clone();
    let managed = ports.managed.clone().expect("managed");
    ports.managed = Some(Managed {
        owner_uid: managed.owner_uid + 1,
        ..managed
    });
    let push = RepositoryPush::new(
        Arc::new(ports),
        Announcement::default(),
        Arc::clone(&rig.log) as Arc<dyn Log>,
    )
    .expect("managed");
    let applied = push.apply_set(&komira(1, 1));
    assert!(matches!(applied, Applied::Refused(_)), "{applied:?}");
    assert!(!rig.live.exists());
}

/// Proves a set whose digest is not the digest of its entries is refused before anything is
/// written, and an empty set and one the rules refuse are too.
#[test]
fn a_set_that_fails_its_checks_is_refused_and_nothing_is_written() {
    let rig = Rig::new("push-checks");
    let mut digest = komira(1, 1);
    digest.digest = "0".repeat(64);
    let Applied::Refused(reason) = rig.push.apply_set(&digest) else {
        panic!("not refused");
    };
    assert!(
        reason.contains(&registry::digest(&digest.entries)),
        "{reason}"
    );
    let empty = set_of(1, Vec::new());
    assert_eq!(
        rig.push.apply_set(&empty),
        Applied::Refused("the set holds no repository".to_owned())
    );
    let mut invalid = komira(1, 1);
    invalid.entries[0].kind = "rpm".to_owned();
    invalid.digest = registry::digest(&invalid.entries);
    let Applied::Refused(reason) = rig.push.apply_set(&invalid) else {
        panic!("not refused");
    };
    assert!(reason.contains("komira"), "{reason}");
    let nine: Vec<Repository> = (0..9)
        .map(|n| entry(&format!("repo{n}"), &["stable"], n))
        .collect();
    assert!(matches!(
        rig.push.apply_set(&set_of(1, nine)),
        Applied::Refused(_)
    ));
    assert!(!rig.live.exists());
    assert!(!rig.staging().exists());
}

/// Proves a rotation of the keyring and its fingerprints under the same name is applied: the
/// file holds the new keyring, the report shows the new generation and digest, and a refusal
/// that came before is cleared.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "replacing a live directory is renameat2 with RENAME_EXCHANGE, which only Linux has; a Mac is never sent a set"
)]
fn a_key_rotation_is_applied_and_clears_an_earlier_refusal() {
    let rig = Rig::new("push-rotation");
    let first = komira(1, 1);
    rig.push.apply(&first);
    let mut refused = komira(2, 2);
    refused.digest = "0".repeat(64);
    rig.push.apply(&refused);
    assert!(rig.push.report().refused.is_some());
    let rotated = komira(3, 2);
    rig.push.apply(&rotated);
    assert_eq!(
        rig.push.report(),
        RepositoriesReport {
            generation: 3,
            digest: rotated.digest.clone(),
            refused: None,
        }
    );
    assert_eq!(
        fs::read(rig.live.join("komira.gpg")).expect("the keyring"),
        keyring(2).0
    );
}

/// Proves the report is generation 0 and no digest, and no refusal, until a set is applied.
#[test]
fn the_report_is_empty_before_the_first_set() {
    let rig = Rig::new("push-start");
    assert_eq!(rig.push.report(), RepositoriesReport::default());
}

/// Proves a failure of the filesystem (the live directory's parent cannot be made: a file is in
/// its way) is logged and not recorded as a refusal, and nothing is in force.
#[test]
fn a_failure_of_the_filesystem_is_not_recorded_as_a_refusal() {
    let rig = Rig::with("push-io", Installer::Apt, |base| {
        fs::write(base.join("etc"), b"a file").expect("a file in the way");
    });
    let applied = rig.push.apply_set(&komira(1, 1));
    assert!(matches!(applied, Applied::Failed(_)), "{applied:?}");
    rig.push.apply(&komira(1, 1));
    assert_eq!(rig.push.report(), RepositoriesReport::default());
    let lines = rig.log.lines();
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].starts_with("error: repositories generation 1 is not written: "),
        "{lines:?}"
    );
}

/// Proves a live path the system cannot look at (a name too long) fails at the swap, after the
/// set was staged and judged, as a failure of the filesystem: nothing is recorded, and what was
/// staged is left for the next set to remove.
#[test]
fn a_swap_that_the_filesystem_refuses_is_a_failure() {
    let long = "r".repeat(300);
    let rig = Rig::with("push-swap-io", Installer::Apt, |_| {});
    let mut ports = (*rig.ports).clone();
    let managed = ports.managed.clone().expect("managed");
    ports.managed = Some(Managed {
        dir: rig.base.join("etc/hostkeeper").join(&long),
        ..managed
    });
    let push = RepositoryPush::new(
        Arc::new(ports),
        Announcement::default(),
        Arc::clone(&rig.log) as Arc<dyn Log>,
    )
    .expect("managed");
    let applied = push.apply_set(&komira(1, 1));
    assert!(matches!(applied, Applied::Failed(_)), "{applied:?}");
    assert!(rig.staging().exists());
    assert!(rig.ports.repositories.snapshot().is_empty());
}

/// Proves a staging directory left by an earlier run, with a file in it, and one that is a plain
/// file, are removed before the set is staged.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "replacing a live directory is renameat2 with RENAME_EXCHANGE, which only Linux has; a Mac is never sent a set"
)]
fn a_leftover_staging_directory_or_file_is_removed_first() {
    let rig = Rig::new("push-leftover");
    fs::create_dir_all(rig.staging()).expect("a leftover");
    fs::write(rig.staging().join("old.sources"), b"x").expect("a file");
    assert_eq!(rig.push.apply_set(&komira(1, 1)), Applied::Written);
    assert!(!rig.staging().exists());
    fs::write(rig.staging(), b"x").expect("a leftover file");
    assert_eq!(rig.push.apply_set(&komira(2, 2)), Applied::Written);
    assert!(!rig.staging().exists());
    assert_eq!(rig.names(), KOMIRA_FILES);
}

/// Proves a set that finds the installer lock held does nothing and is not recorded, the line
/// says so, and the same set is applied once the lock is let go.
#[test]
fn a_held_installer_lock_defers_the_set_to_the_next_heartbeat() {
    let rig = Rig::new("push-busy");
    let set = komira(1, 1);
    let held = InstallerLock::new(&rig.ports.lock)
        .hold()
        .expect("the lock");
    rig.push.apply(&set);
    assert!(!rig.live.exists());
    assert_eq!(rig.push.report(), RepositoriesReport::default());
    assert_eq!(
        rig.log.lines(),
        [
            "repositories generation 1: the installer lock is held; it is tried again at the next heartbeat"
        ]
    );
    drop(held);
    rig.push.apply(&set);
    assert_eq!(rig.names(), KOMIRA_FILES);
    assert_eq!(rig.push.report().generation, 1);
}

/// Proves a lock file that cannot be opened is a failure of the filesystem, and applies nothing.
#[test]
fn an_installer_lock_that_cannot_be_opened_is_a_failure() {
    let rig = Rig::new("push-lock-io");
    let mut ports = (*rig.ports).clone();
    ports.lock = rig.base.join("missing/installer.lock");
    let push = RepositoryPush::new(
        Arc::new(ports),
        Announcement::default(),
        Arc::clone(&rig.log) as Arc<dyn Log>,
    )
    .expect("managed");
    let applied = push.apply_set(&komira(1, 1));
    let Applied::Failed(error) = applied else {
        panic!("{applied:?}");
    };
    assert!(error.starts_with("the installer lock: "), "{error}");
    assert!(!rig.live.exists());
}

/// Proves the installers the host announces follow the entries in use: apt is not there before
/// a set is applied, and is after, once, without a restart; the entries the installer reads are
/// the set's at its next step.
#[test]
fn the_installers_and_the_installer_follow_the_set_applied() {
    let rig = Rig::new("push-installers");
    assert_eq!(rig.push.installers(), ["hostkeeper-archive"]);
    let snapshot = rig.ports.repositories.snapshot();
    rig.push.apply(&komira(1, 1));
    assert_eq!(rig.push.installers(), ["hostkeeper-archive", "apt"]);
    assert!(
        snapshot.is_empty(),
        "a snapshot taken before is not changed"
    );
    let now = rig.ports.repositories.snapshot();
    let stable = now.get("komira-stable").expect("komira-stable");
    assert_eq!(stable.suite, "stable");
    assert_eq!(stable.keyring, rig.live.join("komira.gpg"));
    rig.push.apply(&komira(2, 2));
    assert_eq!(rig.push.installers(), ["hostkeeper-archive", "apt"]);
}

/// Proves apt is listed once when the daemon announced it at its start, and not added for a host
/// that cannot run it or has no repository directory to manage.
#[test]
fn the_native_installer_is_added_only_once_and_only_when_the_host_can_run_it() {
    let rig = Rig::new("push-native");
    let started = Announcement {
        installers: vec![Installer::HostkeeperArchive, Installer::Apt],
        family: None,
    };
    rig.push.apply(&komira(1, 1));
    assert_eq!(
        installers(&started, &rig.ports),
        [Installer::HostkeeperArchive, Installer::Apt]
    );
    let mut ports = (*rig.ports).clone();
    ports.managed = Some(Managed {
        native: None,
        ..ports.managed.clone().expect("managed")
    });
    assert_eq!(
        installers(&Announcement::default(), &ports),
        [Installer::HostkeeperArchive]
    );
    ports.managed = None;
    assert_eq!(
        installers(&Announcement::default(), &ports),
        [Installer::HostkeeperArchive]
    );
    assert!(
        RepositoryPush::new(
            Arc::new(ports),
            Announcement::default(),
            Arc::clone(&rig.log) as Arc<dyn Log>
        )
        .is_none(),
        "a host with no repository directory has no sink"
    );
}

/// Proves a refusal's reason is cut to the bytes the coordinator reads at a character boundary:
/// two-byte and three-byte characters are cut whole, a short reason is kept, and the empty one.
#[test]
fn a_reason_is_cut_at_a_character_boundary() {
    assert_eq!(clip("short"), "short");
    assert_eq!(clip(""), "");
    let two = "é".repeat(200);
    assert_eq!(clip(&two), "é".repeat(128));
    let three = "€".repeat(100);
    assert_eq!(clip(&three), "€".repeat(85));
    let exact = "a".repeat(MAX_REFUSAL_REASON);
    assert_eq!(clip(&exact), exact);
    assert_eq!(clip(&format!("{exact}b")), exact);
}

/// Proves a long refusal is reported cut: the reason of a refused set is at most 256 bytes.
#[test]
fn a_refusal_reports_a_reason_of_at_most_the_bound() {
    let rig = Rig::new("push-long-reason");
    let mut bad = komira(1, 1);
    bad.digest = "0".repeat(64);
    bad.entries[0].name = "n".repeat(400);
    rig.push.apply(&bad);
    let refused = rig.push.report().refused.expect("a refusal");
    assert!(!refused.reason.is_empty());
    assert!(refused.reason.len() <= MAX_REFUSAL_REASON);
}

/// Proves the shared entries are one value for every clone, a snapshot is not changed by a later
/// replacement, and a handle made by default holds none.
#[test]
fn the_shared_entries_are_replaced_whole_and_snapshots_keep_their_value() {
    let shared = SharedRepositories::default();
    assert!(shared.snapshot().is_empty());
    let other = shared.clone();
    let before = shared.snapshot();
    let rig = Rig::new("push-shared");
    rig.push.apply(&komira(1, 1));
    other.replace((*rig.ports.repositories.snapshot()).clone());
    assert!(before.is_empty());
    assert_eq!(
        shared.snapshot().names(),
        ["komira-stable", "komira-testing"]
    );
}

/// Proves the staged files' digest is what the coordinator computes: the SHA-256 over what the
/// set is written as for the production directory.
#[test]
fn the_digest_of_a_set_is_the_registrys() {
    let set = komira(1, 1);
    let files = registry::render(
        &set.entries,
        registry::REPOSITORIES_DIR,
        registry::MIRROR_DIR,
    );
    let mut hasher = Sha256::new();
    let mut sorted: Vec<&HostFile> = files.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for file in sorted {
        hasher.update(file.name.as_bytes());
        hasher.update([0]);
        hasher.update((file.bytes.len() as u64).to_be_bytes());
        hasher.update(&file.bytes);
    }
    assert_eq!(set.digest, hex::encode(hasher.finalize()));
}

/// A private entry of the host's group `suite`: the origin's URI, no credential (the host is
/// never sent one).
fn private(name: &str, suite: &str, seed: u8) -> Repository {
    Repository {
        access: "hosts".to_owned(),
        ..entry(name, &[suite], seed)
    }
}

/// Makes the file `path` below the host's mirror root, with its directories.
fn mirrored(rig: &Rig, path: &str) {
    let full = rig.ports.dirs.mirror.join(path);
    fs::create_dir_all(full.parent().expect("a parent")).expect("directories");
    fs::write(full, b"x").expect("a file");
}

/// Proves a private entry is written for the host's own mirror: its sources file names
/// `file:<mirror root>/<name>` with the host's group as the suite, the loader takes it as the
/// entry's URI, the set's digest (computed at the production directories) still matches, and the
/// same set is adopted as it is the next time.
#[test]
fn a_private_entry_is_written_with_the_uri_of_the_hosts_mirror() {
    let rig = Rig::new("push-private");
    let set = set_of(1, vec![private("acme", "web", 1)]);
    assert_eq!(rig.push.apply_set(&set), Applied::Written);
    let uri = format!("file:{}/acme", rig.mirror());
    let text = fs::read_to_string(rig.live.join("acme-web.sources")).expect("the sources");
    assert!(
        text.contains(&format!("URIs: {uri}\nSuites: web\n")),
        "{text}"
    );
    let held = rig.ports.repositories.snapshot();
    let loaded = held.get("acme-web").expect("the channel");
    assert_eq!(
        (loaded.uri.as_str(), loaded.suite.as_str()),
        (uri.as_str(), "web")
    );
    assert_eq!(rig.push.apply_set(&set), Applied::Adopted);
}

/// Proves an applied set removes the mirrors it does not keep, whether it was written or its files
/// were already in place: a mirror of a repository the set does not name, and one of the entry's
/// repository holding another group's suite (the host moved), go; the mirror of the entry's own
/// suite stays.
#[test]
fn an_applied_set_removes_the_mirrors_it_does_not_keep() {
    let rig = Rig::new("push-prune");
    let web = set_of(1, vec![private("acme", "web", 1)]);
    mirrored(&rig, "acme/dists/web/InRelease");
    mirrored(&rig, "old/dists/web/InRelease");
    assert_eq!(rig.push.apply_set(&web), Applied::Written);
    assert!(!rig.ports.dirs.mirror.join("old").exists());
    assert!(
        rig.ports
            .dirs
            .mirror
            .join("acme/dists/web/InRelease")
            .is_file()
    );

    mirrored(&rig, "old/dists/web/InRelease");
    assert_eq!(rig.push.apply_set(&web), Applied::Adopted);
    assert!(!rig.ports.dirs.mirror.join("old").exists());

    let dev = set_of(2, vec![private("acme", "dev", 1)]);
    assert_eq!(rig.push.apply_set(&dev), Applied::Written);
    assert!(!rig.ports.dirs.mirror.join("acme").exists());
}

/// Proves a mirror that cannot be removed does not undo the set: it is in force, and the log says
/// what was not removed.
#[test]
fn a_mirror_that_cannot_be_removed_is_logged_and_the_set_stays_in_force() {
    let rig = Rig::new("push-prune-fails");
    fs::create_dir_all(rig.ports.dirs.mirror.parent().expect("a parent")).expect("state");
    fs::write(&rig.ports.dirs.mirror, b"a file where the mirrors belong").expect("a file");
    let set = set_of(1, vec![private("acme", "web", 1)]);
    rig.push.apply(&set);
    assert_eq!(rig.in_use(), ["acme-web"]);
    assert_eq!(rig.push.report().generation, 1);
    let lines = rig.log.lines();
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("error: a mirror the set does not keep is not removed: ")),
        "{lines:?}"
    );
}
