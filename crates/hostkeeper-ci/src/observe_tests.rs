//! Reading the reply of `Converge`.

use hostkeeper_proto::hostkeeper::operator::v1::{ConvergeResponse, Convergence as Proto};

use crate::observe::Convergence;
use crate::testing::role;

/// The reply that holds a convergence of `state` (a number), with no desired version and no
/// roles.
fn reply(state: i32) -> ConvergeResponse {
    ConvergeResponse {
        convergence: Some(Proto {
            state,
            ..Proto::default()
        }),
    }
}

/// Catches a count read from the wrong field, a prefix left on the state, and a role line in the
/// wrong order or form.
#[test]
fn a_convergence_is_described_in_one_line() {
    let reply = ConvergeResponse {
        convergence: Some(Proto {
            desired: Some("1.2.3".to_owned()),
            state: 4,
            roles: vec![role("web", 2, 1, 3, 4), role("db", 1, 0, 0, 0)],
            ..Proto::default()
        }),
    };
    let convergence = Convergence::of(&reply);
    assert_eq!(convergence.desired.as_deref(), Some("1.2.3"));
    assert_eq!(convergence.state, "in_progress");
    assert_eq!(
        convergence.line(),
        "in_progress, desired 1.2.3, web: 2 at the version, 1 updating, 3 failed, 4 down; db: 1 at the version, 0 updating, 0 failed, 0 down"
    );
}

/// Catches an absent convergence read as something else: it is an unspecified state, no desired
/// version and no roles, and so is an empty one; a role with defaults is a nameless role of zeros.
#[test]
fn an_absent_convergence_is_the_default() {
    let empty = Convergence::of(&reply(0));
    assert_eq!(empty.desired, None);
    assert_eq!(empty.line(), "unspecified, desired none");
    let absent = Convergence::of(&ConvergeResponse::default());
    assert_eq!(absent, empty);
    let nameless = Convergence::of(&ConvergeResponse {
        convergence: Some(Proto {
            roles: vec![role("", 0, 0, 0, 0)],
            ..Proto::default()
        }),
    });
    assert_eq!(
        nameless.line(),
        "unspecified, desired none, : 0 at the version, 0 updating, 0 failed, 0 down"
    );
}

/// Catches every state of the enum having its own name, lower case and without the prefix, and a
/// number no state has (a newer coordinator) being read as a state or dropping out of the line.
#[test]
fn every_state_has_a_name_and_an_unknown_number_is_shown() {
    use hostkeeper_proto::hostkeeper::operator::v1::ConvergenceState;
    for state in [
        ConvergenceState::Unspecified,
        ConvergenceState::Converged,
        ConvergenceState::Blocked,
        ConvergenceState::Halted,
        ConvergenceState::InProgress,
    ] {
        let expected = state
            .as_str_name()
            .trim_start_matches("CONVERGENCE_STATE_")
            .to_ascii_lowercase();
        assert_eq!(Convergence::of(&reply(state as i32)).state, expected);
    }
    assert_eq!(Convergence::of(&reply(4)).state, "in_progress");
    assert_eq!(Convergence::of(&reply(99)).state, "unknown_99");
    assert_eq!(
        Convergence::of(&reply(-1)).line(),
        "unknown_-1, desired none"
    );
}
