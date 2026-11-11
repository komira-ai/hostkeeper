//! The certificate upload of the push certificate's lifecycle (`Mdm.PutPushCertificate`): what it
//! stores, refuses and audits, and that no log line of the whole route holds a key. The request and
//! the reply are in `operator_mdm_push_tests.rs`, whose rig this uses.

use std::sync::atomic::Ordering;

use hostkeeper_mdm_push::mdmcert::TransportError;
use hostkeeper_mdm_push::request::pkcs1_pem;
use hostkeeper_mdm_push::testkit::{
    Cipher, Encoding, fixture_key, seal_reply, signed_request_plist,
};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MdmRefusal;
use rsa::pkcs8::EncodePrivateKey as _;
use sha2::Digest as _;

use crate::mdm_secrets::{Secret, SecretKind};
use crate::mdm_setup::MdmSetup;
use crate::operator::Caller;
use crate::operator_mdm_push_tests::{
    DAY, EMAIL, TOPIC, alice, cert_for, flaky_rig, rig, rig_over,
};
use crate::store::audit_failure;

// ---- PutPushCertificate --------------------------------------------------------------

/// Proves: the certificate Apple returns for the pending request is stored with the pending key (as
/// PKCS#1, not with the CSR), the push identity is that pair, the audit holds the topic, the
/// certificate's hash and its expiry and no key, the answer is the topic, the times and what the
/// listing shows, and a renewal for the same topic replaces the pair, says so, and keeps the old
/// versions for their grace.
///
/// Catches: a certificate stored with a key it does not belong to, a CSR stored as a key, and a
/// renewal that cannot be told from a first certificate.
#[tokio::test]
async fn the_certificate_for_the_pending_request_is_stored_with_its_key_and_a_renewal_says_so() {
    let rig = rig("mdm-push-cert");
    rig.create(false).await.expect("made");
    let (key, _) = rig.pending();
    let now = rig.now();
    let cert = cert_for(&key, TOPIC, now - DAY, now + 300 * DAY);
    let first = rig.certificate(&cert, b"").expect("stored");
    assert_eq!(first.topic, TOPIC);
    assert_eq!(first.not_before_unix, now - DAY);
    assert_eq!(first.not_after_unix, now + 300 * DAY);
    assert!(!first.renewal);
    let secret = first.secret.expect("what is shown");
    assert_eq!((secret.kind.as_str(), secret.version), ("push_cert", 1));
    assert_eq!(secret.not_after_unix, Some(now + 300 * DAY));
    assert_eq!(
        rig.audit()[1],
        (
            "alice".to_owned(),
            "mdm_push_cert_put".to_owned(),
            Some("push_cert".to_owned()),
            Some(format!(
                "topic {TOPIC}, fingerprint {}, expires {}",
                hex::encode(sha2::Sha256::digest(cert.as_bytes())),
                now + 300 * DAY
            )),
        )
    );
    let held = rig.open(SecretKind::PushKey).expect("the key moved");
    assert!(
        held.expose()
            .starts_with(b"-----BEGIN RSA PRIVATE KEY-----")
    );
    assert!(!String::from_utf8_lossy(held.expose()).contains("CERTIFICATE REQUEST"));
    let identity = rig
        .world
        .coordinator
        .mdm_setup()
        .push_identity()
        .expect("read")
        .expect("a pair");
    assert_eq!(identity.info.topic, TOPIC);

    rig.create(false).await.expect("a renewal request");
    let (renewed_key, _) = rig.pending();
    let renewal = cert_for(&renewed_key, TOPIC, now - DAY, now + 400 * DAY);
    let second = rig.certificate(&renewal, b"").expect("renewed");
    assert!(second.renewal);
    assert_eq!(second.secret.expect("shown").version, 2);
    let listed = rig
        .world
        .coordinator
        .mdm_list_secrets(&alice())
        .expect("listed");
    let versions: Vec<(String, u32, bool)> = listed
        .secrets
        .iter()
        .filter(|s| s.kind.starts_with("push_cert") || s.kind == "push_key")
        .map(|s| (s.kind.clone(), s.version, s.superseded_unix.is_some()))
        .collect();
    assert_eq!(
        versions,
        [
            ("push_cert".to_owned(), 1, true),
            ("push_cert".to_owned(), 2, false),
            ("push_key".to_owned(), 1, true),
            ("push_key".to_owned(), 2, false),
        ]
    );
    let shown = format!("{:?}{:?}", rig.audit(), listed);
    assert!(!shown.contains("PRIVATE KEY"));
}

