//! `Mdm.PutAbmToken` and `Mdm.GetAbmPublicKey` as plain functions over the simulated ABM: who may
//! call, what each refuses and audits, what is stored, and that no answer, audit entry or log line
//! holds a token part, a key or a session.

use std::sync::Arc;

use cms::builder::ContentEncryptionAlgorithm;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MdmRefusal;
use tracing_subscriber::fmt::MakeWriter;

use crate::abm::Abm;
use crate::abm::fake::{
    FakeAbm, Handler, NOW, Sim, built, client_clock, counting_random, recording_sleep, sample_token,
};
use crate::abm::keys::{generate, parse_key, pkcs1_pem, self_signed};
use crate::login_support::World;
use crate::mdm_secrets::{MasterKey, Secret, SecretKind};
use crate::mdm_setup::MdmSetup;
use crate::operator::Caller;
use crate::store::audit_failure;
use crate::store::mdm::files::FileMdm;
use crate::store::mdm_setup::files::FileSetup;
use crate::store::mdm_setup::{SecretRow, SetupError, SetupStore};
use crate::testutil::{Log, scratch};

/// 2027-10-31T22:01:36Z.
const EXPIRY_UNIX: i64 = 1_825_020_096;

fn alice() -> Caller {
    Caller::Operator("alice".parse().expect("a token name"))
}

fn token_json() -> Vec<u8> {
    let token = sample_token();
    format!(
        r#"{{"consumer_key":"{}","consumer_secret":"{}","access_token":"{}","access_secret":"{}","access_secret_expiry":"{}"}}"#,
        token.consumer_key,
        token.consumer_secret,
        token.access_token,
        token.access_secret,
        token.access_secret_expiry
    )
    .into_bytes()
}

fn json_request(json: &[u8]) -> proto::PutAbmTokenRequest {
    proto::PutAbmTokenRequest {
        token_json: json.to_vec(),
        token_p7m: Vec::new(),
    }
}

fn p7m_request(p7m: &[u8]) -> proto::PutAbmTokenRequest {
    proto::PutAbmTokenRequest {
        token_json: Vec::new(),
        token_p7m: p7m.to_vec(),
    }
}

/// A subscriber's output at every level.
#[derive(Clone, Default)]
struct Everything(Log);

impl<'a> MakeWriter<'a> for Everything {
    type Writer = Log;

    fn make_writer(&'a self) -> Log {
        self.0.clone()
    }
}

/// A coordinator whose ABM is simulated.
struct Rig {
    world: World,
    sim: Sim,
    fake: Arc<FakeAbm>,
    _dir: hostkeeper_testkit::Scratch,
}

/// A coordinator over a simulated ABM, with a master key when `keyed` and an ABM client when `abm`.
fn start(name: &str, keyed: bool, abm: bool) -> Rig {
    start_over(name, keyed, abm, None)
}

/// [`start`], the setup over `store` when given.
fn start_over(name: &str, keyed: bool, abm: bool, setup_store: Option<Arc<dyn SetupStore>>) -> Rig {
    let sim = Sim::default();
    let handler: Handler = sim.handler();
    let fake = FakeAbm::new(handler);
    let dir = scratch(&format!("{name}-tables"));
    let store = Arc::new(FileMdm::open(&dir.path().join("mdm")).expect("the tables"));
    let (sleep, _) = recording_sleep();
    let client = Abm::new(
        Arc::clone(&store) as _,
        Arc::clone(&fake) as _,
        client_clock(),
        counting_random(),
        sleep,
    );
    let world = World::build(
        name,
        crate::login_support::settings(),
        crate::login_support::counting(),
        |coordinator, _| {
            let mut setup = match setup_store {
                Some(store) => MdmSetup::new(store, None, crate::enrollment::os_random()),
                None => MdmSetup::volatile(),
            };
            if keyed {
                setup = setup.with_key(MasterKey::from_bytes(&[0x31; 32]).expect("a key"));
            }
            let coordinator = coordinator.with_mdm_setup(setup);
            if abm {
                coordinator.with_abm(client)
            } else {
                coordinator
            }
        },
    );
    Rig {
        world,
        sim,
        fake,
        _dir: dir,
    }
}

