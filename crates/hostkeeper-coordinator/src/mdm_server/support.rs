//! Builders for the tests of the MDM server: a rig with the tables, a setup holding the certificate
//! of the name and the push identity, a manual clock, a fake APNs, a CA that issues the Macs'
//! identities, and a fake Mac that dials the listener in memory.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_mdm_gate::{Command, CommandUuid, EnrollmentId};
use hostkeeper_mdm_push::clock::Clock;
use hostkeeper_mdm_push::request::pkcs1_pem;
use hostkeeper_mdm_push::testkit::{FakeApns, fixture_key, push_certificate_pem};
use hostkeeper_mdm_push::testkit::{der, oid_der};
use hostkeeper_testkit::Scratch;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, PKCS_RSA_SHA256, SanType,
};
use ring::digest;
use rsa::Pkcs1v15Sign;
use rsa::pkcs8::EncodePrivateKey as _;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use sha2::{Digest as _, Sha256};
use tokio::io::DuplexStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use super::auth::fingerprint;
use super::pushing::{SenderFactory, SetupPushers};
use super::queue::{MdmQueue, Pushers};
use super::tls::{MdmTls, Trust};
use super::{MdmServer, Parts};
use crate::host_groups_support::coordinator_listed;
use crate::mdm_secrets::{MasterKey, Secret, SecretKind};
use crate::mdm_setup::MdmSetup;
use crate::service::Coordinator;
use crate::store::Fake;
use crate::store::mdm::files::FileMdm;
use crate::store::mdm::{
    AbmBatch, AbmDevice, AbmState, Answer, Answered, Challenge, Command as Row, Consumed, Enqueued,
    Enrollment, Identity, Issuance, MdmError, MdmStore, NewCommand,
};
use crate::testutil::scratch;

/// The push topic of the test certificate.
pub(super) const TOPIC: &str = "com.apple.mgmt.External.d1801ae1-3ef7-483e-9f6d-4979e2ebfc09";

/// The serial of the Mac the tests enroll.
pub(super) const SERIAL: &str = "C02TESTSERIAL";

/// The UDID of that Mac.
pub(super) const UDID: &str = "00008103-001A2B3C4D5E6F70";

/// The time the rigs start at, in unix seconds.
pub(super) const NOW: u64 = 1_800_000_000;

/// A clock that moves when a test moves it: the time of the queue and of the pusher.
#[derive(Debug)]
pub(super) struct Time(AtomicU64);

impl Time {
    pub(super) fn at(now: u64) -> Self {
        Self(AtomicU64::new(now))
    }

