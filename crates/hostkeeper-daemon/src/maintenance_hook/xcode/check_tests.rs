//! The tests of the `probe` kind's archive check, over the fakes of [`super::super::fakes`]: a
//! scratch directory standing for the Mac's files, a store that serves one archive by its SHA-256
//! and can break a read part way, a runner of fake programs that records each run, and a fake
//! free space. Each deferral is asserted by equality of its reason and whole detail, the probe's
//! steps by equality of the whole list, and each run of a program by equality of the whole list.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hostkeeper_launchd::{Launchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_link::{GetError, UnixClock};
use hostkeeper_proto::hostkeeper::agent::v1::{
    Maintain, MaintenanceKind as WireKind, MaintenanceProbe,
};

use hostkeeper_package::gh::Spawn;

use super::super::stage::{
    REASON_ARCHIVE_ENCRYPTED, REASON_ARCHIVE_MISMATCH, REASON_FETCH_FAILED, REASON_NOT_IN_STORE,
};
use super::super::store::StoreError;
use super::*;
use crate::maintenance_hook::macos::Programs;
use crate::maintenance_hook::params::MaintainParams;
use crate::maintenance_hook::probe::{PROBE_FILE, ProbeKind, REASON_PROBE_MISSING, read_probe};
use crate::maintenance_hook::xcode::fakes::*;
use crate::maintenance_hook::xcode::signature::tests::APPLE_REPORT;
use crate::maintenance_hook::{Cutover, MaintenanceKind};

/// What `sw_vers` says on the fake Mac.
const SW_VERS_SAYS: &str = "ProductName:\t\tmacOS\nProductVersion:\t\t27.2\n";
/// The steps of `sw_vers` and `uname`, as the fake Mac gives them.
fn recorded() -> Vec<MaintenanceProbeStep> {
    vec![
        kept(
            "sw_vers",
            Some(0),
            "ProductName:\t\tmacOS; ProductVersion:\t\t27.2",
        ),
        kept("uname", Some(0), "Darwin arm64"),
    ]
}

/// A program that ends with `code` (`None`: by a signal), saying `stdout` and `stderr`.
fn says(code: Option<i32>, stdout: &str, stderr: &str) -> Program {
    let (stdout, stderr) = (stdout.as_bytes().to_vec(), stderr.as_bytes().to_vec());
    Box::new(move |_| {
        Ok(Ran {
            code,
            stdout: stdout.clone(),
            stderr: stderr.clone(),
            ..Ran::default()
        })
    })
}

/// A program killed at its deadline.
fn killed() -> Program {
    Box::new(|_| {
        Ok(Ran {
            code: None,
            timed_out: true,
            ..Ran::default()
        })
    })
}

/// A program that cannot be started.
fn cannot_start() -> Program {
    Box::new(|_| Err(io::Error::other("no such program")))
}

/// A step as it is kept: its output cut to 128 bytes (these are ASCII) when it is longer, and
/// said to be cut.
fn kept(name: &str, exit_code: Option<i32>, output: &str) -> MaintenanceProbeStep {
    let cut = output.len() > 128;
    MaintenanceProbeStep {
        name: name.to_owned(),
        exit_code,
        output: output[..output.len().min(128)].to_owned(),
        cut,
    }
}

/// A Mac whose store holds [`ARCHIVE_BYTES`], whose `xip` makes Xcode 27.2 (27B52), whose
/// `codesign` accepts it and whose `pkgutil` reports Apple's own signature, with exactly the
/// space the check needs free, and the answers of `sw_vers` and `uname`.
fn mac(name: &str) -> Mac {
    let mac = Mac::new(name);
    mac.runner.answers(SW_VERS, says(Some(0), SW_VERS_SAYS, ""));
    mac.runner
        .answers(UNAME, says(Some(0), "Darwin arm64\n", ""));
    mac
}

fn check(mac: &Mac) -> ArchiveCheck {
    ArchiveCheck::new(mac.stage())
}

/// The order's one entry, for [`ARCHIVE_BYTES`].
fn order() -> Vec<XcodeEntry> {
    vec![entry_of(ARCHIVE_BYTES)]
}

