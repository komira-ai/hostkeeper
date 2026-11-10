#![cfg(all(target_os = "macos", feature = "privileged"))]
//! hostkeeper's update of itself under the real launchd of a GitHub-hosted macOS runner, as root,
//! in `.github/workflows/privileged.yml`'s job `macos-device` (and pr-device.yml's copy of it): the Mac twin of
//! `tests/self_update.rs`. The daemon runs as a launchd job of the daemon's own plist, its program
//! `current` of its binary directory or, for a Mac as the pkg leaves it, the packaged binary that
//! seeds the binary directory and execs `current`, on the host's macOS adapters (its steps as
//! launchd jobs), and is ordered from [`N`] to [`NEXT`] (`mac_self_update` says how the Mac is
//! made and what is a fixture).
//!
//! **hostkeeper is a native package**, as a release from A24 on is ordered
//! (docs/design/packages-native.md, sections 10.4 and 10.5): its signed `.pkg`, the binary at
//! `/usr/local/bin/hostkeeper`, built here with the Mac's `pkgbuild` and `productbuild` and
//! ordered by the one `macos-pkg` entry `release targets` writes. The daemon fetches it by its
//! pin, has the installer check its signature (answered as a signed package: nothing is signed on
//! a runner, `shared/macos_pkg.rs`), stages it, hands over to the binary of its tree, and the new
//! binary, once it confirmed itself, has the Mac's `installer` install the package, so the
//! evidence of the native path is the host's: `pkgutil`'s receipt, the file `installer` put at
//! `/usr/local/bin/hostkeeper`, the version directory's sealed `target.json` and `tree/`, and
//! `placed.json`.
//!
//! What each test proves:
//! - an installed daemon ordered the update to the next version hands over (stages the binary,
//!   links `next`, exits); launchd starts the binary `current` names, which execs the new one,
//!   whose start finishes the switch, settles the update `Succeeded` and reports it; the package
//!   is installed at the next version only after that;
//! - a daemon the pkg installed (no package of hostkeeper; its job's program the packaged binary,
//!   the binary directory empty at the start) is started by launchd, seeds the binary directory
//!   with itself as [`N`] and runs `current` in the same process; ordered the install of the next
//!   release it does the same as above, and the package is installed at the new version;
//! - a daemon the pkg installed ordered the install of the version it runs is `Succeeded` at once:
//!   no restart, the binary directory as it was, the package installed at that version;
//! - a next binary that cannot start is execed three times by the old binary, which then settles
//!   the update `Failed`, `self_update_fallback`, lists the version bad and runs as the daemon,
//!   with `current` never moved and the package left at the old version: the Mac fallback test
//!   that the fallback job's place in `/Library/LaunchDaemons` once prevented;
//! - ordered by the real coordinator in the test's process (which the daemon enrolls with and
//!   beats to over mutual TLS, as a rollout that bakes a host for two healthy heartbeats), the
//!   daemon installs the version it runs, then updates itself to the next version, the new binary
//!   reports the update's success and its own job live, healthy and at the new version, and the
//!   coordinator reads the host baked and hostkeeper converged.
//!
//! Every wait polls a state a bounded number of times and fails at once when the state can no
//! longer come; no test asserts how long anything took.

mod e2e;
mod mac_self_update;

use std::path::Path;

use e2e::coordinator::CoordinatorHandle;
use e2e::scenario::{CONFIG_TAIL, COORDINATED};
use e2e::{HOST_ID, ROLE, wait_for};
use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_steps::handover::HAND_OVER_TRIES;
use hostkeeper_steps::record::{Record, State};
use hostkeeper_testkit::Scratch;
use mac_self_update::{FALLBACK_REASON, HK, MacSelfHost, N, NEXT, Plan, receipt_version};
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

/// Checks that the daemon staged [`NEXT`] of hostkeeper for the entry that the order carried: the
/// version directory it left holds that entry as its sealed `target.json`.
fn assert_staged_for_the_entry(host: &MacSelfHost) {
    assert_eq!(
        host.staged_entry(NEXT).as_ref(),
        Some(host.entry(NEXT)),
        "the daemon staged the version for the entry of the order\n{}",
        host.logs()
    );
}

/// What every restart into [`NEXT`] leaves: the record `Succeeded`, the job's process the new
/// binary (another process than the old one), `current` naming [`NEXT`] (its start finished the
/// switch), `previous` [`N`] and no `next`, the new daemon reporting itself and hostkeeper at
/// [`NEXT`] with the order succeeded, the version staged for the entry of the order, and the
/// package installed at [`NEXT`] (the receipt, the program installer put at `/usr/local/bin/`,
/// `current` of the packages directory and the daemon's record). The new binary confirms the
/// record before it removes `next`, so the binary directory is read only after its heartbeat,
/// which it sends once both are done.
fn assert_restarted_into_next(host: &MacSelfHost, old_pid: u32) {
    let rec = host.settled(NEXT);
    assert_succeeded(&rec, NEXT);
    host.reports_succeeded(NEXT, NEXT);
    let pid = host.runs(NEXT);
    assert_ne!(pid, old_pid, "launchd started a new process");
    assert_eq!(
        host.layout().previous().expect("previous").as_deref(),
        Some(N)
    );
    assert_eq!(host.layout().next().expect("next"), None);
    assert_staged_for_the_entry(host);
    host.assert_installed(NEXT);
}

