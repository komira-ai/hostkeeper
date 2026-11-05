//! Reading the canonical JSON of `releases converge`.

use serde_json::json;

use crate::observe::Convergence;

/// Catches a count read from the wrong key, a prefix left on the state, and a role line in the
/// wrong order or form.
#[test]
fn a_convergence_is_described_in_one_line() {
    let reply = json!({"convergence": {
        "desired": "1.2.3",
        "state": "CONVERGENCE_STATE_IN_PROGRESS",
        "roles": [
            {"role": "web", "atDesired": 2, "updating": 1, "failed": 3, "down": 4},
            {"role": "db", "atDesired": 1}
        ]
    }});
    let convergence = Convergence::of(&reply);
    assert_eq!(convergence.desired.as_deref(), Some("1.2.3"));
    assert_eq!(convergence.state, "in_progress");
    assert_eq!(
        convergence.line(),
        "in_progress, desired 1.2.3, web: 2 at the version, 1 updating, 3 failed, 4 down; db: 1 at the version, 0 updating, 0 failed, 0 down"
    );
}

/// Catches an omitted default read as something else: proto3 JSON leaves out an unspecified
/// state, a missing desired version, an empty list of roles, and every zero.
#[test]
fn omitted_fields_are_their_defaults() {
    let convergence = Convergence::of(&json!({"convergence": {}}));
    assert_eq!(convergence.desired, None);
    assert_eq!(convergence.line(), "unspecified, desired none");
    let nothing = Convergence::of(&json!({}));
    assert_eq!(nothing, convergence);
    let nameless = Convergence::of(&json!({"convergence": {"roles": [{}]}}));
    assert_eq!(
        nameless.line(),
        "unspecified, desired none, : 0 at the version, 0 updating, 0 failed, 0 down"
    );
}