/// The steps of a check of the archive of the order, after its `prepare` passed.
fn checked(mac: &Mac) -> Vec<MaintenanceProbeStep> {
    let check = check(mac);
    assert_eq!(check.prepare(&order()), Ok(()));
    check.steps(&order())
}

/// The run of `program` with `args` in `cwd`, as the check runs it.
fn spawn(mac: &Mac, program: &str, args: &[&str], cwd: PathBuf, secs: u64) -> Spawn {
    Spawn {
        program: PathBuf::from(program),
        args: args.iter().map(OsString::from).collect(),
        env: vec![(
            "PATH".to_owned(),
            "/usr/bin:/bin:/usr/sbin:/sbin".to_owned(),
        )],
        cwd,
        deadline: mac.deadline(secs),
    }
}

/// The runs of `sw_vers` and `uname`.
fn recording_runs(mac: &Mac) -> Vec<Spawn> {
    let dir = mac.path(CHECK_DIR);
    vec![
        spawn(mac, SW_VERS, &[], dir.clone(), 60),
        spawn(mac, UNAME, &["-sm"], dir, 60),
    ]
}

/// Steps 1 and 2 when the check directory `dir` is not the check's: neither program runs.
fn not_run(dir: &Path) -> Vec<MaintenanceProbeStep> {
    let why = format!(
        "not run: {} is not a directory only this process's user can write",
        dir.display()
    );
    ["sw_vers", "uname"]
        .map(|name| {
            let (output, cut) = bounded(why.clone());
            MaintenanceProbeStep {
                name: name.to_owned(),
                exit_code: None,
                output,
                cut,
            }
        })
        .into()
}

/// The run of `pkgutil` on the checked archive.
fn pkgutil_run(mac: &Mac) -> Spawn {
    let dir = mac.path(CHECK_DIR);
    let archive = dir.join(ARCHIVE);
    spawn(
        mac,
        PKGUTIL,
        &["--check-signature", &archive.to_string_lossy()],
        dir,
        PKGUTIL_SECS,
    )
}

/// The run of `xip` on the checked archive.
fn xip_run(mac: &Mac) -> Spawn {
    let dir = mac.path(CHECK_DIR);
    let archive = dir.join(ARCHIVE);
    spawn(
        mac,
        XIP,
        &["--expand", &archive.to_string_lossy()],
        dir.join(EXPAND),
        XIP_SECS,
    )
}

/// The run of `codesign` on the app of the expansion.
fn codesign_run(mac: &Mac) -> Spawn {
    let dir = mac.path(CHECK_DIR);
    let app = dir.join(EXPAND).join("Xcode.app");
    spawn(
        mac,
        CODESIGN,
        &["--verify", "--deep", "--strict", &app.to_string_lossy()],
        dir,
        CODESIGN_SECS,
    )
}

/// Nothing of a check is left: no check directory, nothing in `/Applications`.
fn assert_gone(mac: &Mac) {
    assert!(
        mac.path(CHECK_DIR).symlink_metadata().is_err(),
        "the check directory is removed"
    );
    assert_eq!(mac.names(APPLICATIONS), Vec::<String>::new());
}

/// Makes the host's directory `path`, with `mode`.
fn dir_with_mode(mac: &Mac, path: &str, mode: u32) {
    let path = mac.path(path);
    fs::create_dir_all(&path).expect("the directory");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("its mode");
}

/// Proves: `prepare` reads the archive from the store once, by its SHA-256 from byte 0, into
/// `<check directory>/Xcode.xip`, which holds exactly the store's bytes and is the only thing in
/// a directory of mode 0700 (the only thing in `/Applications`); no program runs and the
/// stage's directory is not made.
///
/// Catches: an archive fetched into the stage's directory, a check directory others can read, or
/// a program run while the Mac still serves.
#[test]
fn prepare_fetches_the_archive_into_the_check_directory() {
    let mac = mac("chk-fetch");
    let entry = entry_of(ARCHIVE_BYTES);
    assert_eq!(check(&mac).prepare(std::slice::from_ref(&entry)), Ok(()));
    assert_eq!(
        mac.store.asked(),
        [(format!("/v1/xcode/{}", entry.sha256), 0)]
    );
    assert_eq!(mac.names(APPLICATIONS), [".hostkeeper-check"]);
    assert_eq!(mac.names(CHECK_DIR), ["Xcode.xip"]);
    assert_eq!(
        fs::read(mac.path(&format!("{CHECK_DIR}/Xcode.xip"))).expect("the archive"),
        ARCHIVE_BYTES
    );
    let mode = fs::metadata(mac.path(CHECK_DIR))
        .expect("the directory")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    assert_eq!(*lock(&mac.space.reads), 1);
    assert!(mac.runner.ran().is_empty(), "nothing runs in prepare");
}

