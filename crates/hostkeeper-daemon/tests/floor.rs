//! A release of hostkeeper judged against the daemon's oldest reader (docs/design/two-programs.md,
//! sections 4.2 and 4.4): the backward rule ([`judge`]) against [`OLDEST_READER`], the forward rule
//! the manifest loader keeps by the release's floor, no probe of the new binary, the hook's command
//! run for a floor at or below [`LEGACY_FLOOR`] and the built-in steps for one above it.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::ServiceName;
use hostkeeper_daemon::selfupdate::{
    APT_RELEASE_BINARY, Judgement, LEGACY_FLOOR, REASON_PROTOCOL_UNSUPPORTED, binary_dir,
    builtin_steps, judge,
};
use hostkeeper_daemon::testing::manifest;
use hostkeeper_daemon::{Daemon, Refusal, Reply};
use hostkeeper_hook::{Class, Step, Verdict};
use hostkeeper_steps::record::State;
use hostkeeper_steps::{CrashPoint, OLDEST_READER};
use support::selfupdate::{HK, SelfRig, awaiting, own, refused_before_commit};
use support::{crash_at, snapshot};

/// The version of the daemon under test: above the oldest reader.
const DAEMON: &str = "0.1.200";
/// The version it is ordered to.
const TARGET: &str = "0.1.210";
/// A floor above the legacy one.
const ABOVE: &str = "0.1.163";

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
        let binary = dir.join(APT_RELEASE_BINARY);
        fs::create_dir_all(binary.parent().expect("a bin directory")).expect("the directory");
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

/// Catches a backward rule that lets through a release older than this daemon's oldest reader,
/// refuses one it can fall back to, or looks at the floor: the version that runs is accepted with
/// no check; the oldest reader (0.1.194) and anything newer is accepted, whatever floor its
/// package declares; anything older, or a version that is not `major.minor.patch`, is refused.
/// The constants are the design's: the legacy floor 0.1.53, the oldest reader R2.
#[test]
fn the_backward_rule_judges_a_release_by_the_oldest_reader_alone() {
    assert_eq!(LEGACY_FLOOR, v("0.1.53"));
    assert_eq!(OLDEST_READER, v("0.1.194"));
    let running = v(DAEMON);
    for (version, judged) in [
        (DAEMON, Judgement::Accepted),
        (TARGET, Judgement::Accepted),
        ("0.1.194", Judgement::Accepted),
        ("2.0.0", Judgement::Accepted),
        ("0.1.193", Judgement::Unsupported),
        ("0.1.162", Judgement::Unsupported),
        ("0.1.53", Judgement::Unsupported),
        ("0.0.900", Judgement::Unsupported),
        ("1.0.0-rc1", Judgement::Unsupported),
        ("0.1", Judgement::Unsupported),
    ] {
        assert_eq!(judge(running, version), judged, "{version}");
    }
    // The running version is accepted though it is older than the oldest reader.
    assert_eq!(judge(v("0.1.100"), "0.1.100"), Judgement::Accepted);
    let hostkeeper = ServiceName::new(HK).expect("a name");
    let other = ServiceName::new("web").expect("a name");
    assert!(!builtin_steps(&hostkeeper, v("0.1.0")));
    assert!(!builtin_steps(&hostkeeper, LEGACY_FLOOR));
    assert!(builtin_steps(&hostkeeper, v(ABOVE)));
    assert!(!builtin_steps(&other, v(ABOVE)));
}

/// Catches the forward rule lost: a release whose floor is above this daemon is deferred
/// `daemon_too_old` by the manifest's loader, before any step, and nothing changes.
#[test]
fn a_floor_above_the_daemon_is_deferred_before_any_step() {
    let host = Host::new("floor-forward");
    host.release(TARGET, "0.1.201");
    let bin = binary_dir(&host.s.rig.state);
    let before = snapshot(&bin);
    let reply = host.order(TARGET);
    assert_eq!(reply.refusal(), Some(Refusal::DaemonTooOld), "{reply:?}");
    assert!(host.s.hk_steps().is_empty());
    assert_eq!(snapshot(&bin), before);
}

/// Catches a release whose floor is the legacy floor (the releases before stage 2 declare it; R2 and later ones declare a higher floor) being
/// refused, probed or run built in: it is accepted as the oldest reader or newer, runs its hook's
/// command for each step, and is handed over to.
#[test]
fn a_floor_at_the_legacy_floor_runs_its_command_and_is_handed_over_to() {
    let host = Host::new("floor-legacy");
    host.release(TARGET, "0.1.53");
    let reply = host.order(TARGET);
    assert_eq!(reply, awaiting(false));
    assert_eq!(
        host.s.hk_steps(),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    assert_eq!(host.s.current().as_deref(), Some(DAEMON));
    assert_eq!(
        host.s.layout().next().expect("next").as_deref(),
        Some(TARGET)
    );
}

/// Catches a release above the legacy floor having its hook's command run: it is updated to with no
/// step launched, the record waiting for the restart at `cleanup` exactly as the command leaves it
/// (as a hand-over), and `current` not moved.
#[test]
fn a_floor_above_the_legacy_floor_runs_builtin_steps() {
    let host = Host::new("floor-above");
    host.release(TARGET, ABOVE);
    assert_eq!(host.order(TARGET), awaiting(false));
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
    let wait = rec.progress.awaiting_restart.as_ref().expect("a wait");
    assert_eq!(
        (wait.version.as_str(), wait.next.as_deref(), wait.starts),
        (TARGET, Some(TARGET), Some(0))
    );
    assert_eq!(host.s.current().as_deref(), Some(DAEMON));
    assert_eq!(
        host.s.layout().next().expect("next").as_deref(),
        Some(TARGET)
    );
}

/// Catches the backward rule lost: a release older than this daemon's oldest reader is `Failed`
/// before the commit, of class release, with no step, no record and the binary directory as it
/// was, whatever floor its package declares.
#[test]
fn a_release_older_than_the_oldest_reader_is_refused_before_anything_is_staged() {
    for (n, floor) in [ABOVE, "0.1.53"].into_iter().enumerate() {
        let host = Host::new(&format!("floor-backward-{n}"));
        host.release("0.1.193", floor);
        let bin = binary_dir(&host.s.rig.state);
        let before = snapshot(&bin);
        assert_eq!(
            host.order("0.1.193"),
            refused_before_commit(Class::Release, REASON_PROTOCOL_UNSUPPORTED),
            "floor {floor}"
        );
        assert!(host.s.hk_steps().is_empty());
        assert!(host.s.rig.record(HK).is_none());
        assert_eq!(snapshot(&bin), before);
    }
}

/// Catches the version that runs judged: an update of hostkeeper to the daemon's own version, its
/// floor above the legacy one, launches no step and is finished `Succeeded` with no restart.
#[test]
fn the_running_version_is_installed_without_either_check() {
    let host = Host::new("floor-equal");
    host.release(DAEMON, ABOVE);
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
    assert!(host.s.hk_steps().is_empty());
    assert!(!daemon.ctx().restart().requested());
}

/// Catches an update with built-in steps that a crash of the daemon in the middle of them turns
/// back into one that launches the hook's command: the next start's boot pass rebuilds the order
/// from the release's manifest with its steps built in, and the resend that follows finishes the
/// hand-over; no step is launched at any point.
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
    assert_eq!(host.s.current().as_deref(), Some(DAEMON));
    assert_eq!(
        host.s.layout().next().expect("next").as_deref(),
        Some(TARGET)
    );
    assert!(host.s.hk_steps().is_empty(), "{:?}", host.s.hk_steps());
}
