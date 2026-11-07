//! The switch around a hooked step is tried again when it fails: `current` and the placed links
//! are switched back before `rollback` (and to the target before `cutover`) by the daemon, and
//! that switch can meet a service manager that refuses one call for a moment. The update keeps
//! its one run, and a resend of the order while it runs is answered `Running`, not run again.
//!
//! The first test runs the daemon on the launchd unit control over a scripted `launchctl`, with
//! the plists linked on the real filesystem under the scratch directory, so the control's
//! snapshot of what each link resolves to is the one a Mac takes.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_daemon::testing::spawn;
use hostkeeper_daemon::{Daemon, Placement, Ports, Refusal, Reply};
use hostkeeper_hook::{Class, Phase, Step, Verdict};
use hostkeeper_launchd::launchctl::{bootout_args, bootstrap_args, print_args};
use hostkeeper_launchd::{FakeLaunchctl, Label, LaunchctlOutput, LaunchdUnitControl};
use hostkeeper_placed::Roots;
use hostkeeper_placed::testing::{FaultFs, Log as PlacedLog};
use support::{OWN, Rig, alerting, config, d, install, lines, update};

const WEB: &str = "web";
const LABEL: &str = "web.a";

/// `web`'s package at `version`: one plist, `units/web.a.plist`, whose program runs through
/// `current`, sealed with the package digest [`d`].
fn plist_package(rig: &Rig, version: &str) {
    let dir = rig.store.add(
        WEB,
        version,
        &hostkeeper_daemon::testing::manifest(WEB, version),
    );
    let program = rig.packages.join(WEB).join("current").join("bin").join(WEB);
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
         \t<key>Label</key>\n\t<string>{LABEL}</string>\n\
         \t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>{}</string>\n\t</array>\n\
         \t<key>StandardErrorPath</key>\n\t<string>/var/log/{version}/{LABEL}.err.log</string>\n\
         </dict>\n</plist>\n",
        program.display()
    );
    fs::create_dir_all(dir.join("units")).expect("the units directory of the package");
    fs::write(dir.join("units").join(format!("{LABEL}.plist")), plist).expect("the plist");
    rig.store.seal(WEB, version, &d());
}

/// The rig's unit directory, as the real path the launchd control reads.
fn unit_dir(rig: &Rig) -> PathBuf {
    rig.host("/units")
}

/// A daemon of `rig` whose units are the launchd control over `launchctl`, and whose placement
/// writes the real paths under the scratch directory: links in [`unit_dir`] resolve through the
/// rig's packages directory, as `/Library/LaunchDaemons` resolves through `current` on a Mac.
fn launchd_daemon(rig: &Rig, launchctl: &FakeLaunchctl) -> Daemon {
    let units = unit_dir(rig);
    fs::create_dir_all(&units).expect("the unit directory");
    let control =
        LaunchdUnitControl::new(launchctl.clone(), &units).expect("the launchd unit control");
    let ports = Ports {
        units: Arc::new(control),
        placement: Placement {
            roots: Roots::new(&rig.host("/placed"), &units, &rig.packages),
            fs: Arc::new(FaultFs::new(Path::new("/"), PlacedLog::new())),
        },
        ..rig.ports()
    };
    Daemon::new(config(), rig.dirs(), ports, OWN)
}

fn label() -> Label {
    Label::parse(LABEL).expect("a label")
}

/// `launchctl` with `web.a` loaded and running on every print, and every bootout and every
/// bootstrap of its link working.
fn launchd(rig: &Rig) -> FakeLaunchctl {
    let launchctl = FakeLaunchctl::new();
    let printed = format!("system/{LABEL} = {{\n\tstate = running\n\tpid = 7\n}}\n");
    launchctl.always(
        &print_args(&label()),
        Ok(LaunchctlOutput::success(&printed)),
    );
    launchctl.always(&bootout_args(&label()), Ok(LaunchctlOutput::success("")));
    let link = unit_dir(rig).join(format!("{LABEL}.plist"));
    launchctl.always(&bootstrap_args(&link), Ok(LaunchctlOutput::success("")));
    launchctl
}

fn link_of(rig: &Rig, name: &str) -> Option<String> {
    fs::read_link(rig.packages.join(WEB).join(name))
        .ok()
        .map(|target| target.to_string_lossy().into_owned())
}

fn failed_in_verify() -> Reply {
    Reply::Finished {
        verdict: Verdict::Failed {
            phase: Phase::GateRelease,
            class: Class::Release,
        },
        reason: None,
        resent: false,
    }
}

