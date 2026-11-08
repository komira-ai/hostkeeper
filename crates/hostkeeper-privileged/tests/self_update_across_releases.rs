#![cfg(all(any(target_os = "linux", target_os = "macos"), feature = "privileged"))]
//! hostkeeper's update of itself across releases, as root, from the published release that
//! `previous-release.toml` pins to this build: on Linux under the real systemd of a hosted runner
//! (`.github/workflows/privileged.yml`'s job `privileged`), on a Mac under the real launchd
//! (its job `macos-device`), and in pr-device.yml's copies of both jobs on every pull request.
//! The rig (`across`, on the end-to-end rig of `e2e`) runs the released daemon as the host's
//! daemon, the real coordinator in process ordering hostkeeper, and this build's hostkeeper
//! packages from a release channel on loopback, checked by the production attestation verifier.
//!
//! Each test starts from the released daemon converged on its own version's package
//! (`across::Rig::installed`: the coordinator's install of the version that runs), with its
//! fallback unit and drop-in (or fallback job) armed, as the released daemon arms them at its
//! start. What each proves:
//! - the released daemon updates to this build the old way (probe, hook, switch, clean exit), the
//!   unit or job starts this build, which settles the update `Succeeded`, reports it, removes the
//!   released daemon's fallback files before it can update again, and the coordinator reads the
//!   host converged on it;
//! - this build made to fail its start is fallen back from by the released binary's own
//!   fallback (its fallback unit under systemd, its fallback job under launchd): the released
//!   binary runs again, settles the update `Failed`, `self_update_fallback`, and lists the version
//!   bad, and its heartbeat reports that verdict;
//! - this build, once the released daemon updated to it, updates to itself under a newer version
//!   name by the hand-over: it stages the new binary and links `next`, `current` is moved only by
//!   the new binary's own start, and no fallback file is armed.
//!
//! A pinned tarball that is missing or differs from the pin fails every test by name before the
//! host is touched (`across::released_binary`). Every wait is a bounded count of tries of an
//! explicit state; nothing is asserted about time.

mod across;
mod e2e;

use across::{Build, FALLBACK_REASON, Rig, newer};
use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_steps::record::{Record, State};

/// The record of an update to `version` that ended with `verdict` and `reason`, and waits for
/// nothing.
fn assert_finished(rec: &Record, version: &str, verdict: Verdict, reason: Option<&str>) {
    assert_eq!(rec.target_version, version, "{rec:?}");
    assert_eq!(
        rec.state,
        State::Finished {
            verdict,
            reason: reason.map(str::to_owned),
        },
        "{rec:?}"
    );
    assert_eq!(rec.progress.awaiting_restart, None, "{rec:?}");
}

fn succeeded() -> Verdict {
    Verdict::Succeeded {
        promotion_pending: false,
    }
}

/// Updates the rig's host to this build at `version` and checks that the update went through as
/// a self-update does: the coordinator reads the host converged on it, the daemon's process runs
/// the binary directory's file of `version`, `current` names it, `previous` names `from` and
/// `next` is gone, the step record says `Succeeded`, the fallback files of the releases before
/// the hand-over are gone, and the host reported the update `Succeeded` with its order.
fn update_to_this_build(rig: &Rig, from: &str, version: &str) {
    let built = rig.publish(version, Build::This);
    rig.order(&built);
    rig.wait_converged(version);
    rig.wait_running(version);
    assert_eq!(
        rig.links(),
        (Some(version.to_owned()), Some(from.to_owned())),
        "{}",
        rig.host.logs()
    );
    assert_eq!(rig.layout().next().expect("next"), None);
    assert!(
        !rig.host.fallback_files_present(),
        "the daemon of {version} removed the fallback files at its start\n{}",
        rig.host.logs()
    );
    let rec = rig.settled(version);
    assert_finished(&rec, version, succeeded(), None);
    let update = rig.wait_reported(version, "OUTCOME_SUCCEEDED");
    assert!(update["orderId"].is_string(), "{update}");
}

