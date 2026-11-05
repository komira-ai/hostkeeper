//! What the tests of this crate share: a fixed clock, scratch directories, host keys and their
//! CSRs, a CA, and a tracing subscriber that captures events.

#![allow(
    dead_code,
    reason = "each test binary uses a part of the support module"
)]

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_enrollment::ca::{Ca, CaFiles, P256Key};
use hostkeeper_enrollment::enroll::{Claims, Decision, NewRequest};
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::names::{HostId, KeyHash, Role, ServiceName};
use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_testkit::Scratch;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PublicKeyData};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair};

/// The time every test starts at, in unix seconds.
pub const NOW: u64 = 1_800_000_000;

/// A clock that reads what the test sets.
#[derive(Debug)]
pub struct FixedClock(AtomicU64);

impl FixedClock {
    pub fn at(now: u64) -> Arc<FixedClock> {
        Arc::new(FixedClock(AtomicU64::new(now)))
    }

    pub fn set(&self, now: u64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl UnixClock for FixedClock {
    fn now_unix(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// A scratch directory under the target directory, removed when dropped.
pub fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// A CA made in its own scratch directory, at [`NOW`].
pub struct TestCa {
    pub dir: Scratch,
    pub ca: Ca,
    pub clock: Arc<FixedClock>,
}

impl TestCa {
    pub fn new(name: &str) -> TestCa {
        let dir = scratch(name);
        let clock = FixedClock::at(NOW);
        let ca = Ca::init(&RealFs, files(dir.path()), clock.clone()).expect("make the CA");
        TestCa { dir, ca, clock }
    }
}

/// The key and certificate paths of a CA in `dir`.
pub fn files(dir: &Path) -> CaFiles<'static> {
    let key: &'static Path = Box::leak(dir.join("ca.key").into_boxed_path());
    let certificate: &'static Path = Box::leak(dir.join("ca.cert").into_boxed_path());
    CaFiles { key, certificate }
}

/// A host's P-256 key, as a daemon generates it.
pub struct HostKey {
    pub pair: KeyPair,
}

impl HostKey {
    pub fn new() -> HostKey {
        HostKey {
            pair: KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("a P-256 key"),
        }
    }

    /// A CSR of this key with the subject `CN=<common_name>`.
    pub fn csr(&self, common_name: &str) -> Vec<u8> {
        csr_of(&self.pair, subject(common_name))
    }

    /// The SHA-256 of its SubjectPublicKeyInfo.
    pub fn key_hash(&self) -> KeyHash {
        KeyHash::of_spki(&self.pair.subject_public_key_info())
    }

    pub fn p256(&self) -> P256Key {
        P256Key::from_spki(&self.pair.subject_public_key_info()).expect("a P-256 key")
    }

    /// An ECDSA P-256 / SHA-256 signature (ASN.1 DER) of `message`.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        let rng = SystemRandom::new();
        let pkcs8 = self.pair.serialize_der();
        let signer = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8, &rng)
            .expect("the key as ring reads it");
        signer.sign(&rng, message).expect("sign").as_ref().to_vec()
    }
}

/// Certificate parameters whose subject is `CN=<common_name>` alone.
pub fn subject(common_name: &str) -> CertificateParams {
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, common_name);
    params
}

/// The CSR of `pair` with `params`.
pub fn csr_of(pair: &KeyPair, params: CertificateParams) -> Vec<u8> {
    params
        .serialize_request(pair)
        .expect("a CSR")
        .der()
        .to_vec()
}

/// Claims with `machine_id` and nothing else.
pub fn claims(machine_id: &str) -> Claims {
    Claims {
        machine_id: machine_id.to_owned(),
        ..Claims::default()
    }
}

/// A first enrollment of `key` from machine `machine_id`.
pub fn request(key: &HostKey, machine_id: &str) -> NewRequest {
    NewRequest {
        csr_der: key.csr("host:pending"),
        claims: claims(machine_id),
        image: "hostkeeper-image-1".to_owned(),
        renewal_of: None,
    }
}

/// A renewal of `host` by `key` from machine `machine_id`.
pub fn renewal(key: &HostKey, machine_id: &str, host: &str) -> NewRequest {
    NewRequest {
        renewal_of: Some(host_id(host)),
        ..request(key, machine_id)
    }
}

pub fn host_id(text: &str) -> HostId {
    HostId::parse(text).expect("a host id")
}

/// The decision to approve as `host` with role `web` and service `api`.
pub fn decision(host: &str, replace: bool) -> Decision {
    Decision {
        host_id: host_id(host),
        role: Role::parse("web").expect("a role"),
        services: vec![ServiceName::parse("api").expect("a service name")],
        replace,
    }
}

/// One event a subscriber saw: its level under `level`, and each field by name.
pub type Event = BTreeMap<String, String>;

/// Runs `body` with a subscriber that records every event, and returns the events.
pub fn capture<T>(body: impl FnOnce() -> T) -> (T, Vec<Event>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let out = tracing::subscriber::with_default(Capture(events.clone()), body);
    let seen = events
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    (out, seen)
}

/// An event of `level` with `fields`.
pub fn event(level: &str, fields: &[(&str, &str)]) -> Event {
    let mut out: Event = fields
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    out.insert("level".to_owned(), level.to_owned());
    out
}

struct Capture(Arc<Mutex<Vec<Event>>>);

struct Fields<'a>(&'a mut Event);

impl tracing::field::Visit for Fields<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        let mut text = String::new();
        write!(text, "{value:?}").expect("format a field");
        self.0.insert(field.name().to_owned(), text);
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.to_owned());
    }
}

impl tracing::Subscriber for Capture {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::always()
    }

    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Event::new();
        fields.insert("level".to_owned(), event.metadata().level().to_string());
        event.record(&mut Fields(&mut fields));
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(fields);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}