/// Proves: an existing certificate and its key are stored as they are, the key as PKCS#1 (what
/// NanoHUB holds) or as PKCS#8, with no pending request, and the key never appears in an answer, a
/// listing or the audit.
///
/// Catches: a migration that needs a request it has no use for, or that refuses the key it must
/// carry over.
#[tokio::test]
async fn an_existing_certificate_and_key_are_stored_as_they_are() {
    let rig = rig("mdm-push-cert-migrate");
    let key = fixture_key(0);
    let now = rig.now();
    let cert = cert_for(key, TOPIC, now - DAY, now + 200 * DAY);
    let pkcs1 = pkcs1_pem(key).expect("pem");
    let answer = rig.certificate(&cert, pkcs1.expose()).expect("stored");
    assert!(!answer.renewal);
    let held = rig.open(SecretKind::PushKey).expect("held");
    assert_eq!(held.expose(), pkcs1.expose());
    let pkcs8 = key.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).expect("pem");
    let again = rig
        .certificate(&cert, pkcs8.as_bytes())
        .expect("stored again");
    assert!(again.renewal);
    assert_eq!(
        rig.open(SecretKind::PushKey).expect("held").expose(),
        pkcs8.as_bytes()
    );
    let shown = format!(
        "{answer:?}{again:?}{:?}{:?}",
        rig.audit(),
        rig.world.coordinator.mdm_list_secrets(&alice())
    );
    let body = pkcs1_pem(key).expect("pem");
    let marker = std::str::from_utf8(&body.expose()[40..80])
        .expect("text")
        .trim()
        .to_owned();
    assert!(!shown.contains("PRIVATE KEY"));
    assert!(!shown.contains(&marker));
    assert_eq!(
        rig.world
            .coordinator
            .mdm_setup()
            .push_identity()
            .expect("read")
            .expect("a pair")
            .info
            .topic,
        TOPIC
    );
}

/// Proves: each way a certificate can be wrong is refused with its token and audited, and nothing
/// is stored: not a certificate, one with no topic, a topic that is not an MDM one, a key that is
/// not its own (uploaded, or the pending request's), a key that is not a key or not RSA, an expired
/// certificate, a renewal that names another topic, no pending request when no key is given, an
/// upload over 16 KiB, and a coordinator with no master key.
///
/// Catches: a push certificate stored that APNs would then refuse, or one whose topic would
/// strand every enrolled Mac.
#[tokio::test]
async fn a_certificate_that_is_wrong_is_refused_for_its_own_reason() {
    let rig = rig("mdm-push-cert-refused");
    let key = fixture_key(0);
    let now = rig.now();
    let good = cert_for(key, TOPIC, now - DAY, now + 200 * DAY);
    let key_pem = pkcs1_pem(key).expect("pem");
    let refusal = |cert: &str, key: &[u8]| rig.certificate(cert, key).unwrap_err();

    assert_eq!(refusal(&good, b""), MdmRefusal::NoPendingRequest);
    assert_eq!(refusal("", key_pem.expose()), MdmRefusal::InvalidRequest);
    assert_eq!(
        refusal(&"a".repeat(16 * 1024 + 1), key_pem.expose()),
        MdmRefusal::InvalidRequest
    );
    assert_eq!(
        refusal(&good, &vec![b'a'; 16 * 1024 + 1]),
        MdmRefusal::InvalidRequest
    );
    assert_eq!(
        refusal("not a certificate", key_pem.expose()),
        MdmRefusal::CertificateInvalid
    );
    let plain = rcgen::generate_simple_self_signed(vec!["x".to_owned()]).expect("a cert");
    assert_eq!(
        refusal(&plain.cert.pem(), key_pem.expose()),
        MdmRefusal::CertificateInvalid
    );
    assert_eq!(refusal(&good, b"not a key"), MdmRefusal::KeyInvalid);
    let ec = rcgen::KeyPair::generate().expect("a key");
    assert_eq!(
        refusal(&good, ec.serialize_pem().as_bytes()),
        MdmRefusal::KeyInvalid
    );
    let other = pkcs1_pem(fixture_key(1)).expect("pem");
    assert_eq!(refusal(&good, other.expose()), MdmRefusal::KeyMismatch);
    let expired = cert_for(key, TOPIC, now - 3 * DAY, now - DAY);
    assert_eq!(
        refusal(&expired, key_pem.expose()),
        MdmRefusal::CertificateExpired
    );
    let foreign = cert_for(key, "com.example.app", now - DAY, now + DAY);
    assert_eq!(
        refusal(&foreign, key_pem.expose()),
        MdmRefusal::CertificateInvalid
    );
    assert!(rig.stored().is_empty());

    // A pending request whose key is not the certificate's.
    rig.create(false).await.expect("made");
    assert_eq!(refusal(&good, b""), MdmRefusal::KeyMismatch);
    // A certificate is in use: a renewal keeps its topic.
    rig.certificate(&good, key_pem.expose()).expect("stored");
    let changed = cert_for(
        key,
        "com.apple.mgmt.External.other",
        now - DAY,
        now + 200 * DAY,
    );
    assert_eq!(
        refusal(&changed, key_pem.expose()),
        MdmRefusal::TopicChanged
    );
    assert_eq!(
        rig.world
            .coordinator
            .mdm_setup()
            .push_identity()
            .expect("read")
            .expect("a pair")
            .info
            .topic,
        TOPIC
    );

    for (caller, want) in [
        (Caller::Anonymous, MdmRefusal::Unauthorized),
        (Caller::Ci, MdmRefusal::Forbidden),
    ] {
        let denied = rig.world.coordinator.mdm_put_push_certificate(
            &caller,
            proto::PutPushCertificateRequest {
                certificate_pem: good.as_bytes().to_vec(),
                key_pem: Vec::new(),
            },
        );
        assert_eq!(denied.unwrap_err(), want);
    }
    let reasons: Vec<String> = rig
        .audit()
        .into_iter()
        .filter(|entry| entry.1 == "mdm_push_cert_put")
        .map(|entry| entry.3.expect("a reason"))
        .collect();
    assert_eq!(reasons.len(), 14);
    assert!(reasons[12].starts_with("topic "), "{reasons:?}");
    assert_eq!(reasons[13], "refused: MDM_REFUSAL_TOPIC_CHANGED");

    let unkeyed = rig_over("mdm-push-cert-unkeyed", MdmSetup::volatile());
    assert_eq!(
        unkeyed.certificate(&good, key_pem.expose()).unwrap_err(),
        MdmRefusal::SecretsUnavailable
    );
}

