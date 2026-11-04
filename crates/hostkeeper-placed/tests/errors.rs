//! The error values, the systemctl commands, the fakes and the directory layout of
//! `hostkeeper-placed`: the class, reason and text of every refusal, the argument vectors that
//! reach the launcher, and what the test doubles record and leave on disk.

mod support;

use std::error::Error as _;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::Class;
use hostkeeper_config::names::PathError;
use hostkeeper_placed::testing::{
    Event, FakeLauncher, FakeUnitControl, FaultFs, FsWrite, Log, UnitCall,
};
use hostkeeper_placed::{
    ClaimError, FileKind, Fs, LintError, Owner, PackageDirs, PlacedError, PlacedLock, Placer,
    Roots, Systemctl, UnitControl, UnitControlError,
};
use support::World;

fn units_error() -> UnitControlError {
    UnitControlError {
        action: "systemctl daemon-reload".to_owned(),
        why: "exit status 1".to_owned(),
    }
}

/// Catches a wrong class or reason token for any variant, and a changed message: one value of
/// each variant is asked its class, reason and full text.
#[test]
fn every_placed_error_has_its_class_and_reason() {
    let place = PathBuf::from("/etc/tierstore/tierstore.conf");
    let record = PathBuf::from("/var/lib/hostkeeper/placed.json");
    let cases = [
        (
            PlacedError::PathConflict {
                path: place.clone(),
                owner: Owner::Service("other".to_owned()),
            },
            Class::Release,
            "path_conflict",
            "/etc/tierstore/tierstore.conf is held by the service other".to_owned(),
        ),
        (
            PlacedError::Claim(ClaimError::EmptyPath),
            Class::Release,
            "manifest_mismatch",
            "a [[files]] path is empty".to_owned(),
        ),
        (
            PlacedError::Lint(LintError::RunUnit {
                unit: "a.service".to_owned(),
            }),
            Class::Release,
            "manifest_mismatch",
            "the [units] run entry \"a.service\" is neither a file of units/ nor an instance of a template there"
                .to_owned(),
        ),
        (
            PlacedError::Tombstoned {
                service: "tierstore".to_owned(),
            },
            Class::Environment,
            "uninstalling",
            "the service tierstore is being uninstalled; its paths are not linked".to_owned(),
        ),
        (
            PlacedError::Corrupt {
                path: record.clone(),
                why: "expected a map".to_owned(),
            },
            Class::Environment,
            "link_failed",
            "/var/lib/hostkeeper/placed.json is not a placed-path record: expected a map"
                .to_owned(),
        ),
        (
            PlacedError::NewerSchema {
                path: record.clone(),
                schema: 7,
            },
            Class::Environment,
            "link_failed",
            "/var/lib/hostkeeper/placed.json has schema 7; this daemon reads schema 1 and leaves it as it is"
                .to_owned(),
        ),
        (
            PlacedError::Io {
                op: "write",
                path: record.clone(),
                source: io::Error::other("disk full"),
            },
            Class::Environment,
            "link_failed",
            "cannot write /var/lib/hostkeeper/placed.json: disk full".to_owned(),
        ),
        (
            PlacedError::Locked {
                path: record,
                source: io::Error::other("busy"),
            },
            Class::Environment,
            "link_failed",
            "another daemon holds /var/lib/hostkeeper/placed.json, or its lock cannot be taken: busy"
                .to_owned(),
        ),
        (
            PlacedError::Units(units_error()),
            Class::Environment,
            "daemon_reload_failed",
            "systemctl daemon-reload failed: exit status 1".to_owned(),
        ),
    ];
    for (error, class, reason, text) in cases {
        assert_eq!(error.class(), class, "{text}");
        assert_eq!(error.reason(), reason, "{text}");
        assert_eq!(error.to_string(), text);
    }
}