/// Catches a Mac self-update whose new binary never runs (`next` not linked, the job not started
/// again after the clean exit, the old binary not execing the new one), or whose package is not
/// installed once it ran: the daemon of [`N`], its package installed, is ordered the update to
/// [`NEXT`] by the entry of its `.pkg`; the record settles `Succeeded`, the job runs [`NEXT`]'s
/// file, the new daemon reports it, and the package is installed at [`NEXT`] (`previous` of the
/// packages directory [`N`]).
#[test]
fn an_installed_daemon_updated_to_the_next_version_restarts_into_it_under_launchd() {
    let host = MacSelfHost::new("update", Plan::Installed);
    let pid = host.runs(N);
    host.offer("update", NEXT);
    assert_restarted_into_next(&host, pid);
    assert_eq!(host.package_link("previous").as_deref(), Some(N));
}

/// Catches the Mac a pkg installed staying on its first version (the case of a Mac upgraded by
/// hand and then ordered the next release), and a packaged start that does not seed or does not
/// run `current` (the job's program is the packaged binary and the binary directory starts empty:
/// the job's process runs [`N`]'s file of the binary directory, `current` names it, and nothing
/// is `previous`): no package of hostkeeper is installed, the coordinator orders the install of
/// [`NEXT`], and the daemon runs it as its self-update; the record settles `Succeeded` once the
/// new binary started, the job runs it, and the package is installed at [`NEXT`]. The install is
/// set by targets: the order carries the `macos-pkg` entry of the release (the pin of its
/// `.pkg`, a manifest with its package identifier and its floor), which passes the daemon's
/// receipt check, and the daemon stages the version for that entry. Catches: an entry the daemon
/// refuses (`order_invalid`, `manifest_invalid`, `installer_not_listed`, `signature`) on the Mac
/// the pkg leaves, or one it drops on its way to the store, so that the install is staged from
/// another package than the one ordered.
#[test]
fn a_daemon_the_pkg_installed_is_installed_its_next_release_and_restarts_into_it() {
    let host = MacSelfHost::new("install", Plan::Pkg);
    let pid = host.runs(N);
    assert!(
        host.layout().previous().expect("previous").is_none(),
        "the packaged start seeded the binary directory with one version"
    );
    assert!(host.package_link("current").is_none());
    host.offer("install", NEXT);
    assert_restarted_into_next(&host, pid);
    assert_eq!(host.package_link("current").as_deref(), Some(NEXT));
}

/// Catches an install of the version that already runs restarting the daemon, or held back for a
/// binary that already runs: the daemon of [`N`] the pkg installed is ordered the install of
/// [`N`]; the record is `Succeeded` at once, the package is installed at [`N`], the job's process
/// is the same one, and the binary directory is as it was (`current` [`N`], no `previous`, no
/// `next`).
#[test]
fn an_install_of_the_running_version_is_succeeded_without_a_restart() {
    let host = MacSelfHost::new("running", Plan::Pkg);
    let pid = host.runs(N);
    host.offer("install", N);
    let rec = host.settled(N);
    assert_succeeded(&rec, N);
    host.reports_succeeded(N, N);
    assert_eq!(host.runs(N), pid, "the daemon was not restarted");
    assert_eq!(host.layout().previous().expect("previous"), None);
    assert_eq!(host.layout().next().expect("next"), None);
    host.assert_installed(N);
}

/// The Mac fallback test the fallback job's place prevented. Catches a hand-over that does not fall
/// back under launchd, or falls back by moving `current`, or a package installed before the new
/// binary confirmed itself: the daemon of [`N`] is ordered the update to [`NEXT`], which exits 1
/// at its start; launchd restarts the job's program each time, and the old binary execs [`NEXT`]
/// again, [`HAND_OVER_TRIES`] times in all; at the next start the count is spent, the old binary
/// settles the update `Failed` after the commit, of class release, `self_update_fallback`, lists
/// [`NEXT`] bad, and the job runs [`N`] again, `current` never moved (no `previous`, no `next`)
/// and the package still installed at [`N`] (the receipt, the program and `current` of the
/// packages directory). The old binary settles the record before it removes `next`, and runs as
/// the daemon only after both: the binary directory is read once its heartbeat reports the
/// fallback.
#[test]
fn a_next_binary_that_cannot_start_is_fallen_back_from_under_launchd() {
    let host = MacSelfHost::new("badnext", Plan::InstalledFailingNext);
    host.runs(N);
    host.offer("update", NEXT);
    let rec = host.settled(NEXT);
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
    host.reports_fell_back(N);
    host.runs(N);
    assert_eq!(host.layout().previous().expect("previous"), None);
    assert_eq!(host.layout().next().expect("next"), None);
    assert_eq!(
        host.refused_starts(),
        HAND_OVER_TRIES as usize,
        "{}",
        host.logs()
    );
    host.assert_installed(N);
}