fn audit(world: &World) -> Vec<(String, String, Option<String>, Option<String>)> {
    world
        .audit()
        .into_iter()
        .map(|entry| (entry.by, entry.action, entry.subject, entry.reason))
        .collect()
}

fn refusal_audit(
    action: &str,
    subject: &str,
    refusal: &str,
) -> (String, String, Option<String>, Option<String>) {
    (
        "alice".to_owned(),
        action.to_owned(),
        Some(subject.to_owned()),
        Some(format!("refused: {refusal}")),
    )
}

/// Proves: a token uploaded as JSON is proved to Apple (a session, then the account), kept as the
/// next version of `abm_token` with the expiry it carries, audited under the user's name with the
/// fingerprint and that expiry before it is stored, and answered by what `ListSecrets` shows and
/// by who Apple says it belongs to; it opens to what was uploaded, a second upload is version 2,
/// and no answer, audit entry, listing or log line holds a part of the token or the session.
///
/// Catches: a token kept before Apple accepted it, an expiry that is not the token's, and a part
/// of the token in a log (T13).
#[tokio::test]
async fn a_token_is_proved_to_apple_kept_audited_and_never_shown() {
    let rig = start("abm-put-token", true, true);
    let log = Everything::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(log.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish(),
    );
    let json = token_json();
    let answer = rig
        .world
        .coordinator
        .mdm_put_abm_token(&alice(), json_request(&json))
        .await
        .expect("kept");
    let info = answer.secret.clone().expect("what is shown");
    assert_eq!(
        (info.kind.as_str(), info.version, info.not_after_unix),
        ("abm_token", 1, Some(EXPIRY_UNIX))
    );
    assert_eq!(
        (answer.server_name.as_str(), answer.org_name.as_str()),
        ("js-mdm", "Org")
    );
    assert_eq!(rig.fake.calls(), ["GET /session", "GET /account"]);

    let fingerprint = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&json));
    assert_eq!(info.fingerprint_sha256, fingerprint);
    assert_eq!(
        audit(&rig.world),
        [(
            "alice".to_owned(),
            "mdm_abm_token_put".to_owned(),
            Some("abm_token".to_owned()),
            Some(format!("fingerprint {fingerprint}, expires {EXPIRY_UNIX}"))
        )]
    );
    let held = rig
        .world
        .coordinator
        .mdm_setup()
        .open(SecretKind::AbmToken)
        .expect("opens")
        .expect("held");
    assert_eq!(held, Secret::new(json.clone()));

    let again = rig
        .world
        .coordinator
        .mdm_put_abm_token(&alice(), json_request(&json))
        .await
        .expect("kept again");
    assert_eq!(again.secret.clone().expect("shown").version, 2);
    let again_shown = again;
    let listed = rig
        .world
        .coordinator
        .mdm_list_secrets(&alice())
        .expect("listed");
    let everything = format!(
        "{answer:?}|{again_shown:?}|{listed:?}|{:?}|{}",
        rig.world.audit(),
        log.0.text()
    );
    for needle in [
        "CS+secret/x",
        "CK_abc",
        "AT~tok",
        "AS secret&",
        "session-1",
        "OAuth realm",
    ] {
        assert!(!everything.contains(needle), "{needle} is in {everything}");
    }
}

