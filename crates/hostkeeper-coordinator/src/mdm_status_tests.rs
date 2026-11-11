//! `Mdm.Status` and the *configured* computation: each condition that is not met is named, one
//! test each, and what is read is the material's own expiry.

use std::sync::{Arc, Mutex};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::MdmCondition as Missing;
use hostkeeper_proto::hostkeeper::operator::v1::MdmRefusal;

use crate::login_support::World;
use crate::mdm_pki_support::{Pair, TOPIC, other_key, pair, pair_between};
use crate::mdm_secrets::MasterKey;
use crate::mdm_setup::MdmSetup;
use crate::operator::Caller;
use crate::store::mdm_setup::files::FileSetup;
use crate::store::mdm_setup::{SecretRow, SetupError, SetupStore};

/// The time of the tests, 2027-01-15.
const NOW: u64 = 1_800_000_000;
/// When the certificates of the tests end: later than `NOW`.
const ENDS: i64 = 1_900_000_000;
/// `2027-10-31T22:01:36Z`, the shape of the ABM expiry the spike read.
const ABM_ENDS: i64 = 1_825_020_096;
const NAME: &str = "mdm.internal.example.com";

fn alice() -> Caller {
    Caller::Operator("alice".parse().expect("a token name"))
}

fn token(expiry: &str) -> Vec<u8> {
    format!(
        r#"{{"consumer_key":"ck","consumer_secret":"cs","access_token":"at","access_secret":"as","access_secret_expiry":"{expiry}"}}"#
    )
    .into_bytes()
}

fn world_over(store: Arc<dyn SetupStore>, key: u8, name: &str) -> World {
    let world = World::build_timed(
        name,
        crate::login_support::settings(),
        crate::login_support::counting(),
        true,
        |c, _| {
            c.with_mdm_setup(MdmSetup::new(
                store,
                Some(MasterKey::from_bytes(&[key; 32]).expect("a key")),
                crate::enrollment::os_random(),
            ))
        },
    );
    world.at(NOW);
    world
}

fn world(name: &str) -> World {
    world_over(Arc::new(FileSetup::volatile()), 0x21, name)
}

fn upload(world: &World, kind: &str, material: &[u8]) {
    world
        .coordinator
        .mdm_put_secret(
            &alice(),
            proto::PutSecretRequest {
                kind: kind.to_owned(),
                material: material.to_vec(),
                not_after_unix: None,
            },
        )
        .expect("stored");
}

fn upload_pair(world: &World, cert: &str, key: &str, made: &Pair) {
    upload(world, cert, made.cert.as_bytes());
    upload(world, key, made.key.as_bytes());
}

fn set_name(world: &World, name: &str) {
    world
        .coordinator
        .mdm_put_name(
            &alice(),
            &proto::PutNameRequest {
                name: name.to_owned(),
            },
        )
        .expect("named");
}

/// A world with the whole setup in place and valid: the push pair, the ABM token, the name and a
/// TLS pair that names it.
fn complete(name: &str) -> World {
    complete_with(name, ENDS, "2027-10-31T22:01:36Z")
}

/// [`complete`] with certificates that end at `ends` and an ABM token that expires at `expiry`.
fn complete_with(name: &str, ends: i64, expiry: &str) -> World {
    let world = world(name);
    upload_pair(
        &world,
        "push_cert",
        "push_key",
        &pair(Some(TOPIC), &[], ends),
    );
    upload(&world, "abm_token", &token(expiry));
    set_name(&world, NAME);
    upload_pair(&world, "tls_cert", "tls_key", &pair(None, &[NAME], ends));
    world
}

fn status(world: &World) -> proto::MdmStatusResponse {
    world.coordinator.mdm_status(&alice()).expect("a status")
}

fn missing(world: &World) -> Vec<Missing> {
    let status = status(world);
    assert_eq!(status.configured, status.missing.is_empty());
    status
        .missing
        .iter()
        .map(|m| Missing::try_from(*m).expect("a condition"))
        .collect()
}

