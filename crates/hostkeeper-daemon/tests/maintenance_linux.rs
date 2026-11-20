//! The `linux` and `reboot` kinds of the maintenance hook on a Linux host, from the hook's
//! `cutover` down to the programs: the real `apt` upgrader, the real reboot check and the real
//! kinds, over fakes of the programs and a scratch directory standing for the host's files. The
//! reboot is the test changing the boot id and the scratch host's kernel and running the same
//! `cutover` again.

mod support_maintenance;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use hostkeeper_daemon::maintenance_collect::HostRoot;
use hostkeeper_daemon::maintenance_hook::{
    HookStep, Kind, KindState, MAX_DETAIL, MaintainParams, Registry, RunRecord, run_step,
};
use hostkeeper_daemon::maintenance_host::HostFacts;
use hostkeeper_daemon::maintenance_linux::apt::{AptUpgrade, SCRIPTS_ENV};
use hostkeeper_daemon::maintenance_linux::kinds::{
    LinuxKind, RebootCheck, RebootKind, upgrader_under,
};
use hostkeeper_daemon::maintenance_own::OwnPackages;
use hostkeeper_daemon::maintenance_reboot::RebootNeed;
use hostkeeper_launchd::{FakeLaunchctl, LaunchctlOutput};
use hostkeeper_proto::hostkeeper::agent::v1::{
    LinuxScope as WireScope, Maintain, MaintainAction, MaintenanceKind as WireKind,
};
use hostkeeper_steps::BootId;
use hostkeeper_testkit::Scratch;
use support_maintenance::{
    BOOT_A, BOOT_B, Effects, Ending, FakeBootId, FakeRebooter, FakeRecord, Host, RUN, done, failed,
    rebooting,
};

/// The kernel the scratch host runs before its reboot.
const OLD_KERNEL: &str = "6.8.0-40-generic";
/// The kernel an install leaves on the scratch host.
const NEW_KERNEL: &str = "6.8.0-45-generic";
/// The lock wait the upgrader is built with.
const LOCK_WAIT: u64 = 300;
/// What `apt-get --simulate dist-upgrade` prints: one security update.
const SIMULATION: &str =
    "Inst libc6 [2.39-0ubuntu8.2] (2.39-0ubuntu8.3 Ubuntu:24.04/noble-security [amd64])\n";
/// The text a failing `uname` is replaced with, as the unscripted fake words it.
const UNSCRIPTED: &str = "could not be run: the fake has no answer";

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

fn put(root: &Path, path: &str, text: &str) {
    let file = root.join(path);
    fs::create_dir_all(file.parent().expect("a parent")).expect("the directory");
    fs::write(file, text).expect("the file");
}

fn kernel(root: &Path, release: &str) {
    fs::create_dir_all(root.join(format!("lib/modules/{release}/kernel"))).expect("a kernel");
}

/// The packages hostkeeper installs, as the test sets them.
struct Own(Result<BTreeSet<String>, String>);

impl OwnPackages for Own {
    fn names(&self) -> Result<BTreeSet<String>, String> {
        self.0.clone()
    }
}

fn own() -> Own {
    Own(Ok(BTreeSet::from(["hostkeeper".to_owned()])))
}

/// A scratch Linux host and the fakes of its programs.
struct Linux {
    root: Scratch,
    uname: FakeLaunchctl,
    needrestart: FakeLaunchctl,
    refresh: FakeLaunchctl,
    simulate: FakeLaunchctl,
    mark: FakeLaunchctl,
    install: FakeLaunchctl,
    host: Host,
}