/// Proves: a request the call refuses is refused with its token, audited with the token as the
/// reason, and asks Apple nothing and stores nothing: both forms or neither, a value over 32 KiB,
/// a token that is not its five parts, an expiry that is not RFC 3339, a coordinator with no ABM
/// client, and one with no master key; with no master key the refusal is audited too.
///
/// Catches: a malformed upload stored as good, a refusal with no trace in the audit, and a request
/// that reaches Apple before it is judged.
#[tokio::test]
async fn a_token_upload_that_is_refused_is_audited_and_asks_apple_nothing() {
    let rig = start("abm-put-refused", true, true);
    let coordinator = &rig.world.coordinator;
    let both = proto::PutAbmTokenRequest {
        token_json: token_json(),
        token_p7m: vec![0x30],
    };
    let neither = proto::PutAbmTokenRequest::default();
    let too_big = json_request(&vec![b' '; 32 * 1024 + 1]);
    let too_big_p7m = p7m_request(&vec![0x30; 32 * 1024 + 1]);
    let parts = json_request(br#"{"consumer_key":"a"}"#);
    let mut bad_expiry = sample_token();
    bad_expiry.access_secret_expiry = "tomorrow".to_owned();
    let bad_expiry = json_request(
        String::from_utf8(token_json())
            .expect("text")
            .replace(&sample_token().access_secret_expiry, "tomorrow")
            .as_bytes(),
    );
    for request in [both, neither, too_big, too_big_p7m, parts, bad_expiry] {
        assert_eq!(
            coordinator.mdm_put_abm_token(&alice(), request).await,
            Err(MdmRefusal::InvalidRequest)
        );
    }
    assert!(rig.fake.seen().is_empty());
    let want = refusal_audit(
        "mdm_abm_token_put",
        "abm_token",
        "MDM_REFUSAL_INVALID_REQUEST",
    );
    assert_eq!(audit(&rig.world), vec![want; 6]);
    assert!(
        coordinator
            .mdm_list_secrets(&alice())
            .expect("listed")
            .secrets
            .is_empty()
    );

    let keyless = start_over("abm-put-keyless", false, true, None);
    assert_eq!(
        keyless
            .world
            .coordinator
            .mdm_put_abm_token(&alice(), json_request(&token_json()))
            .await,
        Err(MdmRefusal::SecretsUnavailable)
    );
    assert_eq!(
        audit(&keyless.world),
        [refusal_audit(
            "mdm_abm_token_put",
            "abm_token",
            "MDM_REFUSAL_SECRETS_UNAVAILABLE"
        )]
    );
    let none = start("abm-put-none", true, false);
    assert_eq!(
        none.world
            .coordinator
            .mdm_put_abm_token(&alice(), json_request(&token_json()))
            .await,
        Err(MdmRefusal::AbmUnavailable)
    );
    assert_eq!(
        audit(&none.world),
        [refusal_audit(
            "mdm_abm_token_put",
            "abm_token",
            "MDM_REFUSAL_ABM_UNAVAILABLE"
        )]
    );
}

/// Proves: the faults of Apple are told to the operator by their own refusals: a signature Apple
/// refused is `abm_credential_refused`, a clock it refused `abm_clock_refused`, no answer
/// `abm_unreachable`, and a status it answers that the call cannot use is `abm_unreachable` too; each is
/// audited and stores nothing.
///
/// Catches: a wrong clock that sends the operator to ABM to make a token again.
#[tokio::test]
async fn the_faults_of_apple_are_told_and_nothing_is_stored() {
    for (name, fault, refusal, token) in [
        (
            "abm-sig",
            Some((403, "signature_invalid")),
            MdmRefusal::AbmCredentialRefused,
            "MDM_REFUSAL_ABM_CREDENTIAL_REFUSED",
        ),
        (
            "abm-clock",
            Some((403, "timestamp_refused")),
            MdmRefusal::AbmClockRefused,
            "MDM_REFUSAL_ABM_CLOCK_REFUSED",
        ),
        (
            "abm-down",
            Some((503, "")),
            MdmRefusal::AbmUnreachable,
            "MDM_REFUSAL_ABM_UNREACHABLE",
        ),
    ] {
        let rig = start(name, true, true);
        rig.sim.state().session_fault = fault.map(|(status, body)| (status, body.to_owned()));
        assert_eq!(
            rig.world
                .coordinator
                .mdm_put_abm_token(&alice(), json_request(&token_json()))
                .await,
            Err(refusal),
            "{name}"
        );
        assert_eq!(
            audit(&rig.world),
            [refusal_audit("mdm_abm_token_put", "abm_token", token)]
        );
        assert!(
            rig.world
                .coordinator
                .mdm_list_secrets(&alice())
                .expect("listed")
                .secrets
                .is_empty()
        );
    }
}

/// A setup store that fails what it is told to, over one that works.
struct Flaky {
    inner: FileSetup,
    reads: std::sync::atomic::AtomicBool,
    writes: std::sync::atomic::AtomicBool,
}

impl Flaky {
    fn failing(&self, flag: &std::sync::atomic::AtomicBool) -> Result<(), SetupError> {
        if flag.load(std::sync::atomic::Ordering::SeqCst) {
            Err(SetupError::Store("the disk is full".to_owned()))
        } else {
            Ok(())
        }
    }
}

impl SetupStore for Flaky {
    fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError> {
        self.failing(&self.writes)?;
        self.inner.put_secret(row)
    }
    fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
        self.failing(&self.reads)?;
        self.inner.secrets()
    }
    fn prune_superseded(&self, superseded_unix: i64) -> Result<u64, SetupError> {
        self.inner.prune_superseded(superseded_unix)
    }
    fn settings(&self) -> Result<std::collections::BTreeMap<String, String>, SetupError> {
        self.failing(&self.reads)?;
        self.inner.settings()
    }
    fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupError> {
        self.inner.set_setting(name, value)
    }
}