/// Proves: with everything in place the status says configured with nothing missing, reads the
/// push topic and each expiry from the material (the certificates' end of validity, the token's
/// `access_secret_expiry`), shows the name with its enrollment URL and the TLS names, and shows
/// each secret in use by what `ListSecrets` shows.
#[test]
fn a_complete_setup_is_configured_and_shows_what_is_read_of_it() {
    let world = complete("mdm-status-complete");
    let status = status(&world);
    let listed = world
        .coordinator
        .mdm_list_secrets(&alice())
        .expect("listed");
    let shown = |kind: &str| listed.secrets.iter().find(|s| s.kind == kind).cloned();
    assert_eq!(
        status,
        proto::MdmStatusResponse {
            configured: true,
            missing: vec![],
            name: NAME.to_owned(),
            enrollment_url: format!("https://{NAME}/enroll"),
            name_locked: false,
            sealing: true,
            push_cert: shown("push_cert"),
            push_key: shown("push_key"),
            abm_token: shown("abm_token"),
            tls_cert: shown("tls_cert"),
            tls_key: shown("tls_key"),
            push_topic: TOPIC.to_owned(),
            push_not_after_unix: Some(ENDS),
            abm_not_after_unix: Some(ABM_ENDS),
            tls_not_after_unix: Some(ENDS),
            tls_names: vec![NAME.to_owned()],
        }
    );
    assert!(status.push_cert.is_some() && status.tls_key.is_some());
}

/// Proves: before anything is set every condition is named, in the order push, ABM, name, TLS,
/// and nothing is shown; the coordinator still says whether it seals.
#[test]
fn an_empty_setup_names_every_condition() {
    let world = world("mdm-status-empty");
    let status = status(&world);
    assert_eq!(
        missing(&world),
        [
            Missing::PushCertMissing,
            Missing::PushKeyMissing,
            Missing::AbmTokenMissing,
            Missing::NameMissing,
            Missing::TlsCertMissing,
            Missing::TlsKeyMissing,
        ]
    );
    assert_eq!(
        status,
        proto::MdmStatusResponse {
            missing: status.missing.clone(),
            sealing: true,
            ..proto::MdmStatusResponse::default()
        }
    );
}

/// Proves: a coordinator with no master key holds nothing and says so, and is not configured.
#[test]
fn a_coordinator_with_no_master_key_is_not_configured_and_does_not_seal() {
    let world = World::build(
        "mdm-status-no-key",
        crate::login_support::settings(),
        crate::login_support::counting(),
        |c, _| c.with_mdm_setup(MdmSetup::volatile()),
    );
    let status = status(&world);
    assert!(!status.configured && !status.sealing);
    assert_eq!(status.missing.len(), 6);
}

/// Proves: without the push certificate the status names `push_cert_missing` and nothing else.
#[test]
fn a_missing_push_certificate_is_named() {
    let world = world("mdm-status-push-cert-missing");
    upload(&world, "push_key", other_key().as_bytes());
    upload(&world, "abm_token", &token("2027-10-31T22:01:36Z"));
    set_name(&world, NAME);
    upload_pair(&world, "tls_cert", "tls_key", &pair(None, &[NAME], ENDS));
    assert_eq!(missing(&world), [Missing::PushCertMissing]);
}

/// Proves: without the push key the status names `push_key_missing` and nothing else.
#[test]
fn a_missing_push_key_is_named() {
    let world = world("mdm-status-push-key-missing");
    upload(
        &world,
        "push_cert",
        pair(Some(TOPIC), &[], ENDS).cert.as_bytes(),
    );
    upload(&world, "abm_token", &token("2027-10-31T22:01:36Z"));
    set_name(&world, NAME);
    upload_pair(&world, "tls_cert", "tls_key", &pair(None, &[NAME], ENDS));
    assert_eq!(missing(&world), [Missing::PushKeyMissing]);
}

