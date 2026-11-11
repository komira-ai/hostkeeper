//! The calls of the push certificate's lifecycle as plain functions: what each makes, refuses and
//! audits, that nothing secret leaves in an answer, an audit entry or a log line, and the model
//! of the whole route (a request, mdmcert.download's emailed reply, Apple's certificate).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use base64::Engine as _;
use hostkeeper_mdm_push::mdmcert::{API_KEY, HttpReply, TransportError};
use hostkeeper_mdm_push::testkit::{
    Cipher, Encoding, FakeMdmcert, fixture_key, push_certificate_pem, seal_reply, seal_reply_raw,
    signed_request_plist,
};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MdmRefusal;
use rsa::RsaPrivateKey;
use rsa::pkcs1::DecodeRsaPrivateKey as _;
use sha2::Digest as _;

use crate::login_support::World;
use crate::mdm_push::split_pending;
use crate::mdm_secrets::{MasterKey, Secret, SecretKind};
use crate::mdm_setup::MdmSetup;
use crate::operator::Caller;
use crate::store::audit_failure;
use crate::store::mdm_setup::files::FileSetup;
use crate::store::mdm_setup::{SecretRow, SetupError, SetupStore};

pub(crate) const EMAIL: &str = "ops@example.org";
pub(crate) const TOPIC: &str = "com.apple.mgmt.External.d1801ae1-3ef7-483e-9f6d-4979e2ebfc09";
pub(crate) const DAY: i64 = 86_400;

pub(crate) fn alice() -> Caller {
    Caller::Operator("alice".parse().expect("a token name"))
}

pub(crate) fn keyed(setup: MdmSetup) -> MdmSetup {
    setup.with_key(MasterKey::from_bytes(&[0x21; 32]).expect("a key"))
}

pub(crate) struct Rig {
    pub(crate) world: World,
    pub(crate) mdmcert: Arc<FakeMdmcert>,
}

pub(crate) fn rig_over(name: &str, setup: MdmSetup) -> Rig {
    let mdmcert = Arc::new(FakeMdmcert::new());
    let transport = mdmcert.clone();
    let world = World::build(
        name,
        crate::login_support::settings(),
        crate::login_support::counting(),
        |c, _| c.with_mdm_setup(setup).with_mdmcert(transport),
    );
    Rig { world, mdmcert }
}

pub(crate) fn rig(name: &str) -> Rig {
    rig_over(name, keyed(MdmSetup::volatile()))
}

impl Rig {
    pub(crate) fn now(&self) -> i64 {
        i64::try_from(self.world.coordinator.ops.unix_now()).expect("a time")
    }

    pub(crate) fn audit(&self) -> Vec<(String, String, Option<String>, Option<String>)> {
        self.world
            .audit()
            .into_iter()
            .map(|entry| (entry.by, entry.action, entry.subject, entry.reason))
            .collect()
    }

    pub(crate) async fn create(
        &self,
        submit: bool,
    ) -> Result<proto::CreatePushRequestResponse, MdmRefusal> {
        self.world
            .coordinator
            .mdm_create_push_request(
                &alice(),
                proto::CreatePushRequestRequest {
                    email: EMAIL.to_owned(),
                    submit,
                },
            )
            .await
    }

    pub(crate) fn open(&self, kind: SecretKind) -> Option<Secret> {
        self.world
            .coordinator
            .mdm_setup()
            .open(kind)
            .expect("opens")
    }

    /// The key mdmcert.download's reply is encrypted to.
    pub(crate) fn encryption_key(&self) -> RsaPrivateKey {
        let pem = self.open(SecretKind::PushEncryptionKey).expect("held");
        RsaPrivateKey::from_pkcs1_pem(std::str::from_utf8(pem.expose()).expect("text"))
            .expect("a key")
    }

    /// The key of the pending request and its CSR.
    pub(crate) fn pending(&self) -> (RsaPrivateKey, String) {
        let bundle = self.open(SecretKind::PushPendingKey).expect("held");
        let (key, csr) = split_pending(bundle.expose()).expect("a bundle");
        let key =
            RsaPrivateKey::from_pkcs1_pem(std::str::from_utf8(&key).expect("text")).expect("a key");
        (key, csr)
    }

