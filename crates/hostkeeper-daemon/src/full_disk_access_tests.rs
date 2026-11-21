//! The tests of `full_disk_access`: the state a listing comes to, when the check is made again,
//! what is logged, and the real listing over scratch directories. The clock is a fake that moves
//! only when a test moves it.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};

use hostkeeper_link::testing::{FakeClock, RecordingLog};
use hostkeeper_testkit::Scratch;

use super::*;

/// A listing that works or fails with the error kind a test sets, counting its calls.
struct FakeTcc {
    failing: Mutex<Option<io::ErrorKind>>,
    calls: AtomicUsize,
}

impl FakeTcc {
    /// A listing that works until [`FakeTcc::fail_with`].
    fn working() -> Arc<FakeTcc> {
        Arc::new(FakeTcc {
            failing: Mutex::new(None),
            calls: AtomicUsize::new(0),
        })
    }

    /// A listing that fails with `kind` from now on.
    fn fail_with(&self, kind: io::ErrorKind) {
        *self.failing.lock().expect("failing") = Some(kind);
    }

    /// A listing that works from now on.
    fn work(&self) {
        *self.failing.lock().expect("failing") = None;
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TccDirectory for FakeTcc {
    fn list(&self) -> io::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match *self.failing.lock().expect("failing") {
            None => Ok(()),
            Some(kind) => Err(io::Error::new(kind, "listing refused")),
        }
    }
}

struct Rig {
    tcc: Arc<FakeTcc>,
    clock: Arc<FakeClock>,
    log: Arc<RecordingLog>,
}

impl Rig {
    fn new() -> Rig {
        Rig {
            tcc: FakeTcc::working(),
            clock: Arc::new(FakeClock::new(1_800_000_000, 5)),
            log: Arc::new(RecordingLog::default()),
        }
    }

