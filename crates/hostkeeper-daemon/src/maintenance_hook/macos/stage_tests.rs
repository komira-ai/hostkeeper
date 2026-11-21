//! The tests of the `macos` kind's stage, over a fake Mac: its programs, each answer scripted by
//! program and exact argument list and each run recorded with its program, deadline and
//! arguments; and its data volume's free space, each read counted. What ran is asserted by
//! equality of the whole list of runs, each deferral by equality of its reason and whole detail.
//! The listings are the fixtures of the maintenance look (`tests/fixtures/maintenance`), or a
//! listing written here when no fixture has its shape. The host's programs' empty environment is
//! proved over a child of this test binary.

use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use hostkeeper_hook::Outcome;
use hostkeeper_launchd::LaunchctlError;
use hostkeeper_proto::hostkeeper::agent::v1::{
    Maintain, MaintainAction, MaintenanceKind as WireKind,
};
use hostkeeper_steps::{BootId, BootIdError, BootIdSource};

use super::*;
use crate::maintenance_hook::macos::Apply;
use crate::maintenance_hook::{
    Ending, HookArgs, HookPorts, HookStep, Kind, MacosKind, MaintenanceKind, Registry,
    STDIN_PARAMS, Unwired, main_with,
};

/// `sw_vers` on a Mac of macOS 26.6.
const SW_VERS_26_6: &str =
    include_str!("../../../tests/fixtures/maintenance/mac/sw_vers_macos26.txt");
/// `softwareupdate --list` on macOS 26: the minor update macOS Tahoe 26.7 (4718592 KiB) and the
/// major upgrade macOS 27.1.
const LIST_MAJOR: &str =
    include_str!("../../../tests/fixtures/maintenance/softwareupdate/macos26_list_major.txt");
/// `softwareupdate --list` offering nothing.
const LIST_NONE: &str =
    include_str!("../../../tests/fixtures/maintenance/softwareupdate/macos26_list_none.txt");
/// `softwareupdate --list-full-installers` on macOS 26: macOS 27.1 (16252928 KiB), macOS Tahoe
/// 26.7 and macOS Sequoia 15.7.4.
const FULL_26: &str =
    include_str!("../../../tests/fixtures/maintenance/softwareupdate/macos26_full_installers.txt");

/// `softwareupdate --list` offering the major upgrade macOS 27.1 alone.
const LIST_ONLY_MAJOR: &str = "Software Update Tool\n\n\
     Finding available software\n\
     Software Update found the following new or updated software:\n\
     * Label: macOS 27.1-26B70\n\
     \tTitle: macOS 27.1, Version: 27.1, Size: 15728640KiB, Recommended: NO, Action: restart, \n";

/// `softwareupdate --list` offering two macOS updates of macOS 26 (26.6.1, then 26.7) among an
/// update that is not macOS, a macOS of a lower major version, and Safari.
const LIST_TWO_MINORS: &str = "Software Update found the following new or updated software:\n\
     * Label: macOS Tahoe 26.6.1-25G110\n\
     \tTitle: macOS Tahoe 26.6.1, Version: 26.6.1, Size: 3215472KiB, Recommended: YES, Action: restart, \n\
     * Label: Command Line Tools for Xcode 26.4-26.4\n\
     \tTitle: Command Line Tools for Xcode 26.4, Version: 26.4, Size: 912346KiB, Recommended: YES, \n\
     * Label: macOS Sequoia 15.7.5-24G600\n\
     \tTitle: macOS Sequoia 15.7.5, Version: 15.7.5, Size: 2000000KiB, Recommended: YES, Action: restart, \n\
     * Label: Safari 27.2\n\
     \tTitle: Safari 27.2, Version: 27.2, Size: 204800KiB, Recommended: YES, \n\
     * Label: macOS Tahoe 26.7-25H62\n\
     \tTitle: macOS Tahoe 26.7, Version: 26.7, Size: 4718592KiB, Recommended: YES, Action: restart, \n";

const KIB: u64 = 1024;
/// The label of macOS Tahoe 26.7 in `--list`.
const TAHOE_26_7: &str = "macOS Tahoe 26.7-25H62";
/// Its download.
const TAHOE_26_7_BYTES: u64 = 4_718_592 * KIB;
/// The label of macOS Tahoe 26.6.1 in `--list`.
const TAHOE_26_6_1: &str = "macOS Tahoe 26.6.1-25G110";
/// Its download.
const TAHOE_26_6_1_BYTES: u64 = 3_215_472 * KIB;
/// The download of the full installer of macOS 27.1.
const MACOS_27_1_BYTES: u64 = 16_252_928 * KIB;

type Answer = Result<LaunchctlOutput, LaunchctlError>;

/// One run of a program: its path, its deadline and its arguments. Its environment is the
/// runner's: [`HostPrograms`]'s on a Mac, which gives none.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Ran {
    program: PathBuf,
    deadline: Duration,
    args: Vec<String>,
}