fn flaky() -> Arc<Flaky> {
    Arc::new(Flaky {
        inner: FileSetup::volatile(),
        reads: std::sync::atomic::AtomicBool::new(false),
        writes: std::sync::atomic::AtomicBool::new(false),
    })
}

/// Proves: a write without its audit entry is not made (`audit_unavailable`, Apple not asked of the
/// store), a store that cannot write is `store_failed` after the entry, and settings that cannot be
/// read are `store_failed` before Apple is asked.
///
/// Catches: a token stored with no audit entry, and a store failure reported as Apple's.
#[tokio::test]
async fn a_write_without_its_audit_is_not_made_and_a_failed_store_is_told() {
    let store = flaky();
    let rig = start_over("abm-put-store", true, true, Some(store.clone()));
    store
        .writes
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        rig.world
            .coordinator
            .mdm_put_abm_token(&alice(), json_request(&token_json()))
            .await,
        Err(MdmRefusal::StoreFailed)
    );
    assert_eq!(
        audit(&rig.world)
            .iter()
            .map(|e| (e.1.as_str(), e.3.clone().unwrap_or_default()))
            .collect::<Vec<_>>()
            .len(),
        2,
        "the entry of the attempt and one of its refusal"
    );
    store
        .writes
        .store(false, std::sync::atomic::Ordering::SeqCst);

    store.reads.store(true, std::sync::atomic::Ordering::SeqCst);
    let calls = rig.fake.calls().len();
    assert_eq!(
        rig.world
            .coordinator
            .mdm_put_abm_token(&alice(), json_request(&token_json()))
            .await,
        Err(MdmRefusal::StoreFailed)
    );
    assert_eq!(
        rig.fake.calls().len(),
        calls,
        "Apple is asked nothing when the settings cannot be read"
    );
    store
        .reads
        .store(false, std::sync::atomic::Ordering::SeqCst);

    rig.world.fake.refuse(|_| Some(audit_failure()));
    assert_eq!(
        rig.world
            .coordinator
            .mdm_put_abm_token(&alice(), json_request(&token_json()))
            .await,
        Err(MdmRefusal::AuditUnavailable)
    );
    assert_eq!(
        rig.world
            .coordinator
            .mdm_get_abm_public_key(&alice(), &proto::GetAbmPublicKeyRequest {})
            .await,
        Err(MdmRefusal::AuditUnavailable)
    );
}