    fn source(&self, kind: PlatformKind) -> HostFullDiskAccess {
        HostFullDiskAccess::new(kind, self.tcc.clone(), self.clock.clone(), self.log.clone())
    }
}

/// Proves: a Mac whose directory lists is granted from its start, and the start logs one line
/// saying so.
///
/// Catches: a grant read as missing at the start, and a start that logs nothing or twice.
#[test]
fn a_mac_that_can_list_the_directory_starts_granted_and_logs_one_line() {
    let rig = Rig::new();
    let source = rig.source(PlatformKind::Macos);
    assert_eq!(source.full_disk_access(), FullDiskAccess::Granted);
    assert_eq!(rig.log.lines(), ["full disk access: granted"]);
    assert_eq!(rig.tcc.calls(), 1);
}

/// Proves: a listing the system refuses with `PermissionDenied` is not granted, and the refusal is
/// not logged as a failure.
///
/// Catches: a refusal for want of permission read as an undetermined state, or as granted.
#[test]
fn a_refused_listing_is_not_granted() {
    let rig = Rig::new();
    rig.tcc.fail_with(io::ErrorKind::PermissionDenied);
    let source = rig.source(PlatformKind::Macos);
    assert_eq!(source.full_disk_access(), FullDiskAccess::NotGranted);
    assert_eq!(rig.log.lines(), ["full disk access: not_granted"]);
}

/// Proves: any other error is undetermined, and its text is logged before the start line.
///
/// Catches: a missing directory or an unexpected error read as not granted, and a failure the
/// operator cannot see in the log.
#[test]
fn any_other_error_is_undetermined_and_logged() {
    let rig = Rig::new();
    rig.tcc.fail_with(io::ErrorKind::NotFound);
    let source = rig.source(PlatformKind::Macos);
    assert_eq!(source.full_disk_access(), FullDiskAccess::Undetermined);
    assert_eq!(
        rig.log.lines(),
        [
            "full disk access: the check failed: listing refused",
            "full disk access: undetermined"
        ]
    );
}

/// Proves: a host that is not a Mac answers not applicable, never lists the directory, and logs
/// nothing, however much time passes.
///
/// Catches: a Linux host that lists a Mac's directory, or one reported as not granted.
#[test]
fn a_host_that_is_not_a_mac_is_not_applicable_and_never_lists() {
    for kind in [PlatformKind::Linux, PlatformKind::Openrc] {
        let rig = Rig::new();
        rig.tcc.fail_with(io::ErrorKind::PermissionDenied);
        let source = rig.source(kind);
        assert_eq!(source.full_disk_access(), FullDiskAccess::NotApplicable);
        rig.clock.advance(10 * RECHECK_SECS);
        assert_eq!(source.full_disk_access(), FullDiskAccess::NotApplicable);
        assert_eq!(rig.tcc.calls(), 0);
        assert!(rig.log.lines().is_empty());
    }
}

/// Proves: a heartbeat that reads the state less than 60 seconds after the last check gets that
/// check's state without listing again.
///
/// Catches: a check at every heartbeat (a listing every 15 seconds), and a change shown before
/// the minute is up.
#[test]
fn the_state_stands_until_the_recheck_period_has_passed() {
    let rig = Rig::new();
    let source = rig.source(PlatformKind::Macos);
    rig.tcc.fail_with(io::ErrorKind::PermissionDenied);
    rig.clock.advance(RECHECK_SECS - 1);
    assert_eq!(source.full_disk_access(), FullDiskAccess::Granted);
    assert_eq!(source.full_disk_access(), FullDiskAccess::Granted);
    assert_eq!(rig.tcc.calls(), 1);
    assert_eq!(rig.log.lines(), ["full disk access: granted"]);
}

/// Proves: once 60 seconds have passed a heartbeat that reads the state checks again, a state that
/// changed is reported and logged as "from -> to", and the check made then stands for a minute.
///
/// Catches: a profile installed or removed that never shows, and a change that is not logged.
#[test]
fn a_change_shows_at_the_next_read_after_the_period_and_is_logged() {
    let rig = Rig::new();
    let source = rig.source(PlatformKind::Macos);
    rig.tcc.fail_with(io::ErrorKind::PermissionDenied);
    rig.clock.advance(RECHECK_SECS);
    assert_eq!(source.full_disk_access(), FullDiskAccess::NotGranted);
    assert_eq!(rig.tcc.calls(), 2);
    rig.tcc.work();
    rig.clock.advance(RECHECK_SECS - 1);
    assert_eq!(source.full_disk_access(), FullDiskAccess::NotGranted);
    assert_eq!(rig.tcc.calls(), 2);
    rig.clock.advance(1);
    assert_eq!(source.full_disk_access(), FullDiskAccess::Granted);
    assert_eq!(
        rig.log.lines(),
        [
            "full disk access: granted",
            "full disk access: granted -> not_granted",
            "full disk access: not_granted -> granted",
        ]
    );
}

/// Proves: a check that finds the state it found before logs nothing.
///
/// Catches: a log line at every check.
#[test]
fn a_check_that_finds_the_same_state_logs_nothing() {
    let rig = Rig::new();
    let source = rig.source(PlatformKind::Macos);
    for _ in 0..3 {
        rig.clock.advance(RECHECK_SECS);
        assert_eq!(source.full_disk_access(), FullDiskAccess::Granted);
    }
    assert_eq!(rig.tcc.calls(), 4);
    assert_eq!(rig.log.lines(), ["full disk access: granted"]);
}

/// Proves: a unix clock that is set back by the period or more also makes the next read check
/// again.
///
/// Catches: a state that stays stale for as long as the clock was set back.
#[test]
fn a_clock_set_back_makes_the_next_read_check_again() {
    let rig = Rig::new();
    let source = rig.source(PlatformKind::Macos);
    rig.tcc.fail_with(io::ErrorKind::PermissionDenied);
    rig.clock.set_unix(1_800_000_000 - RECHECK_SECS);
    assert_eq!(source.full_disk_access(), FullDiskAccess::NotGranted);
    assert_eq!(rig.tcc.calls(), 2);
}

/// Proves: the real listing is ok for an empty directory and for one holding an entry, which it
/// opens none of, and fails with the system's error for a directory that is not there and for a
/// file.
///
/// Catches: a listing that needs an entry to be present, one that opens the entries, and an
/// error swallowed.
#[test]
fn the_real_listing_reads_a_directory_and_fails_for_anything_else() {
    let scratch = scratch("fda-list");
    let dir = scratch.path().join("tcc");
    fs::create_dir(&dir).expect("make the directory");
    assert!(HostTccDirectory::at(dir.clone()).list().is_ok());
    let inside = dir.join("entry");
    fs::write(&inside, b"x").expect("make an entry");
    fs::set_permissions(&inside, fs::Permissions::from_mode(0o000)).expect("close the entry");
    assert!(HostTccDirectory::at(dir.clone()).list().is_ok());
    let missing = HostTccDirectory::at(scratch.path().join("absent")).list();
    assert_eq!(
        missing.expect_err("no directory").kind(),
        io::ErrorKind::NotFound
    );
    assert!(HostTccDirectory::at(inside).list().is_err());
}

/// Proves: over a real directory the source reads what the system says: a directory with no
/// permission is not granted to a user who is refused it, and listed by root, who is not; a
/// directory that is not there is undetermined.
///
/// Catches: the kind of the system's refusal not mapped to not granted, and a missing directory
/// mapped to it.
#[test]
fn over_real_directories_the_source_maps_the_systems_answer() {
    let scratch = scratch("fda-real");
    let closed = scratch.path().join("closed");
    fs::create_dir(&closed).expect("make the directory");
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).expect("close it");
    let rig = Rig::new();
    let source = |path| {
        HostFullDiskAccess::new(
            PlatformKind::Macos,
            Arc::new(HostTccDirectory::at(path)),
            rig.clock.clone(),
            rig.log.clone(),
        )
    };
    let root = rustix::process::geteuid().is_root();
    let want = if root {
        FullDiskAccess::Granted
    } else {
        FullDiskAccess::NotGranted
    };
    assert_eq!(source(closed.clone()).full_disk_access(), want);
    assert_eq!(
        source(scratch.path().join("absent")).full_disk_access(),
        FullDiskAccess::Undetermined
    );
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o700)).expect("open it again");
}

/// Proves: the host's source on a Linux host answers not applicable.
///
/// Catches: the daemon's wiring reporting a state on a host that has no such grant.
#[test]
fn the_hosts_source_on_linux_is_not_applicable() {
    let rig = Rig::new();
    let source = at_host(PlatformKind::Linux, rig.clock.clone(), rig.log.clone());
    assert_eq!(
        source.expect("a source").full_disk_access(),
        FullDiskAccess::NotApplicable
    );
    assert!(rig.log.lines().is_empty());
}

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// Proves: the host's directory is the one the design names.
///
/// Catches: the directory changed to a path that is not the one only the grant allows.
#[test]
fn the_hosts_directory_is_the_tcc_directory() {
    assert_eq!(TCC_DIR, "/Library/Application Support/com.apple.TCC");
    assert_eq!(HostTccDirectory::host().path, PathBuf::from(TCC_DIR));
    assert_eq!(RECHECK_SECS, 60);
}
