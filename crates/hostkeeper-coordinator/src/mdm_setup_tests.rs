//! The coordinator's hold of the MDM setup over a store: what it seals, what it shows and what it
//! deletes.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{MdmSetup, SetupFailure};
use crate::mdm_secrets::{MasterKey, SealError, Secret, SecretKind};
use crate::store::mdm_setup::files::FileSetup;
use crate::store::mdm_setup::{SecretRow, SetupError, SetupStore};

const DAY: i64 = 86_400;

fn key() -> MasterKey {
    MasterKey::from_bytes(&[0x11; 32]).expect("a key")
}

fn setup() -> (MdmSetup, Arc<FileSetup>) {
    let store = Arc::new(FileSetup::volatile());
    let shared: Arc<dyn SetupStore> = store.clone();
    (
        MdmSetup::new(shared, Some(key()), crate::enrollment::os_random()),
        store,
    )
}

fn secret(text: &str) -> Secret {
    Secret::new(text.as_bytes().to_vec())
}

/// A store that fails what it is told to, over a store that works.
struct Failing {
    inner: FileSetup,
    reads: AtomicBool,
    writes: AtomicBool,
}

impl Failing {
    fn failure(&self, on: &AtomicBool) -> Result<(), SetupError> {
        if on.load(Ordering::SeqCst) {
            Err(SetupError::Store("the disk is full".to_owned()))
        } else {
            Ok(())
        }
    }
}

impl SetupStore for Failing {
    fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError> {
        self.failure(&self.writes)?;
        self.inner.put_secret(row)
    }

    fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
        self.failure(&self.reads)?;
        self.inner.secrets()
    }

    fn prune_superseded(&self, superseded_unix: i64) -> Result<u64, SetupError> {
        self.failure(&self.writes)?;
        self.inner.prune_superseded(superseded_unix)
    }

    fn settings(&self) -> Result<std::collections::BTreeMap<String, String>, SetupError> {
        self.failure(&self.reads)?;
        self.inner.settings()
    }

    fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupError> {
        self.failure(&self.writes)?;
        self.inner.set_setting(name, value)
    }
}

/// Proves: a secret is stored sealed, shown by kind, version, fingerprint and times only, and
/// opens to what was put in; the store never holds the plaintext, and the fingerprint is the
/// SHA-256 of it.
///
/// Catches: a secret kept as plaintext, or shown by anything but its fingerprint.
#[test]
fn a_secret_is_kept_sealed_and_shown_by_its_fingerprint() {
    let (setup, store) = setup();
    let info = setup
        .put_secret(
            SecretKind::PushKey,
            &secret("rsa key bytes"),
            Some(5000),
            100,
        )
        .expect("stored");
    assert_eq!(info.kind, SecretKind::PushKey);
    assert_eq!(info.version, 1);
    assert_eq!(info.fingerprint.len(), 64);
    assert_eq!(info.not_after_unix, Some(5000));
    assert_eq!(info.created_unix, 100);
    assert_eq!(info.superseded_unix, None);
    let rows = store.secrets().expect("rows");
    assert_eq!(rows.len(), 1);
    assert!(
        !rows[0]
            .sealed
            .0
            .windows(13)
            .any(|window| window == b"rsa key bytes"),
        "the store holds no plaintext"
    );
    assert_eq!(
        setup.open(SecretKind::PushKey).expect("opens"),
        Some(secret("rsa key bytes"))
    );
    assert_eq!(setup.open(SecretKind::TlsKey).expect("none"), None);
    assert_eq!(setup.list_secrets().expect("listed"), vec![info]);
}