/// Catches a lost source on the two io variants, which a log line needs to name the cause.
#[test]
fn io_and_locked_errors_keep_their_source() {
    let read_error = PlacedError::Io {
        op: "read",
        path: PathBuf::from("/p"),
        source: io::Error::other("cause"),
    };
    let locked = PlacedError::Locked {
        path: PathBuf::from("/p"),
        source: io::Error::other("cause"),
    };
    assert_eq!(read_error.source().expect("a source").to_string(), "cause");
    assert_eq!(locked.source().expect("a source").to_string(), "cause");
}

/// Catches an owner described wrongly in a path_conflict message: every variant's exact text.
#[test]
fn every_owner_has_its_text() {
    assert_eq!(
        Owner::Service("other".to_owned()).to_string(),
        "the service other"
    );
    assert_eq!(
        Owner::File.to_string(),
        "a regular file placed.json does not list"
    );
    assert_eq!(
        Owner::Directory.to_string(),
        "a directory placed.json does not list"
    );
    assert_eq!(
        Owner::Link(PathBuf::from("/srv/elsewhere")).to_string(),
        "a link to /srv/elsewhere that placed.json does not list"
    );
    assert_eq!(
        Owner::Other.to_string(),
        "a socket, pipe or device placed.json does not list"
    );
}

/// Catches a changed claim refusal text: every variant's exact message.
#[test]
fn every_claim_error_has_its_text() {
    assert_eq!(
        ClaimError::UnitName {
            service: "tierstore".to_owned(),
            file: "other.service".to_owned(),
        }
        .to_string(),
        "the unit file units/other.service does not start with the service name tierstore"
    );
    assert_eq!(
        ClaimError::EmptyPath.to_string(),
        "a [[files]] path is empty"
    );
    assert_eq!(
        ClaimError::BadPath {
            path: "etc/x".to_owned(),
            why: PathError::NotAbsolute,
        }
        .to_string(),
        "the [[files]] path \"etc/x\" is not absolute"
    );
    assert_eq!(
        ClaimError::Refused {
            path: "/usr/bin/x".to_owned(),
            prefix: "/usr",
        }
        .to_string(),
        "the [[files]] path \"/usr/bin/x\" is /usr or below it, where a package may not place a file"
    );
    assert_eq!(
        ClaimError::Duplicate {
            path: PathBuf::from("/etc/x"),
        }
        .to_string(),
        "two entries of the package claim the path /etc/x"
    );
}

/// Catches a lint refusal that is not a release/manifest_mismatch one, and a changed text.
#[test]
fn every_lint_error_is_a_manifest_mismatch() {
    let errors = [
        (
            LintError::RunUnit {
                unit: "a.service".to_owned(),
            },
            "the [units] run entry \"a.service\" is neither a file of units/ nor an instance of a template there",
        ),
        (
            LintError::PackagePath {
                file: "a.service".to_owned(),
                key: "ExecStart".to_owned(),
                path: "/var/lib/hostkeeper/packages/a/1.0.0/bin/a".to_owned(),
                packages: "/var/lib/hostkeeper/packages".to_owned(),
                service: "a".to_owned(),
            },
            "units/a.service ExecStart names \"/var/lib/hostkeeper/packages/a/1.0.0/bin/a\", which is under /var/lib/hostkeeper/packages/ but not /var/lib/hostkeeper/packages/a/current/",
        ),
        (
            LintError::NoWantedBy {
                unit: "a.service".to_owned(),
                file: "a.service".to_owned(),
            },
            "the run unit a.service has no [Install] WantedBy= in units/a.service, so the daemon cannot enable it",
        ),
        (
            LintError::BadTarget {
                file: "a.service".to_owned(),
                target: "x y".to_owned(),
            },
            "units/a.service has WantedBy=\"x y\", which is not a unit name of a known type of at most 249 bytes",
        ),
        (
            LintError::Unreadable {
                file: "a.service".to_owned(),
                why: "not utf-8".to_owned(),
            },
            "units/a.service cannot be read as text: not utf-8",
        ),
    ];
    for (error, text) in errors {
        assert_eq!(error.class(), Class::Release, "{text}");
        assert_eq!(error.reason(), "manifest_mismatch", "{text}");
        assert_eq!(error.to_string(), text);
    }
}