/// Proves: a certificate in use that cannot be read does not block its replacement; the store that
/// fails reading the pending request or the certificate in use is `store_failed`; the store that
/// fails writing, the key or the certificate after it, is `store_failed` and audited; and an audit
/// that cannot be written stops the upload before anything is stored.
///
/// Catches: a push certificate that can never be replaced because the old one is damaged, and a
/// half-stored pair reported as stored.
#[tokio::test]
async fn store_failures_of_a_certificate_upload_are_told_and_an_unaudited_upload_is_not_made() {
    let (rig, store) = flaky_rig("mdm-push-cert-store");
    rig.create(false).await.expect("made");
    let (key, _) = rig.pending();
    let now = rig.now();
    let cert = cert_for(&key, TOPIC, now - DAY, now + 200 * DAY);

    store.reads_fail.store(true, Ordering::SeqCst);
    assert_eq!(
        rig.certificate(&cert, b"").unwrap_err(),
        MdmRefusal::StoreFailed
    );
    let key_pem = pkcs1_pem(&key).expect("pem");
    assert_eq!(
        rig.certificate(&cert, key_pem.expose()).unwrap_err(),
        MdmRefusal::StoreFailed
    );
    store.reads_fail.store(false, Ordering::SeqCst);

    store.puts_before_failing.store(0, Ordering::SeqCst);
    assert_eq!(
        rig.certificate(&cert, b"").unwrap_err(),
        MdmRefusal::StoreFailed
    );
    store.puts_before_failing.store(1, Ordering::SeqCst);
    assert_eq!(
        rig.certificate(&cert, b"").unwrap_err(),
        MdmRefusal::StoreFailed
    );
    assert!(
        rig.world
            .coordinator
            .mdm_setup()
            .push_identity()
            .expect("read")
            .is_none(),
        "no certificate was stored: the key alone is no identity"
    );
    store
        .puts_before_failing
        .store(usize::MAX, Ordering::SeqCst);

    // A certificate in use that does not read is replaced by any valid one.
    rig.world
        .coordinator
        .mdm_setup()
        .put_secret(
            SecretKind::PushCert,
            &Secret::new(b"damaged".to_vec()),
            None,
            1,
        )
        .expect("stored");
    let stored = rig.certificate(&cert, b"").expect("replaced");
    assert!(stored.renewal);

    rig.world.fake.refuse(|_| Some(audit_failure()));
    assert_eq!(
        rig.certificate(&cert, b"").unwrap_err(),
        MdmRefusal::AuditUnavailable
    );
}

/// Proves: the three calls name every error they log without a secret: a key's bytes are in no log
/// line of a whole request, reply and certificate upload.
///
/// Catches: a secret in a log line (T13).
#[tokio::test]
async fn no_log_line_of_the_whole_route_holds_a_key() {
    use crate::testutil::Log;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct Everything(Log);
    impl<'a> MakeWriter<'a> for Everything {
        type Writer = Log;
        fn make_writer(&'a self) -> Log {
            self.0.clone()
        }
    }
    // The rig installs a subscriber of its own, so it is made first.
    let rig = rig("mdm-push-log");
    let log = Everything::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(log.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish(),
    );
    rig.mdmcert
        .answer_next(Err(TransportError("no route".to_owned())));
    assert!(rig.create(true).await.is_err());
    let made = rig.create(true).await.expect("made");
    let (key, _) = rig.pending();
    let key_pem = pkcs1_pem(&key).expect("pem");
    let now = rig.now();
    rig.certificate(&cert_for(&key, TOPIC, now - DAY, now + DAY), b"")
        .expect("stored");
    let reply = seal_reply(
        &signed_request_plist(&made.csr_pem),
        &rig.encryption_key(),
        Cipher::Aes256,
        Encoding::Der,
    );
    rig.reply(&reply).expect("answered");
    let text = log.0.text();
    assert!(text.contains("not taken by mdmcert.download"), "{text}");
    let marker = std::str::from_utf8(&key_pem.expose()[40..80])
        .expect("text")
        .trim()
        .to_owned();
    assert!(!text.contains(&marker));
    assert!(!text.contains("PRIVATE KEY"));
    assert!(!text.contains(EMAIL));
}
