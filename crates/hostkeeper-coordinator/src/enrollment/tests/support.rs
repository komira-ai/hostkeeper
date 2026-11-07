//! What the enrollment tests share: a wall clock the test sets, an enrollment over a directory
//! of its own, host keys with their CSRs and poll signatures, a coordinator with enrollment, and
//! a request body that tells whether it was read.

use std::convert::Infallible;
use std::net::IpAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use hostkeeper_enrollment::enroll::{Decision, poll_message};
use hostkeeper_enrollment::fs::{Fs, RealFs};
use hostkeeper_enrollment::names::{HostId, KeyHash, Role};
use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_proto::hostkeeper::agent::v1::{
    Claims, EnrollRequest, PollRequest, PollResponse, PollState,
};
use hostkeeper_proto::hostkeeper::operator::v1::{AddRecordRequest, ApproveRequest};
use hostkeeper_testkit::Scratch;
use http_body_util::Full;
use hyper::body::{Body, Frame};
use prost::Message;
use rcgen::{
    CertificateParams, DistinguishedName, DnType, KeyPair, PublicKeyData, SignatureAlgorithm,
    SigningKey,
};

use crate::enrollment::{Enrollment, Random, RpcError, Setup, SetupError, os_random, setup};
use crate::identity::Identity;
use crate::testutil::{ops_of, scratch};

/// The wall-clock time every enrollment test starts at, in unix seconds.
pub const NOW: u64 = 1_800_000_000;

/// The DNS name of the coordinator's server certificate in the tests.
pub const SERVER_NAME: &str = "coordinator.example";

/// How long a request body has.
pub const PATIENCE: Duration = Duration::from_secs(30);

/// A desired state of the legacy service `kbf`: version 2.0, one storage host at a time.
pub const ONE_STORAGE: &str = r#"{"version":"2.0","max_updating":{"storage":1},
    "min_up":{"storage":1,"frontend":1}}"#;

/// A wall clock that reads what the test sets.
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

/// The files enrollment starts from in `dir`: the store, the CA key and the server certificate.
pub struct Files {
    pub store: PathBuf,
    pub ca_key: PathBuf,
    pub server_cert: PathBuf,
    pub names: Vec<String>,
    /// The IP addresses the server certificate names besides (`--lan-listen`'s).
    pub ips: Vec<IpAddr>,
}

impl Files {
    pub fn in_dir(dir: &Scratch) -> Files {
        Files {
            store: dir.path().join("enrollment"),
            ca_key: dir.path().join("ca.key"),
            server_cert: dir.path().join("server.pem"),
            names: vec![SERVER_NAME.to_owned()],
            ips: Vec::new(),
        }
    }

    /// The setup of these files, making the CA when `init`.
    pub fn setup(&self, init: bool) -> Setup<'_> {
        Setup {
            dir: &self.store,
            ca_key: &self.ca_key,
            ca_init: init,
            server_cert: &self.server_cert,
            server_names: &self.names,
            server_ips: &self.ips,
        }
    }

    /// Enrollment started over these files through `fs` at the time `clock` reads.
    pub fn start(
        &self,
        fs: Arc<dyn Fs>,
        clock: Arc<FixedClock>,
        random: Random,
    ) -> Result<Enrollment, SetupError> {
        setup(fs, &self.setup(true), clock, random)
    }
}

/// An enrollment over a directory of its own, its CA made at [`NOW`].
pub struct Rig {
    /// The directory, removed when the rig drops.
    _dir: Scratch,
    pub files: Files,
    pub clock: Arc<FixedClock>,
    pub enrollment: Arc<Enrollment>,
}

impl Rig {
    pub fn new(name: &str) -> Rig {
        Rig::over(name, Arc::new(RealFs), os_random())
    }

    /// A rig whose files go through `fs` and whose nonces come from `random`.
    pub fn over(name: &str, fs: Arc<dyn Fs>, random: Random) -> Rig {
        Rig::configured(name, fs, random, |enrollment| enrollment)
    }

