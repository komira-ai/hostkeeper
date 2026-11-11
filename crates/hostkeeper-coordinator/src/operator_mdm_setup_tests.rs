//! The calls of the MDM setup as plain functions: who may call, what each refuses and audits, and
//! that no answer, audit entry or log line holds a secret.

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MdmRefusal;
use sha2::Digest as _;
use tracing_subscriber::fmt::MakeWriter;

use crate::login_support::World;
use crate::mdm_secrets::{MasterKey, Secret, SecretKind};
use crate::mdm_setup::MdmSetup;
use crate::operator::Caller;
use crate::store::audit_failure;
use crate::store::mdm_setup::files::FileSetup;
use crate::store::mdm_setup::{SecretRow, SetupError, SetupStore};
use crate::testutil::Log;

const MATERIAL: &[u8] = b"-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAplaintextmarker\n-----END RSA PRIVATE KEY-----\n";

fn alice() -> Caller {
    Caller::Operator("alice".parse().expect("a token name"))
}

fn keyed(setup: MdmSetup) -> MdmSetup {
    setup.with_key(MasterKey::from_bytes(&[0x21; 32]).expect("a key"))
}

/// A world whose coordinator holds a master key over a store of memory alone.
fn world(name: &str) -> World {
    World::build(
        name,
        crate::login_support::settings(),
        crate::login_support::counting(),
        |c, _| c.with_mdm_setup(keyed(MdmSetup::volatile())),
    )
}

fn put(kind: &str, material: &[u8], not_after: Option<i64>) -> proto::PutSecretRequest {
    proto::PutSecretRequest {
        kind: kind.to_owned(),
        material: material.to_vec(),
        not_after_unix: not_after,
        with_certificate: Vec::new(),
    }
}

fn setting(name: &str, value: &str) -> proto::PutSettingRequest {
    proto::PutSettingRequest {
        name: name.to_owned(),
        value: value.to_owned(),
    }
}

