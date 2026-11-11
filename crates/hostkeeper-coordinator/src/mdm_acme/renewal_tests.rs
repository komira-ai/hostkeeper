use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use futures::FutureExt as _;
use rustls::pki_types::ServerName;
use tokio_rustls::{TlsAcceptor, TlsConnector};

use super::{Parts, Tick, TlsRenewer, leaf_not_after, renew_every};
use crate::mdm_acme::Clock;
use crate::mdm_acme::dns::DnsProviders;
use crate::mdm_acme::https::Https;
use crate::mdm_acme::status::TlsStatus;
use crate::mdm_acme::testkit::{FakeAcme, FakeClock, FakeDns, FakeProviders, Mood};
use crate::mdm_secrets::{MasterKey, Secret, SecretKind};
use crate::mdm_server::tls::MdmTls;
use crate::mdm_setup::MdmSetup;
use crate::store::mdm_setup::files::FileSetup;
use crate::store::mdm_setup::{SecretRow, SetupError, SetupStore};

const NAME: &str = "mdm.internal.example.net";
const TXT: &str = "_acme-challenge.mdm.internal";
const DAY: u64 = 86_400;
const CREDENTIAL: &str = r#"{"provider":"godaddy","kind":"sso-key","key":"KEY1","secret":"SEC2"}"#;

/// A store that fails what it is told to, over one that works.
struct Flaky {
    inner: FileSetup,
    settings: AtomicBool,
    secrets: AtomicBool,
    puts_left: AtomicI64,
}

impl Flaky {
    fn gone(on: &AtomicBool) -> Result<(), SetupError> {
        if on.load(Ordering::SeqCst) {
            Err(SetupError::Store("the disk is gone".to_owned()))
        } else {
            Ok(())
        }
    }
}

impl SetupStore for Flaky {
    fn put_secret(&self, row: &SecretRow) -> Result<(), SetupError> {
        if self.puts_left.fetch_sub(1, Ordering::SeqCst) <= 0 {
            return Err(SetupError::Store("the disk is full".to_owned()));
        }
        self.inner.put_secret(row)
    }
    fn secrets(&self) -> Result<Vec<SecretRow>, SetupError> {
        Self::gone(&self.secrets)?;
        self.inner.secrets()
    }
    fn prune_superseded(&self, at: i64) -> Result<u64, SetupError> {
        self.inner.prune_superseded(at)
    }
    fn settings(&self) -> Result<BTreeMap<String, String>, SetupError> {
        Self::gone(&self.settings)?;
        self.inner.settings()
    }
    fn set_setting(&self, name: &str, value: &str) -> Result<(), SetupError> {
        self.inner.set_setting(name, value)
    }
}

struct Rig {
    setup: Arc<MdmSetup>,
    store: Arc<Flaky>,
    dns: Arc<FakeDns>,
    ca: Arc<FakeAcme>,
    clock: Arc<FakeClock>,
    status: Arc<TlsStatus>,
    renewer: Arc<TlsRenewer>,
    t0: u64,
}

fn master() -> MasterKey {
    MasterKey::from_bytes(&[0x33; 32]).expect("a key")
}

/// A rig whose name, zone and credential are all set.
fn configured() -> Rig {
    let rig = bare();
    rig.setup.set_setting("mdm_name", NAME).expect("set");
    rig.setup
        .set_setting("dns_zone", "example.net")
        .expect("set");
    rig.setup
        .set_setting("acme_directory", crate::mdm_acme::testkit::DIRECTORY)
        .expect("set");
    rig.credential(CREDENTIAL);
    rig
}

fn bare() -> Rig {
    let t0 = hostkeeper_enrollment::validity::UnixClock::now_unix(
        &hostkeeper_enrollment::validity::SystemClock,
    );
    let store = Arc::new(Flaky {
        inner: FileSetup::volatile(),
        settings: AtomicBool::new(false),
        secrets: AtomicBool::new(false),
        puts_left: AtomicI64::new(i64::MAX),
    });
    let setup = Arc::new(MdmSetup::new(
        Arc::clone(&store) as Arc<dyn SetupStore>,
        Some(master()),
        crate::enrollment::os_random(),
    ));
    let dns = FakeDns::new();
    let ca = FakeAcme::new(&dns, NAME, TXT, t0, 90 * DAY);
    let clock = FakeClock::at(t0);
    let status = Arc::clone(setup.tls_status());
    let renewer = Arc::new(TlsRenewer::new(Parts {
        setup: Arc::clone(&setup),
        https: Arc::clone(&ca) as Arc<dyn Https>,
        providers: Arc::new(FakeProviders(Arc::clone(&dns))) as Arc<dyn DnsProviders>,
        clock: Arc::clone(&clock) as Arc<dyn Clock>,
        status: Arc::clone(&status),
    }));
    Rig {
        setup,
        store,
        dns,
        ca,
        clock,
        status,
        renewer,
        t0,
    }
}