/// Proves: an order with no entry is a plain probe: `prepare` reads no free space, asks no store
/// and makes no directory.
///
/// Catches: a plain probe that fetches or makes files.
#[test]
fn prepare_with_no_archive_does_nothing() {
    let mac = mac("chk-none");
    assert_eq!(check(&mac).prepare(&[]), Ok(()));
    assert!(mac.store.asked().is_empty());
    assert_eq!(*lock(&mac.space.reads), 0);
    assert_gone(&mac);
}

/// Proves: more than one entry, an entry with a SHA-256, version, build or size the stage does
/// not take, are deferred `xcode_order_invalid` with the store not asked, and remove a check
/// directory a former run left.
///
/// Catches: an order with two archives that fetches the first, or one with a bad entry that
/// reaches the store.
#[test]
fn an_order_that_is_not_one_valid_archive_is_deferred() {
    let good = entry_of(ARCHIVE_BYTES);
    let bad_size = XcodeEntry {
        size: 0,
        ..good.clone()
    };
    let bad_sha = XcodeEntry {
        sha256: "ABC".to_owned(),
        ..good.clone()
    };
    let cases = [
        (
            vec![good.clone(), good.clone()],
            "a probe checks one archive; the order names 2",
        ),
        (
            vec![bad_size],
            "an Xcode of the order has a SHA-256, version, build or size the stage does not take",
        ),
        (
            vec![bad_sha],
            "an Xcode of the order has a SHA-256, version, build or size the stage does not take",
        ),
    ];
    for (entries, detail) in cases {
        let mac = mac("chk-invalid");
        mac.file(&format!("{CHECK_DIR}/Xcode.xip"), b"left by a former run");
        assert_eq!(
            check(&mac).prepare(&entries),
            deferral(REASON_ORDER_INVALID, detail)
        );
        assert!(mac.store.asked().is_empty());
        assert_gone(&mac);
    }
}

/// Proves: one byte less free than the archive and three times its size need defers
/// `free_space_short` with the whole detail, a free space that cannot be read defers
/// `free_space_unreadable`, the store is not asked, and nothing is left.
///
/// Catches: an archive fetched onto a disk too small to expand it, or an unreadable free space
/// taken as enough.
#[test]
fn a_short_or_unreadable_disk_defers_and_fetches_nothing() {
    let mut mac = mac("chk-short");
    mac.space.free = Ok(needed() - 1);
    assert_eq!(
        check(&mac).prepare(&order()),
        deferral(
            REASON_FREE_SPACE_SHORT,
            &format!(
                "{} bytes are free on the volume of /Applications; the check needs {}",
                needed() - 1,
                needed()
            )
        )
    );
    mac.space.free = Err("statvfs failed".to_owned());
    assert_eq!(
        check(&mac).prepare(&order()),
        deferral(REASON_FREE_SPACE_UNREADABLE, "statvfs failed")
    );
    assert!(mac.store.asked().is_empty());
    assert_gone(&mac);
}