/// Catches a switch back before `rollback` that one refusal of `launchctl` ends: an update of
/// `web` from 1.0.0 to 2.0.0 whose `verify` fails meets, in the switch back, a `launchctl
/// bootout` of `web.a` that exits 5 with the job still loaded (launchd refusing a job it is
/// spawning that instant). The switch is tried again, the bootout works, `web.a` is bootstrapped
/// from its link (which resolves to 1.0.0's plist again), `rollback` runs, and the update ends
/// with `verify`'s failure, not `link_failed`. A resend of the same order (same try) while
/// `rollback` runs is answered `Running`: every step ran once. The retry is logged with what
/// failed, and nothing else is.
#[test]
fn a_refused_bootout_in_the_switch_back_is_tried_again_and_the_rollback_runs_once() {
    let rig = Rig::new("switch-retry-launchd");
    plist_package(&rig, "1.0.0");
    plist_package(&rig, "2.0.0");
    let launchctl = launchd(&rig);
    let (daemon, alerts) = alerting(launchd_daemon(&rig, &launchctl));
    let daemon = Arc::new(daemon);
    assert_eq!(
        daemon.order(&install(WEB, "1.0.0", &d(), 1)),
        Reply::Finished {
            verdict: Verdict::Succeeded {
                promotion_pending: false
            },
            reason: None,
            resent: false,
        }
    );

    rig.launcher.exits(WEB, Step::Verify, &[1]);
    let verify = rig.launcher.hold(WEB, Step::Verify);
    let rollback = rig.launcher.hold(WEB, Step::Rollback);
    let first = {
        let daemon = Arc::clone(&daemon);
        spawn("the update", move || {
            daemon.order(&update(WEB, "2.0.0", &d(), 2))
        })
    };
    verify.entered(&first).expect("verify runs");
    assert_eq!(link_of(&rig, "current").as_deref(), Some("2.0.0"));
    let before_back = launchctl.calls().len();
    let refused = LaunchctlOutput {
        code: Some(5),
        stdout: Vec::new(),
        stderr: b"Boot-out failed: 5: Input/output error\n".to_vec(),
    };
    launchctl.then(&bootout_args(&label()), Ok(refused));
    verify.release();

    rollback
        .entered(&first)
        .expect("rollback runs after the switch back");
    assert_eq!(link_of(&rig, "current").as_deref(), Some("1.0.0"));
    assert_eq!(daemon.order(&update(WEB, "2.0.0", &d(), 2)), Reply::Running);
    rollback.release();
    assert_eq!(first.join().expect("the update"), failed_in_verify());

    assert_eq!(
        rig.launcher.steps(WEB),
        [
            // The install of 1.0.0.
            Step::Prepare,
            Step::Cutover,
            Step::Verify,
            Step::Cleanup,
            // The update to 2.0.0, once.
            Step::Prepare,
            Step::Cutover,
            Step::Verify,
            Step::Rollback,
        ]
    );
    let (print, bootout) = (print_args(&label()), bootout_args(&label()));
    let link = unit_dir(&rig).join(format!("{LABEL}.plist"));
    let bootstrap = bootstrap_args(&link);
    assert_eq!(
        launchctl.calls()[before_back..],
        [
            // The first try: the job is loaded, its bootout is refused and it is still loaded.
            print.clone(),
            print.clone(),
            bootout.clone(),
            print.clone(),
            // The second try boots it out and loads it from its link again.
            print.clone(),
            print,
            bootout,
            bootstrap,
        ]
    );
    let lines = lines(&alerts);
    assert_eq!(
        lines,
        [
            "hostkeeper daemon: level=warn alert=switch_retried service=web: the switch Back \
             failed (try 1 of 5), tried again in 2s: launchctl bootout system/web.a failed: \
             launchctl bootout exited with Some(5): Boot-out failed: 5: Input/output error\n"
        ],
        "{lines:#?}"
    );
}

