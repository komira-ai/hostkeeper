#![cfg(feature = "privileged")]
//! hostkeeper's update of itself under the host's systemd, as root: the daemon runs as the main
//! unit made of hostkeeper's package unit (`packaging/linux/units/hostkeeper.service`), which
//! starts `current` of its binary directory, or of the bootstrap unit the deb installs
//! (`packaging/linux/hostkeeper.service`), which starts the packaged binary. It is ordered to the
//! next version, which it hands over to: it stages the binary, links `next` at it and exits 0;
//! the unit starts the binary `current` names, the old one, which execs the new one while its
//! start count lasts (`support::self_update` says how the host is made and what is a fixture).
//!
//! What each test proves:
//! - a next binary that starts settles the update `Succeeded` and is what the unit runs;
//! - a next binary that cannot start is started three times by the old binary, which then
//!   settles the update `Failed` and the version bad and runs as the daemon, with `current`
//!   never moved; systemd's unit is never failed;
//! - the version that fell back, offered again, is refused from the bad list and nothing moves;
//! - a daemon killed with `SIGKILL` while its update waits for the restart is started again by
//!   systemd, and the hand-over goes on: killed after the exec, the new binary's next start
//!   settles the update `Succeeded`; killed before it, with a next binary that cannot start,
//!   the old binary falls back after three starts of the new one;
//! - a unit that cannot start while no update waits is retried until it can: it has no start
//!   limit, so systemd never leaves it failed.
//!
//! - a daemon the deb installed and enabled (the bootstrap unit over a packaged binary named by
//!   its version, an empty binary directory, no version of hostkeeper in the packages directory)
//!   starts by seeding the binary directory with the packaged binary and running `current`; it is
//!   ordered the install of the next release, whose `.deb` carries the package unit: the install
//!   stages it, the unit restarts the packaged binary, which runs `current`, which execs the new
//!   binary, which settles the install `Succeeded`, installs the package and reports it.
//!
//! The next release is a native package (`support::self_update` says how): the order carries the
//! `apt` entry of its `.deb`, which the daemon stages from the test's signed apt repository, and
//! whose package the new binary, once it confirmed itself, installs with apt.
//!
//! Every wait polls systemd or the files for a state (`support::wait_until`) and stops at once
//! when the state can no longer come; no test asserts how long anything took.

mod support;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_privileged::rig::service_unit;
use hostkeeper_steps::handover::HAND_OVER_TRIES;
use hostkeeper_steps::record::{Record, State};
use hostkeeper_steps::systemd::SYSTEMCTL;
use rustix::process::{Pid, Signal, kill_process};
use support::self_update::{
    DEB_UNIT, FALLBACK_REASON, HK, Host, N, NEXT, Plan, REAL_UNIT, count, refused_line,
};
use support::{Bench, journal, property, run, run_ok, wait_until};