/// Proves: an archive the store does not hold defers `xcode_not_in_store`, a refusal by the
/// coordinator and a store that cannot be reached defer `xcode_fetch_failed` after one read, each
/// with the whole detail, and each leaves nothing.
///
/// Catches: a fetch failure that leaves a partial archive, or one retried as a cut connection.
#[test]
fn a_store_that_gives_no_archive_defers_and_removes_the_check_directory() {
    let entry = entry_of(ARCHIVE_BYTES);
    let path = format!("/v1/xcode/{}", entry.sha256);
    let cases = [
        (
            FakeStore::holding(b"another archive", ARCHIVE_BYTES),
            REASON_NOT_IN_STORE,
            format!("the store holds no archive {path}"),
        ),
        (
            FakeStore::holding(ARCHIVE_BYTES, ARCHIVE_BYTES)
                .playing(vec![Play::Fails(StoreError::Get(GetError::Status(403)))]),
            REASON_FETCH_FAILED,
            format!("{path} from byte 0, connection 1: the coordinator answered 403"),
        ),
    ];
    for (store, reason, detail) in cases {
        let mut mac = mac("chk-no-archive");
        mac.store = store;
        assert_eq!(
            check(&mac).prepare(std::slice::from_ref(&entry)),
            deferral(reason, &detail)
        );
        assert_gone(&mac);
    }
}

/// Proves: a connection cut after 5 bytes is resumed within the one `prepare` from byte 5, and the
/// archive that results is the whole one and passes.
///
/// Catches: a check that restarts a 30 GB archive from byte 0 at each cut, or fails on the first
/// cut.
#[test]
fn a_cut_connection_is_resumed_within_prepare() {
    let mut mac = mac("chk-resume");
    mac.store = FakeStore::holding(ARCHIVE_BYTES, ARCHIVE_BYTES)
        .playing(vec![Play::Breaks(5, GetError::Truncated)]);
    let entry = entry_of(ARCHIVE_BYTES);
    assert_eq!(check(&mac).prepare(std::slice::from_ref(&entry)), Ok(()));
    let path = format!("/v1/xcode/{}", entry.sha256);
    assert_eq!(mac.store.asked(), [(path.clone(), 0), (path, 5)]);
    assert_eq!(
        fs::read(mac.path(&format!("{CHECK_DIR}/Xcode.xip"))).expect("the archive"),
        ARCHIVE_BYTES
    );
}

/// Proves: an archive of other bytes than the order's SHA-256 (the same length, so only the hash
/// tells) is refused `xcode_archive_mismatch` naming what the file is, and an Apple Encrypted
/// Archive is refused `xcode_archive_encrypted`; neither runs a program, and each leaves nothing.
///
/// Catches: a swapped archive handed to Apple's tools, or an AEA1 archive taken for an xip.
#[test]
fn an_archive_that_is_not_the_orders_is_refused() {
    let mut mac = mac("chk-mismatch");
    let entry = entry_of(ARCHIVE_BYTES);
    let other = vec![b'z'; ARCHIVE_BYTES.len()];
    mac.store = FakeStore::holding(ARCHIVE_BYTES, &other);
    assert_eq!(
        check(&mac).prepare(std::slice::from_ref(&entry)),
        deferral(
            REASON_ARCHIVE_MISMATCH,
            &format!(
                "the archive is {} bytes with SHA-256 {}; the order names {} bytes with SHA-256 {}",
                other.len(),
                sha256(&other),
                entry.size,
                entry.sha256
            )
        )
    );
    assert_gone(&mac);

    let encrypted = b"AEA1 an encrypted archive";
    let mut mac = self::mac("chk-aea");
    mac.store = FakeStore::holding(encrypted, encrypted);
    assert_eq!(
        check(&mac).prepare(&[entry_of(encrypted)]),
        deferral(
            REASON_ARCHIVE_ENCRYPTED,
            "Xcode 27.2 (27B52) is an Apple Encrypted Archive, which this stage does not expand"
        )
    );
    assert!(mac.runner.ran().is_empty());
    assert_gone(&mac);
}

