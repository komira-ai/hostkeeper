//! The `rollback` row of the table for a rollback deferred by the host's package manager: once
//! its retries are spent, nothing committed, the update ends `Deferred` (ordered again later on
//! the same host), never in a `halt` that stops the service on every host. Every other deferred
//! or failed rollback keeps its row.

use hostkeeper_hook::{
    Class, Context, Next, Outcome, Phase, REASON_PACKAGE_MANAGER_BUSY, Report, Step, Verdict,
    is_reason_token, next,
};

/// The last line the daemon's installer ends a rollback with when the package manager's lock
/// stayed held: exit 75, `environment`, `package_manager_busy`, retry after 60 seconds.
fn busy() -> Report {
    Report::parse_line(
        r#"{"class":"environment","reason":"package_manager_busy","detail":"the package manager's lock was held for 300s of 300s (dpkg): dpkg: error: dpkg frontend lock was locked by another process with pid 2058171","retry_after_secs":60}"#,
    )
    .expect("the installer's line reads")
}

/// A context whose retries are spent (`retries_left` false), `committed` as given.
fn spent(committed: bool) -> Context {
    Context {
        committed,
        then: Some(Verdict::Failed {
            phase: Phase::BeforeCommit,
            class: Class::Release,
        }),
        ..Context::default()
    }
}

/// Proves the constant is the token the installer writes, and a token the line accepts.
/// Catches: a constant that drifts from `package_manager_busy`, so no rollback ever matches it.
#[test]
fn the_reason_is_the_installers_token() {
    assert_eq!(REASON_PACKAGE_MANAGER_BUSY, "package_manager_busy");
    assert!(is_reason_token(REASON_PACKAGE_MANAGER_BUSY));
    assert_eq!(busy().reason.as_deref(), Some(REASON_PACKAGE_MANAGER_BUSY));
    assert_eq!(busy().class, Class::Environment);
}

/// Proves a rollback still deferred for the package manager once its retries are spent, nothing
/// committed, ends the update `Deferred` with the rollback's own wait, whatever the verdict the
/// rollback was to report: the host is ordered again later, and the service is not halted.
/// Catches: a lock held through a first install's rollback (an unattended upgrade) halting the
/// service on every host.
#[test]
fn a_rollback_deferred_by_a_busy_package_manager_ends_deferred_not_in_a_halt() {
    assert_eq!(
        next(Step::Rollback, Outcome::Deferred, &busy(), &spent(false)),
        Next::Report(Verdict::Deferred {
            retry_after_secs: Some(60)
        })
    );
    let no_wait = Report {
        retry_after_secs: None,
        ..busy()
    };
    assert_eq!(
        next(
            Step::Rollback,
            Outcome::Deferred,
            &no_wait,
            &Context::default()
        ),
        Next::Report(Verdict::Deferred {
            retry_after_secs: None
        })
    );
}

/// Proves the rollback is still run again while its retries last: the new row applies only
/// once they are spent.
/// Catches: a busy rollback reported `Deferred` at its first deferral, leaving the host half
/// rolled back when a minute's wait would have finished it.
#[test]
fn a_busy_rollback_is_run_again_while_retries_are_left() {
    let left = Context {
        retries_left: true,
        ..spent(false)
    };
    assert_eq!(
        next(Step::Rollback, Outcome::Deferred, &busy(), &left),
        Next::Again {
            step: Step::Rollback,
            after_secs: Some(60)
        }
    );
}

/// Proves the boundaries of the row: after the commit (in the context, or `reversible: false` on
/// the rollback's own line) a busy rollback keeps its row (`halt`, or `Failed(after_commit)`); a
/// rollback that fails (exit 1) with the same reason is still a `halt`; and a rollback deferred
/// for any other reason (`leases_live`, none) is still a `halt` once its retries are spent.
/// Catches: a deferral that hides a host nothing can repair, or a rule that matches on the class
/// alone and so changes the row for every hook.
#[test]
fn every_other_spent_rollback_keeps_its_row() {
    assert_eq!(
        next(Step::Rollback, Outcome::Deferred, &busy(), &spent(true)),
        Next::Report(Verdict::Halt)
    );
    let committing = Report {
        reversible: false,
        reversible_stated: true,
        ..busy()
    };
    assert_eq!(
        next(
            Step::Rollback,
            Outcome::Deferred,
            &committing,
            &spent(false)
        ),
        Next::Report(Verdict::Failed {
            phase: Phase::AfterCommit,
            class: Class::Environment
        })
    );
    assert_eq!(
        next(Step::Rollback, Outcome::Failure, &busy(), &spent(false)),
        Next::Report(Verdict::Halt)
    );
    for reason in [Some("leases_live"), None] {
        let other = Report {
            reason: reason.map(str::to_owned),
            ..busy()
        };
        assert_eq!(
            next(Step::Rollback, Outcome::Deferred, &other, &spent(false)),
            Next::Report(Verdict::Halt),
            "{reason:?}"
        );
    }
}
