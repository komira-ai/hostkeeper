//! A release of hostkeeper judged by its floor (docs/design/two-programs.md, sections 4.2 and
//! 4.4): the floor rule ([`judge`]) against the daemon's [`OLDEST_READER`] and
//! [`LEGACY_FLOOR`], the probe kept for a floor at or below the legacy floor, and the built-in
//! steps of a release whose floor is above it.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::selfupdate::{
    Judgement, LEGACY_FLOOR, REASON_PROTOCOL_UNSUPPORTED, RELEASE_BINARY, SELF_UPDATE_PROTOCOL,
    binary_dir, builtin_steps, judge, protocol_line,
};
use hostkeeper_daemon::testing::manifest;
use hostkeeper_daemon::{Daemon, Refusal, Reply};
use hostkeeper_hook::{Class, Step, Verdict};
use hostkeeper_steps::record::State;
use hostkeeper_steps::{CrashPoint, OLDEST_READER};
use support::selfupdate::{HK, SelfRig, awaiting, own, refused_before_commit};
use support::{crash_at, snapshot};

/// The version of the daemon under test: above both floors.
const DAEMON: &str = "0.1.60";
/// The version it is ordered to.
const TARGET: &str = "0.1.70";
/// A floor above the legacy one.
const ABOVE: &str = "0.1.54";

fn v(text: &str) -> HostkeeperVersion {
    own(text)
}

/// A host that runs hostkeeper's daemon of [`DAEMON`] (its binary directory's `current`), and its
/// daemon.
struct Host {
    s: SelfRig,
}

impl Host {
    fn new(name: &str) -> Host {
        let s = SelfRig::new(name);
        let source = s.rig.scratch.path().join("daemon-binary");
        fs::write(&source, format!("#!hostkeeper {DAEMON}\n")).expect("the daemon's binary");
        s.layout().stage(DAEMON, &source).expect("stage it");
        s.layout().switch(DAEMON).expect("switch to it");
        Host { s }
    }

    /// The release of hostkeeper at `version` whose package declares the floor `floor`, with its
    /// binary, in the package store.
    fn release(&self, version: &str, floor: &str) {
        let text = manifest(HK, version).replace(
            "min_hostkeeper = \"0.1.0\"",
            &format!("min_hostkeeper = \"{floor}\""),
        );
        assert!(text.contains(&format!("min_hostkeeper = \"{floor}\"")));
        let dir = self.s.rig.store.add(HK, version, &text);
        let binary = dir.join(RELEASE_BINARY);
        fs::write(&binary, format!("#!hostkeeper {version}\n")).expect("the binary");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).expect("its mode");
    }

    fn daemon(&self) -> Daemon {
        self.s.daemon_at(DAEMON)
    }

    fn order(&self, version: &str) -> Reply {
        self.daemon().order(&self.s.order(version, 1))
    }
}

/// Catches a floor rule that probes what it should judge, judges what it should probe, or lets
/// through a release older than this daemon's oldest reader: the version that runs is accepted
/// with no check whatever its floor; a floor of the legacy floor or below is probed, whatever the
/// target; above it, the oldest reader and anything newer is accepted and anything older, or a
/// version that is not `major.minor.patch`, is refused. Both constants are 0.1.53 in stage 1, so
/// every daemon of today updates to it, and the probe verb still prints 1.
#[test]
fn the_floor_rule_judges_forward_backward_equal_and_at_the_legacy_floor() {
    assert_eq!(LEGACY_FLOOR, v("0.1.53"));
    assert_eq!(OLDEST_READER, v("0.1.53"));
    assert_eq!(SELF_UPDATE_PROTOCOL, 1);
    assert_eq!(protocol_line(), "1\n");
    let running = v(DAEMON);
    for (version, floor, judged) in [
        // The version that runs: neither check.
        (DAEMON, "0.1.0", Judgement::Accepted),
        (DAEMON, ABOVE, Judgement::Accepted),
        // At or below the legacy floor: the probe, whatever the target.
        (TARGET, "0.1.0", Judgement::Probe),
        (TARGET, "0.1.53", Judgement::Probe),
        ("0.1.52", "0.1.53", Judgement::Probe),
        ("1.0.0-rc1", "0.1.53", Judgement::Probe),
        // Above it: the backward rule, with no probe.
        (TARGET, ABOVE, Judgement::Accepted),
        ("0.1.53", ABOVE, Judgement::Accepted),
        ("0.1.52", ABOVE, Judgement::Unsupported),
        ("0.0.900", ABOVE, Judgement::Unsupported),
        ("1.0.0-rc1", ABOVE, Judgement::Unsupported),
        ("0.1", ABOVE, Judgement::Unsupported),
        ("2.0.0", "1.0.0", Judgement::Accepted),
    ] {
        assert_eq!(
            judge(running, version, v(floor)),
            judged,
            "{version} with floor {floor}"
        );
    }
    let hostkeeper = ServiceName::new(HK).expect("a name");
    let other = ServiceName::new("web").expect("a name");
    assert!(!builtin_steps(&hostkeeper, v("0.1.0")));
    assert!(!builtin_steps(&hostkeeper, LEGACY_FLOOR));
    assert!(builtin_steps(&hostkeeper, v(ABOVE)));
    assert!(!builtin_steps(&other, v(ABOVE)));
}

