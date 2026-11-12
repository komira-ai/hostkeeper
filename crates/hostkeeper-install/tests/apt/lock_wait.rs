//! The wait for the package manager's lock (packages-repositories.md section 4.5): a tool with no
//! lock timeout of its own (`apt-mark`, `dpkg --purge`) that finds the lock held is tried again
//! every two seconds on the installer's clock, for at most `lock_wait_secs`, the wait reported at
//! its start and every thirty seconds; a lock held to the end is `package_manager_busy`,
//! deferred, the host's. The clock is the harness's [`RecPause`], which waits for nothing: the
//! tests count the pauses, never time.

use hostkeeper_install::repository::{Class, LockWait, Outcome, Reason, Tool, ToolOutput};

use crate::harness::*;

/// What the fake's apt-mark prints when the lock is held, its last `E:` line first.
const APT_HOLDER: &str = "apt-mark: E: Unable to acquire the dpkg frontend lock \
                          (/var/lib/dpkg/lock-frontend), is another process using it?";

/// A wait of `tool` reported after `waited_secs`, the harness's bound, the fake's holder.
fn reported(tool: Tool, waited_secs: u64, holder: &str) -> LockWait {
    LockWait {
        tool,
        waited_secs,
        bound_secs: LOCK_WAIT_SECS,
        holder: holder.to_owned(),
    }
}

/// Proves an install whose hold finds the lock held for three tries waits for it: three pauses
/// of two seconds, a fourth `apt-mark hold` that gets it, and the version installed, held and
/// recorded done; the wait is reported once, at its start, naming apt-mark and the holder.
/// Catches: an `activate` that fails at the first lock its hold finds (an unattended upgrade
/// starting between the install and the hold).
#[test]
fn an_install_whose_hold_finds_the_lock_held_for_three_polls_succeeds() {
    let env = Env::new("apt-lock-hold-released");
    let (order, dir) = env.staged(&mdm_deb(V1), &RUN3);
    env.host.edit(|s| {
        s.faults.busy.insert("hold", 3);
    });
    env.installer()
        .activate(&order, &dir)
        .expect("installed once the lock is let go");
    assert_eq!(env.pause.calls(), [2, 2, 2]);
    assert_eq!(env.host.calls_of("apt-mark hold").len(), 4);
    let installed = env.host.installed(PACKAGE).expect("installed");
    assert_eq!((installed.version.as_str(), installed.held), (V1, true));
    let record = env.records.get(SERVICE).expect("a record");
    assert!(!record.pending, "the record is done");
    assert_eq!(env.pause.waits(), [reported(Tool::AptMark, 0, APT_HOLDER)]);
}

/// Proves an install whose hold never gets the lock ends `package_manager_busy`, deferred,
/// environment, after a try every two seconds for all of `lock_wait_secs` (six tries, five
/// pauses for ten seconds), its detail naming the wait, the bound, the tool and the holder whole;
/// the record stays pending, so the next try of the order finds the install its own.
/// Catches: a lock held to the bound reported as a failure of the release, or a wait that never
/// ends.
#[test]
fn an_install_whose_hold_never_gets_the_lock_is_deferred() {
    let env = Env::new("apt-lock-hold-held");
    let (order, dir) = env.staged(&mdm_deb(V1), &RUN3);
    env.host.edit(|s| {
        s.faults.busy.insert("hold", u32::MAX);
    });
    let failure = env
        .installer()
        .activate(&order, &dir)
        .expect_err("the lock is never let go");
    assert_failure(
        &failure,
        Reason::PackageManagerBusy,
        Class::Environment,
        Outcome::Deferred,
    );
    assert_eq!(
        failure.detail(),
        format!("the package manager's lock was held for 10s of 10s (apt-mark): {APT_HOLDER}")
    );
    assert_eq!(env.pause.calls(), [2, 2, 2, 2, 2]);
    assert_eq!(env.host.calls_of("apt-mark hold").len(), 6);
    assert!(env.records.get(SERVICE).expect("a record").pending);
    assert_eq!(env.pause.waits(), [reported(Tool::AptMark, 0, APT_HOLDER)]);
}

