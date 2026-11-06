//! The fakes the enroller is driven with: an in-memory coordinator that follows the enrollment
//! contract and issues certificates from a test CA, a wall clock that records who reads it, a
//! monotonic clock that records its sleeps, entropy shared by every enroller of one test, and a
//! store that cannot write the key. Each test owns a scratch directory for its pki directory.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use hostkeeper_config::daemon::{DaemonConfig, Sha256Hex};
use hostkeeper_enroll::{
    Claims, ClaimsError, ClaimsSource, CoordinatorEnroll, EnrollAnswer, EnrollRequest, EnrollState,
    Enroller, Entropy, EntropyError, Error, FaultGate, HostId, Issued, KeyFrom, PkiDir, Poll,
    PollAnswer, Ports, Reason, RenewAnswer, RenewRequest, RequestId, Settings, Site, Store,
    StoreError, StoreLock, Stored, TransportError, WallClock,
};
use hostkeeper_steps::{BootId, BootIdError, Clock};
use hostkeeper_testkit::Scratch;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{DerSignature, SigningKey, VerifyingKey};
use p256::pkcs8::{DecodePublicKey, EncodePrivateKey};
use sha2::{Digest, Sha256};
use x509_cert::builder::profile::BuilderProfile;
use x509_cert::builder::{Builder, CertificateBuilder};
use x509_cert::der::asn1::{ObjectIdentifier, OctetString};
use x509_cert::der::oid::AssociatedOid;
use x509_cert::der::oid::db::rfc5280::ID_KP_CLIENT_AUTH;
use x509_cert::der::pem::{LineEnding, encode_string};
use x509_cert::der::{DateTime, Decode, DecodePem, Encode, EncodePem};
use x509_cert::ext::Extension;
use x509_cert::ext::pkix::{BasicConstraints, ExtendedKeyUsage};
use x509_cert::name::Name;
use x509_cert::request::CertReq;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::{SubjectPublicKeyInfoOwned, SubjectPublicKeyInfoRef};
use x509_cert::time::{Time, Validity};
use x509_cert::{Certificate, TbsCertificate};

/// Seconds in the seven days of a host certificate.
pub const WEEK: u64 = 7 * 24 * 3600;
/// The wall clock when a test starts, in seconds since the epoch.
pub const T0: u64 = 1_900_000_000;
/// The host id the tests' operator approves.
pub const HOST: &str = "web-1";
/// The image the tests' enrollers send.
pub const IMAGE: &str = "image-1";

/// The directory of this test binary's scratch directories.
fn base() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("hostkeeper-enroll")
}

/// A P-256 key from one repeated byte (any byte below 0xff is a valid scalar).
pub fn signing_key(byte: u8) -> SigningKey {
    SigningKey::from_slice(&[byte; 32]).expect("a P-256 scalar")
}

/// `secs` after the epoch as a certificate time.
pub fn time(secs: u64) -> Time {
    Time::from(DateTime::from_unix_duration(Duration::from_secs(secs)).expect("a date"))
}

/// The lower-case hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// `der` as a PEM certificate.
pub fn pem(der: &[u8]) -> String {
    encode_string("CERTIFICATE", LineEnding::LF, der).expect("PEM")
}

/// The pin of a daemon config whose `ca_sha256` is the SHA-256 of `ca_der`, read by the real
/// config loader.
pub fn pin_of(ca_der: &[u8]) -> Sha256Hex {
    let text = format!(
        r#"coordinator     = "https://coordinator.example"
ca_sha256       = "{}"
release_channel = "https://releases.example"

[trusted_signers]
organisation = "example-org"
workflow     = ".github/workflows/release.yml"
refs         = ["refs/tags/*"]
hosted_runner = true
"#,
        sha256_hex(ca_der)
    );
    DaemonConfig::parse(&text)
        .expect("the daemon config")
        .ca_sha256
}

/// The extension holding `value`.
pub fn extension<T: Encode + AssociatedOid>(value: &T) -> Extension {
    Extension {
        extn_id: T::OID,
        critical: false,
        extn_value: OctetString::new(value.to_der().expect("DER")).expect("an octet string"),
    }
}

