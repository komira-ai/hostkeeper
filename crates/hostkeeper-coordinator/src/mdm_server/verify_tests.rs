//! `Mdm.Verify` (task CM11b): the setup read back, the names the listener serves, Apple's answer
//! about the ABM token, and a test push to an enrolled Mac, over the MDM server's rig with a fake
//! APNs and a fake ABM client.

use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_mdm_push::BoxFuture;
use hostkeeper_mdm_push::apns::PushError;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::{MdmAbmResult, MdmPushResult, MdmRefusal};
use rcgen::{CertificateParams, KeyPair};

use super::support::{Rig, SERIAL, TOPIC};
use crate::mdm_secrets::{AbmToken, SecretKind};
use crate::mdm_verify::{AbmAccount, AbmAccountCheck, AbmCheckError};
use crate::operator::Caller;
use crate::store::audit_failure;
use crate::store::mdm::{EnrollmentState, MdmStore as _};

const TOKEN_JSON: &str = r#"{"consumer_key":"CK-unique","consumer_secret":"CS-unique","access_token":"AT-unique","access_secret":"AS-unique","access_secret_expiry":"2027-10-31T22:01:36Z"}"#;

fn alice() -> Caller {
    Caller::Operator("alice".parse().expect("a token name"))
}

fn ask(id: &str) -> proto::VerifyRequest {
    proto::VerifyRequest {
        enrollment_id: id.to_owned(),
    }
}

/// An ABM client that answers as told and records the consumer key it was asked with.
struct FakeAbm {
    answer: Result<AbmAccount, AbmCheckError>,
    asked: Mutex<Vec<String>>,
}