/// Proves: whatever is at the check directory (a link to a directory that holds a file, a
/// directory anyone can write, a directory of the user's own that holds a stale archive, a plain
/// file) is removed without being followed, and the directory is made again 0700 holding the new
/// archive alone; the target of the link is left as it was.
///
/// Catches: a link in `/Applications` that makes the check write into, or remove from, another
/// directory, or a stale archive that is checked in place of the fetched one.
#[test]
fn what_is_at_the_check_directory_is_replaced_not_followed() {
    for case in ["link", "open", "stale", "file"] {
        let mac = mac("chk-replace");
        let elsewhere = mac.path("/elsewhere");
        fs::create_dir(&elsewhere).expect("the directory the link points at");
        fs::write(elsewhere.join("keep"), b"not the check's").expect("the file");
        match case {
            "link" => symlink(&elsewhere, mac.path(CHECK_DIR)).expect("the link"),
            "open" => {
                mac.file(&format!("{CHECK_DIR}/stale"), b"stale");
                dir_with_mode(&mac, CHECK_DIR, 0o777);
            }
            "stale" => {
                dir_with_mode(&mac, CHECK_DIR, 0o700);
                mac.file(&format!("{CHECK_DIR}/Xcode.xip"), b"a stale archive");
                mac.file(&format!("{CHECK_DIR}/expand/Old.app/x"), b"x");
            }
            _ => mac.file(CHECK_DIR, b"a file"),
        }
        assert_eq!(check(&mac).prepare(&order()), Ok(()), "{case}");
        let meta = fs::symlink_metadata(mac.path(CHECK_DIR)).expect("the directory");
        assert!(meta.is_dir() && !meta.is_symlink(), "{case}");
        assert_eq!(meta.permissions().mode() & 0o777, 0o700, "{case}");
        assert_eq!(mac.names(CHECK_DIR), ["Xcode.xip"], "{case}");
        assert_eq!(mac.names("/elsewhere"), ["keep"], "{case}");
    }
}

/// Proves: a directory put in the check directory's place after the free space was read (one
/// anyone can write) is not taken: `prepare` defers `xcode_staging_failed` before any fetch and
/// leaves nothing.
///
/// Catches: an archive fetched into a directory another user of the Mac can write.
#[test]
fn a_directory_swapped_in_after_the_space_check_is_not_taken() {
    let mut mac = mac("chk-swap");
    mac.space.swap_in = Some(mac.path(CHECK_DIR));
    assert_eq!(
        check(&mac).prepare(&order()),
        deferral(
            REASON_STAGING_FAILED,
            &format!(
                "{} is not a directory only this process's user can write",
                mac.path(CHECK_DIR).display()
            )
        )
    );
    assert!(mac.store.asked().is_empty());
    assert_gone(&mac);
}

/// A `Programs` that records each program asked for and runs it as a program that exits 0 saying
/// nothing.
#[derive(Clone, Default)]
struct Quiet(Arc<Mutex<Vec<PathBuf>>>);

impl Programs for Quiet {
    fn tool(&self, program: &Path, _deadline: Duration) -> Box<dyn Launchctl> {
        lock(&self.0).push(program.to_owned());
        Box::new(Quiet::default())
    }
}

