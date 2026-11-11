//! Builders for the tests of the MDM server: a rig with the tables, a setup holding the certificate
//! of the name and the push identity, a manual clock, a fake APNs, a CA that issues the Macs'
//! identities, and a fake Mac that dials the listener in memory.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_mdm_gate::{Command, CommandUuid, EnrollmentId};
use hostkeeper_mdm_push::clock::Clock;
use hostkeeper_mdm_push::request::pkcs1_pem;
use hostkeeper_mdm_push::testkit::{FakeApns, fixture_key, push_certificate_pem};
use hostkeeper_testkit::Scratch;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, PKCS_RSA_SHA256, SanType,
};
use rsa_legacy::pkcs8::EncodePrivateKey as _;
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use sha2::{Digest as _, Sha256};
use tokio::io::DuplexStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use super::auth::fingerprint;
use super::flaky::Flaky;
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
    pub(crate) key: Option<rsa_legacy::RsaPrivateKey>,
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
        Self::build(name, None, counting(), None)
    }

    /// A rig started without `--mdm-pkg-dir`, with the certificate and the push identity.
    pub(crate) fn without_pkg_dir(name: &str) -> Self {
        let rig = Self::build_with(name, None, counting(), None, false, None, None);
        rig.store_tls();
        rig.store_push();
        rig
    }

    /// [`Self::with_apple`] over the setup `setup`, whose store a test makes fail.
    pub(crate) fn with_apple_over(name: &str, roots: Vec<Vec<u8>>, setup: MdmSetup) -> Self {
        let rig = Self::build_with(name, None, counting(), None, true, Some(roots), Some(setup));
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
        let rig = Self::build_with(name, None, random, None, true, Some(roots), None);
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
        let rig = Self::build_with(name, None, counting(), None, true, Some(roots), None);
        rig.store_tls();
        rig.store_push();
        rig.setup
            .set_setting("mdm_name", "localhost")
            .expect("the name is set");
        rig
    }

    /// The directory `--mdm-pkg-dir` names in this rig (it is made by the test that fills it).
    pub(crate) fn pkg_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("pkgs")
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

    /// A queue of a node that holds no push identity, over the same tables and setup.
    pub(crate) fn queue_without_push_identity(&self, lease: Lease) -> MdmQueue {
        MdmQueue::new(
            Arc::clone(&self.store) as Arc<dyn MdmStore>,
            Arc::clone(&self.time) as Arc<dyn UnixClock>,
            Arc::new(|| None),
            lease,
            Arc::clone(&self.setup),
            86_400,
        )
    }

    /// A queue as a coordinator builds it before a push identity is stored: nobody pushes.
    pub(crate) fn queue_without_pusher(&self) -> MdmQueue {
        MdmQueue::new(
            Arc::clone(&self.store) as Arc<dyn MdmStore>,
            Arc::clone(&self.time) as Arc<dyn UnixClock>,
            Arc::new(|| None),
            Lease::new(|| true),
            Arc::clone(&self.setup),
            86_400,
        )
    }

    /// A rig whose server asks Apple about the ABM token through `abm`, with the certificate and the
    /// push identity.
    pub(crate) fn asking(name: &str, abm: Arc<dyn crate::mdm_verify::AbmAccountCheck>) -> Self {
        let rig = Self::build(name, None, counting(), Some(abm));
        rig.store_tls();
        rig.store_push();
        rig
    }

    /// A rig whose source of random bytes is `random`, with the certificate and the push identity.
    pub(crate) fn with_random(name: &str, random: crate::enrollment::Random) -> Self {
        let rig = Self::build(name, None, random, None);
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
        let rig = Self::build(name, Some(Box::new(gate)), counting(), None);
        rig.store_tls();
        rig.store_push();
        rig
    }

    fn build(
        name: &str,
        gate: Option<GateBuilder>,
        random: crate::enrollment::Random,
        abm: Option<Arc<dyn crate::mdm_verify::AbmAccountCheck>>,
    ) -> Self {
        Self::build_with(name, gate, random, abm, true, None, None)
    }

    #[allow(
        clippy::type_complexity,
        reason = "a test builder takes a closure that builds the gate over the queue"
    )]
    fn build_with(
        name: &str,
        gate: Option<GateBuilder>,
        random: crate::enrollment::Random,
        abm: Option<Arc<dyn crate::mdm_verify::AbmAccountCheck>>,
        pkg_dir: bool,
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
            abm,
            renewer: Arc::new(idle_renewer(&setup)),
            pkg_dir: pkg_dir.then(|| dir.path().join("pkgs")),
        });
        let server = Arc::new(match roots {
            Some(roots) => server.with_apple_roots(roots),
            None => server,
        });
        let fake = Fake::new();
        let coordinator = Arc::new(coordinator_over(&fake, &setup, Some(&server)));
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

    /// A coordinator over the rig's setup that runs no Macs' listener.
    pub(crate) fn coordinator_without_listener(&self) -> Coordinator {
        coordinator_over(&self.fake, &self.setup, None)
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

/// A coordinator over `fake` that keeps `setup` and serves the Macs with `server`, if any.
fn coordinator_over(
    fake: &Fake,
    setup: &Arc<MdmSetup>,
    server: Option<&Arc<MdmServer>>,
) -> Coordinator {
    coordinator_listed(fake, &Arc::new(AtomicU64::new(1_000)), None, "")
        .with_shared_mdm_setup(Arc::clone(setup))
        .with_mdm_server(server.map(Arc::clone))
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
        Self::dial_to(rig, &rig.coordinator, identity).await
    }

    /// [`Self::dial`] to the listener of `coordinator`, which serves the rig's server.
    pub(crate) async fn dial_to(
        rig: &Rig,
        coordinator: &Arc<Coordinator>,
        identity: Option<&Identity2>,
    ) -> Option<Self> {
        let config = rig.server.tls().config()?;
        let (client, server) = tokio::io::duplex(256 * 1024);
        tokio::spawn(crate::listen::connection_with(
            server,
            config,
            crate::http::Surface::Mdm,
            Arc::clone(coordinator),
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

    /// `method path` with `headers` and no body: the head of the answer and its body, which is
    /// read to the end (or fails where the server cut it).
    pub(crate) async fn fetch_parts(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
    ) -> Result<(hyper::http::response::Parts, Vec<u8>), hyper::Error> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost");
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        let request = request.body(Full::new(Bytes::new())).expect("a request");
        let (head, body) = self.send.send_request(request).await?.into_parts();
        Ok((head, body.collect().await?.to_bytes().to_vec()))
    }

    /// `method path` with the `headers`: the status, the headers and the body of the answer, or
    /// the error of a connection the server ended.
    pub(crate) async fn fetch(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
    ) -> Result<(StatusCode, hyper::HeaderMap, Vec<u8>), hyper::Error> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost");
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let request = request.body(Full::new(Bytes::new())).expect("a request");
        let response = self.send.send_request(request).await?;
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await?.to_bytes();
        Ok((parts.status, parts.headers, bytes.to_vec()))
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

mod signature;

pub(crate) use signature::{
    BODY, RSA, SHA1, SHA256, SHA256_RSA, SHA384, SHA512, Spec, header, message,
};

/// A renewer over `setup` whose CA is down and whose clock is the test's: with no name set it
/// does nothing.
fn idle_renewer(setup: &Arc<MdmSetup>) -> crate::mdm_acme::renewal::TlsRenewer {
    use crate::mdm_acme::testkit::{FakeAcme, FakeClock, FakeDns, FakeProviders, Mood};
    let dns = FakeDns::new();
    let ca = FakeAcme::new(&dns, "localhost", "_acme-challenge", NOW, 1);
    ca.be(Mood::Down);
    crate::mdm_acme::renewal::TlsRenewer::new(crate::mdm_acme::renewal::Parts {
        setup: Arc::clone(setup),
        https: ca,
        providers: Arc::new(FakeProviders(dns)),
        clock: FakeClock::at(NOW),
        status: Arc::clone(setup.tls_status()),
    })
}