/// What the fake Mac answers, and what was asked of it.
struct Script {
    answers: BTreeMap<(PathBuf, Vec<String>), Answer>,
    runs: Vec<Ran>,
    free: Result<u64, String>,
    free_reads: usize,
}

/// A fake Mac: the stage's programs and data volume. Clones share one script.
#[derive(Clone)]
struct Mac(Arc<Mutex<Script>>);

impl Mac {
    /// A Mac whose data volume answers `free`, with no program scripted.
    fn new(free: Result<u64, String>) -> Mac {
        Mac(Arc::new(Mutex::new(Script {
            answers: BTreeMap::new(),
            runs: Vec::new(),
            free,
            free_reads: 0,
        })))
    }

    /// A Mac of macOS 26.6 whose data volume answers `free`, whose `--list` answers `list` and
    /// whose `--list-full-installers` answers [`FULL_26`]; no download scripted.
    fn on_26(free: Result<u64, String>, list: &str) -> Mac {
        let mac = Mac::new(free);
        mac.answer(SW_VERS, &[], ok(SW_VERS_26_6));
        mac.answer(SOFTWAREUPDATE, &["--list"], ok(list));
        mac.answer(SOFTWAREUPDATE, &["--list-full-installers"], ok(FULL_26));
        mac
    }

    fn script(&self) -> MutexGuard<'_, Script> {
        self.0.lock().expect("the script")
    }

    /// Answers `program` run with exactly `args` with `answer`, in place of any answer before.
    fn answer(&self, program: &str, args: &[&str], answer: Answer) {
        self.script()
            .answers
            .insert((PathBuf::from(program), strings(args)), answer);
    }

    fn runs(&self) -> Vec<Ran> {
        self.script().runs.clone()
    }

    fn free_reads(&self) -> usize {
        self.script().free_reads
    }

    /// The stage over this Mac.
    fn stage(&self) -> Stage {
        Stage::new(Box::new(self.clone()), Box::new(self.clone()))
    }
}

impl Programs for Mac {
    fn tool(&self, program: &Path, deadline: Duration) -> Box<dyn Launchctl> {
        Box::new(Tool {
            mac: self.clone(),
            program: program.to_owned(),
            deadline,
        })
    }
}

impl FreeSpace for Mac {
    fn free_bytes(&self) -> Result<u64, String> {
        let mut script = self.script();
        script.free_reads += 1;
        script.free.clone()
    }
}

/// One program of the fake Mac: a run is recorded, and answered as scripted; a run with no
/// answer could not be run.
struct Tool {
    mac: Mac,
    program: PathBuf,
    deadline: Duration,
}

