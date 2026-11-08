//! `Releases.WaitConverged`, added after `Halt`: one held call that ends when a service's hosts
//! have converged on a version, the service is halted, another version is desired, or the wait's
//! `wait_secs` pass. Its request and response are new messages, `WaitOutcome` a new enum, and its
//! two refusals (`invalid_wait`, `too_many_waits`) new values of `ReleasesRefusal`. A reader of
//! the release before (the baseline, `proto/compat/baseline.binpb`) knows none of them and keeps
//! every method, message and value it knows; the `Convergence` an answer carries is the one it
//! reads from `Converge`.

use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Value};

use hostkeeper_proto::hostkeeper::operator::v1 as op;

/// The descriptor set the last release committed: what a reader of that release knows.
fn baseline_pool() -> DescriptorPool {
    let bytes = include_bytes!("../proto/compat/baseline.binpb");
    DescriptorPool::decode(&bytes[..]).expect("the baseline decodes")
}

/// The descriptor set this build serves.
fn current_pool() -> DescriptorPool {
    DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("the set decodes")
}

/// Each method of `service` in `pool`: its name, request and response.
fn methods(pool: &DescriptorPool, service: &str) -> Vec<(String, String, String)> {
    pool.get_service_by_name(service)
        .expect("the service")
        .methods()
        .map(|m| {
            (
                m.name().to_owned(),
                m.input().full_name().to_owned(),
                m.output().full_name().to_owned(),
            )
        })
        .collect()
}

/// Catches: the wait replacing or retyping a method the release before has, or the release
/// before already knowing a method, message or enum of that name (the addition would then re-mean
/// it): the current `Releases` is the baseline's methods, in order, then `WaitConverged` alone.
#[test]
fn the_wait_is_a_new_method_of_releases_after_every_method_the_release_before_has() {
    let older = baseline_pool();
    let releases = "hostkeeper.operator.v1.Releases";
    let before = methods(&older, releases);
    let now = methods(&current_pool(), releases);
    let wait = (
        "WaitConverged".to_owned(),
        "hostkeeper.operator.v1.WaitConvergedRequest".to_owned(),
        "hostkeeper.operator.v1.WaitConvergedResponse".to_owned(),
    );
    let mut want = before.clone();
    want.push(wait);
    assert_eq!(now, want);
    assert!(before.iter().all(|(name, _, _)| name != "WaitConverged"));
    for message in [
        "hostkeeper.operator.v1.WaitConvergedRequest",
        "hostkeeper.operator.v1.WaitConvergedResponse",
    ] {
        assert!(older.get_message_by_name(message).is_none(), "{message}");
    }
    assert!(
        older
            .get_enum_by_name("hostkeeper.operator.v1.WaitOutcome")
            .is_none()
    );
}

/// Catches: a refusal of the wait taking the number of a value the release before already gave a
/// meaning, or renumbering one: the baseline knows no value 23 or 24, and every value it knows
/// keeps its name and number. A caller of the release before reads the new refusals by their
/// token, the status message, which needs no value.
#[test]
fn the_wait_refusals_are_new_values_of_releases() {
    let older = baseline_pool();
    let refusal = older
        .get_enum_by_name("hostkeeper.operator.v1.ReleasesRefusal")
        .expect("released");
    for (added, number, token) in [
        (
            op::ReleasesRefusal::InvalidWait,
            23,
            "RELEASES_REFUSAL_INVALID_WAIT",
        ),
        (
            op::ReleasesRefusal::TooManyWaits,
            24,
            "RELEASES_REFUSAL_TOO_MANY_WAITS",
        ),
    ] {
        assert_eq!(added as i32, number, "{added:?}");
        assert_eq!(added.as_str_name(), token);
        assert!(refusal.get_value(number).is_none(), "{added:?}");
    }
    for value in refusal.values() {
        assert_eq!(
            op::ReleasesRefusal::try_from(value.number())
                .expect("a value of the release before")
                .as_str_name(),
            value.name()
        );
    }
}

/// Catches: an answer of the wait whose `Convergence` a reader of the release before cannot read
/// as `Converge` answers it, an outcome that does not round-trip, and a request with no field set
/// read as anything but an empty service, an empty version and a wait of 0 (which the coordinator
/// refuses `invalid_wait`). An outcome a later release adds reads as its number, which is no
/// `WaitOutcome` this build names: a client takes it as no answer.
#[test]
fn a_wait_round_trips_and_its_convergence_is_the_one_the_release_before_reads() {
    let convergence = op::Convergence {
        desired: Some("2.0".to_owned()),
        state: op::ConvergenceState::Converged as i32,
        roles: vec![op::RoleCounts {
            role: "daemon".to_owned(),
            at_desired: 3,
            ..op::RoleCounts::default()
        }],
        coordinator_commit: "0123456789ab".to_owned(),
        ..op::Convergence::default()
    };
    let answer = op::WaitConvergedResponse {
        outcome: op::WaitOutcome::Converged as i32,
        convergence: Some(convergence.clone()),
    };
    let bytes = answer.encode_to_vec();
    assert_eq!(
        op::WaitConvergedResponse::decode(bytes.as_slice()).expect("decodes"),
        answer
    );

    let older = baseline_pool();
    let read = DynamicMessage::decode(
        older
            .get_message_by_name("hostkeeper.operator.v1.Convergence")
            .expect("released"),
        convergence.encode_to_vec().as_slice(),
    )
    .expect("decodes");
    assert_eq!(
        read.get_field_by_name("desired").as_deref(),
        Some(&Value::String("2.0".to_owned()))
    );
    let back = op::Convergence::decode(read.encode_to_vec().as_slice()).expect("decodes");
    assert_eq!(back, convergence);

    for (outcome, number) in [
        (op::WaitOutcome::Unspecified, 0),
        (op::WaitOutcome::Converged, 1),
        (op::WaitOutcome::Halted, 2),
        (op::WaitOutcome::Superseded, 3),
        (op::WaitOutcome::Deadline, 4),
    ] {
        assert_eq!(outcome as i32, number, "{outcome:?}");
    }
    let later = op::WaitConvergedResponse {
        outcome: 9,
        convergence: None,
    };
    let read = op::WaitConvergedResponse::decode(later.encode_to_vec().as_slice()).expect("reads");
    assert_eq!(read.outcome, 9);
    assert!(op::WaitOutcome::try_from(read.outcome).is_err());

    let empty = op::WaitConvergedRequest::decode(&[][..]).expect("decodes");
    assert_eq!(
        (
            empty.service.as_str(),
            empty.version.as_str(),
            empty.wait_secs
        ),
        ("", "", 0)
    );
    let request = op::WaitConvergedRequest {
        service: "kbf".to_owned(),
        version: "2.0".to_owned(),
        wait_secs: 1800,
    };
    assert_eq!(
        op::WaitConvergedRequest::decode(request.encode_to_vec().as_slice()).expect("decodes"),
        request
    );
}