/// Proves: a push key that is not the certificate's, and a certificate that does not read, are
/// each `push_pair_invalid`, and the latter shows no topic or expiry.
#[test]
fn a_push_pair_that_does_not_match_is_named() {
    let world = complete("mdm-status-push-pair");
    upload(&world, "push_key", other_key().as_bytes());
    assert_eq!(missing(&world), [Missing::PushPairInvalid]);
    assert_eq!(status(&world).push_topic, TOPIC);

    upload(&world, "push_cert", b"not a certificate");
    assert_eq!(missing(&world), [Missing::PushPairInvalid]);
    let unread = status(&world);
    assert_eq!(
        (unread.push_topic.as_str(), unread.push_not_after_unix),
        ("", None)
    );
}

/// Proves: a push certificate whose end of validity is the time or before is `push_cert_expired`
/// and one a second later is not, so the status goes back to not configured by itself when the
/// time passes, with no change to any record.
#[test]
fn an_expired_push_certificate_is_named_at_the_second_it_ends() {
    let ends = 1_810_000_000;
    let world = complete_with("mdm-status-push-expired", ends, "2030-01-01T00:00:00Z");
    assert!(status(&world).configured);
    world.at(u64::try_from(ends).expect("a time") - 1);
    assert!(status(&world).configured, "one second before");
    world.at(u64::try_from(ends).expect("a time"));
    assert_eq!(
        missing(&world),
        [Missing::PushCertExpired, Missing::TlsCertExpired]
    );

    let fresh = complete("mdm-status-push-expired-upload");
    upload_pair(
        &fresh,
        "push_cert",
        "push_key",
        &pair(Some(TOPIC), &[], 1_700_000_000),
    );
    assert_eq!(missing(&fresh), [Missing::PushCertExpired]);
    upload_pair(
        &fresh,
        "push_cert",
        "push_key",
        &pair(Some(TOPIC), &[], ENDS),
    );
    assert!(
        status(&fresh).configured,
        "the newer version is the one in use"
    );
}

/// Proves: without the ABM token the status names `abm_token_missing` and nothing else.
#[test]
fn a_missing_abm_token_is_named() {
    let world = world("mdm-status-abm-missing");
    upload_pair(
        &world,
        "push_cert",
        "push_key",
        &pair(Some(TOPIC), &[], ENDS),
    );
    set_name(&world, NAME);
    upload_pair(&world, "tls_cert", "tls_key", &pair(None, &[NAME], ENDS));
    assert_eq!(missing(&world), [Missing::AbmTokenMissing]);
    assert_eq!(status(&world).abm_not_after_unix, None);
}

/// Proves: an ABM token whose expiry is no RFC 3339 time is `abm_token_unreadable` with no expiry
/// shown, and an expiry with an offset is read as that instant.
#[test]
fn an_abm_token_whose_expiry_does_not_read_is_named() {
    let world = complete("mdm-status-abm-unreadable");
    upload(&world, "abm_token", &token("next thursday"));
    assert_eq!(missing(&world), [Missing::AbmTokenUnreadable]);
    assert_eq!(status(&world).abm_not_after_unix, None);
    upload(&world, "abm_token", &token("2027-11-01T00:01:36+02:00"));
    assert!(status(&world).configured);
    assert_eq!(status(&world).abm_not_after_unix, Some(ABM_ENDS));
}

/// Proves: an ABM token whose expiry is the time or before is `abm_token_expired`, and one a
/// second after is not.
#[test]
fn an_expired_abm_token_is_named_at_the_second_it_ends() {
    let world = complete("mdm-status-abm-expired");
    assert!(status(&world).configured);
    world.at(u64::try_from(ABM_ENDS).expect("a time") - 1);
    assert!(status(&world).configured);
    world.at(u64::try_from(ABM_ENDS).expect("a time"));
    assert_eq!(missing(&world), [Missing::AbmTokenExpired]);
}