/// `extension`, marked critical.
pub fn critical(mut extension: Extension) -> Extension {
    extension.critical = true;
    extension
}

/// The extended key usage `usages`.
pub fn eku(usages: &[ObjectIdentifier]) -> Extension {
    extension(&ExtendedKeyUsage(usages.to_vec()))
}

struct Profile {
    subject: Name,
    issuer: Name,
    extensions: Vec<Extension>,
}

impl BuilderProfile for Profile {
    fn get_issuer(&self, _subject: &Name) -> Name {
        self.issuer.clone()
    }

    fn get_subject(&self) -> Name {
        self.subject.clone()
    }

    fn build_extensions(
        &self,
        _spk: SubjectPublicKeyInfoRef<'_>,
        _issuer_spk: SubjectPublicKeyInfoRef<'_>,
        _tbs: &TbsCertificate,
    ) -> x509_cert::builder::Result<Vec<Extension>> {
        Ok(self.extensions.clone())
    }
}

/// A certificate for `spki`, signed by `signer`.
fn certificate(
    signer: &SigningKey,
    subject: &str,
    issuer: &Name,
    extensions: Vec<Extension>,
    window: (u64, u64),
    spki: SubjectPublicKeyInfoOwned,
) -> Certificate {
    let profile = Profile {
        subject: Name::from_str(subject).expect("a subject"),
        issuer: issuer.clone(),
        extensions,
    };
    let validity = Validity::new(time(window.0), time(window.1));
    CertificateBuilder::new(profile, SerialNumber::from(7u32), validity, spki)
        .expect("a builder")
        .build::<_, DerSignature>(signer)
        .expect("a certificate")
}

/// A test CA: a P-256 key and its self-signed certificate.
pub struct Ca {
    pub key: SigningKey,
    pub name: Name,
    pub der: Vec<u8>,
    pub pem: String,
}

impl Ca {
    /// The CA of key byte `byte`.
    pub fn new(byte: u8) -> Ca {
        let key = signing_key(byte);
        let subject = format!("CN=hostkeeper test CA {byte}");
        let name = Name::from_str(&subject).expect("a name");
        let spki = SubjectPublicKeyInfoOwned::from_key(key.verifying_key()).expect("SPKI");
        let ca = BasicConstraints {
            ca: true,
            path_len_constraint: None,
        };
        let cert = certificate(
            &key,
            &subject,
            &name,
            vec![extension(&ca)],
            (T0 - WEEK, T0 + 400 * WEEK),
            spki,
        );
        let der = cert.to_der().expect("DER");
        Ca {
            pem: pem(&der),
            key,
            name,
            der,
        }
    }

    /// A PEM certificate this CA signs for `spki`, with `subject` (an RFC 4514 name), the
    /// `extensions` and the validity `window`.
    pub fn leaf(
        &self,
        spki: &SubjectPublicKeyInfoOwned,
        subject: &str,
        extensions: Vec<Extension>,
        window: (u64, u64),
    ) -> String {
        let cert = certificate(
            &self.key,
            subject,
            &self.name,
            extensions,
            window,
            spki.clone(),
        );
        cert.to_pem(LineEnding::LF).expect("PEM")
    }

    /// [`Ca::leaf`], naming `issuer` (an RFC 4514 name) as its issuer although this CA signs it.
    pub fn leaf_with_issuer(
        &self,
        spki: &SubjectPublicKeyInfoOwned,
        subject: &str,
        issuer: &str,
        extensions: Vec<Extension>,
        window: (u64, u64),
    ) -> String {
        let issuer = Name::from_str(issuer).expect("an issuer");
        let cert = certificate(
            &self.key,
            subject,
            &issuer,
            extensions,
            window,
            spki.clone(),
        );
        cert.to_pem(LineEnding::LF).expect("PEM")
    }
}