    pub(super) fn advance(&self, seconds: u64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl UnixClock for Time {
    fn now_unix(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

impl Clock for Time {
    fn now_unix(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// The tables with a switch: a call of the method named in [`Flaky::fail`] fails with a store
/// error, any other is the real table's.
pub(super) struct Flaky {
    inner: FileMdm,
    failing: Mutex<Option<&'static str>>,
    not_live: std::sync::atomic::AtomicBool,
}

impl Flaky {
    fn new(inner: FileMdm) -> Self {
        Self {
            inner,
            failing: Mutex::new(None),
            not_live: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// From now on the method `name` fails; `None` mends it.
    pub(super) fn fail(&self, name: Option<&'static str>) {
        *self.failing.lock().unwrap_or_else(PoisonError::into_inner) = name;
    }

    /// From now on `set_push` finds the enrollment not live.
    pub(super) fn end_enrollments_before_set_push(&self) {
        self.not_live.store(true, Ordering::SeqCst);
    }

    fn check(&self, name: &'static str) -> Result<(), MdmError> {
        match *self.failing.lock().unwrap_or_else(PoisonError::into_inner) {
            Some(failing) if failing == name => Err(MdmError::Store(format!("{name} failed"))),
            _ => Ok(()),
        }
    }
}

macro_rules! delegate {
    ($(fn $name:ident(&self $(, $arg:ident: $ty:ty)*) -> $ret:ty;)*) => {
        $(fn $name(&self $(, $arg: $ty)*) -> $ret {
            self.check(stringify!($name))?;
            self.inner.$name($($arg),*)
        })*
    };
}

impl MdmStore for Flaky {
    fn set_push(
        &self,
        enrollment_id: &str,
        token: &[u8],
        magic: &str,
        topic: &str,
    ) -> Result<bool, MdmError> {
        self.check("set_push")?;
        if self.not_live.load(Ordering::SeqCst) {
            // The enrollment ended between the read and this write.
            return Ok(false);
        }
        self.inner.set_push(enrollment_id, token, magic, topic)
    }

    delegate! {
        fn replace_live(&self, new: &Enrollment) -> Result<Vec<String>, MdmError>;
        fn check_out(&self, enrollment_id: &str, now_unix: i64) -> Result<bool, MdmError>;
        fn record_push(&self, enrollment_id: &str, now_unix: i64) -> Result<(), MdmError>;
        fn record_contact(&self, enrollment_id: &str, now_unix: i64) -> Result<(), MdmError>;
        fn enrollment(&self, enrollment_id: &str) -> Result<Option<Enrollment>, MdmError>;
        fn live_enrollment(&self, serial: &str) -> Result<Option<Enrollment>, MdmError>;
        fn issue_challenge(&self, challenge: &Challenge) -> Result<(), MdmError>;
        fn consume_challenge(&self, challenge_sha256: &str, key_sha256: &str, issuance: &Issuance, now_unix: i64) -> Result<Consumed, MdmError>;
        fn challenge(&self, challenge_sha256: &str) -> Result<Option<Identity>, MdmError>;
        fn identity(&self, cert_sha256: &str) -> Result<Option<Identity>, MdmError>;
        fn kill_identity(&self, cert_sha256: &str) -> Result<bool, MdmError>;
        fn enqueue(&self, command: &NewCommand) -> Result<Enqueued, MdmError>;
        fn claim(&self, enrollment_id: &str, tried: &[String], now_unix: i64) -> Result<Option<Row>, MdmError>;
        fn answer(&self, enrollment_id: &str, id: &str, answer: Answer, response: Option<&[u8]>, now_unix: i64) -> Result<Answered, MdmError>;
        fn requeue_timed_out(&self, sent_before_unix: i64) -> Result<Vec<(String, String)>, MdmError>;
        fn command(&self, id: &str) -> Result<Option<Row>, MdmError>;
        fn commands(&self, enrollment_id: &str) -> Result<Vec<Row>, MdmError>;
        fn enrollments_with_queued(&self) -> Result<Vec<String>, MdmError>;
        fn abm_apply(&self, batch: &AbmBatch) -> Result<(), MdmError>;
        fn abm_device(&self, serial: &str) -> Result<Option<AbmDevice>, MdmError>;
        fn abm_devices(&self) -> Result<Vec<AbmDevice>, MdmError>;
        fn abm_state(&self) -> Result<Option<AbmState>, MdmError>;
    }
}

/// A CA that issues certificates to the Macs, and the Mac certificates it issued.
pub(super) struct Ca {
    issuer: CertifiedIssuer<'static, KeyPair>,
}

impl Ca {
    pub(super) fn new() -> Self {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let issuer = CertifiedIssuer::self_signed(params, KeyPair::generate().expect("a key"))
            .expect("a CA certificate");
        Self { issuer }
    }

    pub(super) fn der(&self) -> Vec<u8> {
        self.issuer.der().to_vec()
    }

    /// A certificate for the RSA key number `index` of the test binary, with the client-auth use.
    pub(super) fn issue(&self, index: usize, subject: &str) -> Identity2 {
        self.issue_with(index, subject, None)
    }

    /// [`Self::issue`], with the key identifier `ski` in the certificate when it is given.
    pub(super) fn issue_with(&self, index: usize, subject: &str, ski: Option<&[u8]>) -> Identity2 {
        let der = fixture_key(index).to_pkcs8_der().expect("the key encodes");
        let pair = KeyPair::from_pkcs8_der_and_sign_algo(
            &PrivatePkcs8KeyDer::from(der.as_bytes().to_vec()),
            &PKCS_RSA_SHA256,
        )
        .expect("ring takes the key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, subject);
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        if let Some(ski) = ski {
            params
                .custom_extensions
                .push(rcgen::CustomExtension::from_oid_content(
                    &[2, 5, 29, 14],
                    hostkeeper_mdm_push::testkit::der(0x04, ski),
                ));
        }
        let cert = params.signed_by(&pair, &self.issuer).expect("signed");
        Identity2 {
            index,
            der: cert.der().to_vec(),
            key_der: der.as_bytes().to_vec(),
        }
    }
}

/// A Mac's identity: its certificate and the key it is for.
#[derive(Clone)]
pub(super) struct Identity2 {
    pub(super) index: usize,
    pub(super) der: Vec<u8>,
    pub(super) key_der: Vec<u8>,
}

impl Identity2 {
    pub(super) fn sha256(&self) -> String {
        fingerprint(&self.der)
    }
}

/// Builds the gate over the queue, in the rig's scratch directory, by its clock.
type GateBuilder =
    Box<dyn FnOnce(Arc<MdmQueue>, &Scratch, Arc<Time>) -> Arc<hostkeeper_mdm_gate::Gate>>;

/// The tables, the setup, the clock, the fake APNs and the server of one test.
pub(super) struct Rig {
    pub(super) store: Arc<Flaky>,
    pub(super) setup: Arc<MdmSetup>,
    pub(super) time: Arc<Time>,
    pub(super) apns: Arc<FakeApns>,
    pub(super) queue: Arc<MdmQueue>,
    pub(super) server: Arc<MdmServer>,
    pub(super) coordinator: Arc<Coordinator>,
    pub(super) fake: Fake,
    pub(super) mac_ca: Arc<Ca>,
    pub(super) server_ca: Ca,
    dir: Scratch,
}

impl Rig {
    /// A rig with the certificate of the name and the push identity stored, so that both families
    /// are served.
    pub(super) fn new(name: &str) -> Self {
        let rig = Self::bare(name);
        rig.store_tls();
        rig.store_push();
        rig
    }

    /// A rig with nothing in the setup but the master key.
    pub(super) fn bare(name: &str) -> Self {
        Self::build(name, None, counting())
    }

    /// A queue as a restarted coordinator builds it: new pushers and nothing armed, over the same
    /// tables, setup, clock and fake APNs.
    pub(super) fn restarted_queue(&self) -> MdmQueue {
        let apns = Arc::clone(&self.apns);
        let factory: SenderFactory = Box::new(move |_, _| {
            Some(Arc::clone(&apns) as Arc<dyn hostkeeper_mdm_push::apns::PushSender>)
        });
        let clock: Arc<dyn Clock> = Arc::clone(&self.time) as Arc<dyn Clock>;
        let pushing = Arc::new(SetupPushers::new(Arc::clone(&self.setup), clock, factory));
        MdmQueue::new(
            Arc::clone(&self.store) as Arc<dyn MdmStore>,
            Arc::clone(&self.time) as Arc<dyn UnixClock>,
            Arc::new(move || pushing.get()),
            Arc::clone(&self.setup),
            86_400,
        )
    }

    /// A queue as a restarted coordinator builds it before any push certificate is set: nothing
    /// armed, and no pusher to ask.
    pub(super) fn restarted_queue_without_pushers(&self) -> MdmQueue {
        MdmQueue::new(
            Arc::clone(&self.store) as Arc<dyn MdmStore>,
            Arc::clone(&self.time) as Arc<dyn UnixClock>,
            Arc::new(|| None),
            Arc::clone(&self.setup),
            86_400,
        )
    }

    /// A rig whose source of random bytes is `random`, with the certificate and the push identity.
    pub(super) fn with_random(name: &str, random: crate::enrollment::Random) -> Self {
        let rig = Self::build(name, None, random);
        rig.store_tls();
        rig.store_push();
        rig
    }

    /// A rig whose server also has the gate `gate` (built over its queue by the caller's closure).
    pub(super) fn with_gate(
        name: &str,
        gate: impl FnOnce(Arc<MdmQueue>, &Scratch, Arc<Time>) -> Arc<hostkeeper_mdm_gate::Gate>
        + 'static,
    ) -> Self {
        let rig = Self::build(name, Some(Box::new(gate)), counting());
        rig.store_tls();
        rig.store_push();
        rig
    }

    #[allow(
        clippy::type_complexity,
        reason = "a test builder takes a closure that builds the gate over the queue"
    )]
    fn build(name: &str, gate: Option<GateBuilder>, random: crate::enrollment::Random) -> Self {
        let dir = scratch(name);
        let tables = FileMdm::open(&dir.path().join("mdm")).expect("the tables open");
        let store = Arc::new(Flaky::new(tables));
        let setup = Arc::new(
            MdmSetup::volatile().with_key(MasterKey::from_bytes(&[0x21; 32]).expect("a key")),
        );
        let time = Arc::new(Time::at(NOW));
        let apns = Arc::new(FakeApns::new());
        let factory: SenderFactory = {
            let apns = Arc::clone(&apns);
            Box::new(move |_, _| {
                Some(Arc::clone(&apns) as Arc<dyn hostkeeper_mdm_push::apns::PushSender>)
            })
        };
        let clock: Arc<dyn Clock> = Arc::clone(&time) as Arc<dyn Clock>;
        let pushing = Arc::new(SetupPushers::new(Arc::clone(&setup), clock, factory));
        let pushers: Pushers = {
            let pushing = Arc::clone(&pushing);
            Arc::new(move || pushing.get())
        };
        let queue = Arc::new(MdmQueue::new(
            Arc::clone(&store) as Arc<dyn MdmStore>,
            Arc::clone(&time) as Arc<dyn UnixClock>,
            Arc::clone(&pushers),
            Arc::clone(&setup),
            86_400,
        ));
        let mac_ca = Arc::new(Ca::new());
        let trust: Trust = {
            let ca = Arc::clone(&mac_ca);
            Arc::new(move || vec![ca.der()])
        };
        let gate = gate.map(|build| build(Arc::clone(&queue), &dir, Arc::clone(&time)));
        let server = Arc::new(MdmServer::new(Parts {
            store: Arc::clone(&store) as Arc<dyn MdmStore>,
            clock: Arc::clone(&time) as Arc<dyn UnixClock>,
            random,
            gate,
            queue: Arc::clone(&queue),
            pushers: Arc::clone(&pushers),
            tls: Arc::new(MdmTls::new(Arc::clone(&setup), trust)),
            sweep: Duration::from_secs(60),
        }));
        let fake = Fake::new();
        let coordinator = Arc::new(
            coordinator_listed(&fake, &Arc::new(AtomicU64::new(1_000)), None, "")
                .with_shared_mdm_setup(Arc::clone(&setup))
                .with_mdm_server(Some(Arc::clone(&server))),
        );
        Self {
            store,
            setup,
            time,
            apns,
            queue,
            server,
            coordinator,
            fake,
            mac_ca,
            server_ca: Ca::new(),
            dir,
        }
    }

    /// The rig's scratch directory.
    pub(super) fn store_dir(&self) -> std::path::PathBuf {
        self.dir.path().to_path_buf()
    }

    /// Stores the certificate and key of the name `localhost`, signed by the rig's server CA.
    pub(super) fn store_tls(&self) {
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params.subject_alt_names = vec![SanType::DnsName("localhost".try_into().expect("a name"))];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let key = KeyPair::generate().expect("a key");
        let cert = params
            .signed_by(&key, &self.server_ca.issuer)
            .expect("signed");
        self.put(SecretKind::TlsCert, cert.pem().as_bytes());
        self.put(SecretKind::TlsKey, key.serialize_pem().as_bytes());
    }

    /// Stores the push certificate and its key.
    pub(super) fn store_push(&self) {
        let key = fixture_key(0);
        let cert = push_certificate_pem(key, TOPIC, 1_700_000_000, 1_900_000_000);
        self.put(SecretKind::PushCert, cert.as_bytes());
        self.put(SecretKind::PushKey, pkcs1_pem(key).expect("pem").expose());
    }

    pub(super) fn put(&self, kind: SecretKind, bytes: &[u8]) {
        self.setup
            .put_secret(
                kind,
                &Secret::new(bytes.to_vec()),
                None,
                i64::try_from(NOW).unwrap_or(0),
            )
            .expect("stored");
    }

    /// An identity for `serial` and `udid`, as the SCEP step leaves it: issued for `mac`.
    pub(super) fn issue(&self, mac: &Identity2, serial: &str, udid: &str, confirmed: bool) {
        let challenge = Sha256::digest(format!("challenge-{}-{serial}", mac.index));
        let challenge = hex::encode(challenge);
        let now = i64::try_from(NOW).unwrap_or(0);
        self.store
            .issue_challenge(&Challenge {
                challenge_sha256: challenge.clone(),
                serial: serial.to_owned(),
                udid: udid.to_owned(),
                expires_unix: now + 3600,
                confirmed,
            })
            .expect("a challenge");
        let issuance = Issuance {
            cert_sha256: mac.sha256(),
            cert_der: mac.der.clone(),
            not_after_unix: now + 1_000_000,
        };
        let key = hex::encode(Sha256::digest(&mac.key_der));
        assert_eq!(
            self.store
                .consume_challenge(&challenge, &key, &issuance, now)
                .expect("consumed"),
            Consumed::Issued
        );
    }

    /// The live enrollment `id` of `serial`, with a push token when `token` is set.
    pub(super) fn live(&self, id: &str, serial: &str, token: bool) -> Enrollment {
        let mut enrollment = Enrollment::new(id, serial, UDID, i64::try_from(NOW).unwrap_or(0));
        if token {
            enrollment.push_token = Some(vec![9; 32]);
            enrollment.push_magic = Some("magic".to_owned());
            enrollment.topic = Some(TOPIC.to_owned());
        }
        self.store.replace_live(&enrollment).expect("entered");
        enrollment
    }

    /// Queues the `DeviceInformation` number `n` for the enrollment `id` through the gate's port.
    pub(super) async fn enqueue(
        &self,
        id: &str,
        n: u8,
    ) -> Result<hostkeeper_mdm_gate::Queued, hostkeeper_mdm_gate::EnqueueError> {
        use hostkeeper_mdm_gate::CommandQueue as _;
        let command = Command::device_information(CommandUuid::from_random([n; 16]));
        self.queue.enqueue(&enrollment_id(id), &command).await
    }

    /// The id of the command `enqueue(_, n)` queued.
    pub(super) fn command_id(n: u8) -> String {
        CommandUuid::from_random([n; 16]).as_str().to_owned()
    }

    /// The serial in the ABM table.
    pub(super) fn list_in_abm(&self, serial: &str) {
        self.store
            .abm_apply(&AbmBatch {
                upserts: vec![AbmDevice {
                    serial: serial.to_owned(),
                    model: None,
                    description: None,
                    profile_status: "pushed".to_owned(),
                    profile_uuid: None,
                    first_seen_unix: 1,
                    synced_unix: 1,
                    removed_unix: None,
                }],
                removed: Vec::new(),
                removed_unix: 0,
                state: None,
            })
            .expect("the ABM table takes the device");
    }

    /// The audit actions so far, oldest first.
    pub(super) fn audited(&self) -> Vec<String> {
        self.coordinator
            .ops()
            .state()
            .audit
            .iter()
            .map(|entry| entry.action.clone())
            .collect()
    }
}

/// Random bytes that count: 1, 2, 3, ... in each byte, so two ids differ.
pub(crate) fn counting() -> crate::enrollment::Random {
    let next = AtomicU64::new(1);
    Box::new(move |buffer| {
        let n = next.fetch_add(1, Ordering::SeqCst);
        for (i, byte) in buffer.iter_mut().enumerate() {
            *byte = u8::try_from((n + i as u64) % 251).unwrap_or(0);
        }
        true
    })
}

/// A command for the gate's queue, of the type `request_type` (a type off the list is refused).
pub(super) fn gate_command(n: u8, request_type: &str) -> Command {
    let uuid = CommandUuid::from_random([n; 16]);
    Command::new(request_type, uuid, plist::Dictionary::new())
}

/// The enrollment id `text`.
pub(super) fn enrollment_id(text: &str) -> EnrollmentId {
    EnrollmentId::parse(text).expect("an enrollment id")
}

/// A fake Mac's connection to the listener: in memory, TLS, HTTP/1.1.
pub(super) struct Session {
    send: hyper::client::conn::http1::SendRequest<Full<Bytes>>,
}

impl Session {
    /// A connection from a client that offers `identity` (none: no certificate) to the rig's listener.
    pub(super) async fn dial(rig: &Rig, identity: Option<&Identity2>) -> Option<Self> {
        let config = rig.server.tls().config()?;
        let (client, server) = tokio::io::duplex(256 * 1024);
        tokio::spawn(crate::listen::connection_with(
            server,
            config,
            crate::http::Surface::Mdm,
            Arc::clone(&rig.coordinator),
            Duration::from_secs(10),
        ));
        let tls = connector(rig, identity)
            .connect(ServerName::try_from("localhost").expect("a name"), client)
            .await
            .ok()?;
        Some(Self::over(tls).await)
    }

    async fn over(tls: TlsStream<DuplexStream>) -> Self {
        let (send, driver) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .expect("an HTTP/1.1 handshake");
        tokio::spawn(driver);
        Self { send }
    }

    /// `PUT path` with `body` and the `headers`: the status and the body of the answer.
    pub(super) async fn put(
        &mut self,
        path: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> (StatusCode, Vec<u8>) {
        self.request("PUT", path, headers, body).await
    }

    /// `method path` with `body` and the `headers`.
    pub(super) async fn request(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> (StatusCode, Vec<u8>) {
        self.try_request(method, path, headers, body)
            .await
            .expect("an answer")
    }

    /// [`Self::request`], or the error of a connection the server ended.
    pub(super) async fn try_request(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> Result<(StatusCode, Vec<u8>), hyper::Error> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost");
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        let request = request
            .body(Full::new(Bytes::copy_from_slice(body)))
            .expect("a request");
        let response = self.send.send_request(request).await?;
        let status = response.status();
        let bytes = response.into_body().collect().await?.to_bytes();
        Ok((status, bytes.to_vec()))
    }
}

/// A TLS client that trusts the rig's server CA and offers `identity` when it is given.
pub(super) fn connector(rig: &Rig, identity: Option<&Identity2>) -> TlsConnector {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(rig.server_ca.der()))
        .expect("a root");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(roots);
    let mut config = match identity {
        Some(mac) => builder
            .with_client_auth_cert(
                vec![CertificateDer::from(mac.der.clone())],
                PrivatePkcs8KeyDer::from(mac.key_der.clone()).into(),
            )
            .expect("a client identity"),
        None => builder.with_no_client_auth(),
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    TlsConnector::from(Arc::new(config))
}

/// A handshake to the rig's listener by a client whose certificate `resolver` decides.
pub(super) async fn dial_resolving(
    rig: &Rig,
    resolver: Arc<dyn rustls::client::ResolvesClientCert>,
) -> Option<TlsStream<DuplexStream>> {
    let config = rig.server.tls().config()?;
    let (client, server) = tokio::io::duplex(256 * 1024);
    tokio::spawn(crate::listen::connection_with(
        server,
        config,
        crate::http::Surface::Mdm,
        Arc::clone(&rig.coordinator),
        Duration::from_secs(10),
    ));
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(rig.server_ca.der()))
        .expect("a root");
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_root_certificates(roots)
        .with_client_cert_resolver(resolver);
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("localhost").expect("a name"), client)
        .await
        .ok()
}

/// The DER of the certificate a new handshake to the rig's listener is served.
pub(super) async fn served_leaf(rig: &Rig) -> Vec<u8> {
    let tls = connector(rig, None)
        .connect(ServerName::try_from("localhost").expect("a name"), {
            let config = rig.server.tls().config().expect("a configuration");
            let (client, server) = tokio::io::duplex(256 * 1024);
            tokio::spawn(crate::listen::connection_with(
                server,
                config,
                crate::http::Surface::Mdm,
                Arc::clone(&rig.coordinator),
                Duration::from_secs(10),
            ));
            client
        })
        .await
        .expect("a handshake");
    tls.get_ref().1.peer_certificates().expect("a certificate")[0].to_vec()
}

/// The plist of a check-in `message` of the device `UDID`, with `fields` beside it.
pub(super) fn checkin(message: &str, fields: &[(&str, plist::Value)]) -> Vec<u8> {
    let mut dict = plist::Dictionary::new();
    dict.insert("MessageType".to_owned(), message.into());
    dict.insert("UDID".to_owned(), UDID.into());
    dict.insert("Topic".to_owned(), TOPIC.into());
    for (key, value) in fields {
        dict.insert((*key).to_owned(), value.clone());
    }
    let mut out = Vec::new();
    plist::Value::Dictionary(dict)
        .to_writer_xml(&mut out)
        .expect("a plist");
    out
}

/// The `Authenticate` of the Mac `SERIAL`.
pub(super) fn authenticate(serial: &str) -> Vec<u8> {
    checkin("Authenticate", &[("SerialNumber", serial.into())])
}

/// The `TokenUpdate` with a 32-byte token and the magic `magic`.
pub(super) fn token_update(magic: &str) -> Vec<u8> {
    checkin(
        "TokenUpdate",
        &[
            ("Token", plist::Value::Data(vec![9; 32])),
            ("PushMagic", magic.into()),
        ],
    )
}

/// A result posted to `/mdm`: `status` for `command`.
pub(super) fn result(status: &str, command: Option<&str>) -> Vec<u8> {
    result_with(status, command, &[])
}

/// [`result`], with `fields` beside the status (a command's answer keys).
pub(super) fn result_with(
    status: &str,
    command: Option<&str>,
    fields: &[(&str, plist::Value)],
) -> Vec<u8> {
    let mut dict = plist::Dictionary::new();
    for (key, value) in fields {
        dict.insert((*key).to_owned(), value.clone());
    }
    dict.insert("Status".to_owned(), status.into());
    dict.insert("UDID".to_owned(), UDID.into());
    if let Some(command) = command {
        dict.insert("CommandUUID".to_owned(), command.into());
    }
    let mut out = Vec::new();
    plist::Value::Dictionary(dict)
        .to_writer_xml(&mut out)
        .expect("a plist");
    out
}

/// The command in the plist an `/mdm` answer carries.
pub(super) fn command_in(body: &[u8]) -> hostkeeper_mdm_server::CommandRequest {
    hostkeeper_mdm_server::CommandRequest::parse(body).expect("a command")
}

// The CMS messages of a client that signs its messages.
pub(super) const SHA1: &str = "1.3.14.3.2.26";
pub(super) const SHA256: &str = "2.16.840.1.101.3.4.2.1";
pub(super) const SHA384: &str = "2.16.840.1.101.3.4.2.2";
pub(super) const SHA512: &str = "2.16.840.1.101.3.4.2.3";
pub(super) const RSA: &str = "1.2.840.113549.1.1.1";
pub(super) const SHA256_RSA: &str = "1.2.840.113549.1.1.11";
pub(super) const BODY: &[u8] = b"<plist>a check-in</plist>";

/// The DER prefix of a `DigestInfo` for the digest `oid`, and the digest algorithm.
pub(super) fn digest_info(oid: &str) -> (&'static [u8], &'static digest::Algorithm) {
    match oid {
        SHA1 => (
            &[
                0x30, 0x21, 0x30, 0x09, 0x06, 0x05, 0x2b, 0x0e, 0x03, 0x02, 0x1a, 0x05, 0x00, 0x04,
                0x14,
            ],
            &digest::SHA1_FOR_LEGACY_USE_ONLY,
        ),
        SHA256 => (
            &[
                0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x01, 0x05, 0x00, 0x04, 0x20,
            ],
            &digest::SHA256,
        ),
        SHA384 => (
            &[
                0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x02, 0x05, 0x00, 0x04, 0x30,
            ],
            &digest::SHA384,
        ),
        _ => (
            &[
                0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x03, 0x05, 0x00, 0x04, 0x40,
            ],
            &digest::SHA512,
        ),
    }
}

/// How a message is built.
#[derive(Clone)]
pub(super) struct Spec {
    pub(super) digest: &'static str,
    pub(super) signature: &'static str,
    pub(super) attributes: bool,
    pub(super) ski: Option<Vec<u8>>,
    /// The digest in the attributes is of this instead of the body, when set.
    pub(super) claimed: Option<Vec<u8>>,
    pub(super) leave_out_digest_attribute: bool,
    pub(super) certificates: usize,
    pub(super) names_other_serial: bool,
    pub(super) indefinite_attributes: bool,
    pub(super) signers: usize,
    pub(super) sid_tag: Option<u8>,
    pub(super) flip_signature: bool,
    pub(super) drop_certificates: bool,
    /// Keeps only this many of the six fields of the signer's info, when set.
    pub(super) info_fields: Option<usize>,
    /// Names the signer by its issuer alone, without a serial number.
    pub(super) short_sid: bool,
}

impl Default for Spec {
    fn default() -> Self {
        Self {
            digest: SHA256,
            signature: RSA,
            attributes: true,
            ski: None,
            claimed: None,
            leave_out_digest_attribute: false,
            certificates: 1,
            names_other_serial: false,
            indefinite_attributes: false,
            signers: 1,
            sid_tag: None,
            flip_signature: false,
            drop_certificates: false,
            info_fields: None,
            short_sid: false,
        }
    }
}

fn null() -> Vec<u8> {
    vec![0x05, 0x00]
}

fn algorithm(oid: &str) -> Vec<u8> {
    der(0x30, &[oid_der(oid), null()].concat())
}

/// The cms message for `body` signed by `mac`, built as `spec` says.
pub(super) fn message(body: &[u8], mac: &Identity2, spec: &Spec) -> Vec<u8> {
    let key = fixture_key(mac.index);
    let (prefix, algorithm_of) = digest_info(spec.digest);
    let body_digest = digest::digest(algorithm_of, body).as_ref().to_vec();
    let claimed = spec.claimed.clone().unwrap_or_else(|| body_digest.clone());
    let attribute =
        |oid: &str, value: Vec<u8>| der(0x30, &[oid_der(oid), der(0x31, &value)].concat());
    let mut attributes = vec![attribute(
        "1.2.840.113549.1.9.3",
        oid_der("1.2.840.113549.1.7.1"),
    )];
    if !spec.leave_out_digest_attribute {
        attributes.push(attribute("1.2.840.113549.1.9.4", der(0x04, &claimed)));
    }
    let attributes = attributes.concat();
    let signed_over = if spec.attributes {
        der(0x31, &attributes)
    } else {
        body.to_vec()
    };
    let hashed = digest::digest(algorithm_of, &signed_over);
    let mut to_sign = prefix.to_vec();
    to_sign.extend_from_slice(hashed.as_ref());
    let mut signature = key
        .sign(Pkcs1v15Sign::new_unprefixed(), &to_sign)
        .expect("signs");
    if spec.flip_signature {
        signature[0] ^= 0xff;
    }
    let (_, parsed) = x509_parser::parse_x509_certificate(&mac.der).expect("a certificate");
    let sid = match (&spec.ski, spec.sid_tag) {
        (_, Some(tag)) => der(tag, b"x"),
        (Some(ski), None) => der(0x80, ski),
        (None, None) => {
            let issuer = parsed.tbs_certificate.issuer.as_raw().to_vec();
            let serial = if spec.names_other_serial {
                vec![0x7f, 1, 2, 3]
            } else {
                parsed.raw_serial().to_vec()
            };
            if spec.short_sid {
                der(0x30, &issuer)
            } else {
                der(0x30, &[issuer, der(0x02, &serial)].concat())
            }
        }
    };
    let attributes_field = if !spec.attributes {
        Vec::new()
    } else if spec.indefinite_attributes {
        hostkeeper_mdm_push::testkit::indefinite(0xa0, &attributes)
    } else {
        der(0xa0, &attributes)
    };
    let mut fields = vec![
        der(0x02, &[1]),
        sid,
        algorithm(spec.digest),
        attributes_field,
        algorithm(spec.signature),
        der(0x04, &signature),
    ];
    if let Some(keep) = spec.info_fields {
        fields.truncate(keep);
    }
    let info = der(0x30, &fields.concat());
    let infos = vec![info; spec.signers].concat();
    let certificates = if spec.drop_certificates {
        Vec::new()
    } else {
        der(0xa0, &vec![mac.der.clone(); spec.certificates].concat())
    };
    let signed = der(
        0x30,
        &[
            der(0x02, &[1]),
            der(0x31, &algorithm(spec.digest)),
            der(0x30, &oid_der("1.2.840.113549.1.7.1")),
            certificates,
            der(0x31, &infos),
        ]
        .concat(),
    );
    der(
        0x30,
        &[oid_der("1.2.840.113549.1.7.2"), der(0xa0, &signed)].concat(),
    )
}

pub(super) fn header(body: &[u8], mac: &Identity2, spec: &Spec) -> String {
    STANDARD.encode(message(body, mac, spec))
}
