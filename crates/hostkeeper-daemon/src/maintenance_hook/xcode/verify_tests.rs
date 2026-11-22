//! The tests of the check of the `xcode` kind, each wrong fact apart, and of the kind in the
//! maintenance hook's sequence: after a macOS update's reboot, and failing on a first launch that
//! does not succeed. The fakes are those of the apply's tests.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use hostkeeper_hook::Outcome;
use hostkeeper_package::gh::SystemRunner;
use hostkeeper_steps::{BootId, BootIdError, BootIdSource, Clock};

use super::*;
use crate::maintenance_hook::xcode::XcodeKind;
use crate::maintenance_hook::xcode::stage::Stage;
use crate::maintenance_hook::xcode::store::HostStore;
use crate::maintenance_hook::{
    Ending, HookPorts, HookStep, Kind, KindState, MaintenanceKind, REASON_REBOOT_UNDER_WAY,
    RebootError, Rebooter, RecordError, Registry, RunRecord, run_step,
};
use crate::maintenance_mac::Statvfs;
use crate::testing::FakeClock;

const SELECTED_27: &str = "/Applications/Xcode-27.2.app/Contents/Developer\n";

/// What `xcodebuild -version` of Xcode `version` (`build`) says.
fn says_version(version: &str, build: &str) -> Answer {
    let said = format!("Xcode {version}\nBuild version {build}\n");
    Arc::new(move |_| Ok(LaunchctlOutput::success(&said)))
}

/// A program that exits 0 saying `stdout`.
fn says(stdout: &str) -> Answer {
    let said = stdout.to_owned();
    Arc::new(move |_| Ok(LaunchctlOutput::success(&said)))
}

/// A Mac with Xcode 26.3 (26D10) and 27.2 (27B52) installed, each `xcodebuild -version` naming
/// its build, and `xcode-select -p` naming 27.2.
fn checked_mac(name: &str) -> (Mac, [XcodeEntry; 2]) {
    let mac = Mac::new(name);
    mac.installed("26.3", "26D10");
    mac.installed("27.2", "27B52");
    mac.answers(
        &xcodebuild("26.3", "-version"),
        says_version("26.3", "26D10"),
    );
    mac.answers(
        &xcodebuild("27.2", "-version"),
        says_version("27.2", "27B52"),
    );
    mac.answers("/usr/bin/xcode-select -p", says(SELECTED_27));
    (mac, [entry("26.3", "26D10"), entry("27.2", "27B52")])
}

fn wrong(reason: &'static str, detail: &str) -> Result<(), KindError> {
    Err(KindError::Failed {
        reason,
        detail: detail.to_owned(),
    })
}

/// Proves: a Mac with each Xcode of the order at its path with its build, each `xcodebuild
/// -version` naming that build, each licence accepted and `xcode-select -p` naming the default's
/// developer directory passes, by exactly these runs in this order, each with its deadline; with
/// no default, `xcode-select` is not run.
///
/// Catches: a check that reads the `/usr/bin` shim's version in place of each Xcode's, or that
/// skips the licence of one Xcode.
#[test]
fn a_mac_with_every_fact_right_passes_the_check() {
    let (mac, xcodes) = checked_mac("xcode-verify-right");
    assert_eq!(mac.apply().verify(&params(&xcodes, &[], "27.2")), Ok(()));
    assert_eq!(
        mac.ran(),
        [
            xcodebuild("26.3", "-version"),
            xcodebuild("26.3", "-license check"),
            xcodebuild("27.2", "-version"),
            xcodebuild("27.2", "-license check"),
            "/usr/bin/xcode-select -p".to_owned(),
        ]
    );
    assert_eq!(
        mac.deadlines(),
        [
            CHECK_DEADLINE,
            CHECK_DEADLINE,
            CHECK_DEADLINE,
            CHECK_DEADLINE,
            SELECT_DEADLINE
        ]
    );
    let (mac, xcodes) = checked_mac("xcode-verify-no-default");
    assert_eq!(mac.apply().verify(&params(&xcodes, &[], "")), Ok(()));
    assert!(!mac.ran().iter().any(|line| line.contains("xcode-select")));
    let (mac, xcodes) = checked_mac("xcode-verify-app-path");
    mac.answers(
        "/usr/bin/xcode-select -p",
        says("/Applications/Xcode-27.2.app\n"),
    );
    assert_eq!(mac.apply().verify(&params(&xcodes, &[], "27.2")), Ok(()));
}