impl Rig {
    /// Days later, for the clock and for the CA's certificates.
    fn days_later(&self, days: u64) {
        self.clock.advance(days * DAY);
        self.ca.set_now(self.clock.now_unix());
    }

    fn credential(&self, json: &str) {
        self.put(SecretKind::DnsCredential, json.as_bytes());
    }

    fn put(&self, kind: SecretKind, bytes: &[u8]) {
        self.setup
            .put_secret(kind, &Secret::new(bytes.to_vec()), None, 1)
            .expect("stored");
    }

    fn version(&self, kind: SecretKind) -> u32 {
        self.setup
            .list_secrets()
            .expect("listed")
            .iter()
            .filter(|info| info.kind == kind)
            .map(|info| info.version)
            .max()
            .unwrap_or(0)
    }

    fn not_after(&self) -> Option<i64> {
        self.setup
            .list_secrets()
            .expect("listed")
            .iter()
            .filter(|info| info.kind == SecretKind::TlsCert && info.superseded_unix.is_none())
            .find_map(|info| info.not_after_unix)
    }
}

/// Proves: with the name, the zone or the credential missing, or the settings unreadable, a tick
/// does nothing, asks the CA for nothing, and reports nothing to the alerts.
///
/// Catches: an alert on a setup nobody has configured yet.
#[tokio::test]
async fn an_unconfigured_setup_is_not_a_failing_one() {
    let rig = bare();
    assert_eq!(rig.renewer.tick().await, Tick::NotConfigured);
    rig.setup.set_setting("mdm_name", NAME).expect("set");
    assert_eq!(rig.renewer.tick().await, Tick::NotConfigured);
    rig.setup
        .set_setting("dns_zone", "example.net")
        .expect("set");
    assert_eq!(rig.renewer.tick().await, Tick::NotConfigured);
    rig.credential(CREDENTIAL);
    rig.store.settings.store(true, Ordering::SeqCst);
    assert_eq!(rig.renewer.tick().await, Tick::NotConfigured);
    assert!(rig.ca.requests().is_empty());
    assert_eq!(rig.status.snapshot(), Default::default());
}

/// Proves: with nothing held a tick orders the certificate: the order runs against the CA with the
/// record at the zone's challenge name, the account key, then the TLS key and the chain are
/// stored as version 1 with the chain's expiry as its label, the record is gone, and the next
/// tick finds the certificate not due and asks the CA for nothing.
///
/// Catches: an order that stores the certificate without its key, or a certificate with no
/// expiry that the status cannot warn about.
#[tokio::test]
async fn the_first_certificate_is_ordered_and_stored() {
    let rig = configured();
    assert_eq!(rig.renewer.tick().await, Tick::Renewed);
    assert_eq!(rig.version(SecretKind::AcmeAccountKey), 1);
    assert_eq!(rig.version(SecretKind::TlsKey), 1);
    assert_eq!(rig.version(SecretKind::TlsCert), 1);
    assert_eq!(
        rig.not_after(),
        Some(i64::try_from(rig.t0 + 90 * DAY).expect("fits"))
    );
    assert!(rig.dns.values(TXT).is_empty());
    assert_eq!(rig.ca.challenge_values().len(), 1);
    assert_eq!(
        rig.dns.calls().first().map(String::as_str),
        Some("publish _acme-challenge.mdm.internal ttl 600")
    );
    let before = rig.ca.requests().len();
    assert_eq!(rig.renewer.tick().await, Tick::NotDue);
    assert_eq!(rig.ca.requests().len(), before);
    assert_eq!(rig.status.snapshot(), Default::default());
}

