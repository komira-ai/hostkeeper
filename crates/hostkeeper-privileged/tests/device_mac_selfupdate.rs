#![cfg(all(target_os = "macos", feature = "privileged"))]
//! hostkeeper's update of itself under the real launchd of a GitHub-hosted macOS runner, as root,
//! in `.github/workflows/privileged.yml`'s job `macos-device` (and pr-device.yml's copy of it): the Mac twin of
//! `tests/self_update.rs`. The daemon runs as a launchd job of the daemon's own plist, its program
//! `current` of its binary directory or, for a Mac as the pkg leaves it, the packaged binary that
//! seeds the binary directory and execs `current`, on the host's macOS adapters (its steps as
//! launchd jobs), and is ordered from [`N`] to [`NEXT`] (`mac_self_update` says how the Mac is
//! made and what is a fixture).
//!
//! What each test proves:
//! - an installed daemon ordered the update to the next version hands over (stages the binary,
//!   links `next`, exits); launchd starts the binary `current` names, which execs the new one,
//!   whose start finishes the switch, settles the update `Succeeded` and reports it;
//! - a daemon the pkg installed (no package of hostkeeper; its job's program the packaged binary,
//!   the binary directory empty at the start) is started by launchd, seeds the binary directory
//!   with itself as [`N`] and runs `current` in the same process; ordered the install of the next
//!   release it does the same as above, and the package is installed at the new version;
//! - a daemon the pkg installed ordered the install of the version it runs is `Succeeded` at once:
//!   no restart, the binary directory as it was;
//! - a next binary that cannot start is execed three times by the old binary, which then settles
//!   the update `Failed`, `self_update_fallback`, lists the version bad and runs as the daemon,
//!   with `current` never moved: the Mac fallback test that the fallback job's place in
//!   `/Library/LaunchDaemons` once prevented;
//! - ordered by the real coordinator in the test's process (a rollout that bakes a host for two
//!   healthy heartbeats), the daemon updates itself to the next version, the new binary reports
//!   the update's success and its own job live, healthy and at the new version, and the
//!   coordinator reads the host baked and hostkeeper converged.

mod mac_self_update;

use std::fs;
use std::path::PathBuf;

use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_steps::handover::HAND_OVER_TRIES;
use hostkeeper_steps::record::{Record, State};
use mac_self_update::coordinator::Relay;
use mac_self_update::{DIGEST, FALLBACK_REASON, HK, MacSelfHost, N, NEXT, Plan, wait};
use serde_json::Value;

fn succeeded() -> Verdict {
    Verdict::Succeeded {
        promotion_pending: false,
    }
}

/// The record of an update to `version` that ended `Succeeded`, and waits for nothing.
fn assert_succeeded(rec: &Record, version: &str) {
    assert_eq!(rec.target_version, version, "{rec:?}");
    assert_eq!(
        rec.state,
        State::Finished {
            verdict: succeeded(),
            reason: None,
        },
        "{rec:?}"
    );
    assert_eq!(rec.progress.awaiting_restart, None, "{rec:?}");
}

/// What every restart into [`NEXT`] leaves: the record `Succeeded`, the job's process the new
/// binary (another process than the old one), `current` naming [`NEXT`] (its start finished the
/// switch), `previous` [`N`] and no `next`, and the new daemon reporting itself and hostkeeper at
/// [`NEXT`] with the order succeeded.
fn assert_restarted_into_next(host: &MacSelfHost, old_pid: u32) {
    let rec = host.settled();
    assert_succeeded(&rec, NEXT);
    let pid = host.runs(NEXT);
    assert_ne!(pid, old_pid, "launchd started a new process");
    assert_eq!(
        host.layout().previous().expect("previous").as_deref(),
        Some(N)
    );
    assert_eq!(host.layout().next().expect("next"), None);
    host.reports_succeeded(NEXT, NEXT);
}

/// Catches a Mac self-update whose new binary never runs (`next` not linked, the job not started
/// again after the clean exit, the old binary not execing the new one): the daemon of [`N`], its
/// package installed, is ordered the update to [`NEXT`]; the record settles `Succeeded`, the job
/// runs [`NEXT`]'s file, and the new daemon reports it.
#[test]
fn an_installed_daemon_updated_to_the_next_version_restarts_into_it_under_launchd() {
    let host = MacSelfHost::new("update", Plan::Installed);
    let pid = host.runs(N);
    host.offer("update", NEXT);
    assert_restarted_into_next(&host, pid);
}

/// Catches the Mac a pkg installed staying on its first version (the case of a Mac upgraded by
/// hand and then ordered the next release), and a packaged start that does not seed or does not
/// run `current` (the job's program is the packaged binary and the binary directory starts empty:
/// the job's process runs [`N`]'s file of the binary directory, `current` names it, and nothing
/// is `previous`): no package of hostkeeper is installed, the
/// coordinator orders the install of [`NEXT`], and the daemon runs it as its self-update; the
/// record settles `Succeeded` once the new binary started, the job runs it, and the package is
/// installed at [`NEXT`].
#[test]
fn a_daemon_the_pkg_installed_is_installed_its_next_release_and_restarts_into_it() {
    let host = MacSelfHost::new("install", Plan::Pkg(NEXT));
    let pid = host.runs(N);
    assert!(
        host.layout().previous().expect("previous").is_none(),
        "the packaged start seeded the binary directory with one version"
    );
    assert!(fs::read_link(host.service().join("current")).is_err());
    host.offer("install", NEXT);
    assert_restarted_into_next(&host, pid);
    assert_eq!(
        fs::read_link(host.service().join("current")).ok(),
        Some(PathBuf::from(NEXT))
    );
}

