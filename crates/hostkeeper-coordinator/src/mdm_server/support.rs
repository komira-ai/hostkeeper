//! Builders for the tests of the MDM server: a rig with the tables, a setup holding the certificate
//! of the name and the push identity, a manual clock, a fake APNs, a CA that issues the Macs'
//! identities, and a fake Mac that dials the listener in memory.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
use rsa09::Pkcs1v15Sign;
use rsa09::pkcs8::EncodePrivateKey as _;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use sha2::{Digest as _, Sha256};
use tokio::io::DuplexStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use super::auth::fingerprint;
pub(crate) use super::flaky::Flaky;
use super::pushing::{SenderFactory, SetupPushers};
use super::queue::{MdmQueue, Pushers};
use super::tls::{MdmTls, Trust};
use super::{MdmServer, Parts};
use crate::host_groups_support::coordinator_listed;
use crate::mdm_lease::Lease;
use crate::mdm_secrets::{MasterKey, Secret, SecretKind};
use crate::mdm_setup::MdmSetup;
use crate::service::Coordinator;
use crate::store::Fake;
use crate::store::mdm::files::FileMdm;
use crate::store::mdm::{AbmBatch, AbmDevice, Challenge, Consumed, Enrollment, Issuance, MdmStore};
use crate::testutil::scratch;

/// The push topic of the test certificate.
pub(crate) const TOPIC: &str = "com.apple.mgmt.External.d1801ae1-3ef7-483e-9f6d-4979e2ebfc09";

/// The serial of the Mac the tests enroll.
pub(crate) const SERIAL: &str = "C02TESTSERIAL";

/// The UDID of that Mac.
pub(crate) const UDID: &str = "00008103-001A2B3C4D5E6F70";

/// The time the rigs start at, in unix seconds.
pub(crate) const NOW: u64 = 1_800_000_000;

/// A clock that moves when a test moves it: the time of the queue and of the pusher.
#[derive(Debug)]
pub(crate) struct Time(AtomicU64);

impl Time {
    pub(crate) fn at(now: u64) -> Self {
        Self(AtomicU64::new(now))
    }