/// Proves: only an operator calls: no token is `unauthorized` and CI's is `forbidden`, on both
/// calls, before anything is looked at or audited.
///
/// Catches: a setup call an unauthenticated caller reaches.
#[tokio::test]
async fn only_an_operator_makes_the_abm_calls() {
    let rig = start("abm-callers", true, true);
    for (caller, expected) in [
        (Caller::Anonymous, MdmRefusal::Unauthorized),
        (Caller::Ci, MdmRefusal::Forbidden),
    ] {
        assert_eq!(
            rig.world
                .coordinator
                .mdm_put_abm_token(&caller, json_request(&token_json()))
                .await
                .err(),
            Some(expected)
        );
        assert_eq!(
            rig.world
                .coordinator
                .mdm_get_abm_public_key(&caller, &proto::GetAbmPublicKeyRequest {})
                .await
                .err(),
            Some(expected)
        );
    }
    assert!(rig.world.audit().is_empty());
    assert!(rig.fake.seen().is_empty());
}

/// Proves: the certificate call makes the key pair when none is held (a PKCS#1 PEM key and a
/// self-signed certificate of it, both stored, the certificate answered with its fingerprint and
/// `created`), answers the same certificate with `created` false the next time and makes nothing
/// new, makes only the certificate when a key is held without one, never answers the private key,
/// and is audited as `mdm_abm_cert_get`.
///
/// Catches: a key pair replaced by a second call (every token ABM issued for it would stop opening),
/// and a private key in an answer.
#[tokio::test]
async fn the_public_key_is_made_once_and_the_private_key_never_leaves() {
    let rig = start("abm-pubkey", true, true);
    let coordinator = &rig.world.coordinator;
    let first = coordinator
        .mdm_get_abm_public_key(&alice(), &proto::GetAbmPublicKeyRequest {})
        .await
        .expect("made");
    assert!(first.created);
    assert!(
        first
            .certificate_pem
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
    assert_eq!(first.fingerprint_sha256.len(), 64);
    let key = coordinator
        .mdm_setup()
        .open(SecretKind::AbmKey)
        .expect("opens")
        .expect("held");
    let pem = String::from_utf8(key.expose().to_vec()).expect("text");
    assert!(pem.starts_with("-----BEGIN RSA PRIVATE KEY-----"));
    let parsed = parse_key(pem.as_bytes()).expect("a key");
    crate::abm::keys::check_certificate(first.certificate_pem.as_bytes(), Some(&parsed))
        .expect("its certificate");
    assert!(!format!("{first:?}").contains("PRIVATE KEY"));

    let second = coordinator
        .mdm_get_abm_public_key(&alice(), &proto::GetAbmPublicKeyRequest {})
        .await
        .expect("answered");
    assert_eq!(
        (
            second.created,
            second.certificate_pem.as_str(),
            second.fingerprint_sha256.as_str()
        ),
        (
            false,
            first.certificate_pem.as_str(),
            first.fingerprint_sha256.as_str()
        )
    );
    let listed = coordinator.mdm_list_secrets(&alice()).expect("listed");
    let kinds: Vec<(&str, u32)> = listed
        .secrets
        .iter()
        .map(|s| (s.kind.as_str(), s.version))
        .collect();
    assert_eq!(kinds, [("abm_cert", 1), ("abm_key", 1)]);
    assert_eq!(
        audit(&rig.world),
        vec![
            (
                "alice".to_owned(),
                "mdm_abm_cert_get".to_owned(),
                Some("abm_cert".to_owned()),
                Some("requested".to_owned())
            );
            2
        ]
    );
    assert!(rig.fake.seen().is_empty(), "nothing is asked of Apple");

    // A key held with no certificate: only the certificate is made.
    let lone = start_over("abm-pubkey-lone", true, true, None);
    let key = generate(2048).expect("a key");
    lone.world
        .coordinator
        .mdm_setup()
        .put_secret(
            SecretKind::AbmKey,
            &Secret::new(pkcs1_pem(&key).expect("a PEM").into_bytes()),
            None,
            1,
        )
        .expect("stored");
    let made = lone
        .world
        .coordinator
        .mdm_get_abm_public_key(&alice(), &proto::GetAbmPublicKeyRequest {})
        .await
        .expect("made");
    assert!(made.created);
    crate::abm::keys::check_certificate(made.certificate_pem.as_bytes(), Some(&key))
        .expect("its certificate");
    let kinds: Vec<(String, u32)> = lone
        .world
        .coordinator
        .mdm_list_secrets(&alice())
        .expect("listed")
        .secrets
        .into_iter()
        .map(|s| (s.kind, s.version))
        .collect();
    assert_eq!(
        kinds,
        [("abm_cert".to_owned(), 1), ("abm_key".to_owned(), 1)]
    );
}

/// Proves: two certificate calls at once make one pair: both answer the same certificate, one says
/// it was made, and the key held is the one that certificate is for; a certificate held with no
/// key is refused `abm_key_missing` (audited) and stays as it was, not replaced by one for a new
/// key.
///
/// Catches: a double click that shows a certificate for a key that is then overwritten, so that
/// ABM encrypts the token to a key the coordinator no longer has.
#[tokio::test]
async fn two_calls_at_once_make_one_pair_and_a_held_certificate_is_never_replaced() {
    let rig = start("abm-pubkey-race", true, true);
    let coordinator = &rig.world.coordinator;
    let request = proto::GetAbmPublicKeyRequest {};
    let caller = alice();
    let (a, b) = tokio::join!(
        coordinator.mdm_get_abm_public_key(&caller, &request),
        coordinator.mdm_get_abm_public_key(&caller, &request)
    );
    let (a, b) = (a.expect("a"), b.expect("b"));
    assert_eq!(a.certificate_pem, b.certificate_pem);
    assert_eq!(a.fingerprint_sha256, b.fingerprint_sha256);
    assert_eq!(u8::from(a.created) + u8::from(b.created), 1);
    let key = coordinator
        .mdm_setup()
        .open(SecretKind::AbmKey)
        .expect("opens")
        .expect("held");
    let parsed = parse_key(key.expose()).expect("a key");
    crate::abm::keys::check_certificate(a.certificate_pem.as_bytes(), Some(&parsed))
        .expect("the certificate shown is for the key held");

    let lone = start_over("abm-pubkey-cert-only", true, true, None);
    let key = generate(2048).expect("a key");
    let cert = crate::abm::keys::self_signed(&key, 1).expect("a certificate");
    lone.world
        .coordinator
        .mdm_setup()
        .put_secret(
            SecretKind::AbmCert,
            &Secret::new(cert.into_bytes()),
            None,
            1,
        )
        .expect("stored");
    assert_eq!(
        lone.world
            .coordinator
            .mdm_get_abm_public_key(&alice(), &request)
            .await,
        Err(MdmRefusal::AbmKeyMissing)
    );
    let kinds: Vec<(String, u32)> = lone
        .world
        .coordinator
        .mdm_list_secrets(&alice())
        .expect("listed")
        .secrets
        .into_iter()
        .map(|s| (s.kind, s.version))
        .collect();
    assert_eq!(kinds, [("abm_cert".to_owned(), 1)]);
    assert_eq!(
        audit(&lone.world).last(),
        Some(&refusal_audit(
            "mdm_abm_cert_get",
            "abm_cert",
            "MDM_REFUSAL_ABM_KEY_MISSING"
        ))
    );
}

/// Proves: the certificate call refuses when the coordinator has no ABM client (`abm_unavailable`)
/// or no master key (`secrets_unavailable`), each audited; a store that cannot be read or written is
/// `store_failed`, with the key pair not half-made when the write of the key fails.
///
/// Catches: a key generated and lost because the store refused it.
#[tokio::test]
async fn the_public_key_call_refuses_what_it_cannot_keep() {
    let none = start("abm-pubkey-none", true, false);
    assert_eq!(
        none.world
            .coordinator
            .mdm_get_abm_public_key(&alice(), &proto::GetAbmPublicKeyRequest {})
            .await,
        Err(MdmRefusal::AbmUnavailable)
    );
    assert_eq!(
        audit(&none.world),
        [refusal_audit(
            "mdm_abm_cert_get",
            "abm_cert",
            "MDM_REFUSAL_ABM_UNAVAILABLE"
        )]
    );
    let keyless = start_over("abm-pubkey-keyless", false, true, None);
    assert_eq!(
        keyless
            .world
            .coordinator
            .mdm_get_abm_public_key(&alice(), &proto::GetAbmPublicKeyRequest {})
            .await,
        Err(MdmRefusal::SecretsUnavailable)
    );

    let store = flaky();
    let rig = start_over("abm-pubkey-store", true, true, Some(store.clone()));
    store
        .writes
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        rig.world
            .coordinator
            .mdm_get_abm_public_key(&alice(), &proto::GetAbmPublicKeyRequest {})
            .await,
        Err(MdmRefusal::StoreFailed)
    );
    store
        .writes
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(
        rig.world
            .coordinator
            .mdm_list_secrets(&alice())
            .expect("listed")
            .secrets
            .is_empty()
    );
    store.reads.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        rig.world
            .coordinator
            .mdm_get_abm_public_key(&alice(), &proto::GetAbmPublicKeyRequest {})
            .await,
        Err(MdmRefusal::StoreFailed)
    );
}

