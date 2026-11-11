//! The enrollment model: the design's rules hold every invariant, each rule given up one at a
//! time breaks the invariant it protects, and the paths the design cares about are walked step
//! by step.

use hostkeeper_mdm_model::enrollment::Action::*;
use hostkeeper_mdm_model::enrollment::*;
use hostkeeper_mdm_model::explore::{Model, Trace, explore};

const LIMIT: usize = 2_000_000;

fn find(rules: Rules) -> Trace {
    let report = explore(&EnrollmentModel { rules }, LIMIT);
    assert!(!report.truncated, "the search was cut at {LIMIT} states");
    report.trace.expect("the rules should break an invariant")
}

fn assert_breaks(rules: Rules, invariant: &str, steps: &[&str]) {
    let trace = find(rules);
    assert_eq!(trace.violation.invariant, invariant, "{trace:?}");
    assert_eq!(trace.steps, steps, "{trace:?}");
}

/// A supervised, ADE-enrolled device with a group, not enrolled in the old stack.
fn good_device(model: &EnrollmentModel) -> State {
    let init = model.init();
    init.into_iter()
        .find(|s| {
            let d = s.device;
            d.supervised && d.via_dep && d.group && !d.foreign
        })
        .expect("the start states hold every combination")
}

fn walk(model: &EnrollmentModel, mut s: State, steps: Vec<Action>) -> State {
    for a in steps {
        s = model
            .step(&s, &a)
            .unwrap_or_else(|v| panic!("{a:?} broke {v:?}"));
        model
            .check(&s)
            .unwrap_or_else(|v| panic!("after {a:?}: {v:?}"));
    }
    s
}

fn first_enrollment() -> Vec<Action> {
    vec![
        AbmAssign,
        PostEnroll {
            key: 0,
            verified: true,
        },
        Scep {
            challenge: 0,
            key: 0,
            lost: false,
        },
        Authenticate { cert: 0 },
    ]
}

#[test]
fn the_design_rules_hold_every_invariant_over_every_reachable_state() {
    let report = explore(
        &EnrollmentModel {
            rules: Rules::design(),
        },
        LIMIT,
    );
    assert!(!report.truncated);
    assert!(report.trace.is_none(), "{:?}", report.trace);
    // Measured: all sixteen combinations of what the device is.
    assert_eq!(report.states, 683400);
}

#[test]
fn a_challenge_is_consumed_at_most_once() {
    assert_breaks(
        Rules {
            consume_once: false,
            ..Rules::design()
        },
        CHALLENGE_ONCE,
        &[
            "AbmAssign",
            "PostEnroll { key: 0, verified: true }",
            "Scep { challenge: 0, key: 0, lost: false }",
            "Scep { challenge: 0, key: 0, lost: false }",
        ],
    );
}

#[test]
fn a_lost_certrep_retried_with_the_same_key_gets_the_certificate() {
    assert_breaks(
        Rules {
            same_key_gets_stored_certificate: false,
            ..Rules::design()
        },
        LOST_CERTREP_RECOVERS,
        &[
            "AbmAssign",
            "PostEnroll { key: 0, verified: true }",
            "Scep { challenge: 0, key: 0, lost: true }",
            "Scep { challenge: 0, key: 0, lost: false }",
        ],
    );
}

#[test]
fn a_consumed_challenge_presented_with_another_key_issues_nothing() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let s = walk(
        &model,
        good_device(&model),
        vec![
            AbmAssign,
            PostEnroll {
                key: 0,
                verified: true,
            },
            Scep {
                challenge: 0,
                key: 0,
                lost: false,
            },
            Scep {
                challenge: 0,
                key: 1,
                lost: false,
            },
            Expire { challenge: 0 },
        ],
    );
    let issued: Vec<_> = s
        .certs
        .iter()
        .filter(|c| c.state != CertState::Free)
        .collect();
    assert_eq!(issued.len(), 1);
    assert_eq!(issued[0].key, 0);
}