    /// [`Rig::over`], with the enrollment `configure` makes of the one started.
    pub fn configured(
        name: &str,
        fs: Arc<dyn Fs>,
        random: Random,
        configure: impl FnOnce(Enrollment) -> Enrollment,
    ) -> Rig {
        Rig::started(name, fs, random, NOW, configure)
    }

    /// [`Rig::new`], its clock (and so its CA and server certificate) started at `now`.
    pub fn at(name: &str, now: u64) -> Rig {
        Rig::started(name, Arc::new(RealFs), os_random(), now, |enrollment| {
            enrollment
        })
    }

    fn started(
        name: &str,
        fs: Arc<dyn Fs>,
        random: Random,
        now: u64,
        configure: impl FnOnce(Enrollment) -> Enrollment,
    ) -> Rig {
        let dir = scratch(name);
        let files = Files::in_dir(&dir);
        let clock = FixedClock::at(now);
        let enrollment = Arc::new(configure(
            files
                .start(fs, Arc::clone(&clock), random)
                .expect("enrollment starts"),
        ));
        Rig {
            _dir: dir,
            files,
            clock,
            enrollment,
        }
    }

    /// Enrollment started again over the same files: what a restart makes.
    pub fn restart(&self) -> Arc<Enrollment> {
        self.restart_with(|enrollment| enrollment)
    }

    /// [`Rig::restart`], with the enrollment `configure` makes of the one started.
    pub fn restart_with(
        &self,
        configure: impl FnOnce(Enrollment) -> Enrollment,
    ) -> Arc<Enrollment> {
        Arc::new(configure(
            self.files
                .start(Arc::new(RealFs), Arc::clone(&self.clock), os_random())
                .expect("enrollment starts again"),
        ))
    }
}

/// The serial number of the Mac of the tests.
pub const SERIAL: &str = "C02XK1ABJG5J";

/// The `Enroll` of a Mac's `key` from machine `machine_id`, claiming `serial` and presenting
/// `nonce`.
pub fn mac_request(key: &HostKey, machine_id: &str, serial: &str, nonce: &[u8]) -> EnrollRequest {
    EnrollRequest {
        claims: Some(Claims {
            machine_id: machine_id.to_owned(),
            mac_serial: serial.to_owned(),
            platform_uuid: "4C4C4544-0042-5A10-8048-B7C04F4E3332".to_owned(),
            ..Claims::default()
        }),
        mac_nonce: nonce.to_vec(),
        ..enroll_request(key, machine_id)
    }
}

/// The operator's approval of `key`'s request as `host`, a storage host running `kbf`.
pub fn approval(key: &HostKey, host: &str) -> ApproveRequest {
    ApproveRequest {
        request_id: key.key_hash().to_hex(),
        key_sha256: key.key_hash().to_hex(),
        host_id: host.to_owned(),
        role: "storage".to_owned(),
        services: vec!["kbf".to_owned()],
        replace: false,
        reason: "new rack".to_owned(),
    }
}

/// An audit entry as the tests compare it: who, what, which host, and the reason.
pub type Audited = (String, String, Option<String>, Option<String>);

/// Every entry of the audit log of `coordinator`, oldest first, without its number.
pub fn audit_of(coordinator: &crate::service::Coordinator) -> Vec<Audited> {
    coordinator
        .ops()
        .audit_log()
        .expect("the audit log")
        .into_iter()
        .map(|entry| (entry.by, entry.action, entry.node_id, entry.reason))
        .collect()
}

/// The entries of the audit log of `coordinator` after the first `skip`.
pub fn audit_after(coordinator: &crate::service::Coordinator, skip: usize) -> Vec<Audited> {
    audit_of(coordinator).split_off(skip)
}

/// An audit entry by `by` of `action` about `node`, with `reason`.
pub fn audited(by: &str, action: &str, node: Option<&str>, reason: &str) -> Audited {
    (
        by.to_owned(),
        action.to_owned(),
        node.map(str::to_owned),
        Some(reason.to_owned()),
    )
}