    pub(crate) fn reply(&self, upload: &[u8]) -> Result<proto::PutPushReplyResponse, MdmRefusal> {
        self.world.coordinator.mdm_put_push_reply(
            &alice(),
            proto::PutPushReplyRequest {
                reply: upload.to_vec(),
            },
        )
    }

    pub(crate) fn certificate(
        &self,
        certificate_pem: &str,
        key_pem: &[u8],
    ) -> Result<proto::PutPushCertificateResponse, MdmRefusal> {
        self.world.coordinator.mdm_put_push_certificate(
            &alice(),
            proto::PutPushCertificateRequest {
                certificate_pem: certificate_pem.as_bytes().to_vec(),
                key_pem: key_pem.to_vec(),
            },
        )
    }

    pub(crate) fn stored(&self) -> Vec<(String, u32)> {
        self.world
            .coordinator
            .mdm_setup()
            .list_secrets()
            .expect("listed")
            .into_iter()
            .map(|info| (info.kind.as_str().to_owned(), info.version))
            .collect()
    }
}

pub(crate) fn cert_for(key: &RsaPrivateKey, topic: &str, from: i64, until: i64) -> String {
    push_certificate_pem(key, topic, from, until)
}

pub(crate) fn refused(
    action: &str,
    subject: Option<&str>,
    token: &str,
) -> (String, String, Option<String>, Option<String>) {
    (
        "alice".to_owned(),
        action.to_owned(),
        subject.map(str::to_owned),
        Some(format!("refused: {token}")),
    )
}

// ---- CreatePushRequest ----------------------------------------------------------------

/// Proves: a request is made with a pending key and an encryption key sealed (nothing of either
/// leaves), the CSR and the encryption certificate are the answer, the same two are posted to
/// mdmcert.download as the service's client posts them (base64 of the PEM, the email, the public
/// API key), and the audit holds one entry under the user's name with the CSR's hash and no key.
///
/// Catches: a key returned or audited, a request posted with another key than the pending one,
/// and a request made with no audit entry.
#[tokio::test]
async fn a_request_is_posted_to_mdmcert_and_keeps_both_keys_sealed() {
    let rig = rig("mdm-push-create");
    let made = rig.create(true).await.expect("made");
    assert!(made.submitted);
    assert!(
        made.csr_pem
            .starts_with("-----BEGIN CERTIFICATE REQUEST-----")
    );
    assert!(
        made.encryption_certificate_pem
            .starts_with("-----BEGIN CERTIFICATE-----")
    );

    let posted = rig.mdmcert.posted();
    assert_eq!(posted.len(), 1);
    let json: serde_json::Value = serde_json::from_slice(&posted[0]).expect("json");
    let decode = |name: &str| {
        base64::engine::general_purpose::STANDARD
            .decode(json[name].as_str().expect("a string"))
            .expect("base64")
    };
    assert_eq!(decode("csr"), made.csr_pem.as_bytes());
    assert_eq!(
        decode("encrypt"),
        made.encryption_certificate_pem.as_bytes()
    );
    assert_eq!(json["email"], EMAIL);
    assert_eq!(json["key"], API_KEY);

    assert_eq!(
        rig.stored(),
        [
            ("push_encryption_key".to_owned(), 1),
            ("push_pending_key".to_owned(), 1)
        ]
    );
    let (_, csr) = rig.pending();
    assert_eq!(csr, made.csr_pem, "the CSR kept is the CSR answered");
    let csr_sha = hex::encode(sha2::Sha256::digest(made.csr_pem.as_bytes()));
    assert_eq!(
        rig.audit(),
        [(
            "alice".to_owned(),
            "mdm_push_csr".to_owned(),
            Some("push_pending_key".to_owned()),
            Some(format!("csr {csr_sha}, submitted true")),
        )]
    );
    let shown = format!(
        "{made:?}{:?}{:?}",
        rig.audit(),
        rig.world.coordinator.mdm_list_secrets(&alice())
    );
    assert!(!shown.contains("PRIVATE KEY"));
    assert!(
        !shown.contains(EMAIL),
        "the audit and the listing name no address"
    );
}

