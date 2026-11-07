//! The desired route over a version marked bad, and over a halt whose lift cannot be written.

use super::*;

use crate::desired::StoreError;
use crate::ops::{Halt, HaltOrigin, OpsError, OpsState, OpsStore, ServiceOps};
use crate::testutil::version;

/// A rig whose ops store holds `marks` for kbf and cannot be written.
fn rig_over(name: &str, marks: ServiceOps) -> Rig {
    let mut rig = Rig::new(name);
    let mut state = OpsState::default();
    state.services.insert("kbf".to_owned(), marks);
    let ops = OpsStore::refusing(
        state,
        Box::new(|_| {
            Err(OpsError::Io(StoreError::Io {
                path: "ops".into(),
                source: io::Error::other("disk full"),
            }))
        }),
    );
    rig.coordinator = Rig::start(&rig.dir, &rig.clock).with_ops(ops);
    rig
}

async fn post(rig: &Rig, version: &str) -> (StatusCode, Value) {
    let body = format!(r#"{{"version":"{version}","min_up":{{"storage":0,"frontend":0}}}}"#);
    call(
        rig,
        Surface::Control,
        &None,
        request("POST", DESIRED_PATH, Some(&ci_token()), &body),
    )
    .await
}

/// Catches: CI lifting a bad mark, or told anything but `bad_version`: the answer is 409
/// `bad_version`, nothing is written (the ops store could not be) and no desired state is
/// stored.
#[tokio::test]
async fn a_set_of_a_bad_version_is_a_409_and_sets_nothing() {
    let rig = rig_over(
        "http-bad-version",
        ServiceOps {
            bad: vec![version("2.0")],
            ..ServiceOps::default()
        },
    );
    let (code, text) = post(&rig, "2.0").await;
    assert_eq!(code, StatusCode::CONFLICT);
    assert_eq!(text, json!({"error": "bad_version"}));
    assert_eq!(status(&rig).await["desired"], Value::Null);
}

/// Catches: CI told all went well when the lift of the updater's halt its new version brings
/// could not be written: the answer is 500 `internal` (no path in it), and the halt stands.
#[tokio::test]
async fn a_halt_lift_that_cannot_be_written_is_a_500_and_the_halt_stands() {
    let rig = rig_over(
        "http-unliftable",
        ServiceOps {
            halt: Some(Halt {
                node_id: "s1".to_owned(),
                check: "artifact_changed".to_owned(),
                target: version("2.0"),
                report: None,
                origin: HaltOrigin::Updater,
            }),
            ..ServiceOps::default()
        },
    );
    let (code, text) = post(&rig, "3.0").await;
    assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(text, json!({"error": "internal"}));
    assert_eq!(
        status(&rig).await["ops"]["services"]["kbf"]["halt"]["node_id"],
        "s1"
    );
}