/// Proves: the certificate is renewed when it expires within `tls_renew_days`: at 59 days of 90
/// held it is not due, at 61 it is and the next version is stored on the same account; a smaller
/// window set later moves the day; and a stored certificate that cannot be read is replaced.
///
/// Catches: a window that ignores the setting, an account made again for each renewal, and a
/// garbage certificate kept for ever.
#[tokio::test]
async fn the_certificate_is_renewed_inside_the_window_the_setting_gives() {
    let rig = configured();
    assert_eq!(rig.renewer.tick().await, Tick::Renewed);
    rig.days_later(59);
    assert_eq!(rig.renewer.tick().await, Tick::NotDue);
    rig.days_later(2);
    assert_eq!(rig.renewer.tick().await, Tick::Renewed);
    assert_eq!(rig.version(SecretKind::TlsCert), 2);
    assert_eq!(rig.version(SecretKind::TlsKey), 2);
    assert_eq!(rig.ca.accounts(), 1);
    assert_eq!(rig.version(SecretKind::AcmeAccountKey), 1);

    rig.setup.set_setting("tls_renew_days", "45").expect("set");
    rig.days_later(44);
    assert_eq!(rig.renewer.tick().await, Tick::NotDue);
    rig.days_later(2);
    assert_eq!(rig.renewer.tick().await, Tick::Renewed);
    assert_eq!(rig.version(SecretKind::TlsCert), 3);

    rig.put(SecretKind::TlsCert, b"not a certificate");
    assert_eq!(rig.renewer.tick().await, Tick::Renewed);
    assert_eq!(rig.version(SecretKind::TlsCert), 5);
}

/// Proves: a certificate that does not open under the master key is left alone: it is neither
/// reported as due nor replaced, since an operator has to look at which key is held.
#[tokio::test]
async fn a_certificate_that_will_not_open_is_not_replaced() {
    let rig = configured();
    let other = MdmSetup::new(
        Arc::clone(&rig.store) as Arc<dyn SetupStore>,
        Some(MasterKey::from_bytes(&[0x44; 32]).expect("a key")),
        crate::enrollment::os_random(),
    );
    other
        .put_secret(SecretKind::TlsCert, &Secret::new(b"x".to_vec()), None, 1)
        .expect("stored");
    assert_eq!(rig.renewer.tick().await, Tick::NotDue);
    assert!(rig.ca.requests().is_empty());
}

/// Proves: a failed attempt leaves the challenge record out of the zone (here the CA ends the
/// order invalid after it was published), is reported with its message and a count, makes the
/// next tick wait `tls_retry_s` without asking the CA again, a second failure after the wait
/// counts again from the same first time, and an attempt that succeeds clears the report.
///
/// Catches: a failing renewal retried at every tick, an alert that resets its age on each failure,
/// and a record left behind by a failure.
#[tokio::test]
async fn a_failure_backs_off_alerts_and_is_cleared_by_a_success() {
    let rig = configured();
    rig.ca.be(Mood::OrderInvalid);
    assert_eq!(rig.renewer.tick().await, Tick::Failed);
    assert!(rig.dns.values(TXT).is_empty());
    let first = rig.status.snapshot().failing.expect("failing");
    assert_eq!(first.attempts, 1);
    assert_eq!(first.since_unix, rig.clock.now_unix());
    assert_eq!(first.message, "the CA ended the order invalid");
    let asked = rig.ca.requests().len();
    assert_eq!(rig.renewer.tick().await, Tick::Waiting);
    rig.clock.advance(3_599);
    assert_eq!(rig.renewer.tick().await, Tick::Waiting);
    assert_eq!(rig.ca.requests().len(), asked);
    rig.clock.advance(1);
    assert_eq!(rig.renewer.tick().await, Tick::Failed);
    let second = rig.status.snapshot().failing.expect("failing");
    assert_eq!((second.attempts, second.since_unix), (2, first.since_unix));
    assert!(rig.status.snapshot().dns_rejected.is_none());
    rig.ca.be(Mood::Issues);
    rig.clock.advance(3_600);
    assert_eq!(rig.renewer.tick().await, Tick::Renewed);
    assert_eq!(rig.status.snapshot(), Default::default());
    assert_eq!(rig.status.retry_at_unix(), 0);
}

/// Proves: `tls_retry_s` sets the wait after a failure, and the account the failed attempt made is
/// kept: the next attempt reuses it.
#[tokio::test]
async fn the_wait_after_a_failure_is_a_setting_and_the_account_is_kept() {
    let rig = configured();
    rig.setup.set_setting("tls_retry_s", "120").expect("set");
    rig.ca.be(Mood::RefusesOrder);
    assert_eq!(rig.renewer.tick().await, Tick::Failed);
    assert_eq!(rig.version(SecretKind::AcmeAccountKey), 1);
    assert!(
        rig.status
            .snapshot()
            .failing
            .expect("failing")
            .message
            .contains("too many orders")
    );
    rig.clock.advance(119);
    assert_eq!(rig.renewer.tick().await, Tick::Waiting);
    rig.clock.advance(1);
    rig.ca.be(Mood::Issues);
    assert_eq!(rig.renewer.tick().await, Tick::Renewed);
    assert_eq!(rig.ca.accounts(), 1);
}