impl Launchctl for Tool {
    fn run(&self, args: &[String]) -> Answer {
        let mut script = self.mac.script();
        script.runs.push(Ran {
            program: self.program.clone(),
            deadline: self.deadline,
            args: args.to_vec(),
        });
        let scripted = script.answers.get(&(self.program.clone(), args.to_vec()));
        scripted.cloned().unwrap_or_else(|| {
            Err(LaunchctlError::Spawn {
                args: args.join(" "),
                why: "not scripted".to_owned(),
            })
        })
    }
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

fn ok(stdout: &str) -> Answer {
    Ok(LaunchctlOutput::success(stdout))
}

fn ran(program: &str, deadline: Duration, args: &[&str]) -> Ran {
    Ran {
        program: PathBuf::from(program),
        deadline,
        args: strings(args),
    }
}

fn sw_vers_ran() -> Ran {
    ran(SW_VERS, LOOK_DEADLINE, &[])
}

fn list_ran() -> Ran {
    ran(SOFTWAREUPDATE, LIST_DEADLINE, &["--list"])
}

fn full_ran() -> Ran {
    ran(SOFTWAREUPDATE, LIST_DEADLINE, &["--list-full-installers"])
}

fn download_ran(label: &str) -> Ran {
    ran(SOFTWAREUPDATE, DOWNLOAD_DEADLINE, &["--download", label])
}

fn fetch_args(version: &str) -> Vec<String> {
    strings(&[
        "--fetch-full-installer",
        "--full-installer-version",
        version,
    ])
}

fn fetch_ran(version: &str) -> Ran {
    Ran {
        program: PathBuf::from(SOFTWAREUPDATE),
        deadline: DOWNLOAD_DEADLINE,
        args: fetch_args(version),
    }
}

/// The parameters of a `macos` run, a major upgrade allowed when `major`.
fn params(major: bool) -> MaintainParams {
    MaintainParams::new(Maintain {
        run_id: "run-1".to_owned(),
        action: MaintainAction::Run.into(),
        kinds: vec![WireKind::Macos.into()],
        macos_major: major,
        ..Maintain::default()
    })
    .expect("valid parameters")
}

/// The parameters of a `macos` run on `path`, no major upgrade allowed.
fn on_path(path: MacosPath) -> MaintainParams {
    MaintainParams::new(Maintain {
        macos_path: path.into(),
        ..params(false).maintain().clone()
    })
    .expect("valid parameters")
}

/// The stage's deferral of `reason` with exactly `detail`.
fn deferral(reason: &'static str, detail: &str) -> Result<(), KindError> {
    Err(KindError::Deferred {
        reason,
        detail: detail.to_owned(),
    })
}

/// The short-space detail of `free` bytes free for a download of `bytes`.
fn short(free: u64, bytes: u64) -> String {
    format!(
        "/System/Volumes/Data has {free} bytes free, less than twice the {bytes} bytes to download"
    )
}

/// Proves: on a Mac of macOS 26.6 offered a minor update (macOS Tahoe 26.7) and a major upgrade
/// (macOS 27.1), an order that does not allow a major upgrade downloads the minor update alone,
/// by the label `--list` gives it, with exactly twice its size free: `sw_vers`, then
/// `softwareupdate --list`, then `softwareupdate --download macOS Tahoe 26.7-25H62`, each with
/// exactly these arguments and its deadline; the full installers are not listed, and the free
/// space is read once, before the download.
///
/// Catches: a label taken from anywhere but the Mac's own listing, a major upgrade fetched though
/// the order does not carry it, a check that asks for more than twice the download.
#[test]
fn a_minor_update_is_downloaded_by_its_listed_label() {
    let mac = Mac::on_26(Ok(2 * TAHOE_26_7_BYTES), LIST_MAJOR);
    mac.answer(SOFTWAREUPDATE, &["--download", TAHOE_26_7], ok(""));
    assert_eq!(mac.stage().run(&params(false)), Ok(()));
    assert_eq!(
        mac.runs(),
        vec![sw_vers_ran(), list_ran(), download_ran(TAHOE_26_7)]
    );
    assert_eq!(mac.free_reads(), 1);
}

/// Proves: with the order allowing a major upgrade, the same Mac lists its full installers and
/// fetches the newest of a higher major version, macOS 27.1, by its version, alone (the minor
/// update beside it is not downloaded), with exactly twice the installer's size free:
/// `softwareupdate --fetch-full-installer --full-installer-version 27.1`.
///
/// Catches: the major upgrade not fetched when the order allows it, the minor update staged
/// beside it, the installer's version taken from the `--list` label.
#[test]
fn a_major_upgrade_the_order_allows_fetches_its_full_installer_alone() {
    let mac = Mac::on_26(Ok(2 * MACOS_27_1_BYTES), LIST_MAJOR);
    mac.answer(
        SOFTWAREUPDATE,
        &["--fetch-full-installer", "--full-installer-version", "27.1"],
        ok("Install finished successfully\n"),
    );
    assert_eq!(mac.stage().run(&params(true)), Ok(()));
    assert_eq!(
        mac.runs(),
        vec![sw_vers_ran(), list_ran(), full_ran(), fetch_ran("27.1")]
    );
}

/// Proves: a Mac offered only a major upgrade, under an order that does not carry one, fetches
/// nothing and lists no full installer: the stage defers `macos_update_not_listed`, before
/// measuring the disk.
///
/// Catches: a major upgrade fetched (or its installer listed) because the Mac offers it, though
/// the order does not allow it.
#[test]
fn a_major_upgrade_is_never_fetched_unless_the_order_carries_it() {
    let mac = Mac::on_26(Ok(u64::MAX), LIST_ONLY_MAJOR);
    assert_eq!(
        mac.stage().run(&params(false)),
        deferral(
            REASON_NOT_LISTED,
            "softwareupdate offers no update of macOS 26"
        )
    );
    assert_eq!(mac.runs(), vec![sw_vers_ran(), list_ran()]);
    assert_eq!(mac.free_reads(), 0);
}

/// Proves: an update that is no longer listed defers `macos_update_not_listed` and downloads
/// nothing: a Mac whose `--list` offers nothing; and, with a major upgrade allowed, one whose
/// full installers are all of its own or a lower major version.
///
/// Catches: a stage that succeeds with nothing staged, or downloads what the order once named
/// rather than what the Mac is offered now.
#[test]
fn an_update_no_longer_listed_defers() {
    let mac = Mac::on_26(Ok(u64::MAX), LIST_NONE);
    assert_eq!(
        mac.stage().run(&params(false)),
        deferral(
            REASON_NOT_LISTED,
            "softwareupdate offers no update of macOS 26"
        )
    );
    assert_eq!(mac.runs(), vec![sw_vers_ran(), list_ran()]);

    let mac = Mac::on_26(Ok(u64::MAX), LIST_NONE);
    mac.answer(
        SOFTWAREUPDATE,
        &["--list-full-installers"],
        ok("* Title: macOS Tahoe, Version: 26.7, Size: 15204352KiB, Build: 25H62, Deferred: NO\n\
            * Title: macOS Sequoia, Version: 15.7.4, Size: 14680064KiB, Build: 24G517, Deferred: NO\n"),
    );
    assert_eq!(
        mac.stage().run(&params(true)),
        deferral(
            REASON_NOT_LISTED,
            "softwareupdate offers no update of macOS 26 and no full installer of a later major \
             version"
        )
    );
    assert_eq!(mac.runs(), vec![sw_vers_ran(), list_ran(), full_ran()]);
    assert_eq!(mac.free_reads(), 0);
}

/// Proves: a download that answers `Update not found` (Apple superseded what was listed) defers
/// `macos_update_not_listed`, whether it says so on its standard error with a failing exit code
/// (a minor update) or on its standard output with exit 0 (a major upgrade).
///
/// Catches: a superseded update taken for staged because the tool exited 0, or reported as a
/// failed download rather than as no longer offered.
#[test]
fn a_download_apple_no_longer_offers_defers() {
    let mac = Mac::on_26(Ok(u64::MAX), LIST_MAJOR);
    mac.answer(
        SOFTWAREUPDATE,
        &["--download", TAHOE_26_7],
        Ok(LaunchctlOutput::exit(1, "Update not found\n")),
    );
    assert_eq!(
        mac.stage().run(&params(false)),
        deferral(
            REASON_NOT_LISTED,
            "softwareupdate --download macOS Tahoe 26.7-25H62 answered \"Update not found\": \
             Apple no longer offers it"
        )
    );
    assert_eq!(
        mac.runs(),
        vec![sw_vers_ran(), list_ran(), download_ran(TAHOE_26_7)]
    );

    let mac = Mac::on_26(Ok(u64::MAX), LIST_MAJOR);
    mac.answer(
        SOFTWAREUPDATE,
        &["--fetch-full-installer", "--full-installer-version", "27.1"],
        ok("Install failed with error: Update not found\n"),
    );
    assert_eq!(
        mac.stage().run(&params(true)),
        deferral(
            REASON_NOT_LISTED,
            "softwareupdate --fetch-full-installer --full-installer-version 27.1 answered \
             \"Update not found\": Apple no longer offers it"
        )
    );
}

/// Proves: one byte short of twice the download defers `free_space_short` with the free and the
/// download bytes, and downloads nothing: for a minor update and for a major upgrade (measured by
/// its full installer's size).
///
/// Catches: a check of once the download rather than twice, a download made before the check, a
/// major upgrade measured by its `--list` size.
#[test]
fn a_short_disk_defers_and_downloads_nothing() {
    let free = 2 * TAHOE_26_7_BYTES - 1;
    let mac = Mac::on_26(Ok(free), LIST_MAJOR);
    assert_eq!(
        mac.stage().run(&params(false)),
        deferral(REASON_SPACE_SHORT, &short(free, TAHOE_26_7_BYTES))
    );
    assert_eq!(mac.runs(), vec![sw_vers_ran(), list_ran()]);

    let free = 2 * MACOS_27_1_BYTES - 1;
    let mac = Mac::on_26(Ok(free), LIST_MAJOR);
    assert_eq!(
        mac.stage().run(&params(true)),
        deferral(REASON_SPACE_SHORT, &short(free, MACOS_27_1_BYTES))
    );
    assert_eq!(mac.runs(), vec![sw_vers_ran(), list_ran(), full_ran()]);
}

/// Proves: a data volume whose free space cannot be read defers `free_space_unreadable` with the
/// port's reason, and downloads nothing.
///
/// Catches: an unreadable free space taken as enough.
#[test]
fn an_unreadable_free_space_defers_and_downloads_nothing() {
    let why = "statvfs of /System/Volumes/Data failed: Input/output error (os error 5)";
    let mac = Mac::on_26(Err(why.to_owned()), LIST_MAJOR);
    assert_eq!(
        mac.stage().run(&params(false)),
        deferral(REASON_SPACE_UNREADABLE, why)
    );
    assert_eq!(mac.runs(), vec![sw_vers_ran(), list_ran()]);
}

/// Proves: every macOS update of the running major version is downloaded, in the listing's
/// order, and the check is of twice their sum: the Command Line Tools, Safari and an update of
/// macOS 15 beside them are not downloaded and not counted.
///
/// Catches: only the first update staged, an update that is not macOS or not of the running
/// major downloaded, a check of the largest update rather than of the sum.
#[test]
fn every_macos_update_of_the_running_major_is_downloaded_in_order() {
    let bytes = TAHOE_26_6_1_BYTES + TAHOE_26_7_BYTES;
    let mac = Mac::on_26(Ok(2 * bytes), LIST_TWO_MINORS);
    mac.answer(SOFTWAREUPDATE, &["--download", TAHOE_26_6_1], ok(""));
    mac.answer(SOFTWAREUPDATE, &["--download", TAHOE_26_7], ok(""));
    assert_eq!(mac.stage().run(&params(false)), Ok(()));
    assert_eq!(
        mac.runs(),
        vec![
            sw_vers_ran(),
            list_ran(),
            download_ran(TAHOE_26_6_1),
            download_ran(TAHOE_26_7)
        ]
    );

    let mac = Mac::on_26(Ok(2 * bytes - 1), LIST_TWO_MINORS);
    assert_eq!(
        mac.stage().run(&params(false)),
        deferral(REASON_SPACE_SHORT, &short(2 * bytes - 1, bytes))
    );
}

/// Proves: a download that fails defers `stage_download_failed` naming the command, its exit code
/// and the first line of its standard error, and the downloads after it are not tried; so do a
/// download that does not end by its deadline and one that cannot be run.
///
/// Catches: a failed download taken for staged, the next update downloaded after a failure, a
/// failure whose detail does not say which download failed.
#[test]
fn a_failed_download_defers() {
    let mac = Mac::on_26(Ok(u64::MAX), LIST_TWO_MINORS);
    mac.answer(
        SOFTWAREUPDATE,
        &["--download", TAHOE_26_6_1],
        Ok(LaunchctlOutput::exit(
            1,
            "Failed to download: the network connection was lost\nsecond line\n",
        )),
    );
    assert_eq!(
        mac.stage().run(&params(false)),
        deferral(
            REASON_DOWNLOAD_FAILED,
            "softwareupdate --download macOS Tahoe 26.6.1-25G110 failed with exit code 1: Failed \
             to download: the network connection was lost"
        )
    );
    assert_eq!(
        mac.runs(),
        vec![sw_vers_ran(), list_ran(), download_ran(TAHOE_26_6_1)]
    );

    let mac = Mac::on_26(Ok(u64::MAX), LIST_TWO_MINORS);
    mac.answer(
        SOFTWAREUPDATE,
        &["--download", TAHOE_26_6_1],
        Err(LaunchctlError::TimedOut {
            args: format!("--download {TAHOE_26_6_1}"),
        }),
    );
    assert_eq!(
        mac.stage().run(&params(false)),
        deferral(
            REASON_DOWNLOAD_FAILED,
            "softwareupdate --download macOS Tahoe 26.6.1-25G110 did not finish within its \
             deadline"
        )
    );

    // The first download succeeds; the second has no answer, so it cannot be run.
    let mac = Mac::on_26(Ok(u64::MAX), LIST_TWO_MINORS);
    mac.answer(SOFTWAREUPDATE, &["--download", TAHOE_26_6_1], ok(""));
    assert_eq!(
        mac.stage().run(&params(false)),
        deferral(
            REASON_DOWNLOAD_FAILED,
            "softwareupdate --download macOS Tahoe 26.7-25H62 could not be run: not scripted"
        )
    );
}

/// One listing of [`a_listing_that_fails_defers`]: the program and arguments answered, the
/// answer, whether the order allows a major upgrade, the detail of the deferral and what ran.
struct Case {
    program: &'static str,
    args: &'static [&'static str],
    answer: Answer,
    major: bool,
    detail: &'static str,
    runs: Vec<Ran>,
}