/// Proves: a `.p7m` is opened with the `abm_key` the coordinator holds: made through the
/// certificate call, an envelope encrypted to it by a stand-in for ABM opens, the token inside is
/// proved to Apple and kept as what the envelope held (the JSON, without the framing); with no key
/// held it is `abm_key_missing`, a message encrypted to another key or a file that is not one is
/// `abm_token_unreadable`, and one that opens to something that is not a token is too; a key held
/// that does not open is `store_failed`.
///
/// Catches: the fallback path that no operator can complete because the upload cannot find the key
/// it made.
#[tokio::test]
async fn a_p7m_is_opened_with_the_key_the_coordinator_made() {
    let rig = start("abm-p7m", true, true);
    let coordinator = &rig.world.coordinator;
    let framed = format!(
        "Content-Type: text/plain\r\n\r\n-----BEGIN MDM SERVER TOKEN-----\r\n{}\r\n-----END MDM SERVER TOKEN-----\r\n",
        String::from_utf8(token_json()).expect("text")
    );
    let der = |to: &rsa::PrivateKey, plain: &str| {
        built(plain.as_bytes(), to, ContentEncryptionAlgorithm::Aes256Cbc)
    };
    let stranger = generate(2048).expect("a key");
    assert_eq!(
        coordinator
            .mdm_put_abm_token(&alice(), p7m_request(&der(&stranger, &framed)))
            .await,
        Err(MdmRefusal::AbmKeyMissing)
    );
    coordinator
        .mdm_get_abm_public_key(&alice(), &proto::GetAbmPublicKeyRequest {})
        .await
        .expect("made");
    let pem = coordinator
        .mdm_setup()
        .open(SecretKind::AbmKey)
        .expect("opens")
        .expect("held");
    let ours = parse_key(pem.expose()).expect("a key");

    for refused in [
        der(&stranger, &framed),
        b"not a message".to_vec(),
        der(&ours, "no token inside"),
        der(&ours, "{\"consumer_key\":\"a\"}"),
    ] {
        assert_eq!(
            coordinator
                .mdm_put_abm_token(&alice(), p7m_request(&refused))
                .await,
            Err(MdmRefusal::AbmTokenUnreadable)
        );
    }
    assert!(rig.fake.seen().is_empty());

    let answer = coordinator
        .mdm_put_abm_token(&alice(), p7m_request(&der(&ours, &framed)))
        .await
        .expect("kept");
    assert_eq!(
        answer.secret.expect("shown").not_after_unix,
        Some(EXPIRY_UNIX)
    );
    let held = coordinator
        .mdm_setup()
        .open(SecretKind::AbmToken)
        .expect("opens")
        .expect("held");
    assert_eq!(held, Secret::new(token_json()));
    assert_eq!(rig.fake.calls(), ["GET /session", "GET /account"]);

    // A key held that is not a key, and one the master key does not open.
    let broken = start("abm-p7m-broken", true, true);
    broken
        .world
        .coordinator
        .mdm_setup()
        .put_secret(
            SecretKind::AbmKey,
            &Secret::new(b"not a key".to_vec()),
            None,
            1,
        )
        .expect("stored");
    assert_eq!(
        broken
            .world
            .coordinator
            .mdm_put_abm_token(&alice(), p7m_request(&der(&ours, &framed)))
            .await,
        Err(MdmRefusal::StoreFailed)
    );
    let store = flaky();
    let unreadable = start_over("abm-p7m-store", true, true, Some(store.clone()));
    store.reads.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        unreadable
            .world
            .coordinator
            .mdm_put_abm_token(&alice(), p7m_request(&der(&ours, &framed)))
            .await,
        Err(MdmRefusal::StoreFailed)
    );
}

