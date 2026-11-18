//! The retry after a failure, on the injected monotonic clock, and the pace of the loop.

use hostkeeper_enroll::{
    CHECK_SECS, IdentityFile, IssueAnswer, RETRY_AFTER_SECS, Reason, TransportError,
};

use crate::support::{HOUR, SERVICE, T0, World, wanted};

/// A world with a certificate installed whose renewal is due and answered by `answer`.
fn due_world(name: &str, answer: Result<IssueAnswer, TransportError>) -> World {
    let mut world = World::new(name);
    world.keeper.step(&[wanted(SERVICE)]);
    assert_eq!(world.issuer.calls().len(), 1);
    world.set_wall(T0 + 5 * HOUR);
    world.issuer.answer_with(move |_, _| answer.clone());
    world
}

/// Catches a refused service being asked again before five minutes have passed, or not after:
/// each of the coordinator's refusals and its answer of a coordinator without the call, and a
/// transport failure, keeps the installed certificate, is reported, and is asked again at five
/// minutes and not before.
#[test]
fn a_failed_service_is_asked_again_after_five_minutes_and_not_before() {
    let token = |t: &str| Ok(IssueAnswer::Refused(Reason::new(t).expect("a token")));
    let cases = [
        (token("service_not_assigned"), "service_not_assigned"),
        (token("identity_not_approved"), "identity_not_approved"),
        (token("denied"), "denied"),
        (Ok(IssueAnswer::Unimplemented), "unimplemented"),
        (
            Err(TransportError("cannot connect".to_owned())),
            "cannot connect",
        ),
    ];
    for (answer, reported) in cases {
        let mut world = due_world("retry", answer);
        let installed = world.read(SERVICE, IdentityFile::Cert);
        world.keeper.step(&[wanted(SERVICE)]);
        assert_eq!(world.issuer.calls().len(), 2, "asked at once: {reported}");
        assert_eq!(world.last_error(SERVICE).as_deref(), Some(reported));
        world.advance_mono(RETRY_AFTER_SECS - 1);
        world.keeper.step(&[wanted(SERVICE)]);
        assert_eq!(world.issuer.calls().len(), 2, "not asked at 299 seconds");
        assert_eq!(
            world.last_error(SERVICE).as_deref(),
            Some(reported),
            "the report is kept while the service is left alone"
        );
        world.advance_mono(1);
        world.keeper.step(&[wanted(SERVICE)]);
        assert_eq!(world.issuer.calls().len(), 3, "asked at 300 seconds");
        assert_eq!(world.read(SERVICE, IdentityFile::Cert), installed);
        assert_eq!(world.report(SERVICE).0, Some(T0 + 8 * HOUR));
    }
}

/// Catches a service that was refused staying refused after the coordinator says yes: the next
/// try installs, clears the error and the wait.
#[test]
fn a_success_after_a_refusal_clears_the_error() {
    let mut world = due_world("recovers", Ok(IssueAnswer::Unimplemented));
    world.keeper.step(&[wanted(SERVICE)]);
    assert_eq!(world.last_error(SERVICE).as_deref(), Some("unimplemented"));
    let ca = std::sync::Arc::clone(&world.ca);
    world.issuer.answer_with(move |service, csr| {
        let leaf = crate::support::Leaf::good(service, crate::support::HOST, T0 + 5 * HOUR);
        Ok(IssueAnswer::Issued {
            cert_der: leaf.sign(&ca, csr),
            ca_bundle_pem: ca.pem.clone(),
        })
    });
    world.advance_mono(RETRY_AFTER_SECS);
    world.keeper.step(&[wanted(SERVICE)]);
    assert_eq!(world.report(SERVICE), (Some(T0 + 13 * HOUR), None));
}

/// Catches one service's refusal holding back another: a service whose try failed is left alone
/// while the other is asked at its own time.
#[test]
fn a_failure_of_one_service_does_not_hold_back_another() {
    let mut world = World::new("independent");
    world.issuer.answer_with(|service, _| {
        Ok(IssueAnswer::Refused(
            Reason::new(if service == "alpha" { "denied" } else { "v2" }).expect("a token"),
        ))
    });
    world.keeper.step(&[wanted("alpha"), wanted("beta")]);
    let calls: Vec<String> = world.issuer.calls().into_iter().map(|c| c.0).collect();
    assert_eq!(calls, ["alpha", "beta"]);
    world.advance_mono(RETRY_AFTER_SECS);
    world.keeper.step(&[wanted("alpha")]);
    let calls: Vec<String> = world.issuer.calls().into_iter().map(|c| c.0).collect();
    assert_eq!(calls, ["alpha", "beta", "alpha"]);
}

/// Catches the loop's pace and its use of the clock: a pass every minute on the monotonic clock,
/// the list of services asked of its source at each pass, and a refused service tried again
/// at five minutes by the loop alone.
#[test]
fn the_loop_makes_a_pass_each_minute_and_retries_a_refusal_at_five_minutes() {
    let mut world = World::new("loop");
    world.issuer.answer_with(|_, _| {
        Ok(IssueAnswer::Refused(
            Reason::new("denied").expect("a token"),
        ))
    });
    let mut passes = 0;
    world.keeper.run_loop(
        &mut || vec![wanted(SERVICE)],
        &mut || None,
        &mut |_| {},
        &mut || {
            passes += 1;
            passes <= 7
        },
    );
    assert_eq!(world.sleeps(), vec![CHECK_SECS; 7]);
    // Passes at 0, 60, ..., 360 seconds: the service is asked at 0 and at 300.
    assert_eq!(world.issuer.calls().len(), 2);
}
