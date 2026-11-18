//! The fakes the keeper of service identities is driven with: a coordinator that signs a service
//! certificate from a test CA (or answers as a test says), an injected wall clock and monotonic
//! clock that never wait, entropy that gives a new key for each draw, a host's trust that a test
//! sets, and the real identity directory in a scratch directory owned by the test's own user.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hostkeeper_config::names::ServiceName;
use hostkeeper_enroll::{
    Entropy, EntropyError, FilesError, HostId, IdentityDir, IdentityFile, IdentityFiles,
    IdentityPorts, IdentityState, IssueAnswer, Owner, ServiceCertIssuer, ServiceIdentities, Site,
    TransportError, Trust, WallClock, Wanted,
};
use hostkeeper_steps::{BootId, BootIdError, Clock};
use hostkeeper_testkit::Scratch;
use p256::ecdsa::SigningKey;
use p256::pkcs8::DecodePrivateKey;
use x509_cert::builder::profile::BuilderProfile;
use x509_cert::builder::{Builder, CertificateBuilder};
use x509_cert::der::asn1::{Ia5String, ObjectIdentifier, OctetString};
use x509_cert::der::oid::AssociatedOid;
use x509_cert::der::oid::db::rfc5280::{ID_KP_CLIENT_AUTH, ID_KP_SERVER_AUTH};
use x509_cert::der::pem::{LineEnding, encode_string};
use x509_cert::der::{DateTime, Decode, Encode};
use x509_cert::ext::Extension;
use x509_cert::ext::pkix::name::GeneralName;
use x509_cert::ext::pkix::{
    BasicConstraints, ExtendedKeyUsage, KeyUsage, KeyUsages, SubjectAltName,
};
use x509_cert::name::Name;
use x509_cert::request::CertReq;
use x509_cert::serial_number::SerialNumber;
use x509_cert::spki::{SubjectPublicKeyInfoOwned, SubjectPublicKeyInfoRef};
use x509_cert::time::{Time, Validity};
use x509_cert::{Certificate, TbsCertificate};

/// The wall clock when a test starts, in seconds since the epoch.
pub const T0: u64 = 1_900_000_000;
pub const HOUR: u64 = 3600;
/// The host id of the tests.
pub const HOST: &str = "web-1";
pub const SERVICE: &str = "js-agent";

pub const CLIENT_AUTH: ObjectIdentifier = ID_KP_CLIENT_AUTH;
pub const SERVER_AUTH: ObjectIdentifier = ID_KP_SERVER_AUTH;

/// The test's own user and group: who the files of a test belong to, and who "root" is.
pub fn me() -> Owner {
    Owner {
        uid: rustix::process::geteuid().as_raw(),
        gid: rustix::process::getegid().as_raw(),
    }
}

pub fn scratch(name: &str) -> Scratch {
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("hostkeeper-enroll-identity");
    Scratch::under(&base, name).expect("a scratch directory")
}

pub fn wanted(service: &str) -> Wanted {
    Wanted {
        service: ServiceName::new(service).expect("a service name"),
        account: Ok(me()),
    }
}

fn time(secs: u64) -> Time {
    Time::from(DateTime::from_unix_duration(Duration::from_secs(secs)).expect("a date"))
}