/// The SubjectPublicKeyInfo of `key`.
pub fn spki_of(key: &SigningKey) -> SubjectPublicKeyInfoOwned {
    SubjectPublicKeyInfoOwned::from_key(key.verifying_key()).expect("SPKI")
}

/// The DER SubjectPublicKeyInfo of a PKCS#10 request, after checking that the request is signed
/// by its own key.
pub fn csr_spki(csr_der: &[u8]) -> Vec<u8> {
    let request = CertReq::from_der(csr_der).expect("a PKCS#10 request");
    let spki = request.info.public_key.to_der().expect("DER");
    let key = VerifyingKey::from_public_key_der(&spki).expect("a P-256 key");
    let signature = DerSignature::from_bytes(request.signature.raw_bytes()).expect("a signature");
    key.verify(&request.info.to_der().expect("DER"), &signature)
        .expect("the request is signed by its key");
    spki
}

/// What an issue hook is given.
pub struct Args {
    pub spki: SubjectPublicKeyInfoOwned,
    pub host_id: String,
    pub now: u64,
}

/// Makes the certificate the coordinator sends.
pub type Issue = Box<dyn FnMut(&Ca, &Args) -> Issued + Send>;

/// Sees each `Enroll` request as it arrives.
pub type OnEnroll = Box<dyn FnMut(&EnrollRequest) + Send>;

/// What a well-behaved coordinator issues: a seven-day certificate from now for the request's
/// key, subject `host:<id>`, client authentication, and the CA as the bundle.
pub fn proper(ca: &Ca, args: &Args) -> Issued {
    Issued {
        host_id: args.host_id.clone(),
        cert_pem: ca.leaf(
            &args.spki,
            &format!("CN=host:{}", args.host_id),
            vec![eku(&[ID_KP_CLIENT_AUTH])],
            (args.now, args.now + WEEK),
        ),
        ca_bundle_pem: ca.pem.clone(),
    }
}

/// One request the coordinator holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub spki_der: Vec<u8>,
    pub state: RowState,
    pub renewal_of: Option<HostId>,
    pub issued: Option<Issued>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowState {
    Pending,
    /// Approved as this host id.
    Approved(String),
    Denied(String),
}

/// The wall clock's time and who read it.
#[derive(Debug, Default)]
pub struct Wall {
    pub now: u64,
    pub sites: Vec<&'static str>,
}

/// One call of the enrollment API, for a failure injected at it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Enroll,
    UnsignedPoll,
    SignedPoll,
    Renew,
}

/// The bytes a poll signs, built here from the contract's literal bytes rather than with the
/// crate's `poll_message`, so a change to one side is caught by the other.
pub fn contract_poll_message(request_id: &RequestId, nonce: &[u8]) -> Vec<u8> {
    let mut message = b"hostkeeper-enroll-poll-v1\x00".to_vec();
    message.extend_from_slice(request_id.as_str().as_bytes());
    message.push(0);
    message.extend_from_slice(nonce);
    message
}

/// The `not_after` of a PEM certificate, `0` when it does not parse.
fn not_after(pem: &str) -> u64 {
    Certificate::from_pem(pem.as_bytes()).map_or(0, |cert| {
        cert.tbs_certificate()
            .validity()
            .not_after
            .to_unix_duration()
            .as_secs()
    })
}

/// The in-memory coordinator.
pub struct Coordinator {
    pub ca: Ca,
    /// The CA certificate (DER) presented on connection.
    pub presented: Vec<u8>,
    wall: Arc<Mutex<Wall>>,
    pub enrolls: Vec<EnrollRequest>,
    pub polls: Vec<Poll>,
    pub renews: Vec<RenewRequest>,
    pub rows: BTreeMap<RequestId, Row>,
    nonces: Vec<Vec<u8>>,
    next_nonce: u64,
    /// The length of the nonces it gives.
    pub nonce_len: usize,
    /// Every call fails with this transport error, before anything is done.
    pub down: Option<String>,
    /// The next call of this operation does its work and then loses its reply.
    pub lose_reply: Option<Op>,
    /// Called with each `Enroll` request before it is answered.
    pub on_enroll: Option<OnEnroll>,
    /// Answers `Enroll` with this request id instead of the request's.
    pub answer_id: Option<RequestId>,
    /// Answers an unsigned poll with this instead of a nonce.
    pub unsigned_answer: Option<PollAnswer>,
    /// Answers a signed poll with this, once its signature checked.
    pub signed_answer: Option<PollAnswer>,
    /// Refuses `Renew` with this reason.
    pub renew_refusal: Option<Reason>,
    /// Makes the certificates; [`proper`] when `None`.
    pub issue: Option<Issue>,
}