/// Proves: each listing that fails, or gives no running major version, defers
/// `stage_listing_failed` naming the step, and nothing after it runs: `sw_vers` failing, naming no
/// version, or naming one with no major number; `softwareupdate --list` failing; and, with a
/// major upgrade allowed, `--list-full-installers` failing.
///
/// Catches: a failed listing read as a listing offering nothing, or a Mac whose version is
/// unknown staged by a guess at its major version.
#[test]
fn a_listing_that_fails_defers() {
    let cases = [
        Case {
            program: SW_VERS,
            args: &[],
            answer: Ok(LaunchctlOutput::exit(1, "sw_vers: cannot read\n")),
            major: false,
            detail: "sw_vers failed with exit code 1: sw_vers: cannot read",
            runs: vec![sw_vers_ran()],
        },
        Case {
            program: SW_VERS,
            args: &[],
            answer: ok("ProductName:\t\tmacOS\n"),
            major: false,
            detail: "sw_vers named the version \"\", which has no major number",
            runs: vec![sw_vers_ran()],
        },
        Case {
            program: SW_VERS,
            args: &[],
            answer: ok("ProductVersion:\t\tnext\n"),
            major: false,
            detail: "sw_vers named the version \"next\", which has no major number",
            runs: vec![sw_vers_ran()],
        },
        Case {
            program: SOFTWAREUPDATE,
            args: &["--list"],
            answer: Ok(LaunchctlOutput::exit(1, "Cannot reach the catalog\n")),
            major: false,
            detail: "softwareupdate --list failed with exit code 1: Cannot reach the catalog",
            runs: vec![sw_vers_ran(), list_ran()],
        },
        Case {
            program: SOFTWAREUPDATE,
            args: &["--list-full-installers"],
            answer: Ok(LaunchctlOutput::exit(1, "Cannot reach the catalog\n")),
            major: true,
            detail: "softwareupdate --list-full-installers failed with exit code 1: Cannot reach \
                     the catalog",
            runs: vec![sw_vers_ran(), list_ran(), full_ran()],
        },
    ];
    for case in cases {
        let detail = case.detail;
        let mac = Mac::on_26(Ok(u64::MAX), LIST_MAJOR);
        mac.answer(case.program, case.args, case.answer);
        assert_eq!(
            mac.stage().run(&params(case.major)),
            deferral(REASON_LISTING_FAILED, detail),
            "{detail}"
        );
        assert_eq!(mac.runs(), case.runs, "{detail}");
        assert_eq!(mac.free_reads(), 0, "{detail}");
    }
}

