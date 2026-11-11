use std::sync::Arc;
use std::time::Duration;

use hostkeeper_mdm_push::identity::IdentityError;
use hostkeeper_mdm_push::mdmcert::TransportError;
use hostkeeper_mdm_push::request::pkcs1_pem;
use hostkeeper_mdm_push::testkit::{fixture_key, push_certificate_pem};

use super::*;
use crate::mdm_secrets::{MasterKey, Secret, SecretKind};

const TOPIC: &str = "com.apple.mgmt.External.d1801ae1-3ef7-483e-9f6d-4979e2ebfc09";

fn keyed() -> MdmSetup {
    MdmSetup::volatile().with_key(MasterKey::from_bytes(&[0x21; 32]).expect("a key"))
}

fn store(setup: &MdmSetup, kind: SecretKind, bytes: &[u8]) {
    setup
        .put_secret(kind, &Secret::new(bytes.to_vec()), None, 1_800_000_000)
        .expect("stored");
}

/// Proves: the pending record is the key's PEM and then the CSR's, they split apart again exactly,
/// a key that ends with no newline still splits, and a record with no CSR or that is not text
/// splits to nothing.
///
/// Catches: a pending key stored with its CSR glued on, which a push identity would then try to read.
#[test]
fn the_pending_record_is_the_key_then_the_csr_and_splits_back() {
    let key = b"-----BEGIN RSA PRIVATE KEY-----\nAAAA\n-----END RSA PRIVATE KEY-----\n";
    let csr = "-----BEGIN CERTIFICATE REQUEST-----\nBBBB\n-----END CERTIFICATE REQUEST-----\n";
    let bundle = pending_bundle(key, csr);
    assert_eq!(split_pending(&bundle), Some((key.to_vec(), csr.to_owned())));
    let bare = pending_bundle(b"key without a newline", csr);
    assert_eq!(
        split_pending(&bare),
        Some((b"key without a newline\n".to_vec(), csr.to_owned()))
    );
    assert_eq!(split_pending(key), None);
    assert_eq!(split_pending(&[0xff, 0xfe]), None);
}

/// Proves: with no push certificate or no key the identity is none; with a matching pair (the key
/// as PKCS#1, the shape migrated from NanoHUB) it is the pair, with its topic; a key that is not the
/// certificate's is refused as a pair that does not match, and a setup with no master key says so.
///
/// Catches: a sender built from a certificate and a key that do not belong together, which APNs
/// would answer with a TLS failure at the first push.
#[test]
fn the_push_identity_is_the_pair_in_use_and_none_until_both_are_held() {
    let setup = keyed();
    assert!(setup.push_identity().expect("read").is_none());
    let key = fixture_key(0);
    let cert = push_certificate_pem(key, TOPIC, 1_700_000_000, 1_900_000_000);
    store(&setup, SecretKind::PushCert, cert.as_bytes());
    assert!(setup.push_identity().expect("read").is_none(), "no key yet");
    store(
        &setup,
        SecretKind::PushKey,
        pkcs1_pem(key).expect("pem").expose(),
    );
    let identity = setup.push_identity().expect("read").expect("a pair");
    assert_eq!(identity.info.topic, TOPIC);
    // No held key is the certificate's: a mismatch.
    let other = keyed();
    store(&other, SecretKind::PushCert, cert.as_bytes());
    store(
        &other,
        SecretKind::PushKey,
        pkcs1_pem(fixture_key(1)).expect("pem").expose(),
    );
    assert_eq!(
        other.push_identity().expect_err("a mismatch").to_string(),
        IdentityError::KeyMismatch.to_string()
    );
    assert!(matches!(
        other.push_identity(),
        Err(PushIdentityError::Identity(IdentityError::KeyMismatch))
    ));
    let unkeyed = MdmSetup::volatile();
    assert!(matches!(
        unkeyed.push_identity(),
        Err(PushIdentityError::Setup(SetupFailure::NoKey))
    ));
}

/// Proves: an upload that stored its new key and then failed to store its certificate leaves the
/// certificate in use paired with the key it replaced, which is still held, so pushes keep
/// working; once the new certificate is stored the new key is the one used.
///
/// Catches: a renewal whose second write failed that takes the push path down until the upload is
/// repeated.
#[test]
fn a_new_key_stored_beside_the_old_certificate_leaves_the_old_identity_in_use() {
    let setup = keyed();
    let (old, new) = (fixture_key(0), fixture_key(1));
    let old_cert = push_certificate_pem(old, TOPIC, 1_700_000_000, 1_900_000_000);
    store(&setup, SecretKind::PushCert, old_cert.as_bytes());
    store(
        &setup,
        SecretKind::PushKey,
        pkcs1_pem(old).expect("pem").expose(),
    );
    // The renewal's first write; the certificate's never came.
    store(
        &setup,
        SecretKind::PushKey,
        pkcs1_pem(new).expect("pem").expose(),
    );
    let identity = setup.push_identity().expect("read").expect("the old pair");
    assert_eq!(identity.info.not_after_unix, 1_900_000_000);
    let new_cert = push_certificate_pem(new, TOPIC, 1_700_000_000, 1_950_000_000);
    store(&setup, SecretKind::PushCert, new_cert.as_bytes());
    let identity = setup.push_identity().expect("read").expect("the new pair");
    assert_eq!(identity.info.not_after_unix, 1_950_000_000);
}

/// Proves: the push settings are their defaults until set, then the values set, and the timeout
/// comes with them.
///
/// Catches: a sender that pushes with a priority or an expiry nobody set.
#[test]
fn the_push_settings_are_the_defaults_until_set() {
    let setup = keyed();
    let (settings, timeout) = setup.push_settings().expect("read");
    assert_eq!(
        (
            settings.retry_s,
            settings.expiry_s,
            settings.priority,
            timeout
        ),
        (3600, 86_400, 10, Duration::from_secs(30))
    );
    for (name, value) in [
        ("push_retry_s", "60"),
        ("push_expiry_s", "0"),
        ("push_priority", "5"),
        ("push_timeout_s", "7"),
    ] {
        setup.set_setting(name, value).expect("set");
    }
    let (settings, timeout) = setup.push_settings().expect("read");
    assert_eq!(
        (
            settings.retry_s,
            settings.expiry_s,
            settings.priority,
            timeout
        ),
        (60, 0, 5, Duration::from_secs(7))
    );
}

/// Proves: a coordinator built with no transport reaches no network: the request fails with a
/// message, and a transport that is given replaces it.
///
/// Catches: a test or a coordinator that posts to the real mdmcert.download by accident.
#[tokio::test]
async fn a_coordinator_with_no_transport_posts_nowhere() {
    let none = NoMdmcert;
    let failed = none
        .post_sign_request(Vec::new(), Duration::from_secs(1))
        .await
        .expect_err("no transport");
    assert_eq!(
        failed,
        TransportError("no transport to mdmcert.download".to_owned())
    );
    let fake = Arc::new(hostkeeper_mdm_push::testkit::FakeMdmcert::new());
    let world = crate::login_support::World::build(
        "mdm-push-transport",
        crate::login_support::settings(),
        crate::login_support::counting(),
        |c, _| c.with_mdmcert(fake.clone()),
    );
    world
        .coordinator
        .mdmcert
        .post_sign_request(b"{}".to_vec(), Duration::from_secs(1))
        .await
        .expect("answered");
    assert_eq!(fake.posted(), [b"{}".to_vec()]);
}