/// Proves: without `submit` nothing is posted and the answer says so, and a second request
/// replaces the first (version 2 of both records, the first kept for its grace).
///
/// Catches: an address sent to the service that the operator did not ask for, and a second request
/// that leaves the old pending key in use.
#[tokio::test]
async fn a_request_without_submit_posts_nothing_and_a_second_one_replaces_the_first() {
    let rig = rig("mdm-push-create-again");
    let first = rig.create(false).await.expect("made");
    assert!(!first.submitted);
    assert!(rig.mdmcert.posted().is_empty());
    let second = rig.create(false).await.expect("made");
    assert_ne!(first.csr_pem, second.csr_pem);
    assert_eq!(
        rig.stored(),
        [
            ("push_encryption_key".to_owned(), 1),
            ("push_encryption_key".to_owned(), 2),
            ("push_pending_key".to_owned(), 1),
            ("push_pending_key".to_owned(), 2),
        ]
    );
    assert_eq!(rig.pending().1, second.csr_pem);
    assert_eq!(rig.audit().len(), 2);
}

/// Proves: an email that is not one address is refused `invalid_request` before a key is made, no
/// key is stored and nothing is posted; a coordinator with no master key refuses
/// `secrets_unavailable`; a caller that is not an operator is refused as every call is. Each refusal
/// after the caller is known is audited with its token.
///
/// Catches: keys made for an address that is not one, and a refusal that leaves no word in the audit.
#[tokio::test]
async fn a_request_for_a_bad_address_or_without_a_master_key_is_refused_and_audited() {
    let rig = rig("mdm-push-create-refused");
    for bad in ["", "nobody", "a@@b.c", "a b@example.org"] {
        let refusal = rig
            .world
            .coordinator
            .mdm_create_push_request(
                &alice(),
                proto::CreatePushRequestRequest {
                    email: bad.to_owned(),
                    submit: true,
                },
            )
            .await;
        assert_eq!(refusal.unwrap_err(), MdmRefusal::InvalidRequest, "{bad:?}");
    }
    assert!(rig.stored().is_empty());
    assert!(rig.mdmcert.posted().is_empty());
    assert_eq!(
        rig.audit()[0],
        refused("mdm_push_csr", None, "MDM_REFUSAL_INVALID_REQUEST")
    );
    for (caller, want) in [
        (Caller::Anonymous, MdmRefusal::Unauthorized),
        (Caller::Ci, MdmRefusal::Forbidden),
    ] {
        let denied = rig
            .world
            .coordinator
            .mdm_create_push_request(
                &caller,
                proto::CreatePushRequestRequest {
                    email: EMAIL.to_owned(),
                    submit: true,
                },
            )
            .await;
        assert_eq!(denied.unwrap_err(), want);
    }
    assert_eq!(
        rig.audit().len(),
        4,
        "a caller that is no operator is not audited"
    );

    let unkeyed = rig_over("mdm-push-create-unkeyed", MdmSetup::volatile());
    assert_eq!(
        unkeyed.create(true).await.unwrap_err(),
        MdmRefusal::SecretsUnavailable
    );
    assert_eq!(
        unkeyed.audit(),
        [refused(
            "mdm_push_csr",
            None,
            "MDM_REFUSAL_SECRETS_UNAVAILABLE"
        )]
    );
}

/// Proves: when mdmcert.download cannot be reached, answers a status other than 200, or answers that
/// it did not take the request, the call is refused `mdmcert_unavailable`, the pending request
/// stays stored for the operator to use another route, and the audit holds the attempt and its
/// refusal.
///
/// Catches: a request reported as sent that the service never took, and a key lost with it.
#[tokio::test]
async fn a_service_that_does_not_take_the_request_is_told_and_the_request_stays_pending() {
    let rig = rig("mdm-push-create-service");
    for answer in [
        Err(TransportError("no route".to_owned())),
        Ok(HttpReply {
            status: 502,
            body: Vec::new(),
        }),
        Ok(HttpReply {
            status: 200,
            body: br#"{"result":"failure","reason":"unregistered"}"#.to_vec(),
        }),
    ] {
        rig.mdmcert.answer_next(answer);
        assert_eq!(
            rig.create(true).await.unwrap_err(),
            MdmRefusal::MdmcertUnavailable
        );
    }
    assert_eq!(
        rig.stored().len(),
        6,
        "each attempt kept its pending request"
    );
    let audit = rig.audit();
    assert_eq!(audit.len(), 6);
    assert_eq!(
        audit[1],
        refused(
            "mdm_push_csr",
            Some("push_pending_key"),
            "MDM_REFUSAL_MDMCERT_UNAVAILABLE"
        )
    );
}