/// Proves: what would be staged with no size in KiB defers `stage_listing_failed` naming it, and
/// nothing is measured or downloaded: an update of `--list` with no `Size`, and a full installer
/// whose `Size` is in another unit.
///
/// Catches: an unknown size taken as 0, so that the free-space check passes on a guess.
#[test]
fn a_download_with_no_size_in_kib_defers() {
    let mac = Mac::on_26(
        Ok(u64::MAX),
        "* Label: macOS Tahoe 26.7-25H62\n\
         \tTitle: macOS Tahoe 26.7, Version: 26.7, Recommended: YES, Action: restart, \n",
    );
    assert_eq!(
        mac.stage().run(&params(false)),
        deferral(
            REASON_LISTING_FAILED,
            "softwareupdate --list gave no size in KiB for macOS Tahoe 26.7-25H62"
        )
    );
    assert_eq!(mac.runs(), vec![sw_vers_ran(), list_ran()]);
    assert_eq!(mac.free_reads(), 0);

    let mac = Mac::on_26(Ok(u64::MAX), LIST_NONE);
    mac.answer(
        SOFTWAREUPDATE,
        &["--list-full-installers"],
        ok("* Title: macOS 27.1, Version: 27.1, Size: 15.5GB, Build: 26B70, Deferred: NO\n"),
    );
    assert_eq!(
        mac.stage().run(&params(true)),
        deferral(
            REASON_LISTING_FAILED,
            "softwareupdate --list-full-installers gave no size in KiB for macOS 27.1"
        )
    );
    assert_eq!(mac.free_reads(), 0);
}