impl Coordinator {
    fn up(&self) -> Result<(), TransportError> {
        match &self.down {
            Some(why) => Err(TransportError(why.clone())),
            None => Ok(()),
        }
    }

    /// `answer`, unless the reply of `op` is to be lost.
    fn reply<T>(&mut self, op: Op, answer: T) -> Result<T, TransportError> {
        if self.lose_reply == Some(op) {
            self.lose_reply = None;
            return Err(TransportError(format!("the reply to {op:?} was lost")));
        }
        Ok(answer)
    }

    fn now(&self) -> u64 {
        self.wall.lock().expect("wall").now
    }

    fn issued(&mut self, spki_der: &[u8], host_id: &str) -> Issued {
        let args = Args {
            spki: SubjectPublicKeyInfoOwned::from_der(spki_der).expect("SPKI"),
            host_id: host_id.to_owned(),
            now: self.now(),
        };
        match self.issue.as_mut() {
            Some(issue) => issue(&self.ca, &args),
            None => proper(&self.ca, &args),
        }
    }

    /// Approves every pending request as `host_id`.
    pub fn approve(&mut self, host_id: &str) {
        for row in self.rows.values_mut() {
            if row.state == RowState::Pending {
                row.state = RowState::Approved(host_id.to_owned());
            }
        }
    }

    /// Denies every pending request with `reason`.
    pub fn deny(&mut self, reason: &str) {
        for row in self.rows.values_mut() {
            if row.state == RowState::Pending {
                row.state = RowState::Denied(reason.to_owned());
            }
        }
    }
}

/// The handle an enroller holds.
pub struct Handle(Arc<Mutex<Coordinator>>);

impl CoordinatorEnroll for Handle {
    fn presented_ca_der(&mut self) -> Result<Vec<u8>, TransportError> {
        let c = self.0.lock().expect("coordinator");
        c.up()?;
        Ok(c.presented.clone())
    }

    fn enroll(&mut self, request: &EnrollRequest) -> Result<EnrollAnswer, TransportError> {
        let mut guard = self.0.lock().expect("coordinator");
        let c = &mut *guard;
        c.up()?;
        c.enrolls.push(request.clone());
        if let Some(hook) = c.on_enroll.as_mut() {
            hook(request);
        }
        let now = c.now();
        let spki_der = csr_spki(&request.csr_der);
        let id = RequestId::of_spki(&spki_der);
        let row = c.rows.entry(id.clone()).or_insert_with(|| Row {
            spki_der,
            state: RowState::Pending,
            renewal_of: None,
            issued: None,
        });
        // A return (`renewal_of`) opens a new episode only when the certificate last issued for
        // the key has expired; a repost within an episode keeps its approval.
        let expired = row
            .issued
            .as_ref()
            .is_some_and(|issued| not_after(&issued.cert_pem) < now);
        if request.renewal_of.is_some() && expired {
            row.state = RowState::Pending;
            row.issued = None;
            row.renewal_of = request.renewal_of.clone();
        }
        let state = match &row.state {
            RowState::Pending => EnrollState::Pending,
            RowState::Approved(_) => EnrollState::Approved,
            RowState::Denied(reason) => EnrollState::Denied(Reason::new(reason).expect("a token")),
        };
        let answer = EnrollAnswer {
            request_id: c.answer_id.clone().unwrap_or(id),
            state,
        };
        c.reply(Op::Enroll, answer)
    }