impl Linux {
    /// A host running [`OLD_KERNEL`] with that kernel alone installed, whose `apt-get` refreshes
    /// and simulates [`SIMULATION`]; the install is scripted by each test.
    fn new(name: &str, own: Own, with_apt: bool) -> Linux {
        let root = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("scratch");
        put(
            root.path(),
            "etc/os-release",
            "ID=ubuntu\nVERSION_ID=\"24.04\"\n",
        );
        put(
            root.path(),
            "proc/sys/kernel/random/boot_id",
            "5f1d6a0e-33c4-4b9e-8d2a-0c7b9e1f4a21\n",
        );
        kernel(root.path(), OLD_KERNEL);
        let uname = FakeLaunchctl::new();
        uname.always(
            &args(&["-r"]),
            Ok(LaunchctlOutput::success(&format!("{OLD_KERNEL}\n"))),
        );
        let lock = format!("DPkg::Lock::Timeout={LOCK_WAIT}");
        let refresh = FakeLaunchctl::new();
        refresh.always(
            &args(&["-o", &lock, "update", "-qq"]),
            Ok(LaunchctlOutput::success("")),
        );
        let simulate = FakeLaunchctl::new();
        simulate.always(
            &args(&["-o", &lock, "--simulate", "-qq", "dist-upgrade"]),
            Ok(LaunchctlOutput::success(SIMULATION)),
        );
        let (mark, install, needrestart) = (
            FakeLaunchctl::new(),
            FakeLaunchctl::new(),
            FakeLaunchctl::new(),
        );
        let effects = Effects::default();
        let upgrader = with_apt.then(|| {
            Box::new(AptUpgrade::new(
                Box::new(refresh.clone()),
                Box::new(simulate.clone()),
                Box::new(mark.clone()),
                Box::new(install.clone()),
                LOCK_WAIT,
            )) as Box<dyn hostkeeper_daemon::maintenance_linux::Upgrader>
        });
        let linux = LinuxKind::new(
            upgrader,
            Box::new(own),
            check(root.path(), &uname, &needrestart),
        );
        let reboot = RebootKind::new(check(root.path(), &uname, &needrestart));
        let kinds = Registry::new()
            .with(Kind::Linux, Box::new(linux))
            .with(Kind::Reboot, Box::new(reboot));
        let host = Host {
            effects: effects.clone(),
            kinds,
            record: FakeRecord::new(&effects),
            rebooter: FakeRebooter {
                effects,
                fail: false,
            },
            boot: FakeBootId(Mutex::new(BOOT_A.to_owned())),
        };
        Linux {
            root,
            uname,
            needrestart,
            refresh,
            simulate,
            mark,
            install,
            host,
        }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    /// The install `apt-get` is asked for, after its common arguments.
    fn script_install(&self, rest: &[&str], answer: LaunchctlOutput) {
        let lock = format!("DPkg::Lock::Timeout={LOCK_WAIT}");
        let mut full: Vec<&str> = SCRIPTS_ENV.to_vec();
        full.extend([
            "/usr/bin/apt-get",
            "-o",
            &lock,
            "-o",
            "Dpkg::Options::=--force-confdef",
            "-o",
            "Dpkg::Options::=--force-confold",
            "-qq",
            "-y",
        ]);
        full.extend(rest);
        self.install.always(&args(&full), Ok(answer));
    }

    /// The install of the security scope: libc6, the one security update of the simulation.
    fn script_security_install(&self) {
        self.script_install(
            &["install", "--only-upgrade", "libc6"],
            LaunchctlOutput::success(""),
        );
    }

    /// The host after a reboot into [`NEW_KERNEL`]: the new boot id, the new kernel running and
    /// installed, no reboot asked for.
    fn reboot_into_new_kernel(&self) {
        let _ = fs::remove_file(self.path().join("run/reboot-required"));
        kernel(self.path(), NEW_KERNEL);
        self.uname.always(
            &args(&["-r"]),
            Ok(LaunchctlOutput::success(&format!("{NEW_KERNEL}\n"))),
        );
        self.host.boot.set(BOOT_B);
    }

    /// Every call the four package programs got, in one list.
    fn package_calls(&self) -> usize {
        [&self.refresh, &self.simulate, &self.mark, &self.install]
            .iter()
            .map(|fake| fake.calls().len())
            .sum()
    }

    /// The hook's `cutover` of `kinds` with `scope`.
    fn cutover(&self, kinds: &[WireKind], scope: WireScope) -> Ending {
        let params = MaintainParams::new(Maintain {
            run_id: RUN.to_owned(),
            action: MaintainAction::Run.into(),
            kinds: kinds.iter().map(|kind| i32::from(*kind)).collect(),
            linux_scope: scope.into(),
            ..Maintain::default()
        })
        .expect("valid parameters");
        let ending = run_step(HookStep::Cutover, &params, &self.host.ports());
        Ending {
            line: ending.report().to_line(),
            outcome: ending.outcome,
        }
    }

    fn security(&self) -> Ending {
        self.cutover(&[WireKind::Linux], WireScope::Security)
    }
}

/// The reboot check over the scratch host `root`, `uname` and `needrestart`.
fn check(root: &Path, uname: &FakeLaunchctl, needrestart: &FakeLaunchctl) -> RebootCheck {
    RebootCheck::new(
        HostFacts::new(Box::new(uname.clone()), HostRoot::at(root)),
        RebootNeed::new(
            Box::new(needrestart.clone()),
            Box::new(FakeLaunchctl::new()),
            HostRoot::at(root),
        ),
    )
}

/// Proves: a run whose install leaves `/run/reboot-required` records the reboot under way with
/// the boot's id before it asks for the reboot, and ends done `reboot_under_way`; `apt-get`
/// refreshed, simulated and installed once. In the next boot the same `cutover` records `linux`
/// applied at the new kernel, installs nothing again and does not reboot again.
///
/// Catches: a reboot asked for before the record is durable; an install repeated in the boot
/// after the reboot; a reboot loop.
#[test]
fn a_linux_run_that_needs_a_reboot_records_it_first_and_applies_after_it() {
    let host = Linux::new("linux-reboot", own(), true);
    host.script_security_install();
    put(
        host.path(),
        "run/reboot-required",
        "*** System restart required ***\n",
    );
    assert_eq!(host.security(), rebooting("linux"));
    assert_eq!(
        host.host.effects(),
        [
            format!("record run-7 linux reboot_under_way {BOOT_A}"),
            "reboot".to_owned(),
        ]
    );
    assert_eq!(
        (host.refresh.calls().len(), host.simulate.calls().len()),
        (1, 1)
    );
    assert_eq!(host.install.calls().len(), 1);
    assert!(host.mark.calls().is_empty());

    host.reboot_into_new_kernel();
    assert_eq!(host.security(), done());
    assert_eq!(
        host.host.effects(),
        [format!("record run-7 linux applied {NEW_KERNEL}")]
    );
    assert_eq!(host.package_calls(), 3);

    assert_eq!(host.security(), done());
    assert_eq!(host.host.effects(), Vec::<String>::new());
    assert_eq!(host.package_calls(), 3);
}

/// Proves: a newer kernel under `/lib/modules` than the one `uname` names makes the install need
/// a reboot, as `/run/reboot-required` does.
///
/// Catches: a new kernel installed and the host left running the old one with the run applied.
#[test]
fn a_newer_installed_kernel_needs_a_reboot() {
    let host = Linux::new("linux-newer-kernel", own(), true);
    host.script_security_install();
    kernel(host.path(), NEW_KERNEL);
    assert_eq!(host.security(), rebooting("linux"));
    assert_eq!(
        host.host.effects(),
        [
            format!("record run-7 linux reboot_under_way {BOOT_A}"),
            "reboot".to_owned(),
        ]
    );
}

/// Proves: a `reboot`-only run runs no package program at all, records the reboot under way
/// before it asks for it, and in the next boot records `reboot` applied at the kernel the host
/// runs.
///
/// Catches: a reboot-only run that installs updates, or that reboots without a record.
#[test]
fn a_reboot_only_run_installs_nothing_and_reboots_once() {
    let host = Linux::new("linux-reboot-only", own(), true);
    assert_eq!(
        host.cutover(&[WireKind::Reboot], WireScope::Unspecified),
        rebooting("reboot")
    );
    assert_eq!(
        host.host.effects(),
        [
            format!("record run-7 reboot reboot_under_way {BOOT_A}"),
            "reboot".to_owned(),
        ]
    );
    assert_eq!(host.package_calls(), 0);

    host.host.boot.set(BOOT_B);
    assert_eq!(
        host.cutover(&[WireKind::Reboot], WireScope::Unspecified),
        done()
    );
    assert_eq!(
        host.host.effects(),
        [format!("record run-7 reboot applied {OLD_KERNEL}")]
    );
    assert_eq!(host.package_calls(), 0);
}

/// Proves: a run of `linux` and `reboot` reboots once for both: after the reboot `linux` is
/// applied at the new kernel and `reboot` is recorded applied without a second reboot.
///
/// Catches: a second reboot for the `reboot` kind of a run whose `linux` kind just rebooted.
#[test]
fn a_linux_and_reboot_run_reboots_once() {
    let host = Linux::new("linux-and-reboot", own(), true);
    host.script_security_install();
    kernel(host.path(), NEW_KERNEL);
    let kinds = [WireKind::Linux, WireKind::Reboot];
    assert_eq!(
        host.cutover(&kinds, WireScope::Security),
        rebooting("linux")
    );
    host.host.effects();
    host.reboot_into_new_kernel();
    assert_eq!(host.cutover(&kinds, WireScope::Security), done());
    assert_eq!(
        host.host.effects(),
        [
            format!("record run-7 linux applied {NEW_KERNEL}"),
            format!("record run-7 reboot applied {BOOT_B}"),
        ]
    );
}

/// Proves: a run that leaves nothing to reboot for is applied at once at the kernel the host
/// runs, with no reboot asked for.
#[test]
fn a_linux_run_that_needs_no_reboot_is_applied_at_the_running_kernel() {
    let host = Linux::new("linux-no-reboot", own(), true);
    host.script_security_install();
    assert_eq!(host.security(), done());
    assert_eq!(
        host.host.effects(),
        [format!("record run-7 linux applied {OLD_KERNEL}")]
    );
    assert_eq!(host.install.calls().len(), 1);
}

/// Proves: the `all` scope runs `dist-upgrade` through the install program.
#[test]
fn the_all_scope_installs_with_dist_upgrade() {
    let host = Linux::new("linux-all", own(), true);
    host.script_install(&["dist-upgrade"], LaunchctlOutput::success(""));
    assert_eq!(host.cutover(&[WireKind::Linux], WireScope::All), done());
    assert_eq!(host.install.calls().len(), 1);
}

/// Proves: a failed install ends the step failed `install_failed`, the detail naming the step,
/// its exit and the first line of its standard error, and records nothing and reboots nothing,
/// though the host asks for a reboot.
///
/// Catches: a half-installed host recorded applied, or rebooted over a failed install.
#[test]
fn a_failed_install_is_reported_and_neither_recorded_nor_rebooted() {
    let host = Linux::new("linux-failed", own(), true);
    host.script_install(
        &["install", "--only-upgrade", "libc6"],
        LaunchctlOutput::exit(
            100,
            "E: Sub-process /usr/bin/dpkg returned an error code (1)\nmore",
        ),
    );
    put(host.path(), "run/reboot-required", "");
    assert_eq!(
        host.security(),
        failed(
            "install_failed",
            "apt-get install failed with exit code 100: E: Sub-process /usr/bin/dpkg returned an error code (1)"
        )
    );
    assert_eq!(host.host.effects(), Vec::<String>::new());
}

/// Proves: the detail of a failed install whose first line of standard error is longer than the
/// report allows is cut to `MAX_DETAIL` bytes of the detail.
///
/// Catches: a report line too long for the run record's reason.
#[test]
fn a_long_failure_detail_is_cut_to_the_bound() {
    let host = Linux::new("linux-long", own(), true);
    let stderr = "x".repeat(MAX_DETAIL * 2);
    host.script_install(
        &["install", "--only-upgrade", "libc6"],
        LaunchctlOutput::exit(100, &stderr),
    );
    let params = MaintainParams::new(Maintain {
        run_id: RUN.to_owned(),
        kinds: vec![i32::from(WireKind::Linux)],
        linux_scope: WireScope::Security.into(),
        ..Maintain::default()
    })
    .expect("valid parameters");
    let ending = run_step(HookStep::Cutover, &params, &host.host.ports());
    let full = format!("apt-get install failed with exit code 100: {stderr}");
    let report = ending.report();
    assert_eq!(report.reason.as_deref(), Some("install_failed"));
    assert_eq!(report.detail.as_deref(), Some(&full[..MAX_DETAIL]));
    assert_eq!(host.host.effects(), Vec::<String>::new());
}

/// Proves: after the reboot, a newer kernel still installed than the one running fails the step
/// `new_kernel_not_running`, naming both, and the kind is not recorded applied.
///
/// Catches: a run recorded applied on a host that came back on its old kernel.
#[test]
fn a_new_kernel_that_does_not_run_after_the_reboot_fails_the_run() {
    let host = Linux::new("linux-not-running", own(), true);
    kernel(host.path(), NEW_KERNEL);
    host.host.record.set(
        Kind::Linux,
        KindState::RebootUnderWay {
            boot: BootId::uuid("test", BOOT_A).expect("uuid"),
        },
    );
    host.host.boot.set(BOOT_B);
    assert_eq!(
        host.security(),
        failed(
            "new_kernel_not_running",
            &format!("the kernel {OLD_KERNEL} runs; {NEW_KERNEL} is installed")
        )
    );
    assert_eq!(host.host.effects(), Vec::<String>::new());
    assert_eq!(
        host.host.record.state(RUN, Kind::Linux),
        Ok(KindState::RebootUnderWay {
            boot: BootId::uuid("test", BOOT_A).expect("uuid"),
        })
    );
    assert_eq!(host.package_calls(), 0);
}

/// Proves: a run with no scope, or one this release does not know, fails `linux_scope_invalid`
/// and runs no program.
///
/// Catches: a run with no scope installing everything.
#[test]
fn a_scope_that_is_not_security_or_all_runs_nothing() {
    let host = Linux::new("linux-scope", own(), true);
    let expected = failed("linux_scope_invalid", "linux_scope is not security or all");
    assert_eq!(
        host.cutover(&[WireKind::Linux], WireScope::Unspecified),
        expected
    );
    let params = MaintainParams::new(Maintain {
        run_id: RUN.to_owned(),
        kinds: vec![i32::from(WireKind::Linux)],
        linux_scope: 99,
        ..Maintain::default()
    })
    .expect("valid parameters");
    let ending = run_step(HookStep::Cutover, &params, &host.host.ports());
    assert_eq!(ending.reason, Some("linux_scope_invalid"));
    assert_eq!(host.package_calls(), 0);
    assert_eq!(host.host.effects(), Vec::<String>::new());
}

/// Proves: a host with no package manager fails `no_package_manager`, and one whose own packages
/// cannot be read fails `own_packages_unreadable`, each before a program runs.
///
/// Catches: an install run without knowing which packages must not move.
#[test]
fn a_host_without_a_package_manager_or_its_own_packages_runs_nothing() {
    let host = Linux::new("linux-no-manager", own(), false);
    assert_eq!(
        host.security(),
        failed(
            "no_package_manager",
            "the host has none of apt-get, dnf and apk"
        )
    );
    let host = Linux::new(
        "linux-no-own",
        Own(Err("placed.json is not readable".to_owned())),
        true,
    );
    assert_eq!(
        host.security(),
        failed(
            "own_packages_unreadable",
            "own packages: placed.json is not readable"
        )
    );
    assert_eq!(host.package_calls(), 0);
}

/// Proves: an install after which the host cannot be asked about its reboot fails
/// `reboot_check_failed` naming the collector (the kernel's `uname`, then `needrestart`), and the
/// run is neither recorded nor rebooted; a `uname` that fails after the reboot fails
/// `kernel_unreadable`.
///
/// Catches: a run recorded applied on a host whose state is unknown.
#[test]
fn a_host_that_cannot_be_asked_about_its_reboot_fails_the_run() {
    let host = Linux::new("linux-check-failed", own(), true);
    host.script_security_install();
    put(host.path(), "usr/sbin/needrestart", "");
    assert_eq!(
        host.security(),
        failed(
            "reboot_check_failed",
            &format!("reboot: needrestart -b {UNSCRIPTED}")
        )
    );
    assert_eq!(host.needrestart.calls(), vec![args(&["-b"])]);
    assert_eq!(host.host.effects(), Vec::<String>::new());

    let host = Linux::new("linux-uname-failed", own(), true);
    host.script_security_install();
    host.uname.always(
        &args(&["-r"]),
        Err(hostkeeper_launchd::LaunchctlError::Spawn {
            args: "-r".to_owned(),
            why: "gone".to_owned(),
        }),
    );
    assert_eq!(
        host.security(),
        failed(
            "reboot_check_failed",
            "host: uname -r could not be run: gone"
        )
    );
    host.host.record.set(
        Kind::Linux,
        KindState::RebootUnderWay {
            boot: BootId::uuid("test", BOOT_A).expect("uuid"),
        },
    );
    host.host.boot.set(BOOT_B);
    assert_eq!(
        host.security(),
        failed("kernel_unreadable", "host: uname -r could not be run: gone")
    );
}

/// Proves: the package manager of a host is `apt-get` when it has it, else `dnf`, else `apk`,
/// and none when it has none of them, by the files under the host's root.
///
/// Catches: a host with `dnf` and `apk` both installed upgraded by the wrong one.
#[test]
fn the_package_manager_is_picked_apt_then_dnf_then_apk() {
    let scratch =
        Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "linux-pick").expect("scratch");
    let root = HostRoot::at(scratch.path());
    let name = || upgrader_under(&root, LOCK_WAIT).map(|upgrader| upgrader.name());
    assert_eq!(name(), None);
    put(scratch.path(), "sbin/apk", "");
    assert_eq!(name(), Some("apk"));
    put(scratch.path(), "usr/bin/dnf", "");
    assert_eq!(name(), Some("dnf"));
    put(scratch.path(), "usr/bin/apt-get", "");
    assert_eq!(name(), Some("apt"));
}