#[test]
fn an_expired_challenge_issues_nothing() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let s = walk(
        &model,
        good_device(&model),
        vec![
            AbmAssign,
            PostEnroll {
                key: 0,
                verified: true,
            },
            Expire { challenge: 0 },
            Scep {
                challenge: 0,
                key: 0,
                lost: false,
            },
        ],
    );
    assert!(s.certs.iter().all(|c| c.state == CertState::Free));
}

#[test]
fn a_serial_has_one_live_enrollment() {
    assert_breaks(
        Rules {
            supersede_old: false,
            ..Rules::design()
        },
        ONE_LIVE,
        &[
            "AbmAssign",
            "PostEnroll { key: 0, verified: true }",
            "PostEnroll { key: 0, verified: true }",
            "Scep { challenge: 0, key: 0, lost: false }",
            "Scep { challenge: 1, key: 0, lost: false }",
            "Authenticate { cert: 0 }",
            "OperatorConfirmCert { cert: 1 }",
            "Authenticate { cert: 1 }",
        ],
    );
}

#[test]
fn a_superseded_certificate_is_never_accepted() {
    // A checked-out identity presents itself again.
    assert_breaks(
        Rules {
            refuse_dead_identity: false,
            ..Rules::design()
        },
        SUPERSEDED_NEVER_ACCEPTED,
        &[
            "AbmAssign",
            "PostEnroll { key: 0, verified: true }",
            "Scep { challenge: 0, key: 0, lost: false }",
            "Authenticate { cert: 0 }",
            "CheckOut { cert: 0 }",
            "Authenticate { cert: 0 }",
        ],
    );
}

#[test]
fn the_push_token_is_the_live_enrollments() {
    // The invariant on a row: a token stored for a certificate that is no longer live.
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let mut s = walk(&model, good_device(&model), first_enrollment());
    s.certs[0].state = CertState::Replaced;
    s.token_for = Some(0);
    let v = model.check(&s).unwrap_err();
    assert_eq!(v.invariant, TOKEN_IS_LIVE);
    assert_eq!(
        v.detail,
        "the push token is stored for certificate 0, which is Replaced"
    );
}

#[test]
fn a_token_update_on_a_replaced_certificate_stores_nothing() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let mut steps = first_enrollment();
    steps.extend([CheckOut { cert: 0 }, TokenUpdate { cert: 0 }]);
    assert_eq!(walk(&model, good_device(&model), steps).token_for, None);
    // With the rule given up, the update is the violation.
    let model = EnrollmentModel {
        rules: Rules {
            refuse_dead_identity: false,
            ..Rules::design()
        },
    };
    let mut s = walk(&model, good_device(&model), first_enrollment());
    s = walk(&model, s, vec![CheckOut { cert: 0 }]);
    let v = model.step(&s, &TokenUpdate { cert: 0 }).unwrap_err();
    assert_eq!(v.invariant, SUPERSEDED_NEVER_ACCEPTED);
}

#[test]
fn a_certificate_of_another_ca_is_never_accepted() {
    assert_breaks(
        Rules {
            refuse_foreign_identity: false,
            ..Rules::design()
        },
        FOREIGN_NEVER_ACCEPTED,
        &["ForeignCheckin"],
    );
}

#[test]
fn a_mac_enrolled_in_the_old_stack_is_refused_and_keeps_no_state_here() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let start = model.init().into_iter().find(|s| s.device.foreign).unwrap();
    let after = walk(&model, start.clone(), vec![ForeignCheckin]);
    assert_eq!(after, start);
    // A wipe ends the old enrollment.
    assert!(!walk(&model, start, vec![Wipe]).device.foreign);
}