/// Proves: a DNS provider that refuses the credential is reported apart from a failing renewal
/// (both stand), and the alerts say so.
///
/// Catches: a refused credential reported as a generic failure the operator would look for in the
/// CA.
#[tokio::test]
async fn a_refused_dns_credential_is_reported_as_such() {
    let rig = configured();
    rig.dns.reject_with(401);
    assert_eq!(rig.renewer.tick().await, Tick::Failed);
    let snapshot = rig.status.snapshot();
    assert_eq!(
        snapshot.dns_rejected.as_deref(),
        Some("the DNS provider refused the credential (status 401)")
    );
    assert!(snapshot.failing.is_some());
    assert_eq!(crate::mdm_acme::alerts::derive(&snapshot).len(), 2);
}

/// Proves: a credential that is not understood, one for a provider this release has not, and one
/// that does not open each fail the attempt, in words that hold nothing of the record, and
/// the CA is asked for nothing.
#[tokio::test]
async fn a_credential_that_cannot_be_used_fails_the_attempt() {
    let rig = configured();
    rig.credential("SECRET-TEXT");
    assert_eq!(rig.renewer.tick().await, Tick::Failed);
    assert_eq!(
        rig.status.snapshot().failing.expect("failing").message,
        "the DNS credential is not understood: it is not the JSON of a credential"
    );
    rig.clock.advance(3_600);
    rig.credential(r#"{"provider":"route53","kind":"bearer","token":"t"}"#);
    assert_eq!(rig.renewer.tick().await, Tick::Failed);
    assert_eq!(
        rig.status.snapshot().failing.expect("failing").message,
        "no DNS provider called route53"
    );
    assert!(rig.ca.requests().is_empty());

    let rig = configured();
    let other = MdmSetup::new(
        Arc::clone(&rig.store) as Arc<dyn SetupStore>,
        Some(MasterKey::from_bytes(&[0x44; 32]).expect("a key")),
        crate::enrollment::os_random(),
    );
    other
        .put_secret(
            SecretKind::DnsCredential,
            &Secret::new(CREDENTIAL.as_bytes().to_vec()),
            None,
            2,
        )
        .expect("stored");
    assert_eq!(rig.renewer.tick().await, Tick::Failed);
    assert!(
        rig.status
            .snapshot()
            .failing
            .expect("failing")
            .message
            .contains("setup could not be read")
    );
}

/// Proves: a store that fails when the account, the key or the certificate is stored fails the
/// attempt, and a CA that issues a chain that is not a certificate fails it too, in each case
/// with the report in the status.
#[tokio::test]
async fn a_store_or_a_chain_that_fails_fails_the_attempt() {
    for (puts, text) in [
        (0, "the MDM setup store failed"),
        (1, "the MDM setup store failed"),
        (2, "the MDM setup store failed"),
    ] {
        let rig = configured();
        rig.store.puts_left.store(puts, Ordering::SeqCst);
        assert_eq!(rig.renewer.tick().await, Tick::Failed, "{puts} puts");
        let message = rig.status.snapshot().failing.expect("failing").message;
        assert!(message.contains(text), "{message}");
        assert!(rig.dns.values(TXT).is_empty());
    }
    let rig = configured();
    rig.ca.be(Mood::GarbageChain);
    assert_eq!(rig.renewer.tick().await, Tick::Failed);
    assert_eq!(
        rig.status.snapshot().failing.expect("failing").message,
        "the certificate the CA issued is not usable: its first certificate cannot be read"
    );
    assert_eq!(rig.version(SecretKind::TlsCert), 0);
}

/// Proves: an attempt in progress makes a tick that finds it do nothing.
///
/// Catches: two orders for one name at once, which the CA's duplicate-certificate limit counts.
#[tokio::test]
async fn one_attempt_runs_at_a_time() {
    let rig = configured();
    let held = rig.renewer.attempt.lock().await;
    assert_eq!(rig.renewer.tick().await, Tick::Busy);
    assert!(rig.ca.requests().is_empty());
    drop(held);
    assert_eq!(rig.renewer.tick().await, Tick::Renewed);
}

/// Proves: a renewed certificate is served by the listener's resolver on the next handshake with
/// no restart: a connection made after the renewal, to the same `MdmTls`, completes against the
/// roots of the CA and presents the leaf the renewal stored (a version 2 that is not version 1),
/// while the resolver gave a different configuration before it.
///
/// Catches: a listener that keeps the certificate it started with, which would let the name's
/// certificate expire under a running coordinator.
#[tokio::test]
async fn a_renewed_certificate_is_served_on_the_next_handshake() {
    let rig = configured();
    let tls = MdmTls::new(Arc::clone(&rig.setup), Arc::new(Vec::new));
    assert!(tls.config().is_none(), "no certificate is held yet");
    assert_eq!(rig.renewer.tick().await, Tick::Renewed);
    let first = tls.config().expect("the first certificate is served");
    let leaf = |bytes: Vec<u8>| leaf_der(&bytes);
    let presented = handshake(&tls, &rig.ca.root_der()).await;
    let stored = rig
        .setup
        .open(SecretKind::TlsCert)
        .expect("opened")
        .expect("held");
    assert_eq!(presented, leaf(stored.expose().to_vec()));

    rig.days_later(61);
    assert_eq!(rig.renewer.tick().await, Tick::Renewed);
    let second = tls.config().expect("the renewed certificate is served");
    assert!(!Arc::ptr_eq(&first, &second));
    let renewed = handshake(&tls, &rig.ca.root_der()).await;
    let stored = rig
        .setup
        .open(SecretKind::TlsCert)
        .expect("opened")
        .expect("held");
    assert_eq!(renewed, leaf(stored.expose().to_vec()));
    assert_ne!(renewed, presented);
}

fn leaf_der(chain: &[u8]) -> Vec<u8> {
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject;
    CertificateDer::pem_slice_iter(chain)
        .next()
        .expect("a certificate")
        .expect("a PEM")
        .to_vec()
}

/// The leaf a client sees when it connects to `tls` trusting `root`.
async fn handshake(tls: &MdmTls, root: &[u8]) -> Vec<u8> {
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(root.to_vec()))
        .expect("a root");
    let client = Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(roots)
        .with_no_client_auth(),
    );
    let (near, far) = tokio::io::duplex(64 * 1024);
    let acceptor = TlsAcceptor::from(tls.config().expect("a configuration"));
    let server = tokio::spawn(async move { acceptor.accept(far).await.map(drop) });
    let stream = TlsConnector::from(client)
        .connect(ServerName::try_from(NAME).expect("a name"), near)
        .await
        .expect("the handshake completes");
    server.await.expect("joined").expect("accepted");
    stream
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|chain| chain.first())
        .map(|cert| cert.to_vec())
        .expect("a certificate")
}