    fn poll(&mut self, poll: &Poll) -> Result<PollAnswer, TransportError> {
        let mut guard = self.0.lock().expect("coordinator");
        let c = &mut *guard;
        c.up()?;
        c.polls.push(poll.clone());
        if poll.signature.is_empty() {
            if let Some(answer) = c.unsigned_answer.clone() {
                return Ok(answer);
            }
            c.next_nonce += 1;
            let counter = c.next_nonce.to_be_bytes();
            let nonce: Vec<u8> = (0..c.nonce_len).map(|i| counter[i % 8]).collect();
            c.nonces.push(nonce.clone());
            return c.reply(Op::UnsignedPoll, PollAnswer::Challenge(nonce));
        }
        let Some(at) = c.nonces.iter().position(|n| *n == poll.nonce) else {
            return Err(TransportError(
                "a nonce this coordinator did not give, or gave once".into(),
            ));
        };
        c.nonces.remove(at);
        let row = c
            .rows
            .get(&poll.request_id)
            .cloned()
            .ok_or_else(|| TransportError("no such request".into()))?;
        let key = VerifyingKey::from_public_key_der(&row.spki_der).expect("a P-256 key");
        let signature = DerSignature::from_bytes(&poll.signature).expect("a DER signature");
        key.verify(
            &contract_poll_message(&poll.request_id, &poll.nonce),
            &signature,
        )
        .map_err(|_| TransportError("the poll is not signed by the request key".into()))?;
        if let Some(answer) = c.signed_answer.clone() {
            return Ok(answer);
        }
        let answer = match row.state {
            RowState::Pending => PollAnswer::Pending,
            RowState::Denied(reason) => PollAnswer::Denied(Reason::new(&reason).expect("a token")),
            RowState::Approved(host_id) => {
                let issued = match row.issued {
                    Some(issued) => issued,
                    None => c.issued(&row.spki_der, &host_id),
                };
                if let Some(row) = c.rows.get_mut(&poll.request_id) {
                    row.issued = Some(issued.clone());
                }
                PollAnswer::Issued(issued)
            }
        };
        c.reply(Op::SignedPoll, answer)
    }

    fn renew(&mut self, request: &RenewRequest) -> Result<RenewAnswer, TransportError> {
        let mut guard = self.0.lock().expect("coordinator");
        let c = &mut *guard;
        c.up()?;
        c.renews.push(request.clone());
        let enrolled = c
            .rows
            .values()
            .find(|row| row.state == RowState::Approved(request.host_id.as_str().to_owned()))
            .map(|row| row.spki_der.clone())
            .ok_or_else(|| TransportError("no such host".into()))?;
        if csr_spki(&request.csr_der) != enrolled {
            return Err(TransportError("not signed by the enrolled key".into()));
        }
        if let Some(reason) = c.renew_refusal.clone() {
            return Ok(RenewAnswer::Refused(reason));
        }
        let issued = c.issued(&enrolled, request.host_id.as_str());
        c.reply(Op::Renew, RenewAnswer::Issued(issued))
    }
}

/// The wall clock an enroller holds.
pub struct FakeWall(Arc<Mutex<Wall>>);

impl WallClock for FakeWall {
    fn now_unix_secs(&self, site: Site) -> u64 {
        let mut wall = self.0.lock().expect("wall");
        wall.sites.push(site.name());
        wall.now
    }
}

/// The monotonic clock an enroller holds: it records each sleep and never waits.
pub struct FakeClock(Arc<Mutex<Vec<u64>>>);

impl Clock for FakeClock {
    fn mono_secs(&self) -> u64 {
        self.0.lock().expect("clock").iter().sum()
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        BootId::new("the fake clock", "boot")
    }

    fn sleep_secs(&self, secs: u64) {
        self.0.lock().expect("clock").push(secs);
    }
}

/// Entropy shared by every enroller of a test: each draw is 32 copies of the next byte, so a key
/// made again is a different key.
pub struct SharedEntropy(Arc<Mutex<u8>>);