/// Proves: of the full installers, the newest of macOS of a higher major version is staged, with
/// its own size: a later version wins whichever comes first, of equal versions the first wins,
/// and an installer that is not macOS, of the running major, or of no major version is passed
/// over; without the major upgrade allowed none is.
///
/// Catches: the first or the last installer of a higher major staged rather than the newest, an
/// installer that is not macOS staged.
#[test]
fn the_newest_full_installer_of_a_higher_major_is_staged() {
    let listing = parse_softwareupdate_list(
        "* Title: macOS 28, Version: 28.0, Size: 1KiB, Build: 27A100\n\
         * Title: macOS 28.1, Version: 28.1, Size: 2KiB, Build: 27B10\n\
         * Title: macOS 28.1 again, Version: 28.1, Size: 3KiB, Build: 27B11\n\
         * Title: macOS 28.0.1, Version: 28.0.1, Size: 4KiB, Build: 27A200\n\
         * Title: Safari, Version: 30.0, Size: 5KiB, Build: 1\n\
         * Title: macOS Tahoe, Version: 26.7, Size: 6KiB, Build: 25H62\n\
         * Title: macOS odd, Version: x.1, Size: 7KiB, Build: 1\n",
    );
    assert_eq!(
        plan(26, &listing, true),
        Ok(vec![Download {
            args: fetch_args("28.1"),
            bytes: 2 * KIB,
        }])
    );
    assert_eq!(
        plan(26, &listing, false).map(|_| ()),
        deferral(
            REASON_NOT_LISTED,
            "softwareupdate offers no update of macOS 26"
        )
    );
}

/// The `macos` kind of `mac`'s stage, its apply the Mac's own, which a `prepare` never runs.
fn macos_kind(mac: &Mac) -> MacosKind {
    MacosKind::new(mac.stage(), Apply::at_host(None))
}

/// Proves: the `macos` kind's `prepare` is the stage, and runs nothing of the apply (its steps
/// are tested in `apply_tests.rs`).
///
/// Catches: a kind whose `prepare` is not the stage.
#[test]
fn the_macos_kinds_prepare_is_the_stage() {
    let mac = Mac::on_26(Ok(2 * TAHOE_26_7_BYTES), LIST_MAJOR);
    mac.answer(SOFTWAREUPDATE, &["--download", TAHOE_26_7], ok(""));
    let kind = macos_kind(&mac);
    assert_eq!(kind.prepare(&params(false)), Ok(()));
    assert_eq!(
        mac.runs(),
        vec![sw_vers_ran(), list_ran(), download_ran(TAHOE_26_7)]
    );
}

/// A boot id the hook's `prepare` has no need of.
struct Boot;