/// Catches an install of the version that already runs restarting the daemon, or held back for a
/// binary that already runs: the daemon of [`N`] the pkg installed is ordered the install of
/// [`N`]; the record is `Succeeded` at once, the package is installed at [`N`], the job's process
/// is the same one, and the binary directory is as it was (`current` [`N`], no `previous`, no
/// `next`).
#[test]
fn an_install_of_the_running_version_is_succeeded_without_a_restart() {
    let host = MacSelfHost::new("running", Plan::Pkg(N));
    let pid = host.runs(N);
    host.offer("install", N);
    let rec = host.settled();
    assert_succeeded(&rec, N);
    host.reports_succeeded(N, N);
    assert_eq!(host.runs(N), pid, "the daemon was not restarted");
    assert_eq!(host.layout().previous().expect("previous"), None);
    assert_eq!(host.layout().next().expect("next"), None);
    assert_eq!(
        fs::read_link(host.service().join("current")).ok(),
        Some(PathBuf::from(N))
    );
}

/// The Mac fallback test the fallback job's place prevented. Catches a hand-over that does not fall
/// back under launchd, or falls back by moving `current`: the daemon of [`N`] is ordered the update
/// to [`NEXT`], which exits 1 at its start; launchd restarts the job's program each time, and the
/// old binary execs [`NEXT`] again, [`HAND_OVER_TRIES`] times in all; at the next start the count
/// is spent, the old binary settles the update `Failed` after the commit, of class release,
/// `self_update_fallback`, lists [`NEXT`] bad, and the job runs [`N`] again, `current` never moved
/// (no `previous`, no `next`).
#[test]
fn a_next_binary_that_cannot_start_is_fallen_back_from_under_launchd() {
    let host = MacSelfHost::new("badnext", Plan::InstalledFailingNext);
    host.runs(N);
    host.offer("update", NEXT);
    let rec = host.settled();
    assert_eq!(rec.target_version, NEXT, "{rec:?}");
    assert_eq!(
        rec.state,
        State::Finished {
            verdict: Verdict::Failed {
                phase: Phase::AfterCommit,
                class: Class::Release,
            },
            reason: Some(FALLBACK_REASON.to_owned()),
        },
        "{rec:?}"
    );
    assert_eq!(rec.progress.awaiting_restart, None, "{rec:?}");
    assert!(host.next_is_bad());
    host.runs(N);
    assert_eq!(host.layout().previous().expect("previous"), None);
    assert_eq!(host.layout().next().expect("next"), None);
    assert_eq!(
        host.refused_starts(),
        HAND_OVER_TRIES as usize,
        "{}",
        host.logs()
    );
}

/// Catches the farm's Mac whose self-update succeeded and never baked: the daemon observed its own
/// service as `unmonitored` (the package of hostkeeper names no unit; the daemon's launchd job is
/// the installer's), so it reported it unhealthy, and the new binary reported no verdict for the
/// update, so the coordinator kept the host's older verdict. The real coordinator in the test's
/// process, whose rollout asks two healthy heartbeats of a host at its version, orders the
/// daemon of [`N`] its update to [`NEXT`]; the daemon restarts into [`NEXT`] under launchd; and
/// the coordinator reads hostkeeper converged at [`NEXT`], the host up and healthy, its last
/// update the order's `Succeeded`, while the heartbeats report the job running at [`NEXT`].
#[test]
fn the_coordinator_bakes_and_converges_a_mac_that_updated_itself_under_launchd() {
    let host = MacSelfHost::new("bake", Plan::Installed);
    let old_pid = host.runs(N);
    let mut coordinator = Relay::new(
        &host.coordinator_dir(),
        &host.link(),
        HK,
        NEXT,
        DIGEST,
        "bake_beats = 2",
    );
    let mut convergence = Value::Null;
    wait("the coordinator reads hostkeeper converged", || {
        host.check_no_refusal();
        coordinator.relay();
        convergence = coordinator.convergence(HK);
        convergence["state"] == "converged"
    });
    assert_eq!(
        convergence["desired"],
        NEXT,
        "{convergence}\n{}",
        host.logs()
    );

    let ordered = coordinator.ordered();
    assert_eq!(ordered.len(), 1, "{ordered:?}");
    let update = &ordered[0];
    assert!(
        matches!(&update.verb, Some(pb::order::Verb::Update(a)) if a.version == NEXT),
        "{update:?}"
    );
    let rec = host.settled();
    assert_succeeded(&rec, NEXT);
    assert_ne!(host.runs(NEXT), old_pid, "launchd started a new process");

    let on_host = coordinator.on_host(HK).expect("hostkeeper on the host");
    let last = on_host.last_update.clone().expect("a last update");
    assert_eq!(
        (
            on_host.installed_version.as_deref(),
            on_host.health,
            on_host.up,
            last.target.as_str(),
            last.outcome,
            last.order_id.as_deref(),
            last.order_try,
        ),
        (
            Some(NEXT),
            pb::Health::Healthy as i32,
            true,
            NEXT,
            pb::Outcome::Succeeded as i32,
            Some(update.order_id.as_str()),
            Some(update.r#try),
        ),
        "{on_host:?}\n{}",
        host.logs()
    );
    let beats = host.beats();
    let newest = beats
        .iter()
        .rev()
        .find_map(|beat| {
            beat["services"]
                .as_array()?
                .iter()
                .find(|service| service["name"] == HK)
                .cloned()
        })
        .expect("a heartbeat lists hostkeeper");
    assert_eq!(
        (
            &newest["health"],
            &newest["observed"]["liveness"],
            &newest["observed"]["version"]
        ),
        (
            &Value::from("healthy"),
            &Value::from("running"),
            &Value::from(NEXT)
        ),
        "{newest}"
    );
}