/// A store that reads from a file store and fails a write, or every read, on demand.
pub(crate) struct Flaky {
    inner: FileSetup,
    pub(crate) puts_before_failing: AtomicUsize,
    pub(crate) reads_fail: AtomicBool,
}

impl Flaky {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: FileSetup::volatile(),
            puts_before_failing: AtomicUsize::new(usize::MAX),
            reads_fail: AtomicBool::new(false),
        })
    }
}

impl SetupStore for Flaky {
    fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError> {
        let left = self.puts_before_failing.load(Ordering::SeqCst);
        if left == 0 {
            return Err(SetupError::Store("the disk is full".to_owned()));
        }
        self.puts_before_failing
            .store(left.saturating_sub(1), Ordering::SeqCst);
        self.inner.put_secret(row)
    }

    fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
        if self.reads_fail.load(Ordering::SeqCst) {
            return Err(SetupError::Store("unreadable".to_owned()));
        }
        self.inner.secrets()
    }

    fn prune_superseded(&self, at: i64) -> Result<u64, SetupError> {
        self.inner.prune_superseded(at)
    }

    fn settings(&self) -> Result<BTreeMap<String, String>, SetupError> {
        if self.reads_fail.load(Ordering::SeqCst) {
            return Err(SetupError::Store("unreadable".to_owned()));
        }
        self.inner.settings()
    }

    fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupError> {
        self.inner.set_setting(name, value)
    }
}

pub(crate) fn flaky_rig(name: &str) -> (Rig, Arc<Flaky>) {
    let store = Flaky::new();
    let shared: Arc<dyn SetupStore> = store.clone();
    let setup = MdmSetup::new(
        shared,
        Some(MasterKey::from_bytes(&[3; 32]).expect("a key")),
        crate::enrollment::os_random(),
    );
    (rig_over(name, setup), store)
}

/// Proves: a store that cannot take the request's records refuses `store_failed` with the attempt
/// and the refusal audited and nothing posted; a coordinator whose audit cannot be written makes no
/// key and stores nothing.
///
/// Catches: a request posted whose key was never kept, and a key kept with no audit entry.
#[tokio::test]
async fn a_request_whose_keys_cannot_be_kept_is_not_posted_and_one_that_cannot_be_audited_is_not_made()
 {
    let (rig, store) = flaky_rig("mdm-push-create-store");
    store.puts_before_failing.store(0, Ordering::SeqCst);
    assert_eq!(rig.create(true).await.unwrap_err(), MdmRefusal::StoreFailed);
    assert!(rig.mdmcert.posted().is_empty());
    let audit = rig.audit();
    assert_eq!(audit.len(), 2);
    assert_eq!(
        audit[1],
        refused(
            "mdm_push_csr",
            Some("push_pending_key"),
            "MDM_REFUSAL_STORE_FAILED"
        )
    );
    // The second record fails after the first was kept.
    store.puts_before_failing.store(1, Ordering::SeqCst);
    assert_eq!(rig.create(true).await.unwrap_err(), MdmRefusal::StoreFailed);
    assert!(rig.mdmcert.posted().is_empty());

    let audited = rig_over("mdm-push-create-audit", keyed(MdmSetup::volatile()));
    audited.world.fake.refuse(|_| Some(audit_failure()));
    assert_eq!(
        audited.create(true).await.unwrap_err(),
        MdmRefusal::AuditUnavailable
    );
    assert!(audited.stored().is_empty());
    assert!(audited.mdmcert.posted().is_empty());
}

// ---- PutPushReply --------------------------------------------------------------------