/// Catches a last try that is not made: the switch back meets four refused bootouts of `web.a`
/// in a row and works on its fifth and last try, 8 seconds of the daemon's clock later, so
/// `rollback` runs and the update ends with `verify`'s failure; the four failed tries are logged,
/// and the update is not.
#[test]
fn a_switch_back_refused_on_four_tries_works_on_its_fifth() {
    let rig = Rig::new("switch-retry-last");
    plist_package(&rig, "1.0.0");
    plist_package(&rig, "2.0.0");
    let launchctl = launchd(&rig);
    let (daemon, alerts) = alerting(launchd_daemon(&rig, &launchctl));
    let daemon = Arc::new(daemon);
    assert!(matches!(
        daemon.order(&install(WEB, "1.0.0", &d(), 1)),
        Reply::Finished {
            verdict: Verdict::Succeeded { .. },
            ..
        }
    ));
    rig.launcher.exits(WEB, Step::Verify, &[1]);
    let verify = rig.launcher.hold(WEB, Step::Verify);
    let first = {
        let daemon = Arc::clone(&daemon);
        spawn("the update", move || {
            daemon.order(&update(WEB, "2.0.0", &d(), 2))
        })
    };
    verify.entered(&first).expect("verify runs");
    for _ in 0..4 {
        launchctl.then(
            &bootout_args(&label()),
            Ok(LaunchctlOutput {
                code: Some(5),
                stdout: Vec::new(),
                stderr: b"Boot-out failed: 5: Input/output error".to_vec(),
            }),
        );
    }
    let at = hostkeeper_steps::Clock::mono_secs(&*rig.clock);
    verify.release();

    assert_eq!(first.join().expect("the update"), failed_in_verify());
    assert_eq!(hostkeeper_steps::Clock::mono_secs(&*rig.clock) - at, 8);
    assert_eq!(link_of(&rig, "current").as_deref(), Some("1.0.0"));
    assert_eq!(rig.launcher.steps(WEB).last(), Some(&Step::Rollback));
    let expected: Vec<String> = (1..5)
        .map(|attempt| {
            format!(
                "hostkeeper daemon: level=warn alert=switch_retried service=web: the switch Back \
                 failed (try {attempt} of 5), tried again in 2s: launchctl bootout system/web.a \
                 failed: launchctl bootout exited with Some(5): Boot-out failed: 5: Input/output \
                 error"
            )
        })
        .collect();
    assert_eq!(lines(&alerts), expected);
}

/// Catches a switch tried without end, or one whose failure is not told: a `daemon-reload` that
/// keeps failing in the switch back is tried five times, two seconds of the daemon's clock apart,
/// each failure but the last logged, and then stops the update `link_failed` with the step
/// record still at `rollback`, not run, and the last failure logged as `order_interrupted`.
#[test]
fn a_switch_back_that_keeps_failing_is_tried_five_times_then_stops_the_update_link_failed() {
    let rig = Rig::new("switch-retry-exhausted");
    rig.add_units(WEB, "1.0.0", &["web.service"], &["web.service"], &[]);
    rig.add_units(WEB, "2.0.0", &["web.service"], &["web.service"], &[]);
    let (daemon, alerts) = alerting(rig.daemon());
    assert!(matches!(
        daemon.order(&install(WEB, "1.0.0", &d(), 1)),
        Reply::Finished {
            verdict: Verdict::Succeeded { .. },
            ..
        }
    ));
    rig.launcher.exits(WEB, Step::Verify, &[1]);
    let verify = rig.launcher.hold(WEB, Step::Verify);
    let daemon = Arc::new(daemon);
    let first = {
        let daemon = Arc::clone(&daemon);
        spawn("the update", move || {
            daemon.order(&update(WEB, "2.0.0", &d(), 2))
        })
    };
    verify.entered(&first).expect("verify runs");
    rig.units.fail_reload();
    let at = hostkeeper_steps::Clock::mono_secs(&*rig.clock);
    verify.release();

    let reply = first.join().expect("the update");
    let why = "DaemonReload failed: the fake fails this call";
    let stopped = "the switch before the step failed; nothing ran and the step is still pending";
    assert_eq!(
        reply,
        Reply::Interrupted {
            refusal: Refusal::LinkFailed,
            detail: format!("{stopped}: {why}"),
        }
    );
    assert_eq!(hostkeeper_steps::Clock::mono_secs(&*rig.clock) - at, 8);
    let record = rig.record(WEB).expect("the step record");
    assert_eq!(record.step, Step::Rollback);
    assert!(!rig.launcher.steps(WEB).contains(&Step::Rollback));
    let retried = (1..5).map(|attempt| {
        format!(
            "hostkeeper daemon: level=warn alert=switch_retried service=web: the switch Back \
             failed (try {attempt} of 5), tried again in 2s: {why}"
        )
    });
    let interrupted = format!(
        "hostkeeper daemon: level=error alert=order_interrupted service=web: link_failed: \
         {stopped}: {why}"
    );
    assert_eq!(
        lines(&alerts),
        retried.chain([interrupted]).collect::<Vec<_>>()
    );
}