impl BootIdSource for Boot {
    fn read(&self) -> Result<BootId, BootIdError> {
        BootId::new("the test", "boot-1")
    }
}

/// `hostkeeper maintenance-hook prepare --params -` over `mac`'s stage as the `macos` kind, with
/// the parameters `json` on standard input: its outcome and its standard output.
fn hook(mac: &Mac, json: &str) -> (Outcome, String) {
    let kinds = Registry::new().with(Kind::Macos, Box::new(macos_kind(mac)));
    let ports = HookPorts {
        kinds: &kinds,
        record: &Unwired,
        rebooter: &Unwired,
        boot_id: &Boot,
    };
    let args = HookArgs {
        step: HookStep::Prepare,
        params: PathBuf::from(STDIN_PARAMS),
        package_lock_wait_secs: crate::cli::DEFAULT_PACKAGE_LOCK_WAIT_SECS,
    };
    let mut stdout = Vec::new();
    let outcome = main_with(&args, &ports, &mut json.as_bytes(), &mut stdout);
    (outcome, String::from_utf8(stdout).expect("UTF-8"))
}

/// The report line of `ending`, as the hook writes it.
fn line(ending: Ending) -> String {
    format!("{}\n", ending.report().to_line())
}

/// Proves: `maintenance-hook prepare` with the order's JSON (`macosMajor` set) stages the major
/// upgrade and exits done; on a disk one byte short it exits deferred (75) with the report of
/// `free_space_short`, class `environment`, and fetches nothing.
///
/// Catches: the order's `macosMajor` lost between the JSON and the stage, a deferral of the stage
/// reported as a failure (which would halt the run rather than leave the host in service).
#[test]
fn maintenance_hook_prepare_stages_a_macos_update_or_defers() {
    let json = r#"{"runId":"run-1","action":"MAINTAIN_ACTION_RUN","kinds":["MAINTENANCE_KIND_MACOS"],"macosMajor":true}"#;
    let mac = Mac::on_26(Ok(2 * MACOS_27_1_BYTES), LIST_MAJOR);
    mac.answer(
        SOFTWAREUPDATE,
        &["--fetch-full-installer", "--full-installer-version", "27.1"],
        ok(""),
    );
    let done = Ending {
        outcome: Outcome::Success,
        reason: None,
        detail: None,
    };
    assert_eq!(hook(&mac, json), (Outcome::Success, line(done)));
    assert_eq!(mac.runs().last(), Some(&fetch_ran("27.1")));

    let free = 2 * MACOS_27_1_BYTES - 1;
    let mac = Mac::on_26(Ok(free), LIST_MAJOR);
    let deferred = Ending {
        outcome: Outcome::Deferred,
        reason: Some(REASON_SPACE_SHORT),
        detail: Some(short(free, MACOS_27_1_BYTES)),
    };
    assert_eq!(hook(&mac, json), (Outcome::Deferred, line(deferred)));
    assert_eq!(mac.runs(), vec![sw_vers_ran(), list_ran(), full_ran()]);
    // The fake's own boot id, which a `prepare` never reads.
    assert_eq!(Boot.read().expect("a boot id").as_str(), "boot-1");
}

/// The environment this process has, without what a process sets in itself and no parent passed:
/// an instrumented build's `__LLVM_PROFILE_RT_*`, and on macOS CoreFoundation's
/// `__CF_USER_TEXT_ENCODING`, which it sets as it loads (this binary links it through
/// `hostkeeper-bootstrap`'s `rustls-native-certs`).
#[expect(
    clippy::disallowed_methods,
    reason = "the helper child reports the environment its parent gave it"
)]
fn own_environment() -> Vec<String> {
    std::env::vars_os()
        .filter(|(name, _)| !set_by_itself(name.as_bytes()))
        .map(|(name, value)| format!("env:{}={}", name.to_string_lossy(), value.to_string_lossy()))
        .collect()
}

/// Whether the variable `name` is one this process set in itself (see `own_environment`).
fn set_by_itself(name: &[u8]) -> bool {
    name.starts_with(b"__LLVM_PROFILE_RT_")
        || (cfg!(target_os = "macos") && name == b"__CF_USER_TEXT_ENCODING")
}

/// Catches nothing itself: the child of
/// `the_host_programs_run_with_exactly_their_arguments_and_an_empty_environment`, which prints
/// each argument and each variable it was given on a line of its own.
#[test]
#[ignore = "the child process of the_host_programs_run_with_exactly_their_arguments_and_an_empty_environment; run only by it"]
fn child_prints_its_arguments_and_environment() {
    for arg in std::env::args().skip(1) {
        println!("arg:{arg}");
    }
    for line in own_environment() {
        println!("{line}");
    }
}