impl FakeAbm {
    fn answering(answer: Result<AbmAccount, AbmCheckError>) -> Arc<Self> {
        Arc::new(Self {
            answer,
            asked: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> Vec<String> {
        self.asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl AbmAccountCheck for FakeAbm {
    fn account<'a>(
        &'a self,
        token: &'a AbmToken,
    ) -> BoxFuture<'a, Result<AbmAccount, AbmCheckError>> {
        self.asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(token.consumer_key.clone());
        let answer = self.answer.clone();
        Box::pin(async move { answer })
    }
}

fn put_name(rig: &Rig, name: &str) {
    rig.coordinator
        .mdm_put_name(
            &alice(),
            &proto::PutNameRequest {
                name: name.to_owned(),
            },
        )
        .expect("named");
}

/// Stores a self-signed certificate that names `names`, with its key, as the TLS pair.
fn store_tls_naming(rig: &Rig, names: &[&str]) {
    let params = CertificateParams::new(names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>())
        .expect("params");
    let key = KeyPair::generate().expect("a key");
    let cert = params.self_signed(&key).expect("signed");
    rig.put(SecretKind::TlsCert, cert.pem().as_bytes());
    rig.put(SecretKind::TlsKey, key.serialize_pem().as_bytes());
}

fn audit(rig: &Rig) -> Vec<(String, String, Option<String>, Option<String>)> {
    rig.coordinator
        .ops()
        .state()
        .audit
        .iter()
        .map(|e| {
            (
                e.by.clone(),
                e.action.clone(),
                e.subject.clone(),
                e.reason.clone(),
            )
        })
        .collect()
}

async fn push_result(rig: &Rig, id: &str) -> proto::MdmPushCheck {
    rig.coordinator
        .mdm_verify(&alice(), ask(id))
        .await
        .expect("verified")
        .push
        .expect("a push check")
}

/// Proves: with no enrollment named, nothing is pushed and the push check is absent; the answer
/// carries the status as `Status` computes it, says the listener runs and serves, and names the
/// certificate's names; the call is audited as `mdm_verify` by the signed-in user.
#[tokio::test]
async fn verify_without_a_mac_reads_back_the_state_and_pushes_nothing() {
    let rig = Rig::new("verify-state");
    let answer = rig
        .coordinator
        .mdm_verify(&alice(), ask(""))
        .await
        .expect("verified");
    assert_eq!(
        answer.status,
        Some(rig.coordinator.mdm_status(&alice()).expect("status"))
    );
    assert!(answer.listener);
    assert!(answer.serving);
    assert_eq!(answer.served_names, ["localhost"]);
    assert!(!answer.served_names_the_name, "no name is set");
    assert_eq!(answer.push, None);
    assert!(rig.apns.sent().is_empty());
    assert_eq!(
        audit(&rig),
        [(
            "alice".to_owned(),
            "mdm_verify".to_owned(),
            None,
            Some("test push no".to_owned())
        )]
    );
}

/// Proves: the served names cover the MDM name exactly or by a wildcard of the first label, and do
/// not cover another name or a name one label deeper.
#[tokio::test]
async fn the_served_names_are_compared_with_the_mdm_name() {
    let rig = Rig::new("verify-names");
    for (served, name, covers) in [
        ("mdm.example.com", "mdm.example.com", true),
        ("*.example.com", "mdm.example.com", true),
        ("other.example.com", "mdm.example.com", false),
        ("*.example.com", "a.mdm.example.com", false),
    ] {
        store_tls_naming(&rig, &[served]);
        put_name(&rig, name);
        let answer = rig
            .coordinator
            .mdm_verify(&alice(), ask(""))
            .await
            .expect("verified");
        assert_eq!(answer.served_names, [served]);
        assert_eq!(answer.served_names_the_name, covers, "{served} for {name}");
    }
}

/// Proves: a listener with no certificate to serve says so and names nothing, and a coordinator
/// that runs no listener says that, with no pusher to push with.
#[tokio::test]
async fn a_listener_with_no_certificate_or_none_at_all_is_told() {
    let rig = Rig::bare("verify-no-tls");
    let answer = rig
        .coordinator
        .mdm_verify(&alice(), ask(""))
        .await
        .expect("verified");
    assert!(answer.listener);
    assert!(!answer.serving);
    assert!(answer.served_names.is_empty());
    assert!(!answer.served_names_the_name);

    // Even with an ABM token held, a coordinator with no listener holds no client to ask Apple with.
    rig.put(SecretKind::AbmToken, TOKEN_JSON.as_bytes());
    let without = rig.coordinator_without_listener();
    let answer = without
        .mdm_verify(&alice(), ask("mac-1"))
        .await
        .expect("verified");
    assert!(!answer.listener);
    assert!(!answer.serving);
    assert_eq!(
        answer.abm.expect("an ABM check").result,
        i32::from(MdmAbmResult::NotChecked)
    );
    let push = answer.push.expect("a push check");
    assert_eq!(push.result, i32::from(MdmPushResult::NoListener));
    assert_eq!(push.enrollment_id, "mac-1");
}

/// Proves: a test push goes to the live enrollment with its token and magic, the push topic and
/// priority of the settings, and an expiration of 0; the answer names the topic and the `apns-id`;
/// and the queue is undisturbed: nothing is recorded against the Mac, so its next command is
/// still pushed at once.
#[tokio::test]
async fn a_test_push_reaches_the_named_mac_and_records_nothing() {
    let rig = Rig::new("verify-push");
    rig.live("mac-1", SERIAL, true);
    let push = push_result(&rig, "mac-1").await;
    assert_eq!(push.result, i32::from(MdmPushResult::Sent));
    assert_eq!(push.apns_id, "fake-apns-id");
    assert_eq!(push.topic, TOPIC);
    assert_eq!(push.detail, "");
    let sent = rig.apns.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].topic, TOPIC);
    assert_eq!(sent[0].magic, "magic");
    assert_eq!(sent[0].priority, 10);
    assert_eq!(sent[0].expiration_unix, 0);
    let row = rig.store.enrollment("mac-1").expect("read").expect("held");
    assert_eq!((row.push_outstanding, row.last_push_unix), (false, None));
    assert_eq!(
        audit(&rig)[0].2.as_deref(),
        Some("mac-1"),
        "the entry names the Mac"
    );
    // The queue still pushes a command at once.
    rig.enqueue("mac-1", 1).await.expect("queued");
    assert_eq!(rig.apns.sent().len(), 2);
}

/// Proves: every way APNs or the connection can end a push is its own class, with the reason it
/// gave, so that a wrong topic, a dead token and a certificate of the wrong environment are told
/// apart.
#[tokio::test]
async fn each_apns_error_is_its_own_class() {
    let rig = Rig::new("verify-classes");
    rig.live("mac-1", SERIAL, true);
    let cases = [
        (
            PushError::BadDeviceToken("BadDeviceToken".to_owned()),
            MdmPushResult::BadDeviceToken,
            "BadDeviceToken",
        ),
        (PushError::Unregistered, MdmPushResult::Unregistered, ""),
        (
            PushError::TopicDisallowed,
            MdmPushResult::TopicDisallowed,
            "",
        ),
        (
            PushError::CertificateAlert("BadCertificateEnvironment".to_owned()),
            MdmPushResult::BadCertificateEnvironment,
            "BadCertificateEnvironment",
        ),
        (
            PushError::CertificateAlert("CertificateExpired".to_owned()),
            MdmPushResult::CertificateRejected,
            "CertificateExpired",
        ),
        (
            PushError::CertificateRejected("Forbidden".to_owned()),
            MdmPushResult::CertificateRejected,
            "Forbidden",
        ),
        (
            PushError::NoClientCertificate,
            MdmPushResult::NoClientCertificate,
            "",
        ),
        (PushError::Throttled, MdmPushResult::Throttled, ""),
        (
            PushError::Unavailable(503),
            MdmPushResult::ApnsUnavailable,
            "503",
        ),
        (
            PushError::Rejected {
                status: 400,
                reason: "PayloadEmpty".to_owned(),
            },
            MdmPushResult::Rejected,
            "400 PayloadEmpty",
        ),
        (
            PushError::Connection("timed out".to_owned()),
            MdmPushResult::ConnectionFailed,
            "timed out",
        ),
    ];
    let total = cases.len();
    for (error, class, detail) in cases {
        rig.apns.answer_next(Err(error.clone()));
        let push = push_result(&rig, "mac-1").await;
        assert_eq!(push.result, i32::from(class), "{error}");
        assert_eq!(push.detail, detail, "{error}");
        assert_eq!(push.apns_id, "");
        assert_eq!(push.topic, TOPIC, "a push was made, so its topic is named");
    }
    // A failed test push arms no floor: the Mac's own queue is unaffected.
    rig.enqueue("mac-1", 1).await.expect("queued");
    assert_eq!(rig.apns.sent().len(), total + 1);
}

/// Proves: a Mac that cannot be pushed is told why, before anything is sent: no push identity, an
/// enrollment that is unknown or checked out, and one with no push token yet.
#[tokio::test]
async fn a_mac_that_cannot_be_pushed_is_told_why_and_nothing_is_sent() {
    let bare = Rig::bare("verify-no-identity");
    bare.live("mac-1", SERIAL, true);
    assert_eq!(
        push_result(&bare, "mac-1").await.result,
        i32::from(MdmPushResult::NoPushIdentity)
    );

    let rig = Rig::new("verify-cannot");
    assert_eq!(
        push_result(&rig, "nobody").await.result,
        i32::from(MdmPushResult::NoSuchEnrollment)
    );
    rig.live("mac-1", SERIAL, false);
    assert_eq!(
        push_result(&rig, "mac-1").await.result,
        i32::from(MdmPushResult::NoPushToken)
    );
    rig.live("mac-2", "C02OTHER", true);
    assert!(rig.store.check_out("mac-2", 1).expect("checked out"));
    let held = rig.store.enrollment("mac-2").expect("read").expect("held");
    assert_ne!(held.state, EnrollmentState::Live);
    let push = push_result(&rig, "mac-2").await;
    assert_eq!(push.result, i32::from(MdmPushResult::NoSuchEnrollment));
    assert_eq!(push.topic, "", "no push was made, so no topic is named");
    assert!(rig.apns.sent().is_empty());
    assert!(bare.apns.sent().is_empty());
}

/// Proves: an enrollment id that is not one is refused `invalid_request` and audited with that
/// reason; a caller that is not an operator is refused as every operator call is; a store that
/// cannot read the enrollment is `store_failed`, with nothing sent.
#[tokio::test]
async fn bad_ids_callers_and_stores_are_refused_without_a_push() {
    let rig = Rig::new("verify-refused");
    rig.live("mac-1", SERIAL, true);
    let long = "a".repeat(65);
    for bad in ["has space", "semi;colon", long.as_str()] {
        assert_eq!(
            rig.coordinator.mdm_verify(&alice(), ask(bad)).await,
            Err(MdmRefusal::InvalidRequest),
            "{bad}"
        );
    }
    assert_eq!(
        audit(&rig)[0],
        (
            "alice".to_owned(),
            "mdm_verify".to_owned(),
            None,
            Some("refused: invalid_request".to_owned())
        )
    );
    assert_eq!(
        rig.coordinator
            .mdm_verify(&Caller::Anonymous, ask(""))
            .await,
        Err(MdmRefusal::Unauthorized)
    );
    assert_eq!(
        rig.coordinator.mdm_verify(&Caller::Ci, ask("")).await,
        Err(MdmRefusal::Forbidden)
    );
    rig.store.fail(Some("enrollment"));
    assert_eq!(
        rig.coordinator.mdm_verify(&alice(), ask("mac-1")).await,
        Err(MdmRefusal::StoreFailed)
    );
    assert!(rig.apns.sent().is_empty());
}

/// Proves: when the audit entry cannot be written nothing is asked of Apple and no push is sent,
/// whether the id is good or not.
#[tokio::test]
async fn without_its_audit_entry_nothing_is_pushed_or_asked() {
    let abm = FakeAbm::answering(Ok(AbmAccount {
        organization: "Org".to_owned(),
    }));
    let rig = Rig::asking("verify-audit", Arc::clone(&abm) as Arc<dyn AbmAccountCheck>);
    rig.put(SecretKind::AbmToken, TOKEN_JSON.as_bytes());
    rig.live("mac-1", SERIAL, true);
    rig.fake.refuse(|_| Some(audit_failure()));
    assert_eq!(
        rig.coordinator.mdm_verify(&alice(), ask("mac-1")).await,
        Err(MdmRefusal::AuditUnavailable)
    );
    assert_eq!(
        rig.coordinator.mdm_verify(&alice(), ask("bad id")).await,
        Err(MdmRefusal::AuditUnavailable)
    );
    assert!(rig.apns.sent().is_empty());
    assert!(abm.asked().is_empty());
}

/// Proves: with no ABM token the check says `no_token`; with one and no ABM client it says
/// `not_checked`, never `accepted`; an unreadable token is `unreadable` and Apple is not asked.
#[tokio::test]
async fn the_abm_check_says_what_it_could_not_do() {
    let rig = Rig::new("verify-abm-none");
    let abm = |answer: proto::VerifyResponse| answer.abm.expect("an ABM check");
    let answer = rig
        .coordinator
        .mdm_verify(&alice(), ask(""))
        .await
        .expect("verified");
    assert_eq!(abm(answer).result, i32::from(MdmAbmResult::NoToken));

    rig.put(SecretKind::AbmToken, TOKEN_JSON.as_bytes());
    let answer = rig
        .coordinator
        .mdm_verify(&alice(), ask(""))
        .await
        .expect("verified");
    let check = abm(answer);
    assert_eq!(check.result, i32::from(MdmAbmResult::NotChecked));
    assert!(check.organization.is_empty());

    let asking = FakeAbm::answering(Err(AbmCheckError::Unreachable("never asked".to_owned())));
    let rig = Rig::asking(
        "verify-abm-unreadable",
        Arc::clone(&asking) as Arc<dyn AbmAccountCheck>,
    );
    rig.put(SecretKind::AbmToken, b"not json");
    let answer = rig
        .coordinator
        .mdm_verify(&alice(), ask(""))
        .await
        .expect("verified");
    assert_eq!(abm(answer).result, i32::from(MdmAbmResult::Unreadable));
    assert!(asking.asked().is_empty());
}

/// Proves: Apple's answer about the token is the check's result: accepted with the organisation's
/// name, rejected, or not reached, each with the client's words; the client is given the token
/// held; and no part of the token is in the answer.
#[tokio::test]
async fn the_abm_check_reports_apples_answer_and_never_a_token_part() {
    let cases = [
        (
            Ok(AbmAccount {
                organization: "Komira".to_owned(),
            }),
            MdmAbmResult::Accepted,
            "Komira",
            "",
        ),
        (
            Err(AbmCheckError::Rejected("401 from the session".to_owned())),
            MdmAbmResult::Rejected,
            "",
            "401 from the session",
        ),
        (
            Err(AbmCheckError::Unreachable("timed out".to_owned())),
            MdmAbmResult::Unreachable,
            "",
            "timed out",
        ),
    ];
    for (answer, result, organization, detail) in cases {
        let abm = FakeAbm::answering(answer);
        let rig = Rig::asking(
            "verify-abm-answers",
            Arc::clone(&abm) as Arc<dyn AbmAccountCheck>,
        );
        rig.put(SecretKind::AbmToken, TOKEN_JSON.as_bytes());
        let verified = rig
            .coordinator
            .mdm_verify(&alice(), ask(""))
            .await
            .expect("verified");
        let check = verified.abm.clone().expect("an ABM check");
        assert_eq!(check.result, i32::from(result));
        assert_eq!(check.organization, organization);
        assert_eq!(check.detail, detail);
        assert_eq!(abm.asked(), ["CK-unique"]);
        let shown = format!("{verified:?}");
        for part in ["CK-unique", "CS-unique", "AT-unique", "AS-unique"] {
            assert!(!shown.contains(part), "{part} is in the answer");
        }
    }
}