#[test]
fn a_live_enrollment_is_never_superseded_by_a_key_that_is_not_the_devices_at_enroll_alone() {
    // Two requests arrive while nothing is live, the LAN peer replaying a blob it captured. The
    // check at /enroll alone lets the second Authenticate replace the Mac's enrollment.
    assert_breaks(
        Rules {
            live_serial_needs_operator_at_authenticate: false,
            ..Rules::design()
        },
        NO_HIJACK,
        &[
            "AbmAssign",
            "PostEnroll { key: 0, verified: true }",
            "PostEnroll { key: 1, verified: true }",
            "Scep { challenge: 0, key: 0, lost: false }",
            "Scep { challenge: 1, key: 1, lost: false }",
            "Authenticate { cert: 0 }",
            "Authenticate { cert: 1 }",
        ],
    );
}

#[test]
fn the_check_at_authenticate_alone_holds_every_invariant() {
    let rules = Rules {
        live_serial_needs_operator_at_enroll: false,
        ..Rules::design()
    };
    let report = explore(&EnrollmentModel { rules }, LIMIT);
    assert!(!report.truncated);
    assert!(report.trace.is_none(), "{:?}", report.trace);
}

#[test]
fn no_challenge_is_issued_on_an_unverified_blob_without_an_operator() {
    assert_breaks(
        Rules {
            unverified_waits_for_operator: false,
            ..Rules::design()
        },
        NO_UNVERIFIED_CHALLENGE,
        &["AbmAssign", "PostEnroll { key: 0, verified: false }"],
    );
}

#[test]
fn no_challenge_is_issued_for_a_serial_the_abm_does_not_assign() {
    assert_breaks(
        Rules {
            challenge_needs_abm: false,
            ..Rules::design()
        },
        NO_CHALLENGE_OUTSIDE_ABM,
        &["PostEnroll { key: 0, verified: true }"],
    );
}

#[test]
fn an_operator_confirming_after_abm_removed_the_serial_issues_no_challenge() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let s = walk(
        &model,
        good_device(&model),
        vec![
            AbmAssign,
            PostEnroll {
                key: 0,
                verified: false,
            },
            AbmRemove,
            OperatorConfirm,
        ],
    );
    assert!(s.challenges.iter().all(|c| c.state == ChallengeState::Free));
    assert_eq!(s.pending, None);
}

#[test]
fn an_operator_rejecting_a_request_issues_no_challenge() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let s = walk(
        &model,
        good_device(&model),
        vec![
            AbmAssign,
            PostEnroll {
                key: 0,
                verified: false,
            },
            OperatorReject,
        ],
    );
    assert!(s.challenges.iter().all(|c| c.state == ChallengeState::Free));
    assert_eq!(s.pending, None);
}

#[test]
fn an_operator_does_not_confirm_a_request_the_lan_peer_sent() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let s = walk(
        &model,
        good_device(&model),
        vec![
            AbmAssign,
            PostEnroll {
                key: 1,
                verified: false,
            },
            OperatorConfirm,
        ],
    );
    assert!(s.challenges.iter().all(|c| c.state == ChallengeState::Free));
}

#[test]
fn a_third_request_gets_no_challenge_while_two_are_open() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let post = || PostEnroll {
        key: 0,
        verified: true,
    };
    let s = walk(
        &model,
        good_device(&model),
        vec![AbmAssign, post(), post(), post()],
    );
    assert_eq!(
        s.challenges
            .iter()
            .filter(|c| c.state == ChallengeState::Unused)
            .count(),
        2
    );
}

#[test]
fn a_nonce_approves_at_most_once() {
    assert_breaks(
        Rules {
            nonce_single_use: false,
            ..Rules::design()
        },
        NONCE_ONCE,
        &[
            "AbmAssign",
            "PostEnroll { key: 0, verified: true }",
            "Scep { challenge: 0, key: 0, lost: false }",
            "Authenticate { cert: 0 }",
            "TokenUpdate { cert: 0 }",
            "MintNonce { cert: 0 }",
            "Approve { nonce: 0 }",
            "Approve { nonce: 0 }",
        ],
    );
}