/// The audit's `(by, action, subject, reason)`, in order.
fn audit(world: &World) -> Vec<(String, String, Option<String>, Option<String>)> {
    world
        .audit()
        .into_iter()
        .map(|entry| (entry.by, entry.action, entry.subject, entry.reason))
        .collect()
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

/// Proves: an upload is audited under the user's name with the kind, the fingerprint and the
/// expiry, answers the version it made by those alone, and no answer, audit entry, log line or
/// listing holds the value or its base64 or hex, by exact equality of the whole texts; the value
/// is held, sealed, and opens.
///
/// Catches: a secret echoed in an answer, an audit row or a log line (T13), or an upload that is
/// not audited.
#[test]
fn an_upload_is_audited_and_nothing_is_returned_after_it() {
    let world = world("mdm-setup-put");
    let log = Everything::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(log.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish(),
    );
    let answer = world
        .coordinator
        .mdm_put_secret(&alice(), put("push_key", MATERIAL, Some(2_000_000_000)))
        .expect("stored");
    let fingerprint = hex::encode(sha2::Sha256::digest(MATERIAL));
    let info = answer.secret.clone().expect("what is shown");
    assert_eq!(
        (info.kind.as_str(), info.version, info.not_after_unix),
        ("push_key", 1, Some(2_000_000_000))
    );
    assert_eq!(info.fingerprint_sha256, fingerprint);

    let listed = world
        .coordinator
        .mdm_list_secrets(&alice())
        .expect("listed");
    assert_eq!(listed.secrets, vec![info.clone()]);
    assert!(listed.sealing);

    assert_eq!(
        audit(&world),
        [(
            "alice".to_owned(),
            "mdm_push_cert_put".to_owned(),
            Some("push_key".to_owned()),
            Some(format!("fingerprint {fingerprint}, expires 2000000000"))
        )]
    );

    let base64 = base64::engine::general_purpose::STANDARD.encode(MATERIAL);
    let everything = format!("{answer:?}|{listed:?}|{:?}|{}", world.audit(), log.0.text());
    for needle in [
        base64.as_str(),
        "plaintextmarker",
        "BEGIN RSA PRIVATE KEY",
        hex::encode(MATERIAL).as_str(),
    ] {
        assert!(!everything.contains(needle), "{needle} is in {everything}");
    }
    let opened = world
        .coordinator
        .mdm_setup()
        .open(SecretKind::PushKey)
        .expect("opens")
        .expect("held");
    assert_eq!(opened, Secret::new(MATERIAL.to_vec()));
}

/// Proves: each kind is audited under its own action: the push certificate and key as
/// `mdm_push_cert_put`, the ABM token as `mdm_abm_token_put`, the TLS key as `mdm_tls_put`, the
/// DNS credential as `mdm_acme_credential_put`, and the rest as `mdm_secret_put`; an ABM token is
/// taken as its JSON of five parts.
///
/// Catches: an upload audited under the wrong action, which the audit's reader would take for
/// another change.
#[test]
fn each_kind_is_audited_under_its_own_action() {
    let world = world("mdm-setup-actions");
    let token = br#"{"consumer_key":"a","consumer_secret":"b","access_token":"c","access_secret":"d","access_secret_expiry":"2027-10-31T22:01:36Z"}"#;
    let abm_key = crate::abm::keys::pkcs1_pem(&crate::abm::keys::generate(1024).expect("a key"))
        .expect("a PEM")
        .into_bytes();
    for (kind, material) in [
        ("push_cert", &b"cert"[..]),
        ("abm_token", token),
        ("tls_key", b"key"),
        ("dns_credential", b"id:secret"),
        ("abm_key", abm_key.as_slice()),
    ] {
        world
            .coordinator
            .mdm_put_secret(&alice(), put(kind, material, None))
            .expect(kind);
    }
    let actions: Vec<(String, Option<String>, Option<String>)> = audit(&world)
        .into_iter()
        .map(|(_, action, subject, reason)| (action, subject, reason))
        .collect();
    let actions: Vec<(String, Option<String>)> = actions
        .into_iter()
        .map(|(action, subject, reason)| {
            assert!(reason.expect("a reason").ends_with("expires none"));
            (action, subject)
        })
        .collect();
    assert_eq!(
        actions,
        [
            ("mdm_push_cert_put".to_owned(), Some("push_cert".to_owned())),
            ("mdm_abm_token_put".to_owned(), Some("abm_token".to_owned())),
            ("mdm_tls_put".to_owned(), Some("tls_key".to_owned())),
            (
                "mdm_acme_credential_put".to_owned(),
                Some("dns_credential".to_owned())
            ),
            ("mdm_secret_put".to_owned(), Some("abm_key".to_owned())),
        ]
    );
}

/// Proves: a request the call refuses is refused with its token and audited with the token as the
/// reason, and stores nothing: an unknown kind, an empty value, one over 32 KiB, an ABM token that
/// is not its five parts, and an upload to a coordinator that holds no master key.
///
/// Catches: a malformed upload stored as good, or a refusal that leaves no trace in the audit.
#[test]
fn a_refused_upload_is_audited_with_its_reason_and_stores_nothing() {
    let world = world("mdm-setup-refused");
    let too_big = vec![b'x'; 32 * 1024 + 1];
    let cases: [(&str, &[u8], Option<&str>); 4] = [
        ("push_keys", b"x", None),
        ("push_key", b"", Some("push_key")),
        ("push_key", &too_big, Some("push_key")),
        ("abm_token", br#"{"consumer_key":"a"}"#, Some("abm_token")),
    ];
    for (kind, material, _) in cases {
        let refused = world
            .coordinator
            .mdm_put_secret(&alice(), put(kind, material, None))
            .expect_err("refused");
        assert_eq!(refused, MdmRefusal::InvalidRequest, "{kind}");
    }
    let largest = vec![b'y'; 32 * 1024];
    world
        .coordinator
        .mdm_put_secret(&alice(), put("tls_cert", &largest, None))
        .expect("32 KiB is taken");

    let keyless = World::new("mdm-setup-keyless");
    let refused = keyless
        .coordinator
        .mdm_put_secret(&alice(), put("push_key", b"x", None))
        .expect_err("no key");
    assert_eq!(refused, MdmRefusal::SecretsUnavailable);
    assert_eq!(
        audit(&keyless),
        [(
            "alice".to_owned(),
            "mdm_push_cert_put".to_owned(),
            Some("push_key".to_owned()),
            Some("refused: MDM_REFUSAL_SECRETS_UNAVAILABLE".to_owned())
        )]
    );
    assert!(
        keyless
            .coordinator
            .mdm_list_secrets(&alice())
            .expect("listed")
            .secrets
            .is_empty()
    );
    assert!(
        !keyless
            .coordinator
            .mdm_list_secrets(&alice())
            .expect("listed")
            .sealing
    );

    let said: Vec<(String, Option<String>, Option<String>)> = audit(&world)
        .into_iter()
        .take(4)
        .map(|(_, action, subject, reason)| (action, subject, reason))
        .collect();
    assert_eq!(
        said,
        [
            (
                "mdm_secret_put".to_owned(),
                None,
                Some("refused: MDM_REFUSAL_INVALID_REQUEST".to_owned())
            ),
            (
                "mdm_push_cert_put".to_owned(),
                Some("push_key".to_owned()),
                Some("refused: MDM_REFUSAL_INVALID_REQUEST".to_owned())
            ),
            (
                "mdm_push_cert_put".to_owned(),
                Some("push_key".to_owned()),
                Some("refused: MDM_REFUSAL_INVALID_REQUEST".to_owned())
            ),
            (
                "mdm_abm_token_put".to_owned(),
                Some("abm_token".to_owned()),
                Some("refused: MDM_REFUSAL_INVALID_REQUEST".to_owned())
            ),
        ]
    );
    let listed = world
        .coordinator
        .mdm_list_secrets(&alice())
        .expect("listed");
    assert_eq!(
        listed
            .secrets
            .iter()
            .map(|s| s.kind.as_str())
            .collect::<Vec<_>>(),
        ["tls_cert"]
    );
}

/// Proves: no caller but an operator's call is served: no token is `unauthorized` and CI's is
/// `forbidden` on every call of the setup, before anything is looked at or audited.
///
/// Catches: a setup call that an unauthenticated caller reaches.
#[test]
fn only_an_operator_calls_the_setup() {
    let world = world("mdm-setup-callers");
    for (caller, expected) in [
        (Caller::Anonymous, MdmRefusal::Unauthorized),
        (Caller::Ci, MdmRefusal::Forbidden),
    ] {
        assert_eq!(
            world
                .coordinator
                .mdm_put_secret(&caller, put("tls_key", b"x", None))
                .err(),
            Some(expected)
        );
        assert_eq!(
            world
                .coordinator
                .mdm_put_setting(&caller, &setting("tls_renew_days", "9"))
                .err(),
            Some(expected)
        );
        assert_eq!(
            world.coordinator.mdm_list_secrets(&caller).err(),
            Some(expected)
        );
        assert_eq!(
            world.coordinator.mdm_list_settings(&caller).err(),
            Some(expected)
        );
    }
    assert!(world.audit().is_empty());
}

/// Proves: a setting is set, listed with its value and whether that is the default, and audited as
/// `mdm_setting_set` with the name and the value; a name this release does not know and a value the
/// setting does not take are refused and audited as refused, and change nothing.
///
/// Catches: a setting stored without being judged, or changed with no entry in the audit.
#[test]
fn a_setting_is_judged_audited_and_listed() {
    let world = world("mdm-setup-settings");
    let set = world
        .coordinator
        .mdm_put_setting(&alice(), &setting("push_alert_days", " 90 , 30 "))
        .expect("set");
    assert_eq!(
        set,
        proto::PutSettingResponse {
            name: "push_alert_days".to_owned(),
            value: "90,30".to_owned()
        }
    );
    for (name, value) in [
        ("push_alert_days", "30,90"),
        ("mdm_name", "x"),
        ("tls_renew_days", "0"),
    ] {
        assert_eq!(
            world
                .coordinator
                .mdm_put_setting(&alice(), &setting(name, value)),
            Err(MdmRefusal::InvalidRequest),
            "{name} {value}"
        );
    }
    let listed = world
        .coordinator
        .mdm_list_settings(&alice())
        .expect("listed");
    let shown: Vec<(&str, &str, bool)> = listed
        .settings
        .iter()
        .map(|s| (s.name.as_str(), s.value.as_str(), s.is_default))
        .collect();
    assert_eq!(
        shown,
        [
            ("push_alert_days", "90,30", false),
            ("abm_alert_days", "60", true),
            ("tls_renew_days", "30", true),
            ("secret_grace_days", "7", true),
            ("push_retry_s", "3600", true),
            ("push_expiry_s", "86400", true),
            ("push_priority", "10", true),
            ("push_timeout_s", "30", true),
            ("proxy_wait_s", "10", true),
            ("proxy_redirects", "3", true),
            ("proxy_index_ttl_s", "300", true),
            ("proxy_cache_max_bytes", "21474836480", true),
            ("proxy_fetches", "8", true),
        ]
    );
    assert_eq!(
        audit(&world),
        [
            (
                "alice",
                "mdm_setting_set",
                Some("push_alert_days"),
                "set to 90,30"
            ),
            (
                "alice",
                "mdm_setting_set",
                Some("push_alert_days"),
                "refused: MDM_REFUSAL_INVALID_REQUEST"
            ),
            (
                "alice",
                "mdm_setting_set",
                None,
                "refused: MDM_REFUSAL_INVALID_REQUEST"
            ),
            (
                "alice",
                "mdm_setting_set",
                Some("tls_renew_days"),
                "refused: MDM_REFUSAL_INVALID_REQUEST"
            ),
        ]
        .map(|(by, action, subject, reason)| (
            by.to_owned(),
            action.to_owned(),
            subject.map(str::to_owned),
            Some(reason.to_owned())
        ))
    );
}

/// Proves: when the audit entry cannot be written, no secret and no setting is stored and the call
/// is refused `audit_unavailable`; when the audit is written and the store then refuses, the call is
/// `store_failed`, nothing is stored, and the audit holds the entry of the attempt and one of its
/// refusal.
///
/// Catches: a secret stored with no audit entry, and a failed store that leaves no word in the
/// audit.
#[test]
fn a_write_without_its_audit_is_not_made_and_a_failed_store_is_told() {
    let world = world("mdm-setup-audit-fails");
    world.fake.refuse(|_| Some(audit_failure()));
    assert_eq!(
        world
            .coordinator
            .mdm_put_secret(&alice(), put("tls_key", b"x", None)),
        Err(MdmRefusal::AuditUnavailable)
    );
    assert_eq!(
        world
            .coordinator
            .mdm_put_setting(&alice(), &setting("tls_renew_days", "9")),
        Err(MdmRefusal::AuditUnavailable)
    );
    assert_eq!(
        world
            .coordinator
            .mdm_put_secret(&alice(), put("nope", b"x", None)),
        Err(MdmRefusal::AuditUnavailable),
        "a refusal that cannot be audited is the audit's refusal"
    );
    world.fake.refuse(|_| None);
    assert!(
        world
            .coordinator
            .mdm_list_secrets(&alice())
            .expect("listed")
            .secrets
            .is_empty()
    );
    assert!(
        world
            .coordinator
            .mdm_list_settings(&alice())
            .expect("listed")
            .settings
            .iter()
            .all(|s| s.is_default)
    );
    assert!(world.audit().is_empty());

    /// A store that cannot write, and reads empty.
    struct Broken(FileSetup, Mutex<bool>);
    impl SetupStore for Broken {
        fn put_secret(&self, _: &SecretRow) -> Result<(), SetupError> {
            Err(SetupError::Store("the disk is full".to_owned()))
        }
        fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
            if *self.1.lock().expect("a lock") {
                Err(SetupError::Store("unreadable".to_owned()))
            } else {
                self.0.secrets()
            }
        }
        fn prune_superseded(&self, at: i64) -> Result<u64, SetupError> {
            self.0.prune_superseded(at)
        }
        fn settings(&self) -> Result<std::collections::BTreeMap<String, String>, SetupError> {
            if *self.1.lock().expect("a lock") {
                Err(SetupError::Store("unreadable".to_owned()))
            } else {
                self.0.settings()
            }
        }
        fn set_setting(&self, _: &str, _: &str) -> Result<(), SetupError> {
            Err(SetupError::Store("the disk is full".to_owned()))
        }
    }
    let broken = Arc::new(Broken(FileSetup::volatile(), Mutex::new(false)));
    let shared: Arc<dyn SetupStore> = broken.clone();
    let failing = World::build(
        "mdm-setup-store-fails",
        crate::login_support::settings(),
        crate::login_support::counting(),
        |c, _| {
            c.with_mdm_setup(MdmSetup::new(
                shared,
                Some(MasterKey::from_bytes(&[1; 32]).expect("a key")),
                crate::enrollment::os_random(),
            ))
        },
    );
    assert_eq!(
        failing
            .coordinator
            .mdm_put_secret(&alice(), put("tls_key", b"x", None)),
        Err(MdmRefusal::StoreFailed)
    );
    assert_eq!(
        failing
            .coordinator
            .mdm_put_setting(&alice(), &setting("tls_renew_days", "9")),
        Err(MdmRefusal::StoreFailed)
    );
    *broken.1.lock().expect("a lock") = true;
    assert_eq!(
        failing.coordinator.mdm_list_secrets(&alice()),
        Err(MdmRefusal::StoreFailed)
    );
    assert_eq!(
        failing.coordinator.mdm_list_settings(&alice()),
        Err(MdmRefusal::StoreFailed)
    );
    let reasons: Vec<String> = audit(&failing)
        .into_iter()
        .map(|(_, action, _, reason)| format!("{action}: {}", reason.expect("a reason")))
        .collect();
    assert_eq!(reasons.len(), 4);
    assert!(reasons[0].starts_with("mdm_tls_put: fingerprint "));
    assert_eq!(reasons[1], "mdm_tls_put: refused: MDM_REFUSAL_STORE_FAILED");
    assert_eq!(reasons[2], "mdm_setting_set: set to 9");
    assert_eq!(
        reasons[3],
        "mdm_setting_set: refused: MDM_REFUSAL_STORE_FAILED"
    );
}