fn extension<T: Encode + AssociatedOid>(value: &T, critical: bool) -> Extension {
    Extension {
        extn_id: T::OID,
        critical,
        extn_value: OctetString::new(value.to_der().expect("DER")).expect("an octet string"),
    }
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

fn signing_key(byte: u8) -> SigningKey {
    SigningKey::from_slice(&[byte; 32]).expect("a P-256 scalar")
}

/// A test CA: a P-256 key and its self-signed certificate.
pub struct Ca {
    key: SigningKey,
    name: Name,
    pub pem: String,
}

impl Ca {
    /// The CA of key byte `byte`.
    pub fn new(byte: u8) -> Ca {
        let key = signing_key(byte);
        let name = Name::from_str(&format!("CN=test CA {byte}")).expect("a name");
        let spki = SubjectPublicKeyInfoOwned::from_key(key.verifying_key()).expect("SPKI");
        let ca = BasicConstraints {
            ca: true,
            path_len_constraint: None,
        };
        let certificate = build(
            &key,
            &name,
            &name,
            vec![extension(&ca, true)],
            (T0 - HOUR, T0 + 1000 * HOUR),
            spki,
        );
        let der = certificate.to_der().expect("DER");
        Ca {
            pem: encode_string("CERTIFICATE", LineEnding::LF, &der).expect("PEM"),
            key,
            name,
        }
    }
}

impl Ca {
    /// A certificate with `subject` (an RFC 4514 name) for a key of no one's, signed by this CA.
    pub fn leaf_pem(&self, subject: &str) -> String {
        let key = signing_key(7);
        let spki = SubjectPublicKeyInfoOwned::from_key(key.verifying_key()).expect("SPKI");
        let subject = Name::from_str(subject).expect("a subject");
        let certificate = build(
            &self.key,
            &subject,
            &self.name,
            Vec::new(),
            (T0 - HOUR, T0 + 1000 * HOUR),
            spki,
        );
        let der = certificate.to_der().expect("DER");
        encode_string("CERTIFICATE", LineEnding::LF, &der).expect("PEM")
    }
}

fn build(
    signer: &SigningKey,
    subject: &Name,
    issuer: &Name,
    extensions: Vec<Extension>,
    window: (u64, u64),
    spki: SubjectPublicKeyInfoOwned,
) -> Certificate {
    let profile = Profile {
        subject: subject.clone(),
        issuer: issuer.clone(),
        extensions,
    };
    let validity = Validity::new(time(window.0), time(window.1));
    CertificateBuilder::new(profile, SerialNumber::from(7u32), validity, spki)
        .expect("a builder")
        .build::<_, p256::ecdsa::DerSignature>(signer)
        .expect("a certificate")
}

/// What a service certificate is made of; [`Leaf::good`] is the certificate the coordinator
/// issues, and a test changes the one thing it is about.
#[derive(Clone)]
pub struct Leaf {
    pub subject: String,
    pub uri: String,
    pub dns: String,
    /// Extra names after the two.
    pub extra_names: Vec<GeneralName>,
    /// `None`: no extended key usage.
    pub ekus: Option<Vec<ObjectIdentifier>>,
    pub window: (u64, u64),
    pub is_ca: bool,
    pub digital_signature: bool,
    /// An extension this daemon does not read, marked critical.
    pub critical_unknown: bool,
    /// An extended key usage extension whose value is not one.
    pub garbled_eku: bool,
    /// The key to certify instead of the request's.
    pub other_key: Option<SubjectPublicKeyInfoOwned>,
}

impl Leaf {
    /// The certificate for `service` on `host` that the coordinator issues at `now`.
    pub fn good(service: &str, host: &str, now: u64) -> Leaf {
        Leaf {
            subject: format!("CN=service:{service}"),
            uri: format!("urn:hostkeeper:service:{service}"),
            dns: host.to_owned(),
            extra_names: Vec::new(),
            ekus: Some(vec![CLIENT_AUTH]),
            window: (now, now + 8 * HOUR),
            is_ca: false,
            digital_signature: true,
            critical_unknown: false,
            garbled_eku: false,
            other_key: None,
        }
    }

    /// The DER of this certificate for the key of `csr_der`, signed by `ca`.
    pub fn sign(&self, ca: &Ca, csr_der: &[u8]) -> Vec<u8> {
        let spki = self
            .other_key
            .clone()
            .unwrap_or_else(|| CertReq::from_der(csr_der).expect("a CSR").info.public_key);
        let mut names = vec![
            GeneralName::UniformResourceIdentifier(Ia5String::new(&self.uri).expect("a URI")),
            GeneralName::DnsName(Ia5String::new(&self.dns).expect("a DNS name")),
        ];
        names.extend(self.extra_names.clone());
        let mut extensions = vec![
            extension(&SubjectAltName(names), false),
            extension(
                &BasicConstraints {
                    ca: self.is_ca,
                    path_len_constraint: None,
                },
                true,
            ),
        ];
        if self.digital_signature {
            extensions.push(extension(
                &KeyUsage(KeyUsages::DigitalSignature.into()),
                true,
            ));
        } else {
            extensions.push(extension(
                &KeyUsage(KeyUsages::KeyEncipherment.into()),
                true,
            ));
        }
        if let Some(ekus) = &self.ekus {
            extensions.push(extension(&ExtendedKeyUsage(ekus.clone()), false));
        }
        if self.garbled_eku {
            extensions.push(Extension {
                extn_id: ExtendedKeyUsage::OID,
                critical: false,
                extn_value: OctetString::new(vec![5, 0]).expect("an octet string"),
            });
        }
        if self.critical_unknown {
            extensions.push(Extension {
                extn_id: ObjectIdentifier::new_unwrap("1.3.6.1.4.1.99999.1"),
                critical: true,
                extn_value: OctetString::new(vec![5, 0]).expect("an octet string"),
            });
        }
        let subject = Name::from_str(&self.subject).expect("a subject");
        build(&ca.key, &subject, &ca.name, extensions, self.window, spki)
            .to_der()
            .expect("DER")
    }
}

/// The `(service, csr_der)` of each call the coordinator received.
pub type Calls = Vec<(String, Vec<u8>)>;

type Answer = Box<dyn FnMut(&str, &[u8]) -> Result<IssueAnswer, TransportError> + Send>;

struct Coordinator {
    calls: Mutex<Calls>,
    answer: Mutex<Answer>,
}

/// The coordinator as the keeper sees it: it counts its calls and answers as set.
#[derive(Clone)]
pub struct Issuer(Arc<Coordinator>);

impl Issuer {
    /// A coordinator whose CA is `ca`, signing for `host` at the wall clock `wall`, as the real
    /// one does.
    fn proper(ca: Arc<Ca>, host: &'static str, wall: Arc<AtomicU64>) -> Issuer {
        let answer: Answer = Box::new(move |service, csr| {
            let leaf = Leaf::good(service, host, wall.load(Ordering::SeqCst));
            Ok(IssueAnswer::Issued {
                cert_der: leaf.sign(&ca, csr),
                ca_bundle_pem: ca.pem.clone(),
            })
        });
        Issuer(Arc::new(Coordinator {
            calls: Mutex::new(Vec::new()),
            answer: Mutex::new(answer),
        }))
    }

    /// From now on, answers with `answer`.
    pub fn answer_with(
        &self,
        answer: impl FnMut(&str, &[u8]) -> Result<IssueAnswer, TransportError> + Send + 'static,
    ) {
        *self.0.answer.lock().expect("answer") = Box::new(answer);
    }

    pub fn calls(&self) -> Calls {
        self.0.calls.lock().expect("calls").clone()
    }
}

impl ServiceCertIssuer for Issuer {
    fn issue(&mut self, service: &str, csr_der: &[u8]) -> Result<IssueAnswer, TransportError> {
        self.0
            .calls
            .lock()
            .expect("calls")
            .push((service.to_owned(), csr_der.to_vec()));
        (self.0.answer.lock().expect("answer"))(service, csr_der)
    }
}

/// The wall clock of a test: a number a test sets, and the names of the sites that read it.
#[derive(Clone)]
pub struct Wall {
    now: Arc<AtomicU64>,
    sites: Arc<Mutex<Vec<&'static str>>>,
}

impl WallClock for Wall {
    fn now_unix_secs(&self, site: Site) -> u64 {
        self.sites.lock().expect("sites").push(site.name());
        self.now.load(Ordering::SeqCst)
    }
}

/// The monotonic clock of a test: a sleep advances it and is recorded; nothing waits.
#[derive(Clone, Default)]
pub struct Mono {
    now: Arc<AtomicU64>,
    sleeps: Arc<Mutex<Vec<u64>>>,
}

impl Clock for Mono {
    fn mono_secs(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        BootId::new("the test clock", "boot")
    }

    fn sleep_secs(&self, secs: u64) {
        self.sleeps.lock().expect("sleeps").push(secs);
        self.now.fetch_add(secs, Ordering::SeqCst);
    }
}

/// Entropy that gives 32 copies of a new byte at each draw, so each key made is a new key.
#[derive(Clone, Default)]
pub struct Draws(Arc<Mutex<u8>>);

impl Draws {
    /// How many times random bytes were drawn.
    pub fn made(&self) -> u8 {
        *self.0.lock().expect("entropy")
    }
}

impl Entropy for Draws {
    fn fill(&mut self, buf: &mut [u8]) -> Result<(), EntropyError> {
        let mut next = self.0.lock().expect("entropy");
        *next += 1;
        buf.fill(*next);
        Ok(())
    }
}

/// Entropy that has none to give.
pub struct NoEntropy;

impl Entropy for NoEntropy {
    fn fill(&mut self, _buf: &mut [u8]) -> Result<(), EntropyError> {
        Err(EntropyError("no bytes".to_owned()))
    }
}

/// The host's trust, which a test sets.
#[derive(Clone)]
pub struct Host(Arc<Mutex<Result<Trust, String>>>);

impl Host {
    pub fn set(&self, trust: Result<Trust, String>) {
        *self.0.lock().expect("trust") = trust;
    }
}

impl hostkeeper_enroll::HostTrust for Host {
    fn read(&self) -> Result<Trust, String> {
        self.0.lock().expect("trust").clone()
    }
}

/// The identity directory, which fails where a test says.
pub struct Files {
    pub dir: IdentityDir,
    /// The operation and file that fail with an I/O error: `("write", Cert)`.
    pub failing: Arc<Mutex<Option<(&'static str, IdentityFile)>>>,
}

impl Files {
    fn fails(&self, op: &'static str, file: IdentityFile) -> Result<(), FilesError> {
        match *self.failing.lock().expect("failing") {
            Some(point) if point == (op, file) => Err(FilesError::Io {
                path: PathBuf::from(file.name()),
                source: std::io::Error::other(format!("{op} {} failed", file.name())),
            }),
            _ => Ok(()),
        }
    }
}

impl IdentityFiles for Files {
    fn prepare(&self, service: &str, owner: Owner) -> Result<(), FilesError> {
        self.dir.prepare(service, owner)
    }

    fn read(
        &self,
        service: &str,
        file: IdentityFile,
        owner: Owner,
    ) -> Result<Option<Vec<u8>>, FilesError> {
        self.fails("read", file)?;
        self.dir.read(service, file, owner)
    }

    fn write(
        &self,
        service: &str,
        file: IdentityFile,
        owner: Owner,
        bytes: &[u8],
    ) -> Result<(), FilesError> {
        self.fails("write", file)?;
        self.dir.write(service, file, owner, bytes)
    }

    fn remove(&self, service: &str) -> Result<(), FilesError> {
        self.dir.remove(service)
    }
}

/// A host with a keeper of service identities, over the fakes above.
pub struct World {
    pub scratch: Scratch,
    pub ca: Arc<Ca>,
    pub issuer: Issuer,
    pub wall: Wall,
    pub mono: Mono,
    pub host: Host,
    pub draws: Draws,
    pub failing: Failing,
    pub state: IdentityState,
    pub keeper: ServiceIdentities,
}

impl World {
    pub fn new(name: &str) -> World {
        let scratch = scratch(name);
        let ca = Arc::new(Ca::new(1));
        let wall = Wall {
            now: Arc::new(AtomicU64::new(T0)),
            sites: Arc::default(),
        };
        let host = Host(Arc::new(Mutex::new(Ok(Trust {
            host_id: HostId::new(HOST).expect("a host id"),
            ca_bundle_pem: ca.pem.clone(),
        }))));
        let issuer = Issuer::proper(Arc::clone(&ca), HOST, Arc::clone(&wall.now));
        let mono = Mono::default();
        let draws = Draws::default();
        let failing = Arc::default();
        let state = IdentityState::default();
        let keeper = make_keeper(
            &scratch.path().join("identity"),
            (&issuer, &host, &draws, &failing),
            (&wall, &mono),
            &state,
        );
        World {
            scratch,
            ca,
            issuer,
            wall,
            mono,
            host,
            draws,
            failing,
            state,
            keeper,
        }
    }

    /// The identity directory's root.
    pub fn root(&self) -> PathBuf {
        self.scratch.path().join("identity")
    }

    /// A keeper over the same directory, coordinator, clocks and state: the daemon after a
    /// restart.
    pub fn new_keeper(&self) -> ServiceIdentities {
        make_keeper(
            &self.root(),
            (&self.issuer, &self.host, &self.draws, &self.failing),
            (&self.wall, &self.mono),
            &self.state,
        )
    }

    /// The path of a file of a service.
    pub fn path(&self, service: &str, file: IdentityFile) -> PathBuf {
        self.root().join(service).join(file.name())
    }

    pub fn read(&self, service: &str, file: IdentityFile) -> Option<Vec<u8>> {
        std::fs::read(self.path(service, file)).ok()
    }

    pub fn draws_made(&self) -> u8 {
        self.draws.made()
    }

    /// A keeper over the same things whose entropy fails.
    pub fn keeper_without_entropy(&self) -> ServiceIdentities {
        ServiceIdentities::new(
            IdentityPorts {
                issuer: Box::new(self.issuer.clone()),
                files: Box::new(IdentityDir::owned_by(&self.root(), me())),
                trust: Box::new(self.host.clone()),
                entropy: Box::new(NoEntropy),
                wall: Box::new(self.wall.clone()),
                clock: Box::new(self.mono.clone()),
            },
            self.state.clone(),
        )
    }

    pub fn set_wall(&self, secs: u64) {
        self.wall.now.store(secs, Ordering::SeqCst);
    }

    pub fn advance_mono(&self, secs: u64) {
        self.mono.now.fetch_add(secs, Ordering::SeqCst);
    }

    pub fn sleeps(&self) -> Vec<u64> {
        self.mono.sleeps.lock().expect("sleeps").clone()
    }

    pub fn wall_sites(&self) -> Vec<&'static str> {
        self.wall.sites.lock().expect("sites").clone()
    }

    pub fn fail(&self, op: &'static str, file: IdentityFile) {
        *self.failing.lock().expect("failing") = Some((op, file));
    }

    pub fn last_error(&self, service: &str) -> Option<String> {
        self.report(service).1
    }

    /// `(not_after, last_error)` of a service's report.
    pub fn report(&self, service: &str) -> (Option<u64>, Option<String>) {
        let reports = self.state.get();
        let found = reports.iter().find(|r| r.service == service);
        let found = found.expect("the service has a report");
        (found.not_after, found.last_error.clone())
    }
}

type Failing = Arc<Mutex<Option<(&'static str, IdentityFile)>>>;

fn make_keeper(
    root: &Path,
    (issuer, host, draws, failing): (&Issuer, &Host, &Draws, &Failing),
    (wall, mono): (&Wall, &Mono),
    state: &IdentityState,
) -> ServiceIdentities {
    ServiceIdentities::new(
        IdentityPorts {
            issuer: Box::new(issuer.clone()),
            files: Box::new(Files {
                dir: IdentityDir::owned_by(root, me()),
                failing: Arc::clone(failing),
            }),
            trust: Box::new(host.clone()),
            entropy: Box::new(draws.clone()),
            wall: Box::new(wall.clone()),
            clock: Box::new(mono.clone()),
        },
        state.clone(),
    )
}

/// The key a service's `key.pem` holds.
pub fn key_of(pem: &[u8]) -> SigningKey {
    SigningKey::from_pkcs8_pem(std::str::from_utf8(pem).expect("UTF-8")).expect("a PKCS#8 key")
}

/// The certificate a `cert.pem` holds.
pub fn certificate_of(pem: &[u8]) -> Certificate {
    use x509_cert::der::DecodePem;
    Certificate::from_pem(pem).expect("a PEM certificate")
}

/// `(not_before, not_after)` of a certificate.
pub fn window_of(cert: &Certificate) -> (u64, u64) {
    let validity = cert.tbs_certificate().validity();
    (
        validity.not_before.to_unix_duration().as_secs(),
        validity.not_after.to_unix_duration().as_secs(),
    )
}