#[test]
fn a_nonce_approves_only_the_enrollment_it_was_issued_for() {
    assert_breaks(
        Rules {
            nonce_bound_to_enrollment: false,
            ..Rules::design()
        },
        NONCE_BOUND,
        &[
            "AbmAssign",
            "PostEnroll { key: 0, verified: true }",
            "PostEnroll { key: 0, verified: true }",
            "Scep { challenge: 0, key: 0, lost: false }",
            "Scep { challenge: 1, key: 0, lost: false }",
            "Authenticate { cert: 0 }",
            "OperatorConfirmCert { cert: 1 }",
            "TokenUpdate { cert: 0 }",
            "MintNonce { cert: 0 }",
            "Authenticate { cert: 1 }",
            "Approve { nonce: 0 }",
        ],
    );
}

#[test]
fn a_mac_is_approved_only_when_supervised_via_dep_in_abm_with_a_group() {
    assert_breaks(
        Rules {
            require_approval_conditions: false,
            ..Rules::design()
        },
        APPROVAL_CONDITIONS,
        &[
            "AbmAssign",
            "PostEnroll { key: 0, verified: true }",
            "Scep { challenge: 0, key: 0, lost: false }",
            "Authenticate { cert: 0 }",
            "TokenUpdate { cert: 0 }",
            "MintNonce { cert: 0 }",
            "Approve { nonce: 0 }",
        ],
    );
}

#[test]
fn a_supervised_dep_mac_with_a_group_in_abm_is_approved_through_its_own_enrollment() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let mut steps = first_enrollment();
    steps.extend([
        TokenUpdate { cert: 0 },
        MintNonce { cert: 0 },
        ExpireNonce { nonce: 1 },
        Approve { nonce: 0 },
    ]);
    let s = walk(&model, good_device(&model), steps);
    assert_eq!(s.approved, Some(0));
    assert!(s.nonces[0].used);
}

#[test]
fn an_unsupervised_mac_gets_no_nonce_and_an_expired_nonce_approves_nothing() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let bad = model
        .init()
        .into_iter()
        .find(|s| !s.device.supervised)
        .unwrap();
    let mut steps = first_enrollment();
    steps.extend([TokenUpdate { cert: 0 }, MintNonce { cert: 0 }]);
    assert!(!walk(&model, bad, steps.clone()).nonces[0].issued);
    steps.extend([
        MintNonce { cert: 0 },
        ExpireNonce { nonce: 0 },
        Approve { nonce: 0 },
    ]);
    let s = walk(&model, good_device(&model), steps);
    assert_eq!(s.approved, None);
}

#[test]
fn a_nonce_is_not_approved_after_the_abm_removed_the_serial_or_the_mac_was_wiped() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let mut steps = first_enrollment();
    steps.extend([
        TokenUpdate { cert: 0 },
        MintNonce { cert: 0 },
        AbmRemove,
        Approve { nonce: 0 },
    ]);
    assert_eq!(walk(&model, good_device(&model), steps).approved, None);
    let mut steps = first_enrollment();
    steps.extend([
        TokenUpdate { cert: 0 },
        MintNonce { cert: 0 },
        Wipe,
        Approve { nonce: 0 },
    ]);
    assert_eq!(walk(&model, good_device(&model), steps).approved, None);
}

#[test]
fn a_nonce_of_an_enrollment_that_checked_out_approves_nothing() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let mut steps = first_enrollment();
    steps.extend([
        TokenUpdate { cert: 0 },
        MintNonce { cert: 0 },
        CheckOut { cert: 0 },
        Approve { nonce: 0 },
    ]);
    assert_eq!(walk(&model, good_device(&model), steps).approved, None);
}