impl Entropy for SharedEntropy {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), EntropyError> {
        let mut next = self.0.lock().expect("entropy");
        *next += 1;
        buf.fill(*next);
        Ok(())
    }
}

/// Entropy that gives `draws` in turn, then fails.
pub struct ScriptedEntropy {
    pub draws: Vec<u8>,
    pub calls: Arc<Mutex<usize>>,
}

impl Entropy for ScriptedEntropy {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), EntropyError> {
        let mut calls = self.calls.lock().expect("calls");
        let draw = self.draws.get(*calls).copied();
        *calls += 1;
        match draw {
            Some(byte) => {
                buf.fill(byte);
                Ok(())
            }
            None => Err(EntropyError("no more draws".into())),
        }
    }
}

/// Claims a test can change between steps, as a host's interfaces or DMI data can change.
pub struct SharedClaims(Arc<Mutex<Claims>>);

impl ClaimsSource for SharedClaims {
    fn claims(&self) -> Result<Claims, ClaimsError> {
        Ok(self.0.lock().expect("claims").clone())
    }
}

/// The claims of the tests' host.
pub fn claims() -> Claims {
    Claims {
        machine_id: Some("0123456789abcdef0123456789abcdef".into()),
        product_serial: None,
        product_uuid: Some("4c4c4544-0000-1000-8000-000000000001".into()),
        network_addresses: vec!["02:00:00:00:00:01".into()],
        mac_serial: None,
        platform_uuid: None,
        bmc_mac: None,
    }
}

/// The pki directory, except that writing the pending key fails, as a failed fsync would.
pub struct KeyWriteFails(pub PkiDir);

impl Store for KeyWriteFails {
    fn open(&self) -> Result<StoreLock, StoreError> {
        self.0.open()
    }

    fn pending_key(&self) -> Result<Option<Vec<u8>>, StoreError> {
        self.0.pending_key()
    }

    fn write_pending_key(&self, _pkcs8_der: &[u8]) -> Result<(), StoreError> {
        Err(StoreError::Io(io::Error::other("fsync failed")))
    }

    fn current(&self) -> Result<Option<Stored>, StoreError> {
        self.0.current()
    }

    fn write_generation(
        &self,
        cert_pem: &str,
        ca_pem: &str,
        key: KeyFrom,
    ) -> Result<Stored, StoreError> {
        self.0.write_generation(cert_pem, ca_pem, key)
    }

    fn prune(&self, used: u64) -> Result<(), StoreError> {
        self.0.prune(used)
    }
}

/// One test's coordinator, clocks, entropy and pki directory.
pub struct World {
    pub dir: Scratch,
    pub coordinator: Arc<Mutex<Coordinator>>,
    pub wall: Arc<Mutex<Wall>>,
    pub sleeps: Arc<Mutex<Vec<u64>>>,
    entropy: Arc<Mutex<u8>>,
    pub claims: Arc<Mutex<Claims>>,
    pub image: Mutex<String>,
    pub pin: Sha256Hex,
}

impl World {
    /// A coordinator presenting its CA, a daemon config pinning it, the wall clock at `T0`.
    pub fn new(name: &str) -> World {
        let ca = Ca::new(0x70);
        let pin = pin_of(&ca.der);
        let wall = Arc::new(Mutex::new(Wall {
            now: T0,
            sites: Vec::new(),
        }));
        let coordinator = Coordinator {
            presented: ca.der.clone(),
            ca,
            wall: Arc::clone(&wall),
            enrolls: Vec::new(),
            polls: Vec::new(),
            renews: Vec::new(),
            rows: BTreeMap::new(),
            nonces: Vec::new(),
            next_nonce: 0,
            nonce_len: 32,
            down: None,
            lose_reply: None,
            on_enroll: None,
            answer_id: None,
            unsigned_answer: None,
            signed_answer: None,
            renew_refusal: None,
            issue: None,
        };
        World {
            dir: Scratch::under(&base(), name).expect("a scratch directory"),
            coordinator: Arc::new(Mutex::new(coordinator)),
            wall,
            sleeps: Arc::new(Mutex::new(Vec::new())),
            entropy: Arc::new(Mutex::new(0)),
            claims: Arc::new(Mutex::new(claims())),
            image: Mutex::new(IMAGE.to_owned()),
            pin,
        }
    }