/// Proves: each wrong fact fails the check with its own reason and its whole detail: an Xcode not
/// at its path, one whose directories down to its `xcodebuild` others can write, at its path with
/// another build, a version list without its keys or that cannot be
/// read, an `xcodebuild -version` that fails or names another build, a licence not accepted, an
/// `xcode-select -p` that fails, names another Xcode, a directory that only starts like the
/// default's, or nothing.
///
/// Catches: a check that passes a Mac whose new Xcode is missing, of the wrong build, unusable,
/// unlicensed, or not the default; a prefix match taking `/Applications/Xcode-27.2.app.old` for
/// `/Applications/Xcode-27.2.app`.
#[test]
fn each_wrong_fact_fails_the_check_with_its_reason() {
    let app = "/Applications/Xcode-27.2.app";
    let version = xcodebuild("27.2", "-version");
    let licence = xcodebuild("27.2", "-license check");
    let select = "/usr/bin/xcode-select -p";
    let wrong_default = |named: &str| {
        format!("xcode-select -p names {named}; the order's default is {app}/Contents/Developer")
    };
    let cases: [(&str, Setup<'_>, &'static str, String); 12] = [
        (
            "held",
            Box::new(|mac| mac.mode(&format!("{app}/Contents/Developer"), 0o777)),
            REASON_INSTALL_TAKEN,
            format!(
                "{app}/Contents/Developer is a link, is not this process's user's, or is writable \
                 by others"
            ),
        ),
        (
            "missing",
            Box::new(|mac| fs::remove_dir_all(mac.path(app)).expect("removed")),
            REASON_MISSING,
            format!("Xcode 27.2 (27B52) is not at {app}: it has no version list"),
        ),
        (
            "build",
            Box::new(|mac| mac.installed("27.2", "27B51")),
            REASON_BUILD_MISMATCH,
            format!("{app} is Xcode 27.2 (27B51); the order names Xcode 27.2 (27B52)"),
        ),
        (
            "keys",
            Box::new(|mac| {
                mac.file(
                    &format!("{app}/Contents/version.plist"),
                    plist("27.2", "27B52")
                        .replace("ProductBuildVersion", "Other")
                        .replace("CFBundleShortVersionString", "More")
                        .as_bytes(),
                );
            }),
            REASON_BUILD_MISMATCH,
            format!("{app} is Xcode <none> (<none>); the order names Xcode 27.2 (27B52)"),
        ),
        (
            "garbage",
            Box::new(|mac| mac.file(&format!("{app}/Contents/version.plist"), GARBAGE)),
            REASON_BUILD_MISMATCH,
            GARBAGE_DETAIL.replace("<app>", app),
        ),
        (
            "version-fails",
            Box::new(|mac| mac.answers(&xcodebuild("27.2", "-version"), exits(1, "dyld: oops"))),
            REASON_VERSION_FAILED,
            format!("{version} failed with exit code 1: dyld: oops"),
        ),
        (
            "version-other",
            Box::new(|mac| {
                mac.answers(
                    &xcodebuild("27.2", "-version"),
                    says_version("26.3", "26D10"),
                );
            }),
            REASON_VERSION_FAILED,
            format!("{app}/{XCODEBUILD} -version names no build 27B52"),
        ),
        (
            "licence",
            Box::new(|mac| {
                mac.answers(
                    &xcodebuild("27.2", "-license check"),
                    exits(69, "You have not agreed to the Xcode license agreements."),
                );
            }),
            REASON_LICENSE_NOT_ACCEPTED,
            format!(
                "{licence} failed with exit code 69: You have not agreed to the Xcode license \
                 agreements."
            ),
        ),
        (
            "select-fails",
            Box::new(|mac| mac.answers(select, exits(2, ""))),
            REASON_SELECT_FAILED,
            "xcode-select -p failed with exit code 2".to_owned(),
        ),
        (
            "select-other",
            Box::new(|mac| {
                mac.answers(
                    select,
                    says("/Applications/Xcode-26.3.app/Contents/Developer\n"),
                );
            }),
            REASON_DEFAULT_WRONG,
            wrong_default("/Applications/Xcode-26.3.app/Contents/Developer"),
        ),
        (
            "select-prefix",
            Box::new(|mac| {
                mac.answers(
                    select,
                    says("/Applications/Xcode-27.2.app.old/Contents/Developer\n"),
                );
            }),
            REASON_DEFAULT_WRONG,
            wrong_default("/Applications/Xcode-27.2.app.old/Contents/Developer"),
        ),
        (
            "select-nothing",
            Box::new(|mac| mac.answers(select, says(""))),
            REASON_DEFAULT_WRONG,
            wrong_default("<none>"),
        ),
    ];
    for (name, setup, reason, detail) in cases {
        let (mac, xcodes) = checked_mac(&format!("xcode-verify-wrong-{name}"));
        setup(&mac);
        assert_eq!(
            mac.apply().verify(&params(&xcodes, &[], "27.2")),
            wrong(reason, &detail),
            "{name}"
        );
    }
}