/// The record of an update to [`NEXT`] that ended with `verdict` and `reason`, and waits for
/// nothing.
fn assert_finished(rec: &Record, verdict: Verdict, reason: Option<&str>) {
    assert_eq!(rec.target_version, NEXT, "{rec:?}");
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

/// Checks that the daemon staged [`NEXT`] of hostkeeper for the entry that the order carried: the
/// version directory it left holds that entry as its sealed `target.json`.
fn assert_staged_for_the_entry(host: &Host) {
    assert_eq!(
        host.staged_entry().as_ref(),
        Some(host.entry()),
        "the daemon staged the version for the entry of the order\n{}",
        host.journals()
    );
}

fn succeeded() -> Verdict {
    Verdict::Succeeded {
        promotion_pending: false,
    }
}

fn fell_back() -> Verdict {
    Verdict::Failed {
        phase: Phase::AfterCommit,
        class: Class::Release,
    }
}

/// Catches a self-update whose new binary never runs (`next` not linked, the unit not restarted
/// on the clean exit, the old binary not execing the new one, the unit running another file), or
/// whose record is left waiting or is closed by the wrong binary: the record says `Succeeded`
/// once the new binary started, `current` names it (its start finished the switch), `previous`
/// names the old one, `next` is gone, and the unit's main process runs the new binary's file; the
/// new daemon reports itself as the new version with the verdict.
#[test]
fn a_next_binary_that_starts_settles_the_update_succeeded_and_is_what_the_unit_runs() {
    let host = Host::start("su_good", Plan::default());
    host.runs(N);
    host.offer(1);

    let rec = host.settled();
    assert_finished(&rec, succeeded(), None);
    host.runs(NEXT);
    assert_eq!(
        host.layout().previous().expect("previous").as_deref(),
        Some(N)
    );
    assert_eq!(host.layout().next().expect("next"), None, "next is removed");
    assert!(!host.next_is_bad());
    assert_staged_for_the_entry(&host);
    wait_until(|| {
        host.check_not_stuck();
        host.beats().iter().any(|beat| {
            beat["agent"]["version"] == NEXT
                && beat["services"].as_array().is_some_and(|services| {
                    services.iter().any(|service| {
                        service["name"] == HK
                            && service["last_update"]["order_try"] == 1
                            && service["last_update"]["outcome"] == "succeeded"
                    })
                })
        })
    });
}

/// Catches a deb whose bootstrap unit does not bring the daemon up (the packaged binary not
/// seeding the empty binary directory, not running `current`, or its unit restarted into the
/// packaged binary alone) and the self-update of a daemon the deb installed stopping after it
/// placed the package
/// (the deb's enable link refused as a placed path that `placed.json` does not list, or a verdict
/// never reported for a service that is not listed): ordered the install of [`NEXT`], the host
/// runs it as its self-update; the record
/// settles `Succeeded` once the new binary started; the unit's main process runs the new binary,
/// `current` naming it and `previous` the deb's; the package is installed (`current` of the
/// packages directory) with its unit linked in the unit directory and the deb's link left as
/// the deb made it; the new daemon reports itself and hostkeeper at the new version with the
/// install's verdict. Before the order, `current` names [`N`] (seeded by the packaged start,
/// which left nothing `previous`) and the unit's main process runs `versions/N`; after it, the
/// unit's main process runs `versions/NEXT` (the bootstrap unit restarted the packaged binary,
/// which ran `current`, which exec'd `next`): a stage-2 binary started from the bootstrap unit
/// seeds and hands over. The install is set by targets: the order carries the `apt` entry of the
/// release (the repository, the package version, the pin, a manifest with its floor), which passes
/// the daemon's receipt check, and the daemon stages the version for that entry. Catches: an entry
/// the daemon refuses (`order_invalid`, `manifest_invalid`, `installer_not_listed`) on the host the
/// deb leaves, or one it drops on its way to the stage.
#[test]
fn a_daemon_the_deb_installed_is_installed_its_next_release_and_restarts_into_it() {
    let host = Host::start(
        "su_deb",
        Plan {
            deb: true,
            ..Plan::default()
        },
    );
    host.runs(N);
    assert_eq!(host.current().as_deref(), Some(N));
    assert_eq!(host.layout().previous().expect("previous"), None);
    host.offer_install(1);

    let rec = host.settled();
    assert_finished(&rec, succeeded(), None);
    host.runs(NEXT);
    assert_staged_for_the_entry(&host);
    assert_eq!(
        host.layout().previous().expect("previous").as_deref(),
        Some(N)
    );
    let paths = host.paths();
    assert_eq!(
        fs::read_link(paths.packages.join(HK).join("current")).ok(),
        Some(PathBuf::from(NEXT))
    );
    wait_until(|| host.package_version().as_deref() == Some(NEXT));
    assert!(host.package_held(), "the installed package is held");
    assert!(
        fs::symlink_metadata(DEB_UNIT).is_ok_and(|meta| meta.is_file()),
        "the package's unit is installed"
    );
    wait_until(|| {
        host.check_not_stuck();
        host.beats().iter().any(|beat| {
            beat["agent"]["version"] == NEXT
                && beat["services"].as_array().is_some_and(|services| {
                    services.iter().any(|service| {
                        service["name"] == HK
                            && service["version"] == NEXT
                            && service["last_update"]["order_try"] == 1
                            && service["last_update"]["outcome"] == "succeeded"
                    })
                })
        })
    });
}

/// Catches a hand-over that does not fall back, or falls back by moving `current`: the unit
/// restarts the old binary after its clean exit, which execs the new one; the new binary exits 1
/// at its start, so systemd restarts the old binary, which execs it again, and a third time; at
/// the fourth start the count is spent, the old binary settles the update `Failed` after the
/// commit, of class release, `self_update_fallback`, marks the version bad and runs as the daemon.
/// The new binary was started exactly [`HAND_OVER_TRIES`] times, `current` never moved (no
/// `previous`, no `next`), the unit runs the old binary again, and systemd never failed the unit
/// (no start limit, so no `OnFailure=` either).
/// The update is set by targets: the order carries the `apt` entry of the release, which the old
/// daemon's receipt check passes and whose version it stages, so the fall-back is proven for the
/// version staged through an entry. The new binary never confirmed itself, so its package is never
/// installed: dpkg has no package of hostkeeper. Catches: an entry order whose new binary is staged and
/// handed over to but never falls back, or an entry the old daemon refuses before it stages.
#[test]
fn a_next_binary_that_cannot_start_three_times_is_fallen_back_from_by_the_old_one() {
    let host = Host::start(
        "su_bad",
        Plan {
            next_fails: true,
            ..Plan::default()
        },
    );
    host.runs(N);

    let rec = host.fall_back();
    assert_finished(&rec, fell_back(), Some(FALLBACK_REASON));
    assert!(host.next_is_bad());
    assert_staged_for_the_entry(&host);
    assert_eq!(
        host.package_version(),
        None,
        "the package of a binary that never confirmed itself is not installed"
    );
    assert_eq!(host.layout().previous().expect("previous"), None);
    assert_eq!(host.layout().next().expect("next"), None);

    let main = journal(&host.main);
    assert_eq!(
        count(&main, &refused_line(NEXT)),
        HAND_OVER_TRIES as usize,
        "the new binary was started once per try of the hand-over\n{main}"
    );
    for refused in [
        ": Start request repeated too quickly.",
        ": Triggering OnFailure= dependencies.",
    ] {
        assert!(
            !main.contains(refused),
            "systemd spent no start limit: {refused}\n{main}"
        );
    }
    assert_ne!(property(&host.main, "ActiveState"), "failed");
}

/// Catches a version that fell back being tried again (staged, switched to, or its hook run),
/// and a refusal that rewrites the record or moves a link: the same version offered as a new
/// try is answered `Failed` after the commit, `self_update_fallback`, and the record file,
/// `current` (the same link, not one made again) and the running daemon are as they were.
#[test]
fn the_version_that_fell_back_offered_again_is_refused_from_the_bad_list_and_nothing_moves() {
    let host = Host::start(
        "su_again",
        Plan {
            next_fails: true,
            ..Plan::default()
        },
    );
    host.runs(N);
    host.fall_back();
    let record = host.record_bytes();
    let current_path = host.layout().current_path();
    let link = fs::symlink_metadata(&current_path).expect("current").ino();
    let pid = host.main_pid();
    let starts = count(&journal(&host.main), &refused_line(NEXT));

    host.offer(2);
    let mut verdict = None;
    wait_until(|| {
        host.check_not_stuck();
        verdict = host.verdict_of(2);
        verdict.is_some()
    });
    let verdict = verdict.expect("the verdict of try 2");
    assert_eq!(verdict["outcome"], "failed", "{verdict}");
    assert_eq!(verdict["reason"], FALLBACK_REASON, "{verdict}");
    assert_eq!(verdict["phase"], "after_commit", "{verdict}");
    assert_eq!(verdict["class"], "release", "{verdict}");

    assert_eq!(
        host.record_bytes(),
        record,
        "the record is as the fallback left it"
    );
    assert_eq!(
        fs::symlink_metadata(&current_path).expect("current").ino(),
        link,
        "current is the link the fallback made"
    );
    assert_eq!(host.current().as_deref(), Some(N));
    assert_eq!(host.main_pid(), pid, "the daemon was not restarted");
    assert_eq!(host.main_exe(), host.binary_of(N));
    assert_eq!(
        count(&journal(&host.main), &refused_line(NEXT)),
        starts,
        "the new binary was not started again"
    );
}

/// The daemon of a host started with `hold_exit`, ordered to [`NEXT`] and held between the
/// follow-on's hand-over and its exit: checks that the record waits for the restart into
/// [`NEXT`] with `next` and a start count of 0, that `next` names it and `current` still names
/// [`N`], and gives the daemon's pid.
fn held_after_the_hand_over(host: &Host) -> u32 {
    host.runs(N);
    host.offer(1);
    let held = host.paths().held;
    let mut pid = None;
    wait_until(|| {
        host.check_not_stuck();
        pid = fs::read_to_string(&held)
            .ok()
            .filter(|text| text.ends_with('\n'))
            .and_then(|text| text.trim().parse::<u32>().ok());
        pid.is_some()
    });
    let pid = pid.expect("the pid of the held daemon");
    assert_eq!(
        pid,
        host.main_pid(),
        "the unit's main process holds its exit"
    );
    assert_eq!(host.main_exe(), host.binary_of(N));
    let waiting = host.record().expect("the record of the update");
    assert!(
        !matches!(waiting.state, State::Finished { .. }),
        "{waiting:?}"
    );
    let awaiting = waiting
        .progress
        .awaiting_restart
        .as_ref()
        .unwrap_or_else(|| panic!("{waiting:?}"));
    assert_eq!(
        (
            awaiting.version.as_str(),
            awaiting.next.as_deref(),
            awaiting.starts
        ),
        (NEXT, Some(NEXT), Some(0)),
        "{waiting:?}"
    );
    assert_eq!(host.current().as_deref(), Some(N));
    assert_eq!(host.layout().next().expect("next").as_deref(), Some(NEXT));
    pid
}

/// `SIGKILL` to `pid`, then checks systemd saw its unit's main process killed once the record
/// settled: the record is finished and waits for nothing.
fn kill_and_settle(host: &Host, pid: u32) -> Record {
    let raw = i32::try_from(pid).expect("a pid");
    kill_process(Pid::from_raw(raw).expect("a pid"), Signal::KILL)
        .expect("the held daemon is there to kill");
    let rec = host.settled();
    let main = journal(&host.main);
    assert!(
        main.contains("status=9/KILL"),
        "systemd saw the main process killed\n{main}"
    );
    rec
}

/// Catches a self-update left in progress when the new binary is killed after the hand-over
/// exec'd it and before it settled (a record that keeps waiting, a unit not started again after
/// `SIGKILL`, a second start that does not settle): the daemon hands over and exits; the unit
/// starts the old binary, which execs [`NEXT`], which is held at its start (the exec happened and
/// nothing is settled: the count is 1, `current` still [`N`]); it is killed; systemd starts the
/// old binary again, which execs [`NEXT`] a second time, and that start settles the record
/// `Succeeded`; the unit runs the new binary.
#[test]
fn a_daemon_killed_after_the_exec_is_settled_succeeded_by_the_new_binarys_start() {
    let host = Host::start(
        "su_kill_exec",
        Plan {
            hold_start: true,
            ..Plan::default()
        },
    );
    host.runs(N);
    host.offer(1);
    let held = host.paths().held_start;
    let mut pid = None;
    wait_until(|| {
        host.check_not_stuck();
        pid = fs::read_to_string(&held)
            .ok()
            .filter(|text| text.ends_with('\n'))
            .and_then(|text| text.trim().parse::<u32>().ok());
        pid.is_some()
    });
    let pid = pid.expect("the pid of the held new binary");
    assert_eq!(pid, host.main_pid(), "the unit's main process is held");
    assert_eq!(
        host.main_exe(),
        host.binary_of(NEXT),
        "the old binary exec'd the new one"
    );
    assert_eq!(
        host.current().as_deref(),
        Some(N),
        "nothing is switched yet"
    );
    let waiting = host.record().expect("the record of the update");
    assert_eq!(
        waiting
            .progress
            .awaiting_restart
            .as_ref()
            .and_then(|a| a.starts),
        Some(1),
        "{waiting:?}"
    );
    let rec = kill_and_settle(&host, pid);
    assert_finished(&rec, succeeded(), None);
    host.runs(NEXT);
    assert_eq!(
        host.layout().previous().expect("previous").as_deref(),
        Some(N)
    );
    assert_eq!(host.layout().next().expect("next"), None);
    assert!(!host.next_is_bad());
}

/// Catches a self-update left in progress, or confirmed by the wrong binary, when its daemon is
/// killed after the follow-on handed over and before it exits (before any exec): the old binary
/// is killed holding its exit; systemd starts it again, and the hand-over goes on from the
/// record: with a new binary that cannot start, the old binary execs it three times and at the
/// fourth start settles the update `Failed(self_update_fallback)` and the version bad; the unit
/// runs the old binary, and `current` was never moved.
#[test]
fn a_daemon_killed_before_the_exec_falls_back_after_three_starts() {
    let host = Host::start(
        "su_kill_before",
        Plan {
            hold_exit: true,
            next_fails: true,
            ..Plan::default()
        },
    );
    let pid = held_after_the_hand_over(&host);
    let rec = kill_and_settle(&host, pid);
    assert_finished(&rec, fell_back(), Some(FALLBACK_REASON));
    assert!(host.next_is_bad());
    host.runs(N);
    assert_eq!(host.layout().previous().expect("previous"), None);
    assert_eq!(host.layout().next().expect("next"), None);
    assert_eq!(
        count(&journal(&host.main), &refused_line(NEXT)),
        HAND_OVER_TRIES as usize,
        "the new binary was started once per try"
    );
}

/// The unit of a retry test, linked from the bench's directory, and the file whose absence keeps
/// it from starting.
struct Gated {
    main: String,
    gate: PathBuf,
}

/// The value of each of `keys` in the real unit [`REAL_UNIT`]: its lines that begin with one.
fn real_lines(keys: &[&str]) -> String {
    REAL_UNIT
        .lines()
        .filter(|line| keys.iter().any(|key| line.starts_with(key)))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// The unit of a retry test: `main`, the fixture's `serve` behind an `ExecStartPre=` that fails
/// until the gate file exists, with the start limit and restart settings of the real package
/// unit (`StartLimitIntervalSec=`, `Restart=`, `RestartSec=`, `RestartMode=`) in place of the
/// fixture's. `main` has been asked to start once, without waiting for the start job, which a
/// unit that is retried for good never finishes.
fn gated_rig(bench: &Bench) -> Gated {
    let rig = &bench.rig;
    let main = rig.unit();
    let gate = bench.scratch().join("gate");
    let serve = rig.unit_file_text().expect("the fixture's unit");
    let serve: String = serve
        .lines()
        .filter(|line| {
            !["StartLimit", "Restart"]
                .iter()
                .any(|key| line.starts_with(key))
        })
        .map(|line| format!("{line}\n"))
        .collect();
    let serve = serve
        .replacen(
            "[Unit]\n",
            &format!("[Unit]\n{}", real_lines(&["StartLimitIntervalSec="])),
            1,
        )
        .replacen(
            "[Service]\n",
            &format!(
                "[Service]\nExecStartPre=/usr/bin/test -e {}\n{}",
                gate.display(),
                real_lines(&["Restart=", "RestartSec=", "RestartMode="])
            ),
            1,
        );
    let main_file = bench.scratch().join(service_unit(&main));
    fs::write(&main_file, serve).expect("the main unit");
    run_ok(
        SYSTEMCTL,
        &["link", "--system", main_file.to_str().expect("UTF-8")],
    );
    // The start fails at its `ExecStartPre=`; systemd's restarts go on from there. With no start
    // limit the unit stays in its automatic restarts for good, which keeps a start job pending,
    // so the start is not waited for.
    let _ = run(
        SYSTEMCTL,
        &[
            "start",
            "--no-block",
            "--system",
            service_unit(&main).as_str(),
        ],
    );
    Gated { main, gate }
}

/// Catches the dead end of a daemon left failed when it cannot start and no update waits for its
/// restart, under the host's systemd: the main unit, with the start limit and restart settings
/// of the real package unit, fails more starts than the limit the releases before the hand-over
/// set (three within 300 seconds) and is still not failed, because it has none; once it can
/// start it is active.
#[test]
fn a_unit_that_cannot_start_with_no_update_waiting_is_retried_until_it_can() {
    let bench = Bench::new("retry_starts");
    let units = gated_rig(&bench);
    // More failed starts than the old limit, counted from the journal; the unit is never failed.
    let failed_starts = usize::try_from(HAND_OVER_TRIES).expect("a small count") + 2;
    wait_until(|| {
        assert_ne!(
            property(&units.main, "ActiveState"),
            "failed",
            "{}",
            journal(&units.main)
        );
        count(&journal(&units.main), "Control process exited") > failed_starts
    });
    let log = journal(&units.main);
    assert!(
        !log.contains("Start request repeated too quickly") && !log.contains("OnFailure="),
        "{log}"
    );
    fs::write(&units.gate, "").expect("open the gate");
    wait_until(|| property(&units.main, "ActiveState") == "active");
}