/// Proves: the emailed reply (hex text with line breaks, or the raw bytes) decrypts with the
/// pending request's encryption key to the signed request for the pending CSR, which is the answer,
/// and the audit holds the upload's hash and none of the plaintext; nothing is stored.
///
/// Catches: a reply that does not decrypt the way the service's client decrypts it, and a signed
/// request that is for another CSR than the one pending.
#[tokio::test]
async fn the_emailed_reply_is_decrypted_to_the_signed_request_for_the_pending_csr() {
    let rig = rig("mdm-push-reply");
    let made = rig.create(true).await.expect("made");
    let plain = signed_request_plist(&made.csr_pem);
    let hex_text = seal_reply(
        &plain,
        &rig.encryption_key(),
        Cipher::Aes256,
        Encoding::Streaming,
    );
    let wrapped: Vec<u8> = hex_text
        .chunks(76)
        .flat_map(|line| line.iter().copied().chain(*b"\r\n"))
        .collect();
    let answer = rig.reply(&wrapped).expect("decrypted");
    assert_eq!(answer.signed_request, plain);
    let raw = seal_reply_raw(
        &plain,
        &rig.encryption_key(),
        Cipher::TripleDes,
        Encoding::Der,
    );
    assert_eq!(rig.reply(&raw).expect("decrypted").signed_request, plain);

    let audit = rig.audit();
    assert_eq!(audit.len(), 3);
    assert_eq!(
        audit[1],
        (
            "alice".to_owned(),
            "mdm_push_reply_put".to_owned(),
            Some("push_encryption_key".to_owned()),
            Some(format!(
                "reply {}",
                hex::encode(sha2::Sha256::digest(&wrapped))
            )),
        )
    );
    assert!(!format!("{audit:?}").contains("PushCertRequestCSR"));
    assert_eq!(rig.stored().len(), 2, "the reply is not stored");
}

/// Proves: every reply that is not the signed request for the pending CSR is refused with its own
/// token and audited: none pending, empty, over 32 KiB, bytes that are not enveloped data, a message
/// encrypted to another key, one that decrypts to text that is not a property list, and one signed
/// for another CSR.
///
/// Catches: a reply for another request accepted, which would send Apple a request that no pending
/// key can finish.
#[tokio::test]
async fn a_reply_that_is_not_for_the_pending_request_is_refused_with_its_reason() {
    let rig = rig("mdm-push-reply-refused");
    assert_eq!(
        rig.reply(b"abcd").unwrap_err(),
        MdmRefusal::NoPendingRequest
    );
    let made = rig.create(false).await.expect("made");
    assert_eq!(rig.reply(b"").unwrap_err(), MdmRefusal::InvalidRequest);
    assert_eq!(
        rig.reply(&vec![b'a'; 32 * 1024 + 1]).unwrap_err(),
        MdmRefusal::InvalidRequest
    );
    assert_eq!(
        rig.reply(b"not a message").unwrap_err(),
        MdmRefusal::ReplyUndecryptable
    );
    let other_key = seal_reply(b"x", fixture_key(2), Cipher::Aes256, Encoding::Der);
    assert_eq!(
        rig.reply(&other_key).unwrap_err(),
        MdmRefusal::ReplyUndecryptable
    );
    let key = rig.encryption_key();
    let plain_text = seal_reply(b"plain text", &key, Cipher::Aes128, Encoding::Der);
    assert_eq!(
        rig.reply(&plain_text).unwrap_err(),
        MdmRefusal::ReplyNotForRequest
    );
    let elsewhere = hostkeeper_mdm_push::request::csr_pem(fixture_key(3), EMAIL).expect("a csr");
    let for_other = seal_reply(
        &signed_request_plist(&elsewhere),
        &key,
        Cipher::Aes256,
        Encoding::Der,
    );
    assert_eq!(
        rig.reply(&for_other).unwrap_err(),
        MdmRefusal::ReplyNotForRequest
    );
    let right = seal_reply(
        &signed_request_plist(&made.csr_pem),
        &key,
        Cipher::Aes256,
        Encoding::Der,
    );
    rig.reply(&right).expect("the one for the pending CSR");

    let reasons: Vec<String> = rig
        .audit()
        .into_iter()
        .filter(|entry| entry.1 == "mdm_push_reply_put")
        .map(|entry| entry.3.expect("a reason"))
        .collect();
    assert_eq!(
        reasons[..7],
        [
            "refused: MDM_REFUSAL_NO_PENDING_REQUEST",
            "refused: MDM_REFUSAL_INVALID_REQUEST",
            "refused: MDM_REFUSAL_INVALID_REQUEST",
            "refused: MDM_REFUSAL_REPLY_UNDECRYPTABLE",
            "refused: MDM_REFUSAL_REPLY_UNDECRYPTABLE",
            "refused: MDM_REFUSAL_REPLY_NOT_FOR_REQUEST",
            "refused: MDM_REFUSAL_REPLY_NOT_FOR_REQUEST",
        ]
    );
    assert!(reasons[7].starts_with("reply "));
    for (caller, want) in [
        (Caller::Anonymous, MdmRefusal::Unauthorized),
        (Caller::Ci, MdmRefusal::Forbidden),
    ] {
        let denied = rig
            .world
            .coordinator
            .mdm_put_push_reply(&caller, proto::PutPushReplyRequest { reply: vec![1] });
        assert_eq!(denied.unwrap_err(), want);
    }
}