/// The coordinator's audit entry of a new request of `key`, round 0, in `state` (`pending` or
/// `duplicate_claim`), renewing `host` if one is named.
pub fn requested(key: &HostKey, host: Option<&str>, state: &str) -> Audited {
    let round = u32::from(host.is_some());
    audited(
        "coordinator",
        "enroll_requested",
        host,
        &format!("request {}; round {round}; {state}", key.key_hash()),
    )
}

/// A host's key, as a daemon makes it.
pub struct HostKey {
    pub pair: KeyPair,
}

impl HostKey {
    /// A P-256 key.
    pub fn new() -> HostKey {
        HostKey::of(&rcgen::PKCS_ECDSA_P256_SHA256)
    }

    /// A key of `algorithm`.
    pub fn of(algorithm: &'static SignatureAlgorithm) -> HostKey {
        HostKey {
            pair: KeyPair::generate_for(algorithm).expect("a key"),
        }
    }

    /// A CSR of this key with the subject a daemon sends before approval, `host:pending`.
    pub fn csr(&self) -> Vec<u8> {
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, "host:pending");
        params
            .serialize_request(&self.pair)
            .expect("a CSR")
            .der()
            .to_vec()
    }

    /// The SHA-256 of its SubjectPublicKeyInfo: its request id.
    pub fn key_hash(&self) -> KeyHash {
        KeyHash::of_spki(&self.pair.subject_public_key_info())
    }

    /// An ECDSA signature (ASN.1 DER) of `message` by this key.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.pair.sign(message).expect("a signature")
    }
}

/// The `Enroll` of `key` from machine `machine_id`.
pub fn enroll_request(key: &HostKey, machine_id: &str) -> EnrollRequest {
    EnrollRequest {
        csr_der: key.csr(),
        claims: Some(Claims {
            machine_id: machine_id.to_owned(),
            ..Claims::default()
        }),
        image: "hostkeeper-image-1".to_owned(),
        ..EnrollRequest::default()
    }
}

pub fn host_id(text: &str) -> HostId {
    HostId::parse(text).expect("a host id")
}

/// Approves the request of `key` as `host`, a storage host, taking the id over when `replace`.
pub fn approve(enrollment: &Enrollment, key: &HostKey, host: &str, replace: bool) {
    enrollment
        .store()
        .approve(
            &key.key_hash(),
            &key.key_hash(),
            &Decision {
                host_id: host_id(host),
                role: Role::parse("storage").expect("a role"),
                services: Vec::new(),
                replace,
            },
        )
        .expect("approved");
}

/// An unsigned `Poll` for `key`'s request, then one signed by `signer` over the nonce it gave.
pub fn signed_poll(
    enrollment: &Enrollment,
    key: &HostKey,
    signer: &HostKey,
) -> Result<PollResponse, RpcError> {
    let request_id = key.key_hash().to_hex();
    let challenge = enrollment.poll(PollRequest {
        request_id: request_id.clone(),
        ..PollRequest::default()
    })?;
    let message = poll_message(&key.key_hash(), &challenge.nonce);
    enrollment.poll(PollRequest {
        request_id,
        signature: signer.sign(&message),
        nonce: challenge.nonce,
    })
}

/// `key` enrolled from machine `machine_id`, approved as `host`, and its certificate taken by a
/// poll: the certificate, DER.
pub fn enrolled(enrollment: &Enrollment, key: &HostKey, machine_id: &str, host: &str) -> Vec<u8> {
    enrollment
        .enroll(enroll_request(key, machine_id))
        .expect("enrolled");
    approve(enrollment, key, host, false);
    let issued = signed_poll(enrollment, key, key).expect("polled");
    assert_eq!(issued.state, PollState::Issued as i32);
    issued.certificate_der
}

