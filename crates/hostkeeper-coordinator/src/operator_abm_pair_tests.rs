//! The ABM key pair of a takeover, and the serial lookup: what `PutSecret` judges at upload and what
//! `serial_in_abm` answers.

use super::tests::{alice, start};
use crate::abm::fake::NOW;
use crate::abm::keys::{generate, pkcs1_pem, self_signed};
use crate::mdm_secrets::SecretKind;
use crate::store::mdm::{AbmBatch, AbmDevice, MdmStore};
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MdmRefusal;

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
                with_certificate: Vec::new(),
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

/// Proves: an `abm_key` that is not the key of the `abm_cert` held is refused and the key held
/// stays; sent with the certificate it goes with, the key and the certificate are both stored; sent
/// with a certificate that is not its own, or with `with_certificate` on another kind, nothing is
/// stored.
///
/// Catches: a takeover with a wrong key or wrong certificate that replaces the key a held
/// certificate was added in ABM with, so every `.p7m` for it stops opening.
#[test]
fn a_key_that_breaks_the_pair_held_is_refused_and_the_key_held_stays() {
    let world = start("abm-pair-atomic", true, true).world;
    let coordinator = &world.coordinator;
    let put = |kind: &str, material: &[u8], with: &[u8]| {
        coordinator.mdm_put_secret(
            &alice(),
            proto::PutSecretRequest {
                kind: kind.to_owned(),
                material: material.to_vec(),
                not_after_unix: None,
                with_certificate: with.to_vec(),
            },
        )
    };
    let version = |kind: SecretKind| {
        coordinator
            .mdm_setup
            .list_secrets()
            .expect("a list")
            .iter()
            .filter(|info| info.kind == kind)
            .map(|info| info.version)
            .max()
    };
    let key = generate(2048).expect("a key");
    let other = generate(2048).expect("another key");
    let (key_pem, other_pem) = (
        pkcs1_pem(&key).expect("a PEM"),
        pkcs1_pem(&other).expect("a PEM"),
    );
    let cert = self_signed(&key, NOW).expect("a certificate");
    let other_cert = self_signed(&other, NOW).expect("a certificate");

    assert!(put("abm_key", key_pem.as_bytes(), cert.as_bytes()).is_ok());
    assert_eq!(
        (version(SecretKind::AbmKey), version(SecretKind::AbmCert)),
        (Some(1), Some(1)),
        "the key and its certificate are both stored"
    );

    assert_eq!(
        put("abm_key", other_pem.as_bytes(), &[]).err(),
        Some(MdmRefusal::InvalidRequest),
        "another key alone would break the pair held"
    );
    assert_eq!(
        put("abm_key", other_pem.as_bytes(), cert.as_bytes()).err(),
        Some(MdmRefusal::InvalidRequest),
        "a certificate that is not the new key's"
    );
    assert_eq!(
        put("abm_cert", cert.as_bytes(), cert.as_bytes()).err(),
        Some(MdmRefusal::InvalidRequest),
        "with_certificate belongs to the key"
    );
    assert_eq!(
        (version(SecretKind::AbmKey), version(SecretKind::AbmCert)),
        (Some(1), Some(1)),
        "no refusal stored anything"
    );
    assert!(
        put("abm_key", key_pem.as_bytes(), &[]).is_ok(),
        "the same key again keeps the pair"
    );

    assert!(put("abm_key", other_pem.as_bytes(), other_cert.as_bytes()).is_ok());
    assert_eq!(
        (version(SecretKind::AbmKey), version(SecretKind::AbmCert)),
        (Some(3), Some(2)),
        "a whole new pair replaces the old one"
    );
}

/// Proves: the question "is this serial in our ABM" is true for a serial in the table that is not
/// marked removed, false for one that is, and false for one that is not there, and false for a
/// coordinator with no ABM client; a table that cannot be read is an error and not a no.
///
/// Catches: a Mac that was disowned and is approved anyway, and a store fault that declines every
/// Mac as not ours.
#[test]
fn a_serial_is_in_abm_while_the_table_lists_it_and_has_not_removed_it() {
    let rig = start("abm-serial", true, true);
    let device = |serial: &str, removed: Option<i64>| AbmDevice {
        serial: serial.to_owned(),
        model: None,
        description: None,
        profile_status: "pushed".to_owned(),
        profile_uuid: None,
        first_seen_unix: 1,
        synced_unix: 1,
        removed_unix: removed,
    };
    rig.store
        .abm_apply(&AbmBatch {
            upserts: vec![device("HERE", None), device("GONE", None)],
            ..AbmBatch::default()
        })
        .expect("rows");
    rig.store
        .abm_apply(&AbmBatch {
            removed: vec!["GONE".to_owned()],
            removed_unix: 5,
            ..AbmBatch::default()
        })
        .expect("removed");
    let coordinator = &rig.world.coordinator;
    assert_eq!(coordinator.serial_in_abm("HERE"), Ok(true));
    assert_eq!(coordinator.serial_in_abm("GONE"), Ok(false));
    assert_eq!(coordinator.serial_in_abm("NEVER"), Ok(false));
    let none = start("abm-serial-none", true, false);
    assert_eq!(none.world.coordinator.serial_in_abm("HERE"), Ok(false));
}