/// A run record in memory: each kind's state.
#[derive(Default)]
struct MemoryRecord(Mutex<BTreeMap<Kind, KindState>>);

impl RunRecord for MemoryRecord {
    fn state(&self, _run_id: &str, kind: Kind) -> Result<KindState, RecordError> {
        Ok(lock(&self.0)
            .get(&kind)
            .cloned()
            .unwrap_or(KindState::Pending))
    }

    fn reboot_under_way(
        &self,
        _run_id: &str,
        kind: Kind,
        boot: &BootId,
    ) -> Result<(), RecordError> {
        lock(&self.0).insert(kind, KindState::RebootUnderWay { boot: boot.clone() });
        Ok(())
    }

    fn applied(&self, _run_id: &str, kind: Kind, _version: &str) -> Result<(), RecordError> {
        lock(&self.0).insert(kind, KindState::Applied);
        Ok(())
    }
}

/// A reboot that is only logged, in the programs' log.
struct LoggedReboot(Arc<Mutex<Vec<String>>>);

impl Rebooter for LoggedReboot {
    fn reboot(&self) -> Result<(), RebootError> {
        lock(&self.0).push("reboot".to_owned());
        Ok(())
    }
}

/// A boot id the test sets.
struct SetBoot(Mutex<String>);

impl BootIdSource for SetBoot {
    fn read(&self) -> Result<BootId, BootIdError> {
        BootId::new("the test's boot id", &lock(&self.0))
    }
}

/// A `macos` kind whose cutover needs a reboot, logging each step in the programs' log.
struct RebootingMacos(Arc<Mutex<Vec<String>>>);

impl MaintenanceKind for RebootingMacos {
    fn prepare(&self, _params: &MaintainParams) -> Result<(), KindError> {
        Ok(())
    }

    fn cutover(&self, _params: &MaintainParams) -> Result<Cutover, KindError> {
        lock(&self.0).push("macos cutover".to_owned());
        Ok(Cutover::NeedsReboot)
    }

    fn after_reboot(&self, _params: &MaintainParams) -> Result<String, KindError> {
        lock(&self.0).push("macos after_reboot".to_owned());
        Ok("25C56".to_owned())
    }

    fn verify(&self, _params: &MaintainParams) -> Result<(), KindError> {
        lock(&self.0).push("macos verify".to_owned());
        Ok(())
    }
}

/// The `xcode` kind of `mac`: its apply over the Mac's fakes, and a stage over the host's own
/// adapters, which these tests never run.
fn xcode_kind(mac: &Mac) -> XcodeKind {
    let clock: Arc<dyn Clock> = FakeClock::new();
    let stage = Stage {
        store: Box::new(HostStore::new(
            &mac.path("/config.toml"),
            0,
            &mac.path("/pki"),
            Arc::clone(&clock),
        )),
        runner: Box::new(SystemRunner::new(Arc::clone(&clock))),
        space: Box::new(Statvfs::at(&mac.path("/Applications"))),
        plists: Box::new(HostPlists::new(HostRoot::at(mac.root.path()))),
        root: HostRoot::at(mac.root.path()),
        clock,
    };
    XcodeKind::new(stage, mac.apply())
}

/// The parameters of a run of `macos` and `xcode`, keeping 26.3 and 27.2, removing 25.4 and
/// selecting 27.2.
fn macos_and_xcode(xcodes: &[XcodeEntry]) -> MaintainParams {
    MaintainParams::new(Maintain {
        run_id: "run-7".to_owned(),
        kinds: vec![WireKind::Xcode.into(), WireKind::Macos.into()],
        xcodes: xcodes.to_vec(),
        xcode_remove: vec!["25.4".to_owned()],
        xcode_default: "27.2".to_owned(),
        ..Maintain::default()
    })
    .expect("valid parameters")
}