/// Catches a changed systemctl argument vector, a stop that lets the unit name act as an option,
/// and a launcher failure reported without the action: exact argv, error and Display.
#[test]
fn daemon_reload_is_one_systemctl_command() {
    let systemctl = Systemctl::new(FakeLauncher::new());
    systemctl.daemon_reload().expect("reload");
    assert_eq!(
        systemctl.launcher().runs(),
        vec![vec!["systemctl".to_owned(), "daemon-reload".to_owned()]]
    );
    systemctl.stop_unit("tierstore@9301.service").expect("stop");
    assert_eq!(
        systemctl.launcher().runs(),
        vec![
            vec!["systemctl".to_owned(), "daemon-reload".to_owned()],
            vec![
                "systemctl".to_owned(),
                "stop".to_owned(),
                "--".to_owned(),
                "tierstore@9301.service".to_owned(),
            ],
        ]
    );

    let failing = Systemctl::new(FakeLauncher::failing());
    let error = failing.daemon_reload().expect_err("the launcher fails");
    assert_eq!(error, units_error());
    assert_eq!(
        error.to_string(),
        "systemctl daemon-reload failed: exit status 1"
    );
    let error = failing
        .stop_unit("tierstore@9301.service")
        .expect_err("the launcher fails");
    assert_eq!(
        error,
        UnitControlError {
            action: "systemctl stop -- tierstore@9301.service".to_owned(),
            why: "exit status 1".to_owned(),
        }
    );
    assert_eq!(failing.launcher().runs().len(), 2);
}

/// Catches a reload failure swallowed or mis-wrapped: the placer returns Units carrying the
/// fake's action and reason, and the call was made once.
#[test]
fn failing_reload_is_a_units_error() {
    let world = World::new("errors-reload");
    let units = FakeUnitControl::failing(world.log.clone());
    let placer = Placer::new(world.roots.clone(), &world.fs, &units);
    let error = placer.reload().expect_err("reload fails");
    match &error {
        PlacedError::Units(inner) => assert_eq!(
            inner,
            &UnitControlError {
                action: "DaemonReload".to_owned(),
                why: "the fake fails every call".to_owned(),
            }
        ),
        other => panic!("not a Units error: {other}"),
    }
    assert_eq!(error.class(), Class::Environment);
    assert_eq!(error.reason(), "daemon_reload_failed");
    assert_eq!(world.unit_calls(), vec![UnitCall::DaemonReload]);
}

/// Catches a fake unit control that records the wrong call or fails when it should not: calls
/// land in the shared log in order, and the failing one records then errors for stop too.
#[test]
fn fake_unit_control_records_and_fails_on_request() {
    let log = Log::new();
    let fine = FakeUnitControl::new(log.clone());
    fine.daemon_reload().expect("reload");
    fine.stop_unit("a.service").expect("stop");
    assert_eq!(
        log.events(),
        vec![
            Event::Unit(UnitCall::DaemonReload),
            Event::Unit(UnitCall::Stop("a.service".to_owned())),
        ]
    );
    log.clear();
    let failing = FakeUnitControl::failing(log.clone());
    let error = failing.stop_unit("a.service").expect_err("fails");
    assert_eq!(
        error,
        UnitControlError {
            action: "Stop(\"a.service\")".to_owned(),
            why: "the fake fails every call".to_owned(),
        }
    );
    assert_eq!(
        log.events(),
        vec![Event::Unit(UnitCall::Stop("a.service".to_owned()))]
    );
}