/// Proves a long wait is reported at its start and every thirty seconds while it lasts, each
/// report with the seconds waited so far and the bound: a purge whose lock is never let go within
/// a bound of 64 seconds is reported at 0, 30 and 60 seconds, tried 33 times with 32 pauses, and
/// ends `package_manager_busy` at 64 seconds, never past the bound.
/// Catches: a wait reported at every try (a line every two seconds), or never again after its
/// start.
#[test]
fn a_long_wait_is_reported_every_thirty_seconds() {
    let mut env = Env::new("apt-lock-reports");
    env.installed(&mdm_deb(V1), &RUN3);
    env.lock_wait_secs = 64;
    env.host.edit(|s| {
        s.faults.busy.insert("purge", u32::MAX);
    });
    let failure = env
        .installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect_err("busy");
    assert_eq!(failure.reason(), Reason::PackageManagerBusy);
    assert!(
        failure
            .detail()
            .starts_with("the package manager's lock was held for 64s of 64s (dpkg): "),
        "{failure}"
    );
    assert_eq!(env.pause.calls(), vec![2; 32]);
    assert_eq!(env.host.calls_of("dpkg --purge").len(), 33);
    let holder = "dpkg: error: dpkg frontend lock was locked by another process with pid 4321";
    let waits: Vec<(u64, u64)> = env
        .pause
        .waits()
        .iter()
        .inspect(|wait| assert_eq!((wait.tool, wait.holder.as_str()), (Tool::Dpkg, holder)))
        .map(|wait| (wait.waited_secs, wait.bound_secs))
        .collect();
    assert_eq!(waits, [(0, 64), (30, 64), (60, 64)]);
}

/// Proves a remove's release of the hold waits for the lock as the hold does: `apt-mark unhold`
/// finding the lock held twice is tried a third time, and the package is then purged.
/// Catches: a remove that fails at a lock its first tool finds, before the purge's own wait.
#[test]
fn a_remove_whose_unhold_finds_the_lock_held_waits_and_purges() {
    let env = Env::new("apt-lock-unhold");
    env.installed(&mdm_deb(V1), &RUN3);
    env.host.edit(|s| {
        s.faults.busy.insert("hold", 2);
    });
    env.installer()
        .remove(&service(SERVICE), REMOVE_SECS)
        .expect("removed once the lock is let go");
    assert_eq!(env.pause.calls(), [2, 2]);
    assert_eq!(env.host.calls_of("apt-mark unhold").len(), 3);
    assert!(env.host.installed(PACKAGE).is_none());
    assert!(env.records.get(SERVICE).is_none());
}

/// Proves a try that fails for another reason than the lock ends the wait at once with its own
/// failure: an `apt-mark hold` that finds the lock held once, then fails, is `io_error` after one
/// pause, not tried again.
/// Catches: a wait that retries any failure until its bound, hiding a fault of the host for
/// minutes.
#[test]
fn a_failure_that_is_not_the_lock_ends_the_wait() {
    let env = Env::new("apt-lock-other-failure");
    let (order, dir) = env.staged(&mdm_deb(V1), &RUN3);
    env.host.edit(|s| {
        s.faults.busy.insert("hold", 1);
    });
    env.host.answer(
        "apt-mark hold",
        1,
        Ok(ToolOutput {
            code: Some(1),
            stdout: Vec::new(),
            stderr: b"E: Unable to write the selections\n".to_vec(),
            cut: false,
        }),
    );
    let failure = env
        .installer()
        .activate(&order, &dir)
        .expect_err("the hold fails");
    assert_eq!(
        (failure.reason(), failure.detail()),
        (
            Reason::IoError,
            "apt-mark: E: Unable to write the selections"
        )
    );
    assert_eq!(env.pause.calls(), [2]);
    assert_eq!(env.host.calls_of("apt-mark hold").len(), 2);
}