/// Proves: the timer ticks, then sleeps `tls_check_s` seconds (300 until the setting is set, and
/// when the settings cannot be read), and ticks again, for ever.
#[tokio::test]
async fn the_timer_ticks_and_sleeps_by_the_setting() {
    let rig = configured();
    rig.clock.freeze_after(3);
    assert!(
        renew_every(Arc::clone(&rig.renewer))
            .now_or_never()
            .is_none()
    );
    assert_eq!(rig.version(SecretKind::TlsCert), 1);
    assert_eq!(rig.ca.orders(), 1);
    let waits: Vec<u64> = rig
        .clock
        .slept()
        .iter()
        .map(std::time::Duration::as_secs)
        .collect();
    assert!(waits.iter().rev().take(2).all(|seconds| *seconds == 300));

    let rig = configured();
    rig.setup.set_setting("tls_check_s", "120").expect("set");
    rig.clock.freeze_after(2);
    assert!(
        renew_every(Arc::clone(&rig.renewer))
            .now_or_never()
            .is_none()
    );
    let waits: Vec<u64> = rig
        .clock
        .slept()
        .iter()
        .map(std::time::Duration::as_secs)
        .collect();
    assert_eq!(waits.last(), Some(&120));

    let rig = configured();
    rig.store.settings.store(true, Ordering::SeqCst);
    rig.clock.freeze_after(1);
    assert!(
        renew_every(Arc::clone(&rig.renewer))
            .now_or_never()
            .is_none()
    );
    assert_eq!(rig.clock.slept(), [std::time::Duration::from_secs(300)]);
}

/// Proves: the expiry of a chain is that of its first certificate, and none for text that is not
/// one.
#[test]
fn the_expiry_is_read_from_the_first_certificate() {
    let (_, config) = crate::mdm_acme::testkit::tls_server("x.example.net");
    drop(config);
    assert_eq!(leaf_not_after(b"nothing"), None);
    assert_eq!(
        leaf_not_after(b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n"),
        None
    );
}
