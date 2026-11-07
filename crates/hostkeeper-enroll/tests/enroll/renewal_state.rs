//! What the loop notes after each step for the heartbeat ([`RenewalState`]): the end of the
//! current certificate, and the error of the step, cleared by the next step that succeeds.

use std::fs;

use hostkeeper_enroll::{Enroller, Enrollment, Reason, Renewal, RenewalState};

use crate::support::{HOST, T0, WEEK, World};

/// Runs `enroller`'s loop for `steps` steps, calling `before(n)` before step `n`, and returns
/// what `renewal` held after each step, with the enrollment the loop reported beside it.
fn noted(
    enroller: &mut Enroller,
    renewal: &RenewalState,
    steps: usize,
    mut before: impl FnMut(usize),
) -> Vec<(Renewal, Enrollment)> {
    let mut after = Vec::new();
    let mut step = 0;
    enroller.run_loop(
        &mut |_: &str| {},
        &mut |_, enrollment| after.push((renewal.get(), enrollment)),
        renewal,
        &mut || {
            let go = step < steps;
            if go {
                before(step);
            }
            step += 1;
            go
        },
    );
    after
}

fn not_after(enrollment: &Enrollment) -> u64 {
    match enrollment {
        Enrollment::Enrolled { not_after, .. } => *not_after,
        other => panic!("not enrolled: {other:?}"),
    }
}

/// Catches an error that is not noted, a noted error that a later success does not clear, and
/// a failed renewal that loses or moves the end of the certificate the host still holds: the
/// host waits, is enrolled, renews, is refused a renewal (the error, and the old end), and
/// renews again (no error, the new end).
#[test]
fn a_failed_renewal_keeps_the_old_end_with_its_error_and_a_success_clears_it() {
    let world = World::new("renewal-state");
    let mut enroller = world.open();
    let renewal = RenewalState::default();
    let after = noted(&mut enroller, &renewal, 5, |step| match step {
        1 => world.coordinator().approve(HOST),
        2 => world.set_now(T0 + WEEK / 2),
        3 => {
            world.set_now(T0 + WEEK);
            world.coordinator().renew_refusal = Reason::new("host_excluded");
        }
        4 => world.coordinator().renew_refusal = None,
        _ => {}
    });
    let ends: Vec<Option<u64>> = after.iter().map(|(noted, _)| noted.not_after).collect();
    let first = not_after(&after[1].1);
    let second = not_after(&after[2].1);
    let third = not_after(&after[4].1);
    assert_eq!(first, T0 + WEEK);
    assert!(first < second && second < third, "{first} {second} {third}");
    assert_eq!(
        ends,
        [None, Some(first), Some(second), Some(second), Some(third)]
    );
    assert_eq!(not_after(&after[3].1), second, "the refusal stored nothing");
    let errors: Vec<Option<&str>> = after
        .iter()
        .map(|(noted, _)| noted.last_error.as_deref())
        .collect();
    assert_eq!(
        errors,
        [
            None,
            None,
            None,
            Some("renewal refused: host_excluded"),
            None
        ]
    );
}

/// Catches an unreadable pki directory read as a host with no certificate: the step's error is
/// noted, and the end of the certificate noted before is kept.
#[test]
fn a_step_that_cannot_read_the_pki_directory_keeps_the_end_noted_before() {
    let world = World::new("renewal-state-corrupt");
    let mut enroller = world.open();
    let renewal = RenewalState::default();
    let after = noted(&mut enroller, &renewal, 3, |step| match step {
        1 => world.coordinator().approve(HOST),
        2 => fs::write(world.pki().join("gen-1/cert.pem"), "not a certificate").expect("cert"),
        _ => {}
    });
    assert_eq!(
        after[2].0,
        Renewal {
            not_after: Some(T0 + WEEK),
            last_error: Some(
                "the pki directory's generation 1 is not a certificate and key of this host"
                    .to_owned()
            ),
        }
    );
    assert!(matches!(after[2].1, Enrollment::Unreadable(_)));
}

/// Catches a state that starts with something in it, and an error noted before any step (the
/// enroller could not be opened) that moves the end of the certificate: the state starts empty,
/// and `failed` sets the error alone.
#[test]
fn the_state_starts_empty_and_an_error_before_any_step_sets_the_error_alone() {
    let world = World::new("renewal-state-failed");
    let mut enroller = world.enrolled();
    let renewal = RenewalState::default();
    assert_eq!(renewal.get(), Renewal::default());
    noted(&mut enroller, &renewal, 1, |_| {});
    let shared = renewal.clone();
    shared.failed("the pki directory is locked".to_owned());
    assert_eq!(
        renewal.get(),
        Renewal {
            not_after: Some(T0 + WEEK),
            last_error: Some("the pki directory is locked".to_owned()),
        },
        "a clone shares the state"
    );
}