/// Proves: without a name the status names `name_missing` and nothing else, and a TLS certificate
/// is not judged against a name that is not there; the enrollment URL is empty.
#[test]
fn a_missing_name_is_named() {
    let world = world("mdm-status-name-missing");
    upload_pair(
        &world,
        "push_cert",
        "push_key",
        &pair(Some(TOPIC), &[], ENDS),
    );
    upload(&world, "abm_token", &token("2027-10-31T22:01:36Z"));
    upload_pair(&world, "tls_cert", "tls_key", &pair(None, &[NAME], ENDS));
    assert_eq!(missing(&world), [Missing::NameMissing]);
    let status = status(&world);
    assert_eq!(
        (status.name.as_str(), status.enrollment_url.as_str()),
        ("", "")
    );
}

/// Proves: without the TLS certificate the status names `tls_cert_missing` and nothing else.
#[test]
fn a_missing_tls_certificate_is_named() {
    let world = world("mdm-status-tls-cert-missing");
    upload_pair(
        &world,
        "push_cert",
        "push_key",
        &pair(Some(TOPIC), &[], ENDS),
    );
    upload(&world, "abm_token", &token("2027-10-31T22:01:36Z"));
    set_name(&world, NAME);
    upload(&world, "tls_key", other_key().as_bytes());
    assert_eq!(missing(&world), [Missing::TlsCertMissing]);
}

/// Proves: without the TLS key the status names `tls_key_missing` and nothing else.
#[test]
fn a_missing_tls_key_is_named() {
    let world = world("mdm-status-tls-key-missing");
    upload_pair(
        &world,
        "push_cert",
        "push_key",
        &pair(Some(TOPIC), &[], ENDS),
    );
    upload(&world, "abm_token", &token("2027-10-31T22:01:36Z"));
    set_name(&world, NAME);
    upload(
        &world,
        "tls_cert",
        pair(None, &[NAME], ENDS).cert.as_bytes(),
    );
    assert_eq!(missing(&world), [Missing::TlsKeyMissing]);
}

/// Proves: a TLS key that is not the certificate's is `tls_pair_invalid`.
#[test]
fn a_tls_pair_that_does_not_match_is_named() {
    let world = complete("mdm-status-tls-pair");
    upload(&world, "tls_key", other_key().as_bytes());
    assert_eq!(missing(&world), [Missing::TlsPairInvalid]);
}

/// Proves: a TLS certificate whose end of validity is the time or before is `tls_cert_expired`.
#[test]
fn an_expired_tls_certificate_is_named() {
    let world = complete("mdm-status-tls-expired");
    upload_pair(
        &world,
        "tls_cert",
        "tls_key",
        &pair(None, &[NAME], 1_700_000_000),
    );
    assert_eq!(missing(&world), [Missing::TlsCertExpired]);
    assert_eq!(status(&world).tls_not_after_unix, Some(1_700_000_000));
}

/// Proves: a TLS certificate that does not name the MDM name is `tls_name_mismatch`, and one with
/// a wildcard that covers it is configured.
#[test]
fn a_tls_certificate_for_another_name_is_named() {
    let world = complete("mdm-status-tls-name");
    upload_pair(
        &world,
        "tls_cert",
        "tls_key",
        &pair(None, &["other.internal.example.com"], ENDS),
    );
    assert_eq!(missing(&world), [Missing::TlsNameMismatch]);
    upload_pair(
        &world,
        "tls_cert",
        "tls_key",
        &pair(None, &["*.internal.example.com"], ENDS),
    );
    assert!(status(&world).configured);
}

/// Proves: a row sealed under another master key does not read, so its pair or token is named
/// invalid or unreadable rather than missing, and the rest of the status is still answered.
#[test]
fn rows_the_key_does_not_open_are_named_unreadable() {
    let store: Arc<dyn SetupStore> = Arc::new(FileSetup::volatile());
    let before = world_over(store.clone(), 0x01, "mdm-status-rekey-before");
    upload_pair(
        &before,
        "push_cert",
        "push_key",
        &pair(Some(TOPIC), &[], ENDS),
    );
    upload(&before, "abm_token", &token("2027-10-31T22:01:36Z"));
    set_name(&before, NAME);
    upload_pair(&before, "tls_cert", "tls_key", &pair(None, &[NAME], ENDS));
    let after = world_over(store, 0x02, "mdm-status-rekey-after");
    assert_eq!(
        missing(&after),
        [
            Missing::PushPairInvalid,
            Missing::AbmTokenUnreadable,
            Missing::TlsPairInvalid,
        ]
    );
    assert_eq!(status(&after).name, NAME);
}