/// Catches the forward rule lost: a release whose floor is above this daemon is deferred
/// `daemon_too_old` by the manifest's loader, before any probe or step, and nothing changes.
#[test]
fn a_floor_above_the_daemon_is_deferred_before_any_probe() {
    let host = Host::new("floor-forward");
    host.release(TARGET, "0.1.61");
    let bin = binary_dir(&host.s.rig.state);
    let before = snapshot(&bin);
    let reply = host.order(TARGET);
    assert_eq!(reply.refusal(), Some(Refusal::DaemonTooOld), "{reply:?}");
    assert!(host.s.probe.asked().is_empty());
    assert!(host.s.hk_steps().is_empty());
    assert_eq!(snapshot(&bin), before);
}

/// Catches a stage-1 daemon that stops probing the releases of today: a release whose floor is
/// the legacy floor has its binary probed once, runs its hook's command for each step, and is
/// refused `protocol_unsupported` when the probe says so.
#[test]
fn a_floor_at_the_legacy_floor_is_probed_and_runs_its_command() {
    let host = Host::new("floor-legacy");
    host.release(TARGET, "0.1.53");
    let reply = host.order(TARGET);
    assert_eq!(reply, awaiting(false));
    assert_eq!(host.s.probe.asked().len(), 1);
    assert_eq!(
        host.s.hk_steps(),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    assert_eq!(host.s.current().as_deref(), Some(TARGET));

    let host = Host::new("floor-legacy-refused");
    host.release(TARGET, "0.1.53");
    host.s.probe.set(|| Ok(0));
    assert_eq!(
        host.order(TARGET),
        refused_before_commit(Class::Release, REASON_PROTOCOL_UNSUPPORTED)
    );
    assert!(host.s.hk_steps().is_empty());
}

/// Catches a release above the legacy floor still probed, or its hook's command still run: with
/// a probe that would refuse it, the release is updated to with no probe, no step launched, the
/// record waiting for the restart at `cleanup` exactly as the command leaves it, and `current`
/// switched.
#[test]
fn a_floor_above_the_legacy_floor_is_judged_without_a_probe_and_runs_builtin_steps() {
    let host = Host::new("floor-above");
    host.release(TARGET, ABOVE);
    host.s.probe.set(|| Ok(0));
    assert_eq!(host.order(TARGET), awaiting(false));
    assert!(host.s.probe.asked().is_empty());
    assert!(host.s.hk_steps().is_empty(), "{:?}", host.s.hk_steps());
    let rec = host.s.rig.record(HK).expect("the record");
    assert_eq!(rec.step, Step::Cleanup);
    assert_eq!(
        rec.state,
        State::Done {
            code: Some(0),
            last_line: None
        }
    );
    assert_eq!(
        rec.progress
            .awaiting_restart
            .as_ref()
            .map(|wait| wait.version.as_str()),
        Some(TARGET)
    );
    assert_eq!(host.s.current().as_deref(), Some(TARGET));
}

/// Catches the backward rule lost: a release above the legacy floor that is older than this
/// daemon's oldest reader is `Failed` before the commit, of class release, with no probe, no
/// step, no record and the binary directory as it was.
#[test]
fn a_release_older_than_the_oldest_reader_is_refused_before_anything_is_staged() {
    let host = Host::new("floor-backward");
    host.release("0.1.52", ABOVE);
    let bin = binary_dir(&host.s.rig.state);
    let before = snapshot(&bin);
    assert_eq!(
        host.order("0.1.52"),
        refused_before_commit(Class::Release, REASON_PROTOCOL_UNSUPPORTED)
    );
    assert!(host.s.probe.asked().is_empty());
    assert!(host.s.hk_steps().is_empty());
    assert!(host.s.rig.record(HK).is_none());
    assert_eq!(snapshot(&bin), before);
}

/// Catches the version that runs judged or probed: an update of hostkeeper to the daemon's own
/// version, its floor above the legacy one, asks no probe, launches no step and is finished
/// `Succeeded` with no restart.
#[test]
fn the_running_version_is_installed_without_either_check() {
    let host = Host::new("floor-equal");
    host.release(DAEMON, ABOVE);
    host.s.probe.set(|| Ok(0));
    let daemon = host.daemon();
    let reply = daemon.order(&host.s.order(DAEMON, 1));
    assert!(
        matches!(
            reply,
            Reply::Finished {
                verdict: Verdict::Succeeded { .. },
                resent: false,
                ..
            }
        ),
        "{reply:?}"
    );
    assert!(host.s.probe.asked().is_empty());
    assert!(host.s.hk_steps().is_empty());
    assert!(!daemon.ctx().restart().requested());
}

/// Catches an update with built-in steps that a crash of the daemon in the middle of them turns
/// back into one that launches the hook's command: the next start's boot pass rebuilds the order
/// from the release's manifest with its steps built in, and the resend that follows finishes the
/// swap; no step is launched at any point.
#[test]
fn builtin_steps_cut_by_a_crash_are_resumed_built_in() {
    let host = Host::new("floor-crash");
    host.release(TARGET, ABOVE);
    let crashed = host.daemon().with_stores(crash_at(
        Some(CrashPoint::AfterWrite(3)),
        Arc::new(AtomicU64::new(0)),
    ));
    assert!(crashed.order(&host.s.order(TARGET, 1)).refusal().is_some());
    assert_eq!(host.s.current().as_deref(), Some(DAEMON));
    let rec = host.s.rig.record(HK).expect("the record of the cut update");
    assert!(!matches!(rec.state, State::Finished { .. }), "{rec:?}");

    let daemon = host.daemon();
    daemon.boot_pass().expect("the boot pass");
    assert_eq!(daemon.order(&host.s.order(TARGET, 1)), awaiting(false));
    assert_eq!(host.s.current().as_deref(), Some(TARGET));
    assert!(host.s.hk_steps().is_empty(), "{:?}", host.s.hk_steps());
    assert!(host.s.probe.asked().is_empty());
}
