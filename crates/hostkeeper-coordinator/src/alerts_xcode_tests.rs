//! The alerts `xcode_release_waiting` and `xcode_version_taken`.

use std::time::Duration;

use super::{Alert, At, EnrollmentFacts, Kind, Said, Subject, Thresholds, derive};
use crate::fleet::{Fleet, QUIET_START};
use crate::ops::OpsState;
use crate::testutil::kbf_services;
use crate::xcode_feed::{Channel, XcodeId, XcodeRelease, XcodeVersion};
use crate::xcode_store::Taken;

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

/// The alerts of an empty fleet whose enrollment facts name `taken` as the refusals of a second
/// build that stand.
fn derived_taken(taken: Vec<Taken>) -> Vec<Alert> {
    let facts = EnrollmentFacts {
        xcode_taken: taken,
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

/// The refusal of build `refused` of `version` while the store holds `held`.
fn taken(version: &str, held: &str, refused: &str) -> Taken {
    Taken {
        version: version.to_owned(),
        held_build: held.to_owned(),
        refused_build: refused.to_owned(),
    }
}

/// Proves: facts that name no refused second build give no `xcode_version_taken` alert; each
/// refusal that stands gives exactly one, a warning whose subject is the version and whose
/// message names the version, the refused build, the held build and the verb that removes the
/// held archive, so two versions give two alerts.
///
/// Catches: an alert raised with nothing refused, one per refusal missing, two for one version,
/// or a message that swaps the held and the refused build.
#[test]
fn xcode_version_taken_is_one_warning_per_refused_version() {
    assert_eq!(derived_taken(vec![]), vec![]);
    let alerts = derived_taken(vec![taken("27.2", "27B52", "27B53")]);
    assert_eq!(
        alerts,
        vec![Alert {
            subject: Subject {
                version: "27.2".to_owned(),
                ..Subject::of(Kind::XcodeVersionTaken)
            },
            message: "an upload of Xcode 27.2 build 27B53 was refused: the Xcode store holds \
                      build 27B52 of that version, and a version names one build on our hosts; \
                      remove the held archive with `hk xcode remove` before pushing the other \
                      build"
                .to_owned(),
            said: Said::default(),
        }]
    );
    assert_eq!(
        alerts[0].subject.kind.severity(),
        super::proto::AlertSeverity::Warning
    );
    let two = derived_taken(vec![
        taken("27.2", "27B52", "27B53"),
        taken("26.4", "26E10", "26E11"),
    ]);
    let versions: Vec<&str> = two
        .iter()
        .map(|alert| alert.subject.version.as_str())
        .collect();
    assert_eq!(versions.len(), 2);
    assert!(versions.contains(&"27.2") && versions.contains(&"26.4"));
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
            said: Said::default(),
        }]
    );
    assert_eq!(
        alerts[0].subject.kind.severity(),
        super::proto::AlertSeverity::Warning
    );
}