    /// The pki directory.
    pub fn pki(&self) -> PathBuf {
        self.dir.path().join("pki")
    }

    pub fn coordinator(&self) -> MutexGuard<'_, Coordinator> {
        self.coordinator.lock().expect("coordinator")
    }

    pub fn set_now(&self, now: u64) {
        self.wall.lock().expect("wall").now = now;
    }

    pub fn sites(&self) -> Vec<&'static str> {
        self.wall.lock().expect("wall").sites.clone()
    }

    pub fn sleeps(&self) -> Vec<u64> {
        self.sleeps.lock().expect("clock").clone()
    }

    pub fn settings(&self) -> Settings {
        Settings {
            ca_sha256: self.pin.clone(),
            image: self.image.lock().expect("image").clone(),
        }
    }

    /// The pki directory as the enroller's store, failing where `gate` says.
    pub fn store_with_faults(&self, gate: FaultGate) -> Box<dyn Store> {
        Box::new(PkiDir::new(&self.pki()).with_faults(gate))
    }

    /// An enroller over the pki directory that fails where `gate` says.
    pub fn open_with_faults(&self, gate: FaultGate) -> Enroller {
        let ports = self.ports_with(Some(self.store_with_faults(gate)), None);
        Enroller::open(self.settings(), ports).expect("open the enroller")
    }

    /// The ports of an enroller over the pki directory, with `store` and `entropy` in place of
    /// the real directory and the shared entropy when given.
    pub fn ports_with(
        &self,
        store: Option<Box<dyn Store>>,
        entropy: Option<Box<dyn Entropy>>,
    ) -> Ports {
        Ports {
            coordinator: Box::new(Handle(Arc::clone(&self.coordinator))),
            store: store.unwrap_or_else(|| Box::new(PkiDir::new(&self.pki()))),
            claims: Box::new(SharedClaims(Arc::clone(&self.claims))),
            entropy: entropy.unwrap_or_else(|| Box::new(SharedEntropy(Arc::clone(&self.entropy)))),
            wall: Box::new(FakeWall(Arc::clone(&self.wall))),
            clock: Box::new(FakeClock(Arc::clone(&self.sleeps))),
        }
    }

    pub fn try_open(&self) -> Result<Enroller, Error> {
        Enroller::open(self.settings(), self.ports_with(None, None))
    }

    pub fn open(&self) -> Enroller {
        self.try_open().expect("open the enroller")
    }

    /// An enroller that has its first certificate, issued at the wall clock's time.
    pub fn enrolled(&self) -> Enroller {
        let mut enroller = self.open();
        assert_eq!(
            enroller.run_once().expect("post"),
            hostkeeper_enroll::Outcome::Pending
        );
        self.coordinator().approve(HOST);
        let outcome = enroller.run_once().expect("enroll");
        assert!(
            matches!(outcome, hostkeeper_enroll::Outcome::Enrolled(_)),
            "{outcome:?}"
        );
        enroller
    }

    /// The target of `current`, `None` when there is no link.
    pub fn current(&self) -> Option<PathBuf> {
        std::fs::read_link(self.pki().join("current")).ok()
    }

    /// The names in the pki directory, sorted.
    pub fn names(&self) -> Vec<String> {
        names(&self.pki())
    }
}

/// The names in `dir`, sorted.
pub fn names(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .expect("read the directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .into_string()
                .expect("UTF-8")
        })
        .collect();
    out.sort();
    out
}

/// The PKCS#8 DER of `key`.
pub fn pkcs8(key: &SigningKey) -> Vec<u8> {
    key.to_pkcs8_der().expect("PKCS#8").as_bytes().to_vec()
}

/// Writes `bytes` to `path` with mode 0600, as the daemon writes a key.
pub fn write_private(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .expect("create");
    file.write_all(bytes).expect("write");
}