    pub(crate) fn advance(&self, seconds: u64) {
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

/// A CA that issues certificates to the Macs, and the Mac certificates it issued.
pub(crate) struct Ca {
    issuer: CertifiedIssuer<'static, KeyPair>,
}

impl Ca {
    pub(crate) fn new() -> Self {
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

    pub(crate) fn der(&self) -> Vec<u8> {
        self.issuer.der().to_vec()
    }

    /// A certificate for the RSA key number `index` of the test binary, with the client-auth use.
    pub(crate) fn issue(&self, index: usize, subject: &str) -> Identity2 {
        self.issue_with(index, subject, None)
    }

    /// [`Self::issue`], with the key identifier `ski` in the certificate when it is given.
    pub(crate) fn issue_with(&self, index: usize, subject: &str, ski: Option<&[u8]>) -> Identity2 {
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
            key: None,
        }
    }
}

/// A Mac's identity: its certificate and the key it is for.
#[derive(Clone)]
pub(crate) struct Identity2 {
    pub(crate) index: usize,
    pub(crate) der: Vec<u8>,
    pub(crate) key_der: Vec<u8>,
    /// The key to sign with when it is not the fixture key `index`: a Mac that made its own.
    pub(crate) key: Option<rsa09::RsaPrivateKey>,
}

impl Identity2 {
    pub(crate) fn sha256(&self) -> String {
        fingerprint(&self.der)
    }
}

/// Builds the gate over the queue, in the rig's scratch directory, by its clock.
type GateBuilder =
    Box<dyn FnOnce(Arc<MdmQueue>, &Scratch, Arc<Time>) -> Arc<hostkeeper_mdm_gate::Gate>>;

/// The tables, the setup, the clock, the fake APNs and the server of one test.
pub(crate) struct Rig {
    pub(crate) store: Arc<Flaky>,
    pub(crate) setup: Arc<MdmSetup>,
    pub(crate) time: Arc<Time>,
    pub(crate) apns: Arc<FakeApns>,
    pub(crate) queue: Arc<MdmQueue>,
    pub(crate) server: Arc<MdmServer>,
    pub(crate) coordinator: Arc<Coordinator>,
    pub(crate) fake: Fake,
    pub(crate) mac_ca: Arc<Ca>,
    pub(crate) server_ca: Ca,
    dir: Scratch,
}

impl Rig {
    /// A rig with the certificate of the name and the push identity stored, so that both families
    /// are served.
    pub(crate) fn new(name: &str) -> Self {
        let rig = Self::bare(name);
        rig.store_tls();
        rig.store_push();
        rig
    }

    /// A rig with nothing in the setup but the master key.
    pub(crate) fn bare(name: &str) -> Self {
        Self::build(name, None, counting(), None, None)
    }

    /// [`Self::with_apple`] over the setup `setup`, whose store a test makes fail.
    pub(crate) fn with_apple_over(name: &str, roots: Vec<Vec<u8>>, setup: MdmSetup) -> Self {
        let rig = Self::build(name, None, counting(), Some(roots), Some(setup));
        rig.store_tls();
        rig.store_push();
        rig.setup
            .set_setting("mdm_name", "localhost")
            .expect("the name is set");
        rig
    }

    /// [`Self::with_apple`] with a source of random bytes of its own.
    pub(crate) fn with_apple_random(
        name: &str,
        roots: Vec<Vec<u8>>,
        random: crate::enrollment::Random,
    ) -> Self {
        let rig = Self::build(name, None, random, Some(roots), None);
        rig.store_tls();
        rig.store_push();
        rig.setup
            .set_setting("mdm_name", "localhost")
            .expect("the name is set");
        rig
    }

    /// A rig as [`Self::new`] makes it whose device blobs must chain to `roots` (DER), the fake
    /// Apple of a test, and whose name is `localhost`.
    pub(crate) fn with_apple(name: &str, roots: Vec<Vec<u8>>) -> Self {
        let rig = Self::build(name, None, counting(), Some(roots), None);
        rig.store_tls();
        rig.store_push();
        rig.setup
            .set_setting("mdm_name", "localhost")
            .expect("the name is set");
        rig
    }

    /// A queue as a restarted coordinator builds it: new pushers and nothing armed, over the same
    /// tables, setup, clock and fake APNs.
    pub(crate) fn restarted_queue(&self) -> MdmQueue {
        self.queue_under(Lease::new(|| true))
    }

    /// A queue of another node over the same tables, setup, clock and fake APNs, which pushes only
    /// while `lease` is held.
    pub(crate) fn queue_under(&self, lease: Lease) -> MdmQueue {
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
            lease,
            Arc::clone(&self.setup),
            86_400,
        )
    }

    /// A rig whose source of random bytes is `random`, with the certificate and the push identity.
    pub(crate) fn with_random(name: &str, random: crate::enrollment::Random) -> Self {
        let rig = Self::build(name, None, random, None, None);
        rig.store_tls();
        rig.store_push();
        rig
    }

    /// A rig whose server also has the gate `gate` (built over its queue by the caller's closure).
    pub(crate) fn with_gate(
        name: &str,
        gate: impl FnOnce(Arc<MdmQueue>, &Scratch, Arc<Time>) -> Arc<hostkeeper_mdm_gate::Gate>
        + 'static,
    ) -> Self {
        let rig = Self::build(name, Some(Box::new(gate)), counting(), None, None);
        rig.store_tls();
        rig.store_push();
        rig
    }

    #[allow(
        clippy::type_complexity,
        reason = "a test builder takes a closure that builds the gate over the queue"
    )]
    fn build(
        name: &str,
        gate: Option<GateBuilder>,
        random: crate::enrollment::Random,
        roots: Option<Vec<Vec<u8>>>,
        setup: Option<MdmSetup>,
    ) -> Self {
        let dir = scratch(name);
        let tables = FileMdm::open(&dir.path().join("mdm")).expect("the tables open");
        let store = Arc::new(Flaky::new(tables));
        let setup = Arc::new(setup.unwrap_or_else(|| {
            MdmSetup::volatile().with_key(MasterKey::from_bytes(&[0x21; 32]).expect("a key"))
        }));
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
            Lease::new(|| true),
            Arc::clone(&setup),
            86_400,
        ));
        let mac_ca = Arc::new(Ca::new());
        let trust: Trust = {
            let ca = Arc::clone(&mac_ca);
            Arc::new(move || vec![ca.der()])
        };
        let gate = gate.map(|build| build(Arc::clone(&queue), &dir, Arc::clone(&time)));
        let server = MdmServer::new(Parts {
            store: Arc::clone(&store) as Arc<dyn MdmStore>,
            clock: Arc::clone(&time) as Arc<dyn UnixClock>,
            random,
            gate,
            queue: Arc::clone(&queue),
            pushers: Arc::clone(&pushers),
            tls: Arc::new(MdmTls::new(Arc::clone(&setup), trust)),
            sweep: Duration::from_secs(60),
        });
        let server = Arc::new(match roots {
            Some(roots) => server.with_apple_roots(roots),
            None => server,
        });
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
    pub(crate) fn store_dir(&self) -> std::path::PathBuf {
        self.dir.path().to_path_buf()
    }

    /// Stores the certificate and key of the name `localhost`, signed by the rig's server CA.
    pub(crate) fn store_tls(&self) {
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
    pub(crate) fn store_push(&self) {
        let key = fixture_key(0);
        let cert = push_certificate_pem(key, TOPIC, 1_700_000_000, 1_900_000_000);
        self.put(SecretKind::PushCert, cert.as_bytes());
        self.put(SecretKind::PushKey, pkcs1_pem(key).expect("pem").expose());
    }

    pub(crate) fn put(&self, kind: SecretKind, bytes: &[u8]) {
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
    pub(crate) fn issue(&self, mac: &Identity2, serial: &str, udid: &str, confirmed: bool) {
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
    pub(crate) fn live(&self, id: &str, serial: &str, token: bool) -> Enrollment {
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
    pub(crate) async fn enqueue(
        &self,
        id: &str,
        n: u8,
    ) -> Result<hostkeeper_mdm_gate::Queued, hostkeeper_mdm_gate::EnqueueError> {
        use hostkeeper_mdm_gate::CommandQueue as _;
        let command = Command::device_information(CommandUuid::from_random([n; 16]));
        self.queue.enqueue(&enrollment_id(id), &command).await
    }

    /// The id of the command `enqueue(_, n)` queued.
    pub(crate) fn command_id(n: u8) -> String {
        CommandUuid::from_random([n; 16]).as_str().to_owned()
    }

    /// The serial in the ABM table.
    pub(crate) fn list_in_abm(&self, serial: &str) {
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
    pub(crate) fn audited(&self) -> Vec<String> {
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
pub(crate) fn gate_command(n: u8, request_type: &str) -> Command {
    let uuid = CommandUuid::from_random([n; 16]);
    Command::new(request_type, uuid, plist::Dictionary::new())
}

/// The enrollment id `text`.
pub(crate) fn enrollment_id(text: &str) -> EnrollmentId {
    EnrollmentId::parse(text).expect("an enrollment id")
}

/// A fake Mac's connection to the listener: in memory, TLS, HTTP/1.1.
pub(crate) struct Session {
    send: hyper::client::conn::http1::SendRequest<Full<Bytes>>,
}

impl Session {
    /// A connection from a client that offers `identity` (none: no certificate) to the rig's listener.
    pub(crate) async fn dial(rig: &Rig, identity: Option<&Identity2>) -> Option<Self> {
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
    pub(crate) async fn put(
        &mut self,
        path: &str,
        headers: &[(&str, String)],
        body: &[u8],
    ) -> (StatusCode, Vec<u8>) {
        self.request("PUT", path, headers, body).await
    }

    /// `method path` with `body` and the `headers`.
    pub(crate) async fn request(
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
    pub(crate) async fn try_request(
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
pub(crate) fn connector(rig: &Rig, identity: Option<&Identity2>) -> TlsConnector {
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
pub(crate) async fn dial_resolving(
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
pub(crate) async fn served_leaf(rig: &Rig) -> Vec<u8> {
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
pub(crate) fn checkin(message: &str, fields: &[(&str, plist::Value)]) -> Vec<u8> {
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
pub(crate) fn authenticate(serial: &str) -> Vec<u8> {
    checkin("Authenticate", &[("SerialNumber", serial.into())])
}

/// The `TokenUpdate` with a 32-byte token and the magic `magic`.
pub(crate) fn token_update(magic: &str) -> Vec<u8> {
    checkin(
        "TokenUpdate",
        &[
            ("Token", plist::Value::Data(vec![9; 32])),
            ("PushMagic", magic.into()),
        ],
    )
}

/// A result posted to `/mdm`: `status` for `command`.
pub(crate) fn result(status: &str, command: Option<&str>) -> Vec<u8> {
    result_with(status, command, &[])
}

/// [`result`], with `fields` beside the status (a command's answer keys).
pub(crate) fn result_with(
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
pub(crate) fn command_in(body: &[u8]) -> hostkeeper_mdm_server::CommandRequest {
    hostkeeper_mdm_server::CommandRequest::parse(body).expect("a command")
}

// The CMS messages of a client that signs its messages.
pub(crate) const SHA1: &str = "1.3.14.3.2.26";
pub(crate) const SHA256: &str = "2.16.840.1.101.3.4.2.1";
pub(crate) const SHA384: &str = "2.16.840.1.101.3.4.2.2";
pub(crate) const SHA512: &str = "2.16.840.1.101.3.4.2.3";
pub(crate) const RSA: &str = "1.2.840.113549.1.1.1";
pub(crate) const SHA256_RSA: &str = "1.2.840.113549.1.1.11";
pub(crate) const BODY: &[u8] = b"<plist>a check-in</plist>";

/// The DER prefix of a `DigestInfo` for the digest `oid`, and the digest algorithm.
pub(crate) fn digest_info(oid: &str) -> (&'static [u8], &'static digest::Algorithm) {
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
pub(crate) struct Spec {
    pub(crate) digest: &'static str,
    pub(crate) signature: &'static str,
    pub(crate) attributes: bool,
    pub(crate) ski: Option<Vec<u8>>,
    /// The digest in the attributes is of this instead of the body, when set.
    pub(crate) claimed: Option<Vec<u8>>,
    pub(crate) leave_out_digest_attribute: bool,
    pub(crate) certificates: usize,
    pub(crate) names_other_serial: bool,
    pub(crate) indefinite_attributes: bool,
    pub(crate) signers: usize,
    pub(crate) sid_tag: Option<u8>,
    pub(crate) flip_signature: bool,
    pub(crate) drop_certificates: bool,
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
pub(crate) fn message(body: &[u8], mac: &Identity2, spec: &Spec) -> Vec<u8> {
    let key = mac.key.as_ref().unwrap_or_else(|| fixture_key(mac.index));
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
            der(0x30, &[issuer, der(0x02, &serial)].concat())
        }
    };
    let attributes_field = if !spec.attributes {
        Vec::new()
    } else if spec.indefinite_attributes {
        hostkeeper_mdm_push::testkit::indefinite(0xa0, &attributes)
    } else {
        der(0xa0, &attributes)
    };
    let info = der(
        0x30,
        &[
            der(0x02, &[1]),
            sid,
            algorithm(spec.digest),
            attributes_field,
            algorithm(spec.signature),
            der(0x04, &signature),
        ]
        .concat(),
    );
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

pub(crate) fn header(body: &[u8], mac: &Identity2, spec: &Spec) -> String {
    STANDARD.encode(message(body, mac, spec))
}