/// Proves: the Mac's programs are run with exactly the arguments given and an empty environment,
/// over a child of this test binary that prints what it got.
///
/// Catches: a runner that passes the hook's own environment on to `softwareupdate` (a proxy, a
/// locale, a catalog URL), or that adds an argument.
#[test]
fn the_host_programs_run_with_exactly_their_arguments_and_an_empty_environment() {
    let exe = std::env::current_exe().expect("the test binary");
    let helper = strings(&[
        "maintenance_hook::macos::stage::tests::child_prints_its_arguments_and_environment",
        "--exact",
        "--ignored",
        "--nocapture",
    ]);
    let output = HostPrograms
        .tool(&exe, Duration::from_secs(60))
        .run(&helper)
        .expect("the child ran");
    assert_eq!(output.code, Some(0));
    let reported: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with("arg:") || line.starts_with("env:"))
        .map(str::to_owned)
        .collect();
    let expected: Vec<String> = helper.iter().map(|arg| format!("arg:{arg}")).collect();
    assert_eq!(reported, expected);
}

/// Proves: on the order's path `wait` (device management installs the update) the stage runs no
/// program and reads no disk, and succeeds; on the path `install` it stages as on an order that
/// names no path.
///
/// Catches: the daemon running `softwareupdate` for `macos` on the path where it must run no Apple
/// program.
#[test]
fn the_wait_path_runs_no_apple_program() {
    let mac = Mac::on_26(Ok(u64::MAX), LIST_MAJOR);
    assert_eq!(mac.stage().run(&on_path(MacosPath::Wait)), Ok(()));
    assert_eq!(mac.runs(), Vec::new());
    assert_eq!(mac.free_reads(), 0);

    mac.answer(SOFTWAREUPDATE, &["--download", TAHOE_26_7], ok(""));
    assert_eq!(mac.stage().run(&on_path(MacosPath::Install)), Ok(()));
    assert_eq!(
        mac.runs(),
        vec![sw_vers_ran(), list_ran(), download_ran(TAHOE_26_7)]
    );
}

/// Proves: with a major upgrade allowed but no full installer of a higher major offered, the
/// minor update `--list` offers is staged; and a major upgrade `--list` offers that
/// `--list-full-installers` does not is never fetched: with no minor beside it the stage defers.
///
/// Catches: an allowed but unoffered upgrade blocking the minor update, or a major upgrade
/// fetched by the version of a `--list` entry the installer catalog does not hold.
#[test]
fn an_allowed_upgrade_that_is_not_offered_as_an_installer_is_not_fetched() {
    let only_old = "* Title: macOS Tahoe, Version: 26.7, Size: 15204352KiB, Build: 25H62, \
                    Deferred: NO\n";
    let mac = Mac::on_26(Ok(2 * TAHOE_26_7_BYTES), LIST_MAJOR);
    mac.answer(SOFTWAREUPDATE, &["--list-full-installers"], ok(only_old));
    mac.answer(SOFTWAREUPDATE, &["--download", TAHOE_26_7], ok(""));
    assert_eq!(mac.stage().run(&params(true)), Ok(()));
    assert_eq!(
        mac.runs(),
        vec![
            sw_vers_ran(),
            list_ran(),
            full_ran(),
            download_ran(TAHOE_26_7)
        ]
    );

    let mac = Mac::on_26(Ok(u64::MAX), LIST_ONLY_MAJOR);
    mac.answer(SOFTWAREUPDATE, &["--list-full-installers"], ok(only_old));
    assert_eq!(
        mac.stage().run(&params(true)),
        deferral(
            REASON_NOT_LISTED,
            "softwareupdate offers no update of macOS 26 and no full installer of a later major \
             version"
        )
    );
    assert_eq!(mac.runs(), vec![sw_vers_ran(), list_ran(), full_ran()]);
}

/// Proves: sizes whose sum is more than `u64` holds never pass the check, even with every byte
/// of a `u64` free: the stage defers `free_space_short` with the whole sum, and downloads nothing.
///
/// Catches: the sum or its double wrapped (or saturated) in `u64`, so a check passes on a number
/// that is not the download.
#[test]
fn sizes_beyond_u64_never_pass_the_check() {
    let kib = u64::MAX / KIB;
    let list = format!(
        "* Label: macOS Tahoe 26.6.1-25G110\n\
         \tTitle: macOS Tahoe 26.6.1, Version: 26.6.1, Size: {kib}KiB, \n\
         * Label: macOS Tahoe 26.7-25H62\n\
         \tTitle: macOS Tahoe 26.7, Version: 26.7, Size: {kib}KiB, \n"
    );
    let mac = Mac::on_26(Ok(u64::MAX), &list);
    let bytes = 2 * u128::from(kib * KIB);
    assert_eq!(
        mac.stage().run(&params(false)),
        Err(KindError::Deferred {
            reason: REASON_SPACE_SHORT,
            detail: format!(
                "/System/Volumes/Data has {} bytes free, less than twice the {bytes} bytes to \
                 download",
                u64::MAX
            ),
        })
    );
    assert_eq!(mac.runs(), vec![sw_vers_ran(), list_ran()]);
}