/// Catches a log that loses or keeps events: clones share the list, clear empties it.
#[test]
fn log_is_shared_and_clearable() {
    let log = Log::new();
    assert!(log.events().is_empty());
    let other = log.clone();
    log.push(Event::Unit(UnitCall::DaemonReload));
    other.push(Event::Fs(FsWrite::Remove(PathBuf::from("/x"))));
    assert_eq!(
        log.events(),
        vec![
            Event::Unit(UnitCall::DaemonReload),
            Event::Fs(FsWrite::Remove(PathBuf::from("/x"))),
        ]
    );
    other.clear();
    assert!(log.events().is_empty());
}

/// Catches a launcher fake that forgets runs or fails when it should not.
#[test]
fn fake_launcher_records_every_run() {
    use hostkeeper_placed::CommandLauncher;
    let launcher = FakeLauncher::new();
    assert!(launcher.runs().is_empty());
    assert_eq!(launcher.run(&["a", "b"]), Ok(()));
    assert_eq!(launcher.run(&["c"]), Ok(()));
    assert_eq!(
        launcher.runs(),
        vec![vec!["a".to_owned(), "b".to_owned()], vec!["c".to_owned()]]
    );
    let failing = FakeLauncher::failing();
    assert_eq!(failing.run(&["a"]), Err("exit status 1".to_owned()));
    assert_eq!(failing.runs(), vec![vec!["a".to_owned()]]);
}

/// Catches a FaultFs that does not pass each operation through to disk under the root, or
/// that logs it wrongly: every method, its effect on disk and its log entry.
#[test]
fn fault_fs_passes_every_operation_through() {
    let world = World::new("errors-passthrough");
    let fs_ = &world.fs;
    let dir = Path::new("/var/lib/hostkeeper/scratch/deep");
    let file = Path::new("/var/lib/hostkeeper/scratch/deep/a");
    let renamed = Path::new("/var/lib/hostkeeper/scratch/deep/b");
    let link = Path::new("/var/lib/hostkeeper/scratch/deep/l");

    assert_eq!(fs_.host_path(file), world.host(file));
    assert_eq!(fs_.kind(dir).expect("kind"), None);
    fs_.create_dirs(dir).expect("create_dirs");
    assert_eq!(fs_.kind(dir).expect("kind"), Some(FileKind::Dir));
    assert!(world.host(dir).is_dir());

    fs_.write(file, b"hello").expect("write");
    assert_eq!(fs::read(world.host(file)).expect("on disk"), b"hello");
    assert_eq!(fs_.read(file).expect("read"), b"hello");
    assert_eq!(fs_.kind(file).expect("kind"), Some(FileKind::File));
    fs_.sync_file(file).expect("sync_file");
    fs_.sync_dir(dir).expect("sync_dir");

    fs_.rename(file, renamed).expect("rename");
    assert!(!world.host(file).exists());
    assert_eq!(fs::read(world.host(renamed)).expect("on disk"), b"hello");

    fs_.symlink(Path::new("b"), link).expect("symlink");
    assert_eq!(fs_.read_link(link).expect("read_link"), Path::new("b"));
    assert_eq!(fs_.kind(link).expect("kind"), Some(FileKind::Symlink));
    assert_eq!(
        fs_.list(dir).expect("list"),
        vec![OsString::from("b"), OsString::from("l")]
    );

    fs_.remove_file(link).expect("remove_file");
    assert!(fs::symlink_metadata(world.host(link)).is_err());

    assert_eq!(fs_.writes(), 7);
    assert_eq!(
        world.writes(),
        vec![
            FsWrite::CreateDirs(dir.to_path_buf()),
            FsWrite::Write(file.to_path_buf()),
            FsWrite::SyncFile(file.to_path_buf()),
            FsWrite::SyncDir(dir.to_path_buf()),
            FsWrite::Rename {
                from: file.to_path_buf(),
                to: renamed.to_path_buf(),
            },
            FsWrite::Symlink {
                target: PathBuf::from("b"),
                link: link.to_path_buf(),
            },
            FsWrite::Remove(link.to_path_buf()),
        ]
    );
}