/// Proves: records that cannot be read or are not what a request leaves are the store's fault and
/// are refused `store_failed`, never taken for a bad upload: an unreadable store, a pending record
/// with no CSR, an encryption key that is not a key; and a reply for a coordinator whose audit
/// cannot be written is refused `audit_unavailable` before it is answered.
///
/// Catches: an operator told that a good reply was bad because of a damaged record.
#[tokio::test]
async fn damaged_request_records_are_the_stores_failure_and_an_unaudited_reply_is_not_answered() {
    let (rig, store) = flaky_rig("mdm-push-reply-store");
    let made = rig.create(false).await.expect("made");
    let good = seal_reply(
        &signed_request_plist(&made.csr_pem),
        &rig.encryption_key(),
        Cipher::Aes256,
        Encoding::Der,
    );
    store.reads_fail.store(true, Ordering::SeqCst);
    assert_eq!(rig.reply(&good).unwrap_err(), MdmRefusal::StoreFailed);
    store.reads_fail.store(false, Ordering::SeqCst);

    let setup = rig.world.coordinator.mdm_setup();
    let put = |kind: SecretKind, bytes: &[u8]| {
        setup
            .put_secret(kind, &Secret::new(bytes.to_vec()), None, 1)
            .expect("stored");
    };
    put(SecretKind::PushPendingKey, b"a key and no csr");
    assert_eq!(rig.reply(&good).unwrap_err(), MdmRefusal::StoreFailed);
    put(
        SecretKind::PushPendingKey,
        &crate::mdm_push::pending_bundle(b"key", &made.csr_pem),
    );
    put(SecretKind::PushEncryptionKey, b"not a key");
    assert_eq!(rig.reply(&good).unwrap_err(), MdmRefusal::StoreFailed);

    let fine = rig_over("mdm-push-reply-audit", keyed(MdmSetup::volatile()));
    let made = fine.create(false).await.expect("made");
    let good = seal_reply(
        &signed_request_plist(&made.csr_pem),
        &fine.encryption_key(),
        Cipher::Aes256,
        Encoding::Der,
    );
    fine.world.fake.refuse(|_| Some(audit_failure()));
    assert_eq!(fine.reply(&good).unwrap_err(), MdmRefusal::AuditUnavailable);
}

/// Proves: a request whose keys or certificate cannot be made (the clock is past what a certificate
/// can hold) is refused as the coordinator's own failure, `store_failed`, with the attempt's refusal
/// audited, and nothing is stored or posted.
///
/// Catches: a request that reports success with no key made.
#[tokio::test]
async fn a_request_that_cannot_be_made_is_refused_and_audited_and_posts_nothing() {
    use crate::ops::{OpsState, OpsStore};
    use crate::store::Fake;

    let mdmcert = Arc::new(FakeMdmcert::new());
    let transport = mdmcert.clone();
    let world = World::build(
        "mdm-push-create-clock",
        crate::login_support::settings(),
        crate::login_support::counting(),
        |c, _| {
            c.with_ops(
                OpsStore::over(Arc::new(Fake::new()), OpsState::default())
                    .with_unix(Arc::new(|| u64::MAX / 2)),
            )
            .with_mdm_setup(keyed(MdmSetup::volatile()))
            .with_mdmcert(transport)
        },
    );
    let rig = Rig { world, mdmcert };
    assert_eq!(rig.create(true).await.unwrap_err(), MdmRefusal::StoreFailed);
    assert!(rig.stored().is_empty());
    assert!(rig.mdmcert.posted().is_empty());
    assert_eq!(
        rig.audit(),
        [refused("mdm_push_csr", None, "MDM_REFUSAL_STORE_FAILED")]
    );
}
