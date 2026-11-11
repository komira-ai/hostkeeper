//! The CA of the Macs' identities: made at first use, kept sealed, never made twice.

use super::ca::{CaError, held_certificate, identity_ca, pack, unpack};
use super::{Tunables, testkit};
use crate::audit_actions;
use crate::mdm_secrets::{Secret, SecretKind};
use crate::mdm_server::support::Rig;

fn tunables(rig: &Rig) -> Tunables {
    Tunables::read(&rig.setup).expect("the settings read")
}

/// Proves: the tunables are the defaults of the setup until a setting is set, the name is read only
/// when it is a host name, and a setting that was set is what is read.
///
/// Catches: an enrollment that reads a setting by a misspelled name and so never sees a change.
#[test]
fn the_tunables_are_the_settings_with_their_defaults() {
    let rig = Rig::new("enroll-tunables");
    let tune = tunables(&rig);
    assert_eq!(
        (
            tune.identity_days,
            tune.ca_days,
            tune.challenge_ttl_s,
            tune.rate_per_min,
            tune.pending_max
        ),
        (1095, 3650, 3600, 60, 256)
    );
    assert_eq!(
        (tune.org.as_str(), tune.profile_name.as_str()),
        ("hostkeeper", "Komira MDM")
    );
    assert_eq!(tune.dns_name, None);
    rig.setup
        .set_setting("mdm_name", "not a host name")
        .expect("set");
    assert_eq!(tunables(&rig).dns_name, None);
    rig.setup
        .set_setting("mdm_name", "mdm.example.com")
        .expect("set");
    rig.setup.set_setting("identity_days", "30").expect("set");
    let tune = tunables(&rig);
    assert_eq!(tune.dns_name.as_deref(), Some("mdm.example.com"));
    assert_eq!(tune.identity_days, 30);
}

/// Proves: the first need makes a CA with its own key, seals it as one secret that expires with the
/// certificate, audits it before it is stored, and the next need loads the same CA without another
/// audit row.
///
/// Catches: a CA made again at every request, which would orphan every identity issued.
#[tokio::test]
async fn the_ca_is_made_once_and_loaded_after() {
    let rig = Rig::new("enroll-ca-once");
    let tune = tunables(&rig);
    assert_eq!(held_certificate(&rig.setup), None);
    let first = identity_ca(&rig.coordinator, &rig.server, &tune)
        .await
        .expect("a CA is made");
    assert_eq!(rig.audited(), [audit_actions::MDM_CA_CREATED]);
    let info = rig
        .setup
        .list_secrets()
        .expect("secrets")
        .into_iter()
        .find(|info| info.kind == SecretKind::MdmCaKey)
        .expect("the CA is sealed");
    assert_eq!(
        info.not_after_unix,
        Some(i64::try_from(crate::mdm_server::support::NOW).unwrap() + 3650 * 86_400)
    );
    let second = identity_ca(&rig.coordinator, &rig.server, &tune)
        .await
        .expect("the CA is loaded");
    assert_eq!(first.cert_der(), second.cert_der());
    assert_eq!(rig.audited(), [audit_actions::MDM_CA_CREATED]);
    assert_eq!(
        held_certificate(&rig.setup),
        Some(first.cert_der().to_vec())
    );
}

/// Proves: two requests that need the CA at once make one.
///
/// Catches: two CAs, one of which signs identities that nothing trusts.
#[tokio::test]
async fn two_needs_at_once_make_one_ca() {
    let rig = Rig::new("enroll-ca-race");
    let tune = tunables(&rig);
    let (a, b) = tokio::join!(
        identity_ca(&rig.coordinator, &rig.server, &tune),
        identity_ca(&rig.coordinator, &rig.server, &tune)
    );
    assert_eq!(a.expect("a").cert_der(), b.expect("b").cert_der());
    assert_eq!(rig.audited(), [audit_actions::MDM_CA_CREATED]);
    let held: Vec<_> = rig
        .setup
        .list_secrets()
        .expect("secrets")
        .into_iter()
        .filter(|info| info.kind == SecretKind::MdmCaKey)
        .collect();
    assert_eq!(held.len(), 1);
}

/// Proves: a CA that cannot be kept is not made: with no master key the need is refused, and when
/// the audit row cannot be written nothing is stored.
///
/// Catches: a CA made and used that no restart could load again.
#[tokio::test]
async fn a_ca_that_cannot_be_kept_is_not_made() {
    let rig = Rig::new("enroll-ca-nokey");
    let tune = tunables(&rig);
    let keyless = crate::mdm_setup::MdmSetup::volatile();
    assert!(!keyless.has_key());
    let coordinator = std::sync::Arc::new(
        crate::host_groups_support::coordinator_listed(
            &rig.fake,
            &std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1_000)),
            None,
            "",
        )
        .with_shared_mdm_setup(std::sync::Arc::new(keyless)),
    );
    assert_eq!(
        identity_ca(&coordinator, &rig.server, &tune).await.err(),
        Some(CaError::NoKey)
    );
    assert_eq!(held_certificate(&coordinator.mdm_setup), None);
    rig.fake.refuse(|_| Some(crate::store::audit_failure()));
    assert_eq!(
        identity_ca(&rig.coordinator, &rig.server, &tune)
            .await
            .err(),
        Some(CaError::Audit)
    );
    assert_eq!(held_certificate(&rig.setup), None, "nothing was stored");
}

/// Proves: a sealed CA that is not a certificate and a key is an error that names the CA, not a
/// panic and not a new CA over it.
///
/// Catches: a damaged secret silently replaced by a new CA, which would orphan every identity.
#[tokio::test]
async fn a_sealed_ca_that_does_not_read_is_an_error_and_is_not_replaced() {
    let rig = Rig::new("enroll-ca-damaged");
    let tune = tunables(&rig);
    for bytes in [b"x".to_vec(), pack(b"cert", b"key")] {
        rig.put(SecretKind::MdmCaKey, &bytes);
        let error = identity_ca(&rig.coordinator, &rig.server, &tune)
            .await
            .expect_err("it does not read");
        assert!(matches!(error, CaError::Build(_)), "{error:?}");
    }
    assert_eq!(rig.audited(), Vec::<String>::new());
    assert_eq!(held_certificate(&rig.setup), Some(b"cert".to_vec()));
}

/// Proves: the packed form round-trips, and bytes too short or with a length past the end are
/// nothing.
///
/// Catches: a length read past the end of the secret.
#[test]
fn the_packed_ca_round_trips_and_a_bad_length_is_nothing() {
    assert_eq!(
        unpack(&pack(b"abc", b"keybytes")),
        Some((&b"abc"[..], &b"keybytes"[..]))
    );
    assert_eq!(unpack(&pack(b"", b"")), Some((&b""[..], &b""[..])));
    assert_eq!(unpack(&[0, 0, 0]), None);
    assert_eq!(unpack(&[0, 0, 0, 9, 1, 2]), None);
    let _ = (testkit::SERIAL, Secret::new(Vec::new()));
}
