#![cfg(feature = "privileged")]
//! hostkeeper's update of itself under the host's systemd, as root: the daemon runs as the main
//! unit made of `packaging/linux/hostkeeper.service`, with the drop-in and the fallback unit of
//! `hostkeeper_steps::selfupdate`, from `current` of its binary directory, and is ordered to the
//! next version (`support::self_update` says how the host is made and what is a fixture).
//!
//! What each test proves:
//! - a next binary that starts settles the update `Succeeded` and is what the unit runs;
//! - a next binary that cannot start makes systemd spend the start limit and fail the unit,
//!   `OnFailure=` runs the fallback, which puts the previous binary back (`flip_back`) and
//!   records the update `Failed` and the version bad, and the unit runs the previous binary;
//! - the version that fell back, offered again, is refused from the bad list and nothing moves;
//! - a daemon killed with `SIGKILL` while its update waits for the restart is started again by
//!   systemd, and that start settles the update: `Succeeded` when the kill came after the
//!   follow-on's switch, `Failed(self_update_fallback)` when it came before it.
//!
//! Every wait polls systemd or the files for a state (`support::wait_until`) and stops at once
//! when the state can no longer come; no test asserts how long anything took.

mod support;

use std::fs;
use std::os::unix::fs::MetadataExt;

use hostkeeper_hook::{Class, Phase, Verdict};
use hostkeeper_steps::record::{Record, State};
use hostkeeper_steps::selfupdate::START_LIMIT_BURST;
use rustix::process::{Pid, Signal, kill_process};
use support::self_update::{FALLBACK_REASON, HK, Host, N, NEXT, Plan, count, refused_line};
use support::{journal, show, wait_until};

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

/// Catches a self-update whose new binary never runs (`current` not switched, the unit not
/// restarted on the clean exit, the unit running another file), or whose record is left
/// waiting or is closed by the wrong binary: the record says `Succeeded` once the new binary
/// started, `current` names it, `previous` names the old one, and the unit's main process runs
/// the new binary's file; the new daemon reports itself as the new version with the verdict.
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
    assert!(!host.next_is_bad());
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

/// Catches a fallback that never runs (no `OnFailure=`, no start limit, a restart policy that
/// gives up without failing the unit), a fallback that runs and leaves `current` on the bad
/// binary, and a fallback that does not record the failure: systemd starts the new binary until
/// the start limit (the window's starts are the old binary's first one and then the new one's)
/// and fails the unit with `start-limit-hit`; the fallback unit runs once and ends well; the
/// record says `Failed` after the commit, of class release, `self_update_fallback`; the version
/// is bad; and the unit runs the old binary again, `current` naming it.
#[test]
fn a_next_binary_that_cannot_start_spends_the_start_limit_and_the_fallback_puts_the_old_one_back() {
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
    assert_eq!(
        host.layout().previous().expect("previous").as_deref(),
        Some(N)
    );

    let main = journal(&host.main);
    assert_eq!(
        count(&main, &refused_line(NEXT)),
        START_LIMIT_BURST as usize - 1,
        "the new binary started on every start the limit left after the old one's\n{main}"
    );
    assert!(
        main.contains("UNIT_RESULT=start-limit-hit"),
        "systemd failed the unit at its start limit\n{main}"
    );
    wait_until(|| {
        host.check_not_stuck();
        support::property(&host.fallback, "ActiveState") == "inactive"
    });
    let fallback = show(
        &host.fallback,
        &["InvocationID", "Result", "ExecMainStatus"],
    );
    assert!(
        !fallback["InvocationID"].is_empty(),
        "the fallback ran: {fallback:?}"
    );
    assert_eq!(fallback["Result"], "success", "{fallback:?}");
    assert_eq!(fallback["ExecMainStatus"], "0", "{fallback:?}");
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
/// follow-on's switch and its exit: checks that the record waits for the restart into
/// [`NEXT`] and that `current` names it, and gives the daemon's pid.
fn held_after_the_switch(host: &Host) -> u32 {
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
    assert_eq!(
        waiting
            .progress
            .awaiting_restart
            .as_ref()
            .map(|awaiting| awaiting.version.as_str()),
        Some(NEXT),
        "{waiting:?}"
    );
    assert_eq!(host.current().as_deref(), Some(NEXT));
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

/// Catches a self-update left in progress when its daemon is killed after the follow-on's
/// switch and before its exit (a record that keeps waiting, a unit not started again after
/// `SIGKILL`, a start that does not settle): systemd starts `current`, now the new binary, and
/// its start settles the record `Succeeded`; the unit runs the new binary.
#[test]
fn a_daemon_killed_after_the_switch_is_settled_succeeded_by_the_new_binarys_start() {
    let host = Host::start(
        "su_kill_switched",
        Plan {
            hold_exit: true,
            ..Plan::default()
        },
    );
    let pid = held_after_the_switch(&host);
    let rec = kill_and_settle(&host, pid);
    assert_finished(&rec, succeeded(), None);
    host.runs(NEXT);
    assert!(!host.next_is_bad());
}

/// Catches a self-update left in progress, or confirmed by the wrong binary, when its daemon is
/// killed after the follow-on wrote the record and before its switch: the binary directory is
/// put back as it was before the switch (`current` names the old version and there is no
/// `previous`; the switch is the rename of those links, so a kill before it leaves exactly
/// this), then the daemon is killed. systemd starts the old binary, and its start settles the
/// record `Failed(self_update_fallback)` and the version bad; the unit runs the old binary.
#[test]
fn a_daemon_killed_before_the_switch_is_settled_fell_back_by_the_old_binarys_start() {
    let host = Host::start(
        "su_kill_unswitched",
        Plan {
            hold_exit: true,
            ..Plan::default()
        },
    );
    let pid = held_after_the_switch(&host);
    let layout = host.layout();
    let tmp = layout.dir().join("current.rewind");
    std::os::unix::fs::symlink(
        layout
            .version_path(N)
            .strip_prefix(layout.dir())
            .expect("in the directory"),
        &tmp,
    )
    .expect("a link to the old binary");
    fs::rename(&tmp, layout.current_path()).expect("current back on the old binary");
    fs::remove_file(layout.previous_path()).expect("no previous");
    assert_eq!(host.current().as_deref(), Some(N));

    let rec = kill_and_settle(&host, pid);
    assert_finished(&rec, fell_back(), Some(FALLBACK_REASON));
    assert!(host.next_is_bad());
    host.runs(N);
}