/// Proves: the fingerprint is the SHA-256 of the plaintext in hex (a known vector).
///
/// Catches: a fingerprint of the sealed bytes, which changes on every upload of the same value.
#[test]
fn the_fingerprint_is_the_sha256_of_the_value() {
    let (setup, _) = setup();
    let info = setup
        .put_secret(SecretKind::TlsKey, &secret("abc"), None, 1)
        .expect("stored");
    assert_eq!(
        info.fingerprint,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

/// Proves: a replacement is the next version, the one it replaces is kept and marked superseded at
/// the time of the replacement, the version in use is the newest, and a version replaced longer
/// ago than `secret_grace_days` is deleted by the next upload of any kind; the grace is the
/// setting's.
///
/// Catches: a wrong upload that cannot be undone, or replaced secrets that stay sealed for ever.
#[test]
fn a_replaced_secret_is_kept_for_the_grace_and_then_deleted() {
    let (setup, _) = setup();
    setup
        .put_secret(SecretKind::TlsKey, &secret("one"), None, 0)
        .expect("v1");
    let second = setup
        .put_secret(SecretKind::TlsKey, &secret("two"), None, DAY)
        .expect("v2");
    assert_eq!(second.version, 2);
    let versions: Vec<(u32, Option<i64>)> = setup
        .list_secrets()
        .expect("listed")
        .iter()
        .map(|info| (info.version, info.superseded_unix))
        .collect();
    assert_eq!(versions, [(1, Some(DAY)), (2, None)]);
    assert_eq!(
        setup.open(SecretKind::TlsKey).expect("opens"),
        Some(secret("two"))
    );

    // 7 days after the replacement the first version is still the grace's last day.
    setup
        .put_secret(
            SecretKind::TlsCert,
            &secret("cert"),
            None,
            DAY + 7 * DAY - 1,
        )
        .expect("another kind");
    assert_eq!(setup.list_secrets().expect("listed").len(), 3);
    setup
        .put_secret(SecretKind::TlsCert, &secret("cert2"), None, DAY + 7 * DAY)
        .expect("another kind again");
    let left: Vec<(String, u32)> = setup
        .list_secrets()
        .expect("listed")
        .iter()
        .map(|info| (info.kind.as_str().to_owned(), info.version))
        .collect();
    assert_eq!(
        left,
        [
            ("tls_cert".to_owned(), 1),
            ("tls_cert".to_owned(), 2),
            ("tls_key".to_owned(), 2),
        ],
        "the first TLS key went with the grace; the certificate replaced just now stays"
    );
}

/// Proves: with the grace set to 0 days a replaced version is deleted at once, and a grace of
/// 30 days keeps it for 30.
///
/// Catches: a grace that is a constant and not the setting.
#[test]
fn the_grace_is_the_setting() {
    let (setup, _) = setup();
    setup.set_setting("secret_grace_days", "0").expect("set");
    setup
        .put_secret(SecretKind::TlsKey, &secret("one"), None, 10)
        .expect("v1");
    setup
        .put_secret(SecretKind::TlsKey, &secret("two"), None, 20)
        .expect("v2");
    assert_eq!(setup.list_secrets().expect("listed").len(), 1);

    setup.set_setting("secret_grace_days", "30").expect("set");
    setup
        .put_secret(SecretKind::TlsKey, &secret("three"), None, 30)
        .expect("v3");
    setup
        .put_secret(SecretKind::TlsKey, &secret("four"), None, 30 + 29 * DAY)
        .expect("v4");
    assert_eq!(setup.list_secrets().expect("listed").len(), 3);
}

/// Proves: with no master key nothing is stored or opened (`NoKey`) and the settings still work;
/// a source of randomness that fails stores nothing.
///
/// Catches: a secret stored unsealed when no key is held.
#[test]
fn no_key_stores_no_secret_and_no_nonce_stores_none() {
    let store: Arc<dyn SetupStore> = Arc::new(FileSetup::volatile());
    let keyless = MdmSetup::new(Arc::clone(&store), None, crate::enrollment::os_random());
    assert!(!keyless.has_key());
    assert_eq!(
        keyless.put_secret(SecretKind::TlsKey, &secret("x"), None, 1),
        Err(SetupFailure::NoKey)
    );
    assert_eq!(keyless.open(SecretKind::TlsKey), Err(SetupFailure::NoKey));
    assert_eq!(
        SetupFailure::NoKey.to_string(),
        "no master key is held: --mdm-secrets-key-file is not given"
    );
    keyless
        .set_setting("tls_renew_days", "10")
        .expect("a setting needs no key");
    assert!(store.secrets().expect("rows").is_empty());

    let broken = keyless.with_key(key()).with_random(Box::new(|_| false));
    assert!(broken.has_key());
    assert_eq!(
        broken.put_secret(SecretKind::TlsKey, &secret("x"), None, 1),
        Err(SetupFailure::Random)
    );
    assert!(store.secrets().expect("rows").is_empty());
    assert_eq!(
        SetupFailure::Random.to_string(),
        "the system gave no random bytes for a nonce"
    );
}

/// Proves: a row of one kind copied under another kind, or another version, does not open and is
/// named by `unopened`; a row the key does open is not.
///
/// Catches: a database written to by hand that replays a sealed value into another kind.
#[test]
fn a_row_sealed_for_one_kind_does_not_open_as_another() {
    let (setup, store) = setup();
    setup
        .put_secret(SecretKind::PushKey, &secret("push"), None, 1)
        .expect("stored");
    setup
        .put_secret(SecretKind::TlsKey, &secret("tls"), None, 1)
        .expect("stored");
    assert_eq!(setup.unopened().expect("checked"), Vec::<SecretKind>::new());
    let push = store
        .secrets()
        .expect("rows")
        .into_iter()
        .find(|row| row.kind == "push_key")
        .expect("the push row");
    store
        .put_secret(&SecretRow {
            kind: "abm_key".to_owned(),
            version: 1,
            ..push.clone()
        })
        .expect("copied under another kind");
    store
        .put_secret(&SecretRow {
            kind: "tls_key".to_owned(),
            version: 2,
            ..push
        })
        .expect("copied under another version");
    assert_eq!(
        setup.open(SecretKind::AbmKey),
        Err(SetupFailure::Seal(SealError::Open))
    );
    assert_eq!(
        setup.open(SecretKind::TlsKey),
        Err(SetupFailure::Seal(SealError::Open))
    );
    assert_eq!(
        setup.unopened().expect("checked"),
        vec![SecretKind::AbmKey, SecretKind::TlsKey]
    );
    assert_eq!(
        setup.open(SecretKind::PushKey).expect("opens"),
        Some(secret("push"))
    );
}

/// Proves: a row of a kind this release does not know is kept and not listed, and the settings
/// list every known setting with whether it is the default.
///
/// Catches: a later release's row that stops the listing, or a setting that shows no default.
#[test]
fn unknown_kinds_are_not_listed_and_settings_show_their_defaults() {
    let (setup, store) = setup();
    store
        .put_secret(&SecretRow {
            kind: "from_the_future".to_owned(),
            version: 1,
            sealed: crate::store::mdm_setup::Sealed(vec![0; 40]),
            fingerprint: "0".repeat(64),
            not_after_unix: None,
            created_unix: 1,
            superseded_unix: None,
        })
        .expect("a later release's row");
    assert!(setup.list_secrets().expect("listed").is_empty());
    assert_eq!(setup.unopened().expect("checked"), Vec::<SecretKind>::new());

    setup.set_setting("tls_renew_days", "10").expect("set");
    let settings = setup.list_settings().expect("listed");
    assert_eq!(
        settings,
        [
            ("push_alert_days", "60,14".to_owned(), true),
            ("abm_alert_days", "60".to_owned(), true),
            ("tls_renew_days", "10".to_owned(), false),
            ("secret_grace_days", "7".to_owned(), true),
            ("push_retry_s", "3600".to_owned(), true),
            ("push_expiry_s", "86400".to_owned(), true),
            ("push_priority", "10".to_owned(), true),
            ("push_timeout_s", "30".to_owned(), true),
            ("dns_zone", "".to_owned(), true),
            (
                "acme_directory",
                "https://acme-v02.api.letsencrypt.org/directory".to_owned(),
                true
            ),
            ("acme_contact", "".to_owned(), true),
            ("dns_ttl_s", "600".to_owned(), true),
            ("dns_poll_s", "15".to_owned(), true),
            ("dns_timeout_s", "600".to_owned(), true),
            ("dns_settle_s", "60".to_owned(), true),
            ("acme_timeout_s", "300".to_owned(), true),
            ("tls_check_s", "300".to_owned(), true),
            ("tls_retry_s", "3600".to_owned(), true),
        ]
    );
    assert_eq!(
        setup
            .settings()
            .expect("set ones")
            .get("tls_renew_days")
            .map(String::as_str),
        Some("10")
    );
}

/// Proves: a store that fails is answered as a failure with the store's text, for a write, a read,
/// the check of sealed rows and the grace's prune, and a failed write leaves what was held.
///
/// Catches: a failure taken as success, which the operator would see as an upload that was kept.
#[test]
fn a_failing_store_is_a_failure() {
    let failing = Arc::new(Failing {
        inner: FileSetup::volatile(),
        reads: AtomicBool::new(false),
        writes: AtomicBool::new(false),
    });
    let shared: Arc<dyn SetupStore> = failing.clone();
    let setup = MdmSetup::new(shared, Some(key()), crate::enrollment::os_random());
    setup
        .put_secret(SecretKind::TlsKey, &secret("kept"), None, 1)
        .expect("stored");

    failing.writes.store(true, Ordering::SeqCst);
    let why = setup
        .put_secret(SecretKind::TlsKey, &secret("lost"), None, 2)
        .expect_err("refused");
    assert_eq!(
        why.to_string(),
        "the MDM setup store failed: the disk is full"
    );
    assert!(setup.set_setting("tls_renew_days", "9").is_err());
    failing.writes.store(false, Ordering::SeqCst);
    assert_eq!(
        setup.open(SecretKind::TlsKey).expect("opens"),
        Some(secret("kept"))
    );

    failing.reads.store(true, Ordering::SeqCst);
    assert!(setup.list_secrets().is_err());
    assert!(setup.list_settings().is_err());
    assert!(setup.settings().is_err());
    assert!(setup.open(SecretKind::TlsKey).is_err());
    assert!(setup.unopened().is_err());
    assert!(
        setup
            .put_secret(SecretKind::TlsKey, &secret("x"), None, 3)
            .is_err()
    );
    assert!(MdmSetup::holds_secrets(&*failing).is_err());
    failing.reads.store(false, Ordering::SeqCst);
    assert!(MdmSetup::holds_secrets(&*failing).expect("read"));
}

/// Proves: every version of a kind still held is opened, newest first; a kind with none gives an
/// empty list; no master key is `NoKey`; and a version that does not open fails the whole read.
///
/// Catches: a reader of replaced versions that skips a version it cannot open and so pairs a
/// certificate with a key that was never checked.
#[test]
fn every_held_version_of_a_kind_opens_newest_first() {
    let (setup, store) = setup();
    assert_eq!(setup.open_held(SecretKind::PushKey), Ok(Vec::new()));
    setup
        .put_secret(SecretKind::PushKey, &secret("one"), None, 1)
        .expect("v1");
    setup
        .put_secret(SecretKind::PushKey, &secret("two"), None, 2)
        .expect("v2");
    setup
        .put_secret(SecretKind::TlsKey, &secret("other kind"), None, 2)
        .expect("tls");
    assert_eq!(
        setup.open_held(SecretKind::PushKey),
        Ok(vec![secret("two"), secret("one")])
    );
    let keyless = MdmSetup::new(
        Arc::<FileSetup>::clone(&store),
        None,
        crate::enrollment::os_random(),
    );
    assert_eq!(
        keyless.open_held(SecretKind::PushKey),
        Err(SetupFailure::NoKey)
    );
    let tls = store
        .secrets()
        .expect("rows")
        .into_iter()
        .find(|row| row.kind == "tls_key")
        .expect("the tls row");
    store
        .put_secret(&SecretRow {
            kind: "push_key".to_owned(),
            version: 3,
            ..tls
        })
        .expect("copied under another kind");
    assert_eq!(
        setup.open_held(SecretKind::PushKey),
        Err(SetupFailure::Seal(SealError::Open))
    );
}

/// Proves: when the replacement is stored but the settings cannot be read for the grace, or the
/// prune of old versions fails, the upload still reports success with the new version, which is
/// listed; the next upload prunes.
///
/// Catches: an upload answered as failed (and audited as refused) when its version is stored and
/// in use, which a retry would then store a second time.
#[test]
fn a_prune_that_fails_does_not_fail_the_upload() {
    /// Fails the settings read, the prune, or neither, as told; every other call is the file store's.
    struct Partly {
        inner: FileSetup,
        settings: AtomicBool,
        prune: AtomicBool,
    }
    impl SetupStore for Partly {
        fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError> {
            self.inner.put_secret(row)
        }
        fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
            self.inner.secrets()
        }
        fn prune_superseded(&self, at: i64) -> Result<u64, SetupError> {
            if self.prune.load(Ordering::SeqCst) {
                return Err(SetupError::Store("the disk is full".to_owned()));
            }
            self.inner.prune_superseded(at)
        }
        fn settings(&self) -> Result<std::collections::BTreeMap<String, String>, SetupError> {
            if self.settings.load(Ordering::SeqCst) {
                return Err(SetupError::Store("the disk is full".to_owned()));
            }
            self.inner.settings()
        }
        fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupError> {
            self.inner.set_setting(name, value)
        }
    }
    let store = Arc::new(Partly {
        inner: FileSetup::volatile(),
        settings: AtomicBool::new(false),
        prune: AtomicBool::new(true),
    });
    let setup = MdmSetup::new(
        Arc::clone(&store) as Arc<dyn SetupStore>,
        Some(key()),
        crate::enrollment::os_random(),
    );
    let first = setup
        .put_secret(SecretKind::TlsKey, &secret("x"), None, 1)
        .expect("the version is stored, so the upload is done");
    assert_eq!(first.version, 1);
    store.settings.store(true, Ordering::SeqCst);
    let second = setup
        .put_secret(SecretKind::TlsKey, &secret("y"), None, 2)
        .expect("an unreadable grace setting falls to its default");
    assert_eq!(second.version, 2);
    let versions: Vec<u32> = setup
        .list_secrets()
        .expect("listed")
        .iter()
        .map(|info| info.version)
        .collect();
    assert_eq!(versions, [1, 2], "both are kept: nothing was pruned");

    // The next upload that can prune does: version 1 was replaced at once and the grace is over.
    store.settings.store(false, Ordering::SeqCst);
    store.prune.store(false, Ordering::SeqCst);
    let day = 86_400 * 8;
    setup
        .put_secret(SecretKind::TlsKey, &secret("z"), None, day)
        .expect("stored");
    let versions: Vec<u32> = setup
        .list_secrets()
        .expect("listed")
        .iter()
        .map(|info| info.version)
        .collect();
    assert_eq!(versions, [2, 3]);
}