impl Launchctl for Quiet {
    fn run(&self, _args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        Ok(LaunchctlOutput {
            code: Some(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
}

/// A clock at one second.
struct Noon;

impl UnixClock for Noon {
    fn unix_secs(&self) -> u64 {
        1_800_000_000
    }
}

/// The parameters of probe run `run_id` with `xcodes` in its order.
fn params(run_id: &str, xcodes: Vec<XcodeEntry>) -> MaintainParams {
    MaintainParams::new(Maintain {
        run_id: run_id.to_owned(),
        kinds: vec![WireKind::Probe.into()],
        xcodes,
        ..Maintain::default()
    })
    .expect("valid")
}

/// Proves: the probe kind with an archive check, given an order that names the archive, fetches
/// it in `prepare`, in `cutover` writes the probe file with the run id, the clock's second and
/// the check's steps (and no step of the plain probe's list), answers applied with `8 steps`,
/// runs none of its own `Programs`, leaves nothing in `/Applications`, and its `verify` passes on
/// the run id and fails `probe_missing` on another's.
///
/// Catches: a check whose steps are not what the report carries, a probe run that goes on to run
/// the plain probe's programs, or a `verify` of the check's probe that does not hold.
#[test]
fn the_probe_kind_checks_the_archive_its_order_names() {
    let mac = mac("chk-kind");
    let dir = scratch("chk-kind-run");
    let programs = Quiet::default();
    let kind = ProbeKind::new(Box::new(programs.clone()), Box::new(Noon), Some(dir.path()))
        .with_archive_check(check(&mac));
    let params = params("run-9", order());
    assert_eq!(kind.prepare(&params), Ok(()));
    assert_eq!(mac.names(CHECK_DIR), ["Xcode.xip"]);
    assert_eq!(
        kind.cutover(&params),
        Ok(Cutover::Applied {
            version: "8 steps".to_owned()
        })
    );
    let probe = read_probe(&dir.path().join(PROBE_FILE)).expect("the probe file");
    let mut expected = recorded();
    expected.extend([
        kept("pkgutil", Some(0), APPLE_SIGNED),
        kept("xip", Some(0), ""),
        kept("app", None, "Xcode.app"),
        kept("version", None, "27.2 27B52"),
        kept("codesign", Some(0), ""),
        kept("verdict", Some(0), "pass"),
    ]);
    assert_eq!(
        probe,
        MaintenanceProbe {
            run_id: "run-9".to_owned(),
            steps: expected,
            probed_unix: 1_800_000_000,
        }
    );
    assert!(lock(&programs.0).is_empty(), "the plain probe's list ran");
    assert_gone(&mac);
    assert_eq!(kind.verify(&params), Ok(()));
    assert_eq!(
        kind.verify(&self::params("run-10", order())),
        Err(KindError::Failed {
            reason: REASON_PROBE_MISSING,
            detail: "the probe file holds the probe of run run-9".to_owned(),
        })
    );
}

/// Proves: the probe kind's `prepare` gives the check's deferral when the store lacks the
/// archive, and leaves nothing.
///
/// Catches: a probe turn that goes on to its cutover with no archive.
#[test]
fn the_probe_kind_defers_when_the_store_lacks_the_archive() {
    let mut mac = mac("chk-kind-gone");
    mac.store = FakeStore::holding(b"another archive", ARCHIVE_BYTES);
    let kind = ProbeKind::new(Box::new(Quiet::default()), Box::new(Noon), None)
        .with_archive_check(check(&mac));
    let path = format!("/v1/xcode/{}", entry_of(ARCHIVE_BYTES).sha256);
    assert_eq!(
        kind.prepare(&params("run-11", order())),
        deferral(
            REASON_NOT_IN_STORE,
            &format!("the store holds no archive {path}")
        )
    );
    assert_gone(&mac);
}

/// Proves: with no archive in the order, a probe that has an archive check is the plain probe:
/// `prepare` reads no store and makes no directory, and `cutover` runs the eight programs of the
/// list through its own `Programs`; and a probe with no archive check given an order that names
/// an archive is the plain probe too.
///
/// Catches: a plain probe run that runs the check's steps, or fetches, and a kind with no check
/// that fails on an archive.
#[test]
fn without_an_archive_or_a_check_the_probe_is_the_plain_probe() {
    let mac = mac("chk-plain");
    let dir = scratch("chk-plain-run");
    let programs = Quiet::default();
    let with_check = ProbeKind::new(Box::new(programs.clone()), Box::new(Noon), Some(dir.path()))
        .with_archive_check(check(&mac));
    let plain = params("run-12", Vec::new());
    assert_eq!(with_check.prepare(&plain), Ok(()));
    assert_eq!(
        with_check.cutover(&plain),
        Ok(Cutover::Applied {
            version: "8 steps".to_owned()
        })
    );
    assert_eq!(lock(&programs.0).len(), 8);
    assert!(mac.store.asked().is_empty());
    assert!(mac.runner.ran().is_empty());
    assert_gone(&mac);

    let programs = Quiet::default();
    let without = ProbeKind::new(Box::new(programs.clone()), Box::new(Noon), Some(dir.path()));
    let named = params("run-13", order());
    assert_eq!(without.prepare(&named), Ok(()));
    assert_eq!(
        without.cutover(&named),
        Ok(Cutover::Applied {
            version: "8 steps".to_owned()
        })
    );
    assert_eq!(lock(&programs.0).len(), 8);
}

// The steps of the check, on the drained Mac.
#[path = "check_steps_tests.rs"]
mod steps;