/// Catches reads that are counted or logged as writes, and a missing file or a socket that is
/// not reported as it is: reads leave the counter and log alone.
#[test]
fn fault_fs_reads_are_not_writes() {
    let world = World::new("errors-reads");
    let pipe = Path::new("/a-pipe");
    support::fifo(&world.host(pipe));
    assert_eq!(world.fs.kind(pipe).expect("kind"), Some(FileKind::Other));
    let missing = Path::new("/var/lib/hostkeeper/missing");
    assert_eq!(world.fs.kind(missing).expect("kind"), None);
    assert_eq!(
        world.fs.read(missing).expect_err("no file").kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        world.fs.read_link(missing).expect_err("no link").kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        world.fs.list(missing).expect_err("no dir").kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(world.fs.writes(), 0);
    assert!(world.log.events().is_empty());
}

/// Catches a failing FaultFs that changes the disk, logs the attempt, or stops counting: with
/// the first write failing, every write method errors with its number, the tree is unchanged,
/// nothing is logged and the failed attempts are counted.
#[test]
fn failing_fault_fs_changes_nothing_and_counts_attempts() {
    let world = World::new("errors-failing");
    let existing = Path::new("/var/lib/hostkeeper/existing");
    fs::write(world.host(existing), "keep").expect("seed a file");
    let before = world.snapshot();
    let fs_ = world.failing_at(1);
    let new_dir = Path::new("/var/lib/hostkeeper/newdir");
    let new_file = Path::new("/var/lib/hostkeeper/new");
    let new_link = Path::new("/var/lib/hostkeeper/newlink");
    let moved = Path::new("/var/lib/hostkeeper/moved");

    let errors = [
        fs_.write(new_file, b"x").expect_err("write"),
        fs_.sync_file(existing).expect_err("sync_file"),
        fs_.sync_dir(Path::new("/var/lib/hostkeeper"))
            .expect_err("sync_dir"),
        fs_.rename(existing, moved).expect_err("rename"),
        fs_.symlink(Path::new("existing"), new_link)
            .expect_err("symlink"),
        fs_.remove_file(existing).expect_err("remove_file"),
        fs_.create_dirs(new_dir).expect_err("create_dirs"),
    ];
    for (i, error) in errors.iter().enumerate() {
        assert_eq!(
            error.to_string(),
            format!("injected failure of write {}", i + 1)
        );
    }
    assert_eq!(fs_.writes(), 7);
    assert_eq!(world.snapshot(), before);
    assert!(world.log.events().is_empty());
}

/// Catches a failure point off by one: with the third write failing, two writes land and are
/// logged, the third and the fourth fail, and all four are counted.
#[test]
fn fault_fs_fails_the_nth_write_and_every_later_one() {
    let world = World::new("errors-nth");
    let fs_ = world.failing_at(3);
    let a = Path::new("/var/lib/hostkeeper/a");
    let b = Path::new("/var/lib/hostkeeper/b");
    fs_.write(a, b"1").expect("first");
    fs_.sync_file(a).expect("second");
    let third = fs_.write(b, b"2").expect_err("third");
    assert_eq!(third.to_string(), "injected failure of write 3");
    let fourth = fs_.sync_file(a).expect_err("fourth");
    assert_eq!(fourth.to_string(), "injected failure of write 4");
    assert_eq!(fs_.writes(), 4);
    assert!(!world.host(b).exists());
    assert_eq!(
        world.writes(),
        vec![
            FsWrite::Write(a.to_path_buf()),
            FsWrite::SyncFile(a.to_path_buf())
        ]
    );
}

/// Catches a write that fails in the underlying filesystem being logged as done: the error
/// passes through and no event is recorded, yet the attempt is counted.
#[test]
fn fault_fs_does_not_log_a_write_that_failed_on_disk() {
    let world = World::new("errors-disk-failure");
    let fs_ = FaultFs::new(world.root(), world.log.clone());
    let error = fs_
        .remove_file(Path::new("/var/lib/hostkeeper/absent"))
        .expect_err("nothing to remove");
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert_eq!(fs_.writes(), 1);
    assert!(world.log.events().is_empty());
}

/// Catches a wrong standard path or accessor of the Linux roots.
#[test]
fn linux_roots_have_the_standard_paths() {
    let roots = Roots::linux();
    assert_eq!(roots.state_dir(), Path::new("/var/lib/hostkeeper"));
    assert_eq!(roots.unit_dir(), Path::new("/etc/systemd/system"));
    assert_eq!(
        roots.packages().dir(),
        Path::new("/var/lib/hostkeeper/packages")
    );
    assert_eq!(
        roots.placed_file(),
        PathBuf::from("/var/lib/hostkeeper/placed.json")
    );
    assert_eq!(
        roots.lock_file(),
        PathBuf::from("/var/lib/hostkeeper/placed.lock")
    );
}

/// Catches a unit directory that is hard-wired instead of taken from the caller (the launchd
/// directory on macOS), and state files not following the state directory.
#[test]
fn roots_take_the_directories_given() {
    let roots = Roots::new(
        Path::new("/state"),
        Path::new("/Library/LaunchDaemons"),
        Path::new("/state/pkgs"),
    );
    assert_eq!(roots.state_dir(), Path::new("/state"));
    assert_eq!(roots.unit_dir(), Path::new("/Library/LaunchDaemons"));
    assert_eq!(
        roots.packages(),
        &PackageDirs::new(Path::new("/state/pkgs"))
    );
    assert_eq!(roots.placed_file(), PathBuf::from("/state/placed.json"));
    assert_eq!(roots.lock_file(), PathBuf::from("/state/placed.lock"));
}

/// Catches a package path built wrongly: service, current, previous and version directories.
#[test]
fn package_dirs_name_the_service_links_and_versions() {
    let dirs = PackageDirs::new(Path::new("/p"));
    assert_eq!(dirs.dir(), Path::new("/p"));
    assert_eq!(dirs.service("tierstore"), PathBuf::from("/p/tierstore"));
    assert_eq!(
        dirs.current("tierstore"),
        PathBuf::from("/p/tierstore/current")
    );
    assert_eq!(
        dirs.previous("tierstore"),
        PathBuf::from("/p/tierstore/previous")
    );
    assert_eq!(
        dirs.version("tierstore", "1.2.0"),
        PathBuf::from("/p/tierstore/1.2.0")
    );
}

/// Catches a `PackagePath` text that names a fixed packages directory instead of the field: with
/// another packages directory the text carries it twice, and the refusal is a release one.
#[test]
fn a_package_path_refusal_names_the_packages_directory_it_was_given() {
    let error = LintError::PackagePath {
        file: "a.service".to_owned(),
        key: "ExecStart".to_owned(),
        path: "/srv/pk/a/1.0.0/bin/a".to_owned(),
        packages: "/srv/pk".to_owned(),
        service: "a".to_owned(),
    };
    assert_eq!(
        error.to_string(),
        "units/a.service ExecStart names \"/srv/pk/a/1.0.0/bin/a\", which is under /srv/pk/ but not /srv/pk/a/current/"
    );
    assert_eq!(error.class(), Class::Release);
    assert_eq!(error.reason(), "manifest_mismatch");
}

/// Catches a wrong class, reason or text for the refusals that came with the overlap rule, the
/// source rule and the conflicting switch intents.
#[test]
fn overlap_bad_source_and_conflicting_intents_have_class_reason_and_text() {
    let overlap = ClaimError::Overlap {
        path: PathBuf::from("/etc/x"),
        other: PathBuf::from("/etc/x/y.conf"),
    };
    assert_eq!(
        overlap.to_string(),
        "the package claims /etc/x and /etc/x/y.conf, one below the other"
    );
    let bad_source = ClaimError::BadSource {
        entry: "../x".to_owned(),
        why: "is not below files/".to_owned(),
    };
    assert_eq!(
        bad_source.to_string(),
        "the [[files]] source \"../x\" is not below files/"
    );
    for claim in [overlap, bad_source] {
        let text = claim.to_string();
        let error = PlacedError::Claim(claim);
        assert_eq!(error.class(), Class::Release, "{text}");
        assert_eq!(error.reason(), "manifest_mismatch", "{text}");
        assert_eq!(error.to_string(), text);
    }

    let conflicting = PlacedError::ConflictingIntents {
        service: "tierstore".to_owned(),
    };
    assert_eq!(
        conflicting.to_string(),
        "two different switch intents name the service tierstore"
    );
    assert_eq!(conflicting.class(), Class::Environment);
    assert_eq!(conflicting.reason(), "link_failed");

    let foreign = PlacedError::ForeignLock {
        lock: PathBuf::from("/var/lib/other/placed.lock"),
        record: PathBuf::from("/var/lib/hostkeeper/placed.json"),
    };
    assert_eq!(
        foreign.to_string(),
        "the lock /var/lib/other/placed.lock is not the lock of /var/lib/hostkeeper/placed.json"
    );
    assert_eq!(foreign.class(), Class::Environment);
    assert_eq!(foreign.reason(), "link_failed");
}

/// What one read of a [`FaultFs`] does on the scratch tree of [`read_world`].
type ReadOp = fn(&FaultFs) -> io::Result<()>;

fn read_file(fs_: &FaultFs) -> io::Result<()> {
    fs_.read(Path::new("/var/lib/hostkeeper/f")).map(drop)
}

fn kind_of_file(fs_: &FaultFs) -> io::Result<()> {
    fs_.kind(Path::new("/var/lib/hostkeeper/f")).map(drop)
}

fn read_the_link(fs_: &FaultFs) -> io::Result<()> {
    fs_.read_link(Path::new("/var/lib/hostkeeper/l")).map(drop)
}

fn list_the_directory(fs_: &FaultFs) -> io::Result<()> {
    fs_.list(Path::new("/var/lib/hostkeeper")).map(drop)
}

/// The four reads, each on something that exists in [`read_world`].
const READ_OPS: [(&str, ReadOp); 4] = [
    ("read", read_file as ReadOp),
    ("kind", kind_of_file as ReadOp),
    ("read_link", read_the_link as ReadOp),
    ("list", list_the_directory as ReadOp),
];

/// A world with a file `f` and a link `l` in the state directory.
fn read_world(name: &str) -> World {
    let world = World::new(name);
    fs::write(world.host("/var/lib/hostkeeper/f"), "x").expect("file");
    std::os::unix::fs::symlink("f", world.host("/var/lib/hostkeeper/l")).expect("link");
    world
}

/// Catches a one-shot read fault that sticks or misses: for each of read, kind, read_link and
/// list the second read fails with its number, the first and third pass, and `reads()` counts
/// the three.
#[test]
fn failing_read_once_fails_only_that_read() {
    for (name, op) in READ_OPS {
        let world = read_world(&format!("errors-read-once-{name}"));
        let fs_ = FaultFs::new(world.root(), world.log.clone()).failing_read(2, false);
        assert_eq!(fs_.reads(), 0, "{name}");
        op(&fs_).unwrap_or_else(|e| panic!("{name} 1: {e}"));
        let error = op(&fs_).expect_err(name);
        assert_eq!(error.to_string(), "injected failure of read 2", "{name}");
        op(&fs_).unwrap_or_else(|e| panic!("{name} 3: {e}"));
        assert_eq!(fs_.reads(), 3, "{name}");
        assert_eq!(fs_.writes(), 0, "{name}");
        assert!(world.log.events().is_empty(), "{name}");
    }
}

/// Catches a persistent read fault that heals: for each read the second and every later read fail
/// with their number, the first passes, and failed reads are counted.
#[test]
fn failing_read_persistent_fails_that_read_and_every_later_one() {
    for (name, op) in READ_OPS {
        let world = read_world(&format!("errors-read-persistent-{name}"));
        let fs_ = FaultFs::new(world.root(), world.log.clone()).failing_read(2, true);
        op(&fs_).unwrap_or_else(|e| panic!("{name} 1: {e}"));
        for n in 2..=4 {
            let error = op(&fs_).expect_err(name);
            assert_eq!(
                error.to_string(),
                format!("injected failure of read {n}"),
                "{name}"
            );
        }
        assert_eq!(fs_.reads(), 4, "{name}");
    }
}

/// Catches a read counter kept per method: one count runs over read, kind, read_link and list, so
/// the third read in a mixed sequence is the failing one whichever method it is, and a filesystem
/// with no fault set counts its reads and never fails.
#[test]
fn the_read_counter_runs_over_all_four_reads() {
    let world = read_world("errors-read-mixed");
    let fs_ = FaultFs::new(world.root(), world.log.clone()).failing_read(3, false);
    READ_OPS[0].1(&fs_).expect("read 1");
    READ_OPS[1].1(&fs_).expect("read 2");
    let error = READ_OPS[2].1(&fs_).expect_err("read 3 is a read_link");
    assert_eq!(error.to_string(), "injected failure of read 3");
    READ_OPS[3].1(&fs_).expect("read 4");
    assert_eq!(fs_.reads(), 4);

    let plain = FaultFs::new(world.root(), world.log.clone());
    for (_, op) in READ_OPS {
        op(&plain).expect("no fault");
    }
    assert_eq!(plain.reads(), 4);
    assert_eq!(world.fs.reads(), 0, "another filesystem has its own count");
}

/// Catches a failing read that is not an error of the placer's own read: a first read that fails
/// persistently turns `Placer::current` into an Io error, and a once-failing one lets the next
/// call succeed.
#[test]
fn an_injected_read_failure_reaches_the_caller_as_an_io_error() {
    let world = World::new("errors-read-placer");
    let failing = FaultFs::new(world.root(), world.log.clone()).failing_read(1, false);
    let placer = world.placer_on(&failing);
    match placer.current("tierstore") {
        Err(PlacedError::Io { path, .. }) => assert_eq!(
            path,
            PathBuf::from("/var/lib/hostkeeper/packages/tierstore/current")
        ),
        other => panic!("expected an Io error, got {other:?}"),
    }
    assert_eq!(
        placer.current("tierstore").expect("the next read passes"),
        None
    );
}

/// Catches a `Placer::lock` that returns no token or lets two holders in: the first lock is a
/// `PlacedLock`, a second while it is held is `Locked` naming the lock file, and once the first
/// is dropped the lock can be taken again.
#[test]
fn placer_lock_returns_the_token_and_a_second_lock_is_refused() {
    let world = World::new("errors-lock");
    let placer = world.placer();
    let lock: PlacedLock = placer.lock().expect("the lock is free");
    assert!(world.host("/var/lib/hostkeeper/placed.lock").is_file());
    match placer.lock() {
        Err(error @ PlacedError::Locked { .. }) => {
            match &error {
                PlacedError::Locked { path, .. } => {
                    assert_eq!(path, Path::new("/var/lib/hostkeeper/placed.lock"));
                }
                other => panic!("{other:?}"),
            }
            assert_eq!(error.class(), Class::Environment);
            assert_eq!(error.reason(), "link_failed");
            assert!(
                error
                    .to_string()
                    .starts_with("another daemon holds /var/lib/hostkeeper/placed.lock, or its lock cannot be taken: "),
                "{error}"
            );
            assert!(error.source().is_some());
        }
        other => panic!("expected Locked, got {other:?}"),
    }
    drop(lock);
    let again = placer.lock().expect("free again");
    drop(again);
}
