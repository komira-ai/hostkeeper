//! The alert `xcode_release_waiting`.

use std::time::Duration;

use super::{Alert, At, EnrollmentFacts, Kind, Subject, Thresholds, derive};
use crate::fleet::{Fleet, QUIET_START};
use crate::ops::OpsState;
use crate::testutil::kbf_services;
use crate::xcode_feed::{Channel, XcodeId, XcodeRelease, XcodeVersion};

/// Xcode 27.2 (27B52), the release channel, which requires macOS 26.6.
fn release_27_2() -> XcodeRelease {
    XcodeRelease {
        id: XcodeId {
            version: XcodeVersion::parse("27.2").expect("27.2 is a version number"),
            build: "27B52".to_owned(),
        },
        channel: Channel::Release,
        requires_macos: "26.6".to_owned(),
    }
}

/// The alerts of an empty fleet whose enrollment facts name `waiting` as the Xcode release the
/// store lacks.
fn derived_with(waiting: Option<XcodeRelease>) -> Vec<Alert> {
    let facts = EnrollmentFacts {
        xcode_waiting: waiting,
        ..EnrollmentFacts::default()
    };
    derive(
        &Fleet::new(Duration::ZERO, QUIET_START),
        &OpsState::default(),
        &kbf_services(),
        &facts,
        At {
            now: Duration::from_secs(1_000),
            unix: 1_800_001_000,
        },
        &Thresholds::default(),
    )
}

/// Proves: facts that name a waiting Xcode release give exactly one `xcode_release_waiting`
/// alert, a warning, whose version is the release's version and whose message names its version,
/// its build, the macOS it requires and the verb that pushes it; facts that name none give no
/// alert.
///
/// Catches: an alert raised with no release waiting, or raised twice; a release's build or
/// required macOS left out of the message; an alert with no version in its subject.
#[test]
fn xcode_release_waiting_is_one_warning_naming_the_release() {
    assert_eq!(derived_with(None), vec![]);
    let alerts = derived_with(Some(release_27_2()));
    assert_eq!(
        alerts,
        vec![Alert {
            subject: Subject {
                version: "27.2".to_owned(),
                ..Subject::of(Kind::XcodeReleaseWaiting)
            },
            message: "Xcode 27.2 (27B52), which requires macOS 26.6 or later, is released and \
                      the coordinator's Xcode store holds no release as new: push it from a Mac \
                      signed in to the Apple developer account with `hk xcode-push 27.2`"
                .to_owned(),
        }]
    );
    assert_eq!(
        alerts[0].subject.kind.severity(),
        super::proto::AlertSeverity::Warning
    );
}