/// Proves: in a run of `macos` and `xcode`, the first cutover runs macOS's alone, which asks for
/// a reboot: the new Xcode stays staged, the old one installed, and no Xcode program runs. The
/// cutover of the next boot reads macOS's build and only then applies Xcode: the move, each
/// licence and first launch, the select and the removal; the check then passes both kinds.
///
/// Catches: an Xcode installed before the macOS it may need, or on the boot macOS's install is
/// about to restart; the Xcode cutover not run once macOS is back.
#[test]
fn with_macos_the_xcode_cutover_runs_after_the_reboot() {
    let (mac, xcodes) = usual_mac("xcode-hook-after-macos");
    let log = Arc::clone(&mac.programs.log);
    let kinds = Registry::new()
        .with(Kind::Macos, Box::new(RebootingMacos(Arc::clone(&log))))
        .with(Kind::Xcode, Box::new(xcode_kind(&mac)));
    let record = MemoryRecord::default();
    let rebooter = LoggedReboot(Arc::clone(&log));
    let boot = SetBoot(Mutex::new("boot-a".to_owned()));
    let ports = HookPorts {
        kinds: &kinds,
        record: &record,
        rebooter: &rebooter,
        boot_id: &boot,
    };
    let order = macos_and_xcode(&xcodes);
    assert_eq!(
        run_step(HookStep::Cutover, &order, &ports),
        Ending {
            outcome: Outcome::Success,
            reason: Some(REASON_REBOOT_UNDER_WAY),
            detail: Some("macos needs a reboot: rebooting".to_owned()),
        }
    );
    assert_eq!(mac.ran(), ["macos cutover", "reboot"]);
    assert_eq!(
        mac.apps(),
        [
            "/Applications/.hostkeeper-staging",
            "/Applications/Xcode-25.4.app",
            "/Applications/Xcode-26.3.app"
        ]
    );
    assert_eq!(mac.names(&entry_host_dir(&xcodes[1])), [STAGED_APP]);
    lock(&log).clear();
    *lock(&boot.0) = "boot-b".to_owned();
    assert_eq!(run_step(HookStep::Cutover, &order, &ports), done());
    let mut expected = vec!["macos after_reboot".to_owned()];
    expected.extend(licence_and_first_launch("26.3"));
    expected.extend(licence_and_first_launch("27.2"));
    expected.push("/usr/bin/xcode-select -s /Applications/Xcode-27.2.app".to_owned());
    assert_eq!(mac.ran(), expected);
    assert_eq!(
        mac.apps(),
        [
            "/Applications/.hostkeeper-staging",
            "/Applications/Xcode-26.3.app",
            "/Applications/Xcode-27.2.app"
        ]
    );
    lock(&log).clear();
    mac.answers(
        &xcodebuild("26.3", "-version"),
        says_version("26.3", "26D10"),
    );
    mac.answers(
        &xcodebuild("27.2", "-version"),
        says_version("27.2", "27B52"),
    );
    mac.answers("/usr/bin/xcode-select -p", says(SELECTED_27));
    assert_eq!(run_step(HookStep::Verify, &order, &ports), done());
    assert_eq!(mac.ran()[0], "macos verify");
    assert_eq!(mac.ran().len(), 6);
}

/// Proves: in the hook, an Xcode cutover whose first launch fails ends the step failed (exit
/// non-zero, its reason and detail in the report), leaves the kind unrecorded and removes nothing.
///
/// Catches: a failed first launch reported done, recorded applied, or followed by the removal.
#[test]
fn a_failing_first_launch_fails_the_hook_step_and_removes_nothing() {
    let (mac, xcodes) = usual_mac("xcode-hook-first-launch");
    let first_launch = xcodebuild("27.2", "-runFirstLaunch");
    mac.answers(&first_launch, exits(70, "error: packages failed"));
    let kinds = Registry::new().with(Kind::Xcode, Box::new(xcode_kind(&mac)));
    let record = MemoryRecord::default();
    let rebooter = LoggedReboot(Arc::clone(&mac.programs.log));
    let boot = SetBoot(Mutex::new("boot-a".to_owned()));
    let ports = HookPorts {
        kinds: &kinds,
        record: &record,
        rebooter: &rebooter,
        boot_id: &boot,
    };
    let order = params(&xcodes, &["25.4"], "27.2");
    let ended = run_step(HookStep::Cutover, &order, &ports);
    assert_eq!(
        ended,
        Ending {
            outcome: Outcome::Failure,
            reason: Some(REASON_FIRST_LAUNCH_FAILED),
            detail: Some(format!(
                "{first_launch} failed with exit code 70: error: packages failed"
            )),
        }
    );
    assert_eq!(ended.outcome.code(), 1);
    assert_eq!(record.state("run-7", Kind::Xcode), Ok(KindState::Pending));
    assert_eq!(
        mac.apps(),
        [
            "/Applications/.hostkeeper-staging",
            "/Applications/Xcode-25.4.app",
            "/Applications/Xcode-26.3.app",
            "/Applications/Xcode-27.2.app"
        ]
    );
}

fn done() -> Ending {
    Ending {
        outcome: Outcome::Success,
        reason: None,
        detail: None,
    }
}