/// Proves: a store that cannot be read refuses `store_failed`, and a caller who is not the
/// operator is refused before the store is looked at.
#[test]
fn a_status_of_a_store_that_fails_is_refused() {
    /// A store whose reads fail once told to: `.0` is the call of `secrets` that fails (counting
    /// from 1) and `.2` how many have been made; `.1` makes `settings` fail.
    struct Flaky(FileSetup, Mutex<(Option<usize>, bool, usize)>);
    impl SetupStore for Flaky {
        fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError> {
            self.0.put_secret(row)
        }
        fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
            let mut state = self.1.lock().expect("a lock");
            state.2 += 1;
            if state.0 == Some(state.2) {
                Err(SetupError::Store("unreadable".to_owned()))
            } else {
                self.0.secrets()
            }
        }
        fn prune_superseded(&self, at: i64) -> Result<u64, SetupError> {
            self.0.prune_superseded(at)
        }
        fn settings(&self) -> Result<std::collections::BTreeMap<String, String>, SetupError> {
            if self.1.lock().expect("a lock").1 {
                Err(SetupError::Store("unreadable".to_owned()))
            } else {
                self.0.settings()
            }
        }
        fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupError> {
            self.0.set_setting(name, value)
        }
    }
    let flaky = Arc::new(Flaky(FileSetup::volatile(), Mutex::new((None, false, 0))));
    let world = world_over(flaky.clone(), 0x21, "mdm-status-store-fails");
    assert!(world.coordinator.mdm_status(&alice()).is_ok());
    let fail = |secrets_call: Option<usize>, settings: bool| {
        let mut state = flaky.1.lock().expect("a lock");
        *state = (secrets_call, settings, 0);
    };
    // The settings do not read.
    fail(None, true);
    assert_eq!(
        world.coordinator.mdm_status(&alice()),
        Err(MdmRefusal::StoreFailed)
    );
    // The listing of the secrets does not read: the first call of `secrets`.
    fail(Some(1), false);
    assert_eq!(
        world.coordinator.mdm_status(&alice()),
        Err(MdmRefusal::StoreFailed)
    );
    // A secret does not open because the store fails on the second call, after the listing.
    fail(Some(2), false);
    assert_eq!(
        world.coordinator.mdm_status(&alice()),
        Err(MdmRefusal::StoreFailed)
    );
    assert_eq!(
        world.coordinator.mdm_status(&Caller::Anonymous),
        Err(MdmRefusal::Unauthorized)
    );
}

/// Proves: a push or TLS certificate whose validity has not begun is `push_cert_not_yet_valid` or
/// `tls_cert_not_yet_valid` (and shows its end), and one that has begun, at the second it does,
/// is not.
#[test]
fn a_certificate_that_is_not_yet_valid_is_named() {
    let begins = NOW as i64 + 1;
    let world = complete("mdm-status-not-yet-valid");
    upload_pair(
        &world,
        "push_cert",
        "push_key",
        &pair_between(Some(TOPIC), &[], begins, ENDS),
    );
    assert_eq!(missing(&world), [Missing::PushCertNotYetValid]);
    upload_pair(
        &world,
        "tls_cert",
        "tls_key",
        &pair_between(None, &[NAME], begins, ENDS),
    );
    assert_eq!(
        missing(&world),
        [Missing::PushCertNotYetValid, Missing::TlsCertNotYetValid]
    );
    assert_eq!(status(&world).tls_not_after_unix, Some(ENDS));
    world.at(NOW + 1);
    assert!(status(&world).configured, "at the second it begins");
}