#[test]
fn a_replayed_blob_before_the_first_enrollment_squats_the_serial_until_an_operator_confirms_the_mac()
 {
    // The LAN peer enrolls first. The Mac's own request waits for an operator, and the peer's
    // enrollment never approves: its nonce reaches only the peer.
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let s = walk(
        &model,
        good_device(&model),
        vec![
            AbmAssign,
            PostEnroll {
                key: 1,
                verified: true,
            },
            Scep {
                challenge: 0,
                key: 1,
                lost: false,
            },
            Authenticate { cert: 0 },
            TokenUpdate { cert: 0 },
            MintNonce { cert: 0 },
            Approve { nonce: 0 },
            PostEnroll {
                key: 0,
                verified: true,
            },
        ],
    );
    assert_eq!(s.certs[0].state, CertState::Live);
    assert_eq!(s.certs[0].key, 1);
    assert_eq!(s.approved, None);
    assert_eq!(s.pending, Some(0));
}

#[test]
fn a_wiped_mac_that_did_not_check_out_needs_an_operator_to_replace_its_enrollment() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let mut steps = first_enrollment();
    steps.extend([
        Wipe,
        PostEnroll {
            key: 0,
            verified: true,
        },
    ]);
    let s = walk(&model, good_device(&model), steps);
    assert_eq!(s.pending, Some(0));
    assert_eq!(s.challenges[1].state, ChallengeState::Free);
    let s = walk(
        &model,
        s,
        vec![
            OperatorConfirm,
            Scep {
                challenge: 1,
                key: 0,
                lost: false,
            },
            Authenticate { cert: 1 },
        ],
    );
    assert_eq!(s.certs[1].state, CertState::Live);
    assert_eq!(s.certs[0].state, CertState::Replaced);
}

#[test]
fn a_wiped_mac_that_checked_out_enrolls_again_with_no_operator() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let mut steps = first_enrollment();
    steps.extend([
        CheckOut { cert: 0 },
        Wipe,
        PostEnroll {
            key: 0,
            verified: true,
        },
        Scep {
            challenge: 1,
            key: 0,
            lost: false,
        },
        Authenticate { cert: 1 },
    ]);
    let s = walk(&model, good_device(&model), steps);
    assert_eq!(s.certs[1].state, CertState::Live);
    assert_eq!(s.pending, None);
}

#[test]
fn an_enrollment_held_at_authenticate_waits_for_the_operator_and_is_then_accepted() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let post = || PostEnroll {
        key: 0,
        verified: true,
    };
    let steps = vec![
        AbmAssign,
        post(),
        post(),
        Scep {
            challenge: 0,
            key: 0,
            lost: false,
        },
        Scep {
            challenge: 1,
            key: 0,
            lost: false,
        },
        Authenticate { cert: 0 },
    ];
    // Two requests, both issued before the second Authenticate: the first stays live.
    let s = walk(&model, good_device(&model), steps);
    let held = walk(&model, s.clone(), vec![Authenticate { cert: 1 }]);
    assert_eq!(held, s);
    let done = walk(
        &model,
        held,
        vec![OperatorConfirmCert { cert: 1 }, Authenticate { cert: 1 }],
    );
    assert_eq!(done.certs[1].state, CertState::Live);
}

#[test]
fn a_duplicate_authenticate_and_an_update_before_authenticate_change_nothing() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let s = walk(&model, good_device(&model), first_enrollment());
    assert_eq!(walk(&model, s.clone(), vec![Authenticate { cert: 0 }]), s);
    let issued = walk(
        &model,
        good_device(&model),
        vec![
            AbmAssign,
            PostEnroll {
                key: 0,
                verified: true,
            },
            Scep {
                challenge: 0,
                key: 0,
                lost: false,
            },
            TokenUpdate { cert: 0 },
            CheckOut { cert: 0 },
        ],
    );
    assert_eq!(issued.token_for, None);
    assert_eq!(issued.certs[0].state, CertState::Issued);
}

#[test]
fn abm_assignment_is_not_given_back_after_the_serial_is_removed() {
    let model = EnrollmentModel {
        rules: Rules::design(),
    };
    let s = walk(
        &model,
        good_device(&model),
        vec![AbmAssign, AbmRemove, AbmAssign],
    );
    assert_eq!(s.abm, Abm::Removed);
}
