#![cfg(all(target_os = "macos", feature = "privileged"))]
//! hostkeeper's update of itself under the real launchd of a GitHub-hosted macOS runner, as root,
//! in `.github/workflows/privileged.yml`'s job `macos-device` (and pr-device.yml's copy of it): the Mac twin of
//! `tests/self_update.rs`. The daemon runs as a launchd job of the daemon's own plist from
//! `current` of its binary directory, on the host's macOS adapters (its steps and the probe of a
//! new binary as launchd jobs), and is ordered from [`N`] to [`NEXT`] (`mac_self_update` says
//! how the Mac is made and what is a fixture).
//!
//! What each test proves:
//! - an installed daemon ordered the update to the next version asks the new binary its
//!   protocol through a launchd job, switches to it and exits; launchd starts the new binary,
//!   which settles the update `Succeeded` and reports it;
//! - a daemon the pkg installed (no package of hostkeeper) ordered the install of the next
//!   release does the same, and the package is installed at the new version;
//! - a daemon the pkg installed ordered the install of the version it runs is `Succeeded` at once:
//!   no probe, no restart, the binary directory as it was;
//! - ordered by the real coordinator in the test's process (a rollout that bakes a host for two
//!   healthy heartbeats), the daemon updates itself to the next version, the new binary reports
//!   the update's success and its own job live, healthy and at the new version, and the
//!   coordinator reads the host baked and hostkeeper converged.
//!
//! The fallback of a next binary that cannot start is not run on the Mac here (the module of the
//! rig says why).

mod mac_self_update;

use std::fs;
use std::path::PathBuf;

use hostkeeper_hook::Verdict;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_steps::record::{Record, State};
use mac_self_update::coordinator::Relay;
use mac_self_update::{DIGEST, HK, MacSelfHost, N, NEXT, Plan, probed_log_line, wait};
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
/// binary (another process than the old one), `current` naming [`NEXT`] and `previous` [`N`],
/// the probe of the new binary run by the daemon's launcher, and the new daemon reporting itself
/// and hostkeeper at [`NEXT`] with the order succeeded.
fn assert_restarted_into_next(host: &MacSelfHost, old_pid: u32) {
    let rec = host.settled();
    assert_succeeded(&rec, NEXT);
    let pid = host.runs(NEXT);
    assert_ne!(pid, old_pid, "launchd started a new process");
    assert_eq!(
        host.layout().previous().expect("previous").as_deref(),
        Some(N)
    );
    assert!(
        host.daemon_log().contains(&probed_log_line()),
        "the new binary was probed through the launcher\n{}",
        host.logs()
    );
    host.reports_succeeded(NEXT, NEXT);
}

/// Catches a Mac self-update that never gets past its probe (a launchd launcher that runs no
/// probe: every Mac update refused `protocol_unreadable`), or whose new binary never runs
/// (`current` not switched, the job not started again after the clean exit): the daemon of [`N`],
/// its package installed, is ordered the update to [`NEXT`]; the record settles `Succeeded`, the
/// job runs [`NEXT`]'s file, and the new daemon reports it.
#[test]
fn an_installed_daemon_updated_to_the_next_version_restarts_into_it_under_launchd() {
    let host = MacSelfHost::new("update", Plan::Installed);
    let pid = host.runs(N);
    host.offer("update", NEXT);
    assert_restarted_into_next(&host, pid);
}

/// Catches the Mac a pkg installed staying on its first version (the case of a Mac upgraded by
/// hand and then ordered the next release): no package of hostkeeper is installed, the
/// coordinator orders the install of [`NEXT`], and the daemon runs it as its self-update; the
/// record settles `Succeeded` once the new binary started, the job runs it, and the package is
/// installed at [`NEXT`].
#[test]
fn a_daemon_the_pkg_installed_is_installed_its_next_release_and_restarts_into_it() {
    let host = MacSelfHost::new("install", Plan::Pkg(NEXT));
    let pid = host.runs(N);
    assert!(fs::read_link(host.service().join("current")).is_err());
    host.offer("install", NEXT);
    assert_restarted_into_next(&host, pid);
    assert_eq!(
        fs::read_link(host.service().join("current")).ok(),
        Some(PathBuf::from(NEXT))
    );
}

/// Catches an install of the version that already runs being probed (a probe that cannot run
/// refuses it `protocol_unreadable`), restarting the daemon, or held back for a binary that
/// already runs: the daemon of [`N`] the pkg installed is ordered the install of [`N`]; the
/// record is `Succeeded` at once, the package is installed at [`N`], the job's process is the
/// same one, the binary directory is as it was (`current` [`N`], no `previous`), and the
/// daemon's log holds no run of the probe.
#[test]
fn an_install_of_the_running_version_is_succeeded_without_a_probe_or_a_restart() {
    let host = MacSelfHost::new("running", Plan::Pkg(N));
    let pid = host.runs(N);
    host.offer("install", N);
    let rec = host.settled();
    assert_succeeded(&rec, N);
    host.reports_succeeded(N, N);
    assert_eq!(host.runs(N), pid, "the daemon was not restarted");
    assert_eq!(host.layout().previous().expect("previous"), None);
    assert_eq!(
        fs::read_link(host.service().join("current")).ok(),
        Some(PathBuf::from(N))
    );
    assert!(
        !host.daemon_log().contains(&probed_log_line()),
        "the running version was not probed\n{}",
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