/// The host's row of hostkeeper as `Hosts.Get` lists it (the `services` of the host the
/// coordinator knows), `None` while the host or the entry is not listed.
fn on_host(coordinator: &CoordinatorHandle) -> Option<Value> {
    coordinator.host(HOST_ID)["services"]
        .as_array()?
        .iter()
        .find(|service| service["service"] == HK)
        .cloned()
}

/// Waits until the coordinator reports hostkeeper converged on `version`, the host running it.
fn wait_converged(host: &MacSelfHost, coordinator: &CoordinatorHandle, version: &str) {
    wait_for(
        &format!("the coordinator reads hostkeeper converged on {version}"),
        COORDINATED,
        || {
            host.check_not_stuck();
            let convergence = coordinator.converge(HK);
            let host_row = coordinator.status(HK)["hosts"]
                .as_array()
                .and_then(|hosts| hosts.iter().find(|row| row["nodeId"] == HOST_ID))
                .cloned()
                .unwrap_or(Value::Null);
            convergence["state"] == "CONVERGENCE_STATE_CONVERGED"
                && convergence["desired"] == version
                && host_row["beat"]["version"] == version
        },
    );
}

/// Catches the farm's Mac whose self-update succeeded and never baked: the daemon observed its own
/// service as `unmonitored` (the package of hostkeeper names no unit; the daemon's launchd job is
/// the installer's), so it reported it unhealthy, and the new binary reported no verdict for the
/// update, so the coordinator kept the host's older verdict. The real coordinator in the test's
/// process, whose rollout asks two healthy heartbeats of a host at its version, is set to [`N`]
/// by the `macos-pkg` entry of its `.pkg` and reads it converged with no order, since the daemon
/// runs that version (its package not yet installed); then it is set to [`NEXT`] and orders
/// [`NEXT`]: an install or an update, as the last heartbeat it read listed hostkeeper (no package
/// of it is installed yet, so either may come first), and the daemon updates itself through it
/// and restarts into [`NEXT`] under launchd; the coordinator reads hostkeeper converged at
/// [`NEXT`], the host up and healthy, its last update that order's `Succeeded`, while the package
/// is installed at [`NEXT`] and the job runs it.
#[test]
fn the_coordinator_bakes_and_converges_a_mac_that_updated_itself_under_launchd() {
    let scratch = Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "su-bake")
        .expect("the coordinator's directory");
    let coordinator = CoordinatorHandle::start_with_rollout(
        &scratch.path().join("coordinator"),
        HK,
        ROLE,
        "bake_beats = 2\n",
    );
    let config = format!(
        "coordinator = \"{}\"\nca_sha256 = \"{}\"\n{CONFIG_TAIL}",
        coordinator.heartbeat_url(),
        coordinator.ca_sha256(),
    );
    let host = MacSelfHost::new_coordinated("bake", &config);
    let old_pid = host.runs(N);
    coordinator.approve_enrollment(HOST_ID, HK);

    coordinator.set_desired_targets_alone(HK, N, host.targets(N));
    wait_converged(&host, &coordinator, N);
    // The host already runs N, so the coordinator orders it nothing and no package is installed
    // yet: the update to NEXT below is the first install of hostkeeper's package (no receipt and
    // no record, which is no drift).
    assert_eq!(receipt_version(), None, "{}", host.logs());
    assert_eq!(
        host.runs(N),
        old_pid,
        "the install of the running version restarted nothing"
    );

    coordinator.set_desired_targets_alone(HK, NEXT, host.targets(NEXT));
    wait_converged(&host, &coordinator, NEXT);
    let rec = host.settled(NEXT);
    assert_succeeded(&rec, NEXT);
    assert_ne!(host.runs(NEXT), old_pid, "launchd started a new process");
    assert_eq!(
        host.layout().previous().expect("previous").as_deref(),
        Some(N)
    );
    assert_staged_for_the_entry(&host);
    host.assert_installed(NEXT);

    let mut last = None;
    // Each change of the coordinator's row and of the heartbeat's entry of hostkeeper is printed,
    // so that a wait that never ends shows which of them stayed short of the update.
    let mut seen = String::new();
    wait_for(
        "the coordinator lists hostkeeper at the new version, healthy, with the update's verdict",
        COORDINATED,
        || {
            last = on_host(&coordinator);
            let beat = coordinator.host(HOST_ID)["heartbeat"]["services"]
                .as_array()
                .and_then(|services| services.iter().find(|service| service["name"] == HK))
                .cloned();
            let now = format!("row {last:?}\nheartbeat {beat:?}");
            if now != seen {
                println!("{now}");
                seen = now;
            }
            last.as_ref().is_some_and(|row| {
                row["installedVersion"] == NEXT
                    && row["health"] == "HEALTH_HEALTHY"
                    && row["up"] == true
                    && row["lastUpdate"]["target"] == NEXT
                    && row["lastUpdate"]["outcome"] == "OUTCOME_SUCCEEDED"
                    && row["lastUpdate"]["orderId"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("install-") || id.starts_with("update-"))
            })
        },
    );
    println!("The coordinator's row of hostkeeper on the host: {last:?}");
}