/// Proves: `PutSecret` judges the ABM pair at upload: an `abm_key` that is an RSA private key in
/// PEM is kept (PKCS#1, as NanoDEP writes it), one that is not is `invalid_request`; an `abm_cert`
/// that is a certificate is kept when no key is held or when it is the key's, and refused when it
/// is another key's or no certificate.
///
/// Catches: a takeover that uploads the wrong files and finds out when the first token does not
/// open.
#[test]
fn the_key_pair_of_a_takeover_is_judged_at_upload() {
    let world = start("abm-pair-upload", true, true).world;
    let coordinator = &world.coordinator;
    let put = |kind: &str, material: &[u8]| {
        coordinator.mdm_put_secret(
            &alice(),
            proto::PutSecretRequest {
                kind: kind.to_owned(),
                material: material.to_vec(),
                not_after_unix: None,
            },
        )
    };
    let key = generate(2048).expect("a key");
    let other = generate(2048).expect("another key");
    let cert = self_signed(&key, NOW).expect("a certificate");
    let other_cert = self_signed(&other, NOW).expect("a certificate");
    assert!(
        put("abm_cert", cert.as_bytes()).is_ok(),
        "no key is held to check against"
    );
    assert_eq!(
        put("abm_key", b"nonsense").err(),
        Some(MdmRefusal::InvalidRequest)
    );
    assert_eq!(
        put("abm_cert", b"nonsense").err(),
        Some(MdmRefusal::InvalidRequest)
    );
    assert!(put("abm_key", pkcs1_pem(&key).expect("a PEM").as_bytes()).is_ok());
    assert!(put("abm_cert", cert.as_bytes()).is_ok());
    assert_eq!(
        put("abm_cert", other_cert.as_bytes()).err(),
        Some(MdmRefusal::InvalidRequest)
    );
    assert_eq!(
        put("abm_key", &[0xff, 0xfe]).err(),
        Some(MdmRefusal::InvalidRequest)
    );
}

/// Proves: the coordinator's timer does nothing and never ends without an ABM client, and with one
/// runs a pass (here with no token: nothing is asked of Apple) and then waits.
///
/// Catches: a timer that ends and takes the serve with it, or one that asks Apple with no token.
#[tokio::test(start_paused = true)]
async fn the_coordinators_timer_idles_without_a_client_and_runs_with_one() {
    for abm in [false, true] {
        let rig = start("abm-timer-coordinator", true, abm);
        let timer = crate::operator_abm::abm_sync_every(Arc::clone(&rig.world.coordinator));
        tokio::select! {
            never = timer => match never {},
            () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
        }
        assert!(rig.fake.seen().is_empty());
    }
}