/// A coordinator past its quiet start, with `kbf` desired at 2.0, over the enrollment of
/// `enrollment`.
pub async fn coordinator_over(name: &str, enrollment: &Rig) -> crate::testutil::Rig {
    let mut rig = crate::testutil::Rig::new(name);
    rig.coordinator = crate::testutil::Rig::start(&rig.dir, &rig.clock)
        .with_enrollment(Arc::clone(&enrollment.enrollment));
    rig.at(2_000);
    rig.set(ONE_STORAGE).await;
    rig.at(2_100);
    rig
}

/// A time long before [`NOW`], in unix seconds.
pub const LONG_BEFORE: u64 = 1_000_000_000;

/// A coordinator over the enrollment of `enrollment` whose ops clock reads [`LONG_BEFORE`]: the
/// approval window of a record added through it ended long before the enrollment's clock reads
/// [`NOW`], so a request that matches the record is shown matched and waits for an operator,
/// whatever day the test runs on.
pub fn closed_windows(name: &str, enrollment: &Rig) -> crate::testutil::Rig {
    let mut rig = crate::testutil::Rig::new(name);
    rig.coordinator = crate::testutil::Rig::start(&rig.dir, &rig.clock)
        .with_enrollment(Arc::clone(&enrollment.enrollment))
        .with_ops(ops_of(&rig.dir).with_unix(Arc::new(|| LONG_BEFORE)));
    rig
}

/// A coordinator over the enrollment of `enrollment` whose ops clock reads what the enrollment's
/// clock does: the approval window of a record starts when the test's clock says, and is open or
/// closed by that clock alone.
pub fn aligned(name: &str, enrollment: &Rig) -> crate::testutil::Rig {
    let mut rig = crate::testutil::Rig::new(name);
    let clock = Arc::clone(&enrollment.clock);
    rig.coordinator = crate::testutil::Rig::start(&rig.dir, &rig.clock)
        .with_enrollment(Arc::clone(&enrollment.enrollment))
        .with_ops(ops_of(&rig.dir).with_unix(Arc::new(move || clock.now_unix())));
    rig
}

/// The add of the record `host`, a storage host, with the data MACs `data` and the BMC MAC
/// `bmc` (empty for none), and no window asked for.
pub fn add_of(host: &str, data: &[&str], bmc: &str) -> AddRecordRequest {
    AddRecordRequest {
        host_id: host.to_owned(),
        role: "storage".to_owned(),
        data_macs: data.iter().map(|m| (*m).to_owned()).collect(),
        bmc_mac: bmc.to_owned(),
        ..AddRecordRequest::default()
    }
}

/// The `Enroll` of `key` from machine `machine_id` claiming the addresses `addresses` and the
/// BMC MAC `bmc` (empty for none).
pub fn claiming(key: &HostKey, machine_id: &str, addresses: &[&str], bmc: &str) -> EnrollRequest {
    let mut request = enroll_request(key, machine_id);
    let claims = request.claims.as_mut().expect("claims");
    claims.addresses = addresses.iter().map(|a| (*a).to_owned()).collect();
    claims.bmc_mac = bmc.to_owned();
    request
}

/// Who the certificate `der` names, as the heartbeat path reads it.
pub fn identity(coordinator: &crate::service::Coordinator, der: &[u8]) -> Identity {
    coordinator.identify(der).expect("an enrolled host")
}

/// `message`, encoded, as a request body.
pub fn body_of<M: Message>(message: &M) -> Full<Bytes> {
    Full::new(Bytes::from(message.encode_to_vec()))
}

/// A request body with nothing in it that says, through the flag, whether it was read.
pub struct Watched(Arc<AtomicBool>);

/// A [`Watched`] body and its flag.
pub fn watched() -> (Watched, Arc<AtomicBool>) {
    let read = Arc::new(AtomicBool::new(false));
    (Watched(Arc::clone(&read)), read)
}

impl Body for Watched {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.0.store(true, Ordering::SeqCst);
        Poll::Ready(None)
    }
}