/// Catches a release of this build that a daemon of the pinned release cannot update to: a probe
/// answer it does not accept (the verb `self-update-protocol` must still print 1), a hook it
/// cannot run, a package or attestation it refuses, a new binary that does not start under the
/// old daemon's unit or job, or a settlement or report the coordinator does not read as
/// converged; and a release that leaves the released daemon's fallback unit or job armed beside
/// its own hand-overs. The released daemon, converged on its own version with its fallback files
/// armed, is ordered this build's next version the old way; the host converges on it, the unit or
/// job runs it, the binary directory names it `current` and the release `previous`, this build
/// removes the released daemon's fallback files at its start, and the update is `Succeeded`.
#[test]
fn the_released_daemon_updates_to_this_build_and_this_build_settles_it_succeeded() {
    let rig = Rig::installed("x1upd");
    let released = rig.released.clone();
    rig.wait_running(&released);
    assert_eq!(rig.links(), (Some(released.clone()), None));
    assert!(
        rig.host.fallback_files_present(),
        "the released daemon armed its fallback at its start\n{}",
        rig.host.logs()
    );
    update_to_this_build(&rig, &released, &newer(&released, 1));
}

/// Catches a release of this build that cannot start leaving the host without a daemon, or
/// fallen back from by anything but the released binary's own fallback (the released daemon
/// updated the old way, so its fallback unit or job is what falls back from a stage-2 build that
/// cannot start): the released daemon is ordered a version of this build that exits 1 at its
/// start; the released binary runs again,
/// `current` and `previous` both name it (the fallback moves `current` back and leaves
/// `previous`), the step record says `Failed` after the commit, of class release,
/// `self_update_fallback`, the version is on the local bad list, and the host's heartbeat reports
/// that verdict to the coordinator; on Linux systemd ran the fallback unit to success.
#[test]
fn this_build_made_to_fail_its_start_is_fallen_back_from_to_the_released_binary() {
    let rig = Rig::installed("x1fbk");
    let released = rig.released.clone();
    let failing = newer(&released, 1);
    let built = rig.publish(&failing, Build::Failing);
    rig.order(&built);

    let update = rig.wait_reported(&failing, "OUTCOME_FAILED");
    assert_eq!(update["reason"], FALLBACK_REASON, "{update}");
    assert_eq!(update["class"], "FAILURE_CLASS_RELEASE", "{update}");
    rig.wait_running(&released);
    assert_eq!(
        rig.links(),
        (Some(released.clone()), Some(released.clone())),
        "{}",
        rig.host.logs()
    );
    let rec = rig.settled(&failing);
    assert_finished(
        &rec,
        &failing,
        Verdict::Failed {
            phase: Phase::AfterCommit,
            class: Class::Release,
        },
        Some(FALLBACK_REASON),
    );
    assert!(rig.is_bad(&failing), "{failing} is on the bad list");
    #[cfg(target_os = "linux")]
    assert!(
        rig.host.fallback_ran(),
        "systemd ran the released binary's fallback unit\n{}",
        rig.host.logs()
    );

    rig.wait_running(&released);
}

/// Catches a release of this build that installs over the release but cannot update itself
/// again: the released daemon updates to this build's next version, and that version, ordered
/// the version after it, hands over to it (this build staged under a newer name, `next` linked,
/// execed by the unit's start of `current`); the host converges on each, `previous` names the
/// version it came from, `next` is gone, no fallback file is armed, and each update is
/// `Succeeded`.
#[test]
fn this_build_updates_to_itself_under_a_newer_version_name() {
    let rig = Rig::installed("x1self");
    let released = rig.released.clone();
    let next = newer(&released, 1);
    let after = newer(&released, 2);
    update_to_this_build(&rig, &released, &next);
    update_to_this_build(&rig, &next, &after);
}
