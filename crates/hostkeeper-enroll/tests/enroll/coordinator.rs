//! The requests the enroller makes, against the coordinator's own side of enrollment: each held
//! to the CA's profile (`hostkeeper_enrollment::ca::check_request`), and a daemon enrolled and
//! renewed by the coordinator's real `Enrollment` and CA, started as the coordinator starts and
//! approved as an operator approves.
//!
//! The in-process transport stands in for gRPC: it turns each call into the coordinator's
//! protobuf message, answers it with the handler the gRPC service calls, and turns the answer
//! back. It presents the last certificate the coordinator issued as the client certificate of a
//! `Renew`, as the TLS connection would. The coordinator's wall clock is the test's wall clock.
//! A Mac is approved by the nonce the coordinator issued for its request, as the MDM's gate
//! would hand it over, through the managed preferences.

use std::str::FromStr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use hostkeeper_coordinator::enrollment::{
    DeviceState, DeviceStateError, DeviceStatus, Enrollment, MacAllowlist, MacPolicy, Random,
    RpcError, Setup, StoreAt, setup,
};
use hostkeeper_enroll::{
    CoordinatorEnroll, EnrollAnswer, EnrollRequest, EnrollState, Enroller, Identity, Issued,
    Outcome, Poll, PollAnswer, Ports, Reason, RenewAnswer, RenewRequest, RequestId, Settings,
    TransportError,
};
use hostkeeper_enrollment::ca::check_request;
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::names::KeyHash;
use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::hostkeeper::operator::v1::ApproveRequest;
use x509_cert::Certificate;
use x509_cert::der::{Decode, DecodePem, Encode};
use x509_cert::name::Name;
use x509_cert::request::CertReq;

use crate::support::{
    HOST, IMAGE, Managed, T0, WEEK, Wall, World, claims, nonce_plist, pem, pin_of,
};

/// The subject every request of the daemon carries before and after approval.
const PENDING: &str = "CN=host:pending";

/// The role the tests' operator approves the host as.
const ROLE: &str = "web";

/// The subject of `csr_der`, in RFC 4514 form.
fn request_subject(csr_der: &[u8]) -> String {
    CertReq::from_der(csr_der)
        .expect("a PKCS#10 request")
        .info
        .subject
        .to_string()
}

/// What the CA's profile makes of `csr_der`: the host id of its subject, or the refusal.
fn profile_of(csr_der: &[u8]) -> Result<String, hostkeeper_enrollment::ca::ProfileError> {
    check_request(csr_der).map(|verified| verified.subject_id().as_str().to_owned())
}

/// Catches the daemon's requests carrying a subject the CA refuses (an empty one is
/// `bad_csr`): the request of the first `Enroll` and the request of a `Renew` are both
/// accepted by the CA's profile, with the one subject `CN=host:pending`, and are the same bytes
/// (one key, deterministic signatures).
#[test]
fn the_requests_of_enroll_and_renew_meet_the_ca_profile_as_host_pending() {
    let world = World::new("csr-profile");
    let mut enroller = world.enrolled();
    world.set_now(T0 + WEEK / 2);
    assert!(matches!(
        enroller.run_once().expect("renew"),
        Outcome::Renewed(_)
    ));

    let coordinator = world.coordinator();
    let [enrolled] = coordinator.enrolls.as_slice() else {
        panic!("one Enroll: {:?}", coordinator.enrolls.len());
    };
    let [renewed] = coordinator.renews.as_slice() else {
        panic!("one Renew: {:?}", coordinator.renews.len());
    };
    for csr in [&enrolled.csr_der, &renewed.csr_der] {
        assert_eq!(profile_of(csr), Ok("pending".to_owned()));
        assert_eq!(request_subject(csr), PENDING);
    }
    assert_eq!(enrolled.csr_der, renewed.csr_der);
}

/// The coordinator's wall clock: the test's.
struct SharedWall(Arc<Mutex<Wall>>);

impl UnixClock for SharedWall {
    fn now_unix(&self) -> u64 {
        self.0.lock().expect("wall").now
    }
}

/// Poll nonces from a counter: each draw is the next byte, repeated.
fn counting() -> Random {
    let next = AtomicU8::new(0);
    Box::new(move |buf: &mut [u8]| {
        buf.fill(next.fetch_add(1, Ordering::SeqCst).wrapping_add(1));
        true
    })
}

/// A coordinator's failure, as the enroller sees a failed call.
fn failed(call: &str, error: RpcError) -> TransportError {
    TransportError(format!("{call}: {error:?}"))
}

/// The enroller's side of the in-process transport.
struct InProcess {
    enrollment: Arc<Enrollment>,
    /// The certificate (DER) presented as the client's: the last one issued.
    client: Arc<Mutex<Option<Vec<u8>>>>,
}

impl InProcess {
    fn issued(&self, host_id: String, der: Vec<u8>, ca_bundle_pem: String) -> Issued {
        let cert_pem = pem(&der);
        *self.client.lock().expect("client") = Some(der);
        Issued {
            host_id,
            cert_pem,
            ca_bundle_pem,
        }
    }
}

impl CoordinatorEnroll for InProcess {
    fn presented_ca_der(&mut self) -> Result<Vec<u8>, TransportError> {
        Ok(self.enrollment.ca_bundle().certificate_der().to_vec())
    }

    fn enroll(&mut self, request: &EnrollRequest) -> Result<EnrollAnswer, TransportError> {
        let claims = &request.claims;
        let reply = self
            .enrollment
            .enroll(pb::EnrollRequest {
                csr_der: request.csr_der.clone(),
                claims: Some(pb::Claims {
                    machine_id: claims.machine_id.clone().unwrap_or_default(),
                    dmi_serial: claims.product_serial.clone().unwrap_or_default(),
                    product_uuid: claims.product_uuid.clone().unwrap_or_default(),
                    addresses: claims.network_addresses.clone(),
                    mac_serial: claims.mac_serial.clone().unwrap_or_default(),
                    platform_uuid: claims.platform_uuid.clone().unwrap_or_default(),
                    bmc_mac: claims.bmc_mac.clone().unwrap_or_default(),
                }),
                image: request.image.clone(),
                renewal_of: request
                    .renewal_of
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                mac_nonce: request
                    .mac_nonce
                    .as_ref()
                    .map(|nonce| nonce.as_bytes().to_vec())
                    .unwrap_or_default(),
            })
            .map_err(|e| failed("Enroll", e))?;
        let request_id = RequestId::parse(&reply.request_id)
            .ok_or_else(|| TransportError(format!("Enroll: request id {}", reply.request_id)))?;
        let state = match pb::EnrollState::try_from(reply.state) {
            Ok(pb::EnrollState::Pending | pb::EnrollState::DuplicateClaim) => EnrollState::Pending,
            Ok(pb::EnrollState::Unspecified) => EnrollState::Decided,
            other => return Err(TransportError(format!("Enroll: state {other:?}"))),
        };
        Ok(EnrollAnswer { request_id, state })
    }

    fn poll(&mut self, poll: &Poll) -> Result<PollAnswer, TransportError> {
        let reply = self
            .enrollment
            .poll(pb::PollRequest {
                request_id: poll.request_id.to_string(),
                nonce: poll.nonce.clone(),
                signature: poll.signature.clone(),
            })
            .map_err(|e| failed("Poll", e))?;
        match pb::PollState::try_from(reply.state) {
            Ok(pb::PollState::Unspecified) => Ok(PollAnswer::Challenge(reply.nonce)),
            Ok(pb::PollState::Pending) => Ok(PollAnswer::Pending),
            Ok(pb::PollState::Denied) => Ok(PollAnswer::Denied(
                Reason::new("denied").expect("a reason token"),
            )),
            Ok(pb::PollState::Issued) => Ok(PollAnswer::Issued(self.issued(
                reply.host_id,
                reply.certificate_der,
                reply.ca_bundle_pem,
            ))),
            Err(_) => Err(TransportError(format!("Poll: state {}", reply.state))),
        }
    }

    fn renew(&mut self, request: &RenewRequest) -> Result<RenewAnswer, TransportError> {
        let client = self.client.lock().expect("client").clone();
        let reply = self
            .enrollment
            .renew(
                client.as_deref(),
                pb::RenewRequest {
                    csr_der: request.csr_der.clone(),
                },
            )
            .map_err(|e| failed("Renew", e))?;
        Ok(RenewAnswer::Issued(self.issued(
            request.host_id.to_string(),
            reply.certificate_der,
            reply.ca_bundle_pem,
        )))
    }
}

/// The coordinator's enrollment, started over its own files in the test's scratch directory,
/// its CA made at the test's wall clock.
fn start(world: &World) -> Arc<Enrollment> {
    Arc::new(started(world))
}

/// [`start`], not yet shared.
fn started(world: &World) -> Enrollment {
    let dir = world.dir.path().join("coordinator");
    std::fs::create_dir_all(&dir).expect("the coordinator's directory");
    let (store, ca_key, server_cert) = (
        dir.join("enrollment"),
        dir.join("ca.key"),
        dir.join("server.pem"),
    );
    let names = ["coordinator.example".to_owned()];
    let files = Setup {
        store: StoreAt::Dir(&store),
        ca_key: &ca_key,
        ca_init: true,
        server_cert: &server_cert,
        server_names: &names,
        server_ips: &[],
    };
    let clock = Arc::new(SharedWall(Arc::clone(&world.wall)));
    setup(Arc::new(RealFs), &files, clock, counting()).expect("enrollment starts")
}

/// An enroller whose coordinator is `enrollment`, its config pinning that CA.
fn enroller_of(world: &World, enrollment: &Arc<Enrollment>) -> Enroller {
    let transport = InProcess {
        enrollment: Arc::clone(enrollment),
        client: Arc::new(Mutex::new(None)),
    };
    let settings = Settings {
        ca_sha256: pin_of(enrollment.ca_bundle().certificate_der()),
        ..world.settings()
    };
    let ports = Ports {
        coordinator: Box::new(transport),
        ..world.ports_with(None, None)
    };
    Enroller::open(settings, ports).expect("open the enroller")
}

/// The certificate the enroller stored for `identity`.
fn stored(identity: &Identity) -> Certificate {
    let text = std::fs::read(&identity.cert).expect("the stored certificate");
    Certificate::from_pem(&text).expect("a PEM certificate")
}

/// Asserts that `identity` is generation `generation` of `host:web-1`, signed by the
/// coordinator's CA, and that the coordinator reads its certificate as the approved host and
/// role. Returns the certificate's DER.
fn assert_issued(enrollment: &Enrollment, identity: &Identity, generation: u64) -> Vec<u8> {
    assert_eq!(identity.host_id.as_str(), HOST);
    assert_eq!(identity.generation, generation);
    let certificate = stored(identity);
    let tbs = certificate.tbs_certificate();
    let host = Name::from_str(&format!("CN=host:{HOST}")).expect("a name");
    assert_eq!(tbs.subject(), &host);
    let ca = Certificate::from_der(enrollment.ca_bundle().certificate_der()).expect("the CA");
    assert_eq!(tbs.issuer(), ca.tbs_certificate().subject());
    let der = certificate.to_der().expect("DER");
    let known = enrollment
        .identity_of(&der)
        .expect("a certificate the coordinator reads");
    assert_eq!((known.host_id.as_str(), known.role.as_str()), (HOST, ROLE));
    der
}

/// Catches a daemon that cannot finish enrollment with the coordinator it is built for: its
/// `Enroll` is accepted and stays pending (also at a poll before approval), an operator approves
/// it, the next poll brings a certificate of subject `host:web-1` from the coordinator's CA,
/// and the renewal at half life brings the next one.
#[test]
fn a_daemon_enrolls_and_renews_with_the_coordinators_own_enrollment_and_ca() {
    let world = World::new("own-coordinator");
    let enrollment = start(&world);
    let mut enroller = enroller_of(&world, &enrollment);

    assert_eq!(enroller.run_once().expect("posted"), Outcome::Pending);
    let [pending] = enrollment
        .store()
        .pending()
        .expect("pending")
        .try_into()
        .expect("one");
    assert_eq!(pending.image, IMAGE);
    assert_eq!(
        Some(pending.claims.machine_id.as_str()),
        claims().machine_id.as_deref()
    );
    assert_eq!(
        enroller.run_once().expect("still pending"),
        Outcome::Pending
    );

    let id = pending.request_id.to_hex();
    let approved = enrollment
        .approve(
            "operator",
            ApproveRequest {
                request_id: id.clone(),
                key_sha256: id,
                host_id: HOST.to_owned(),
                role: ROLE.to_owned(),
                services: Vec::new(),
                replace: false,
                reason: "new rack".to_owned(),
            },
            &|_| false,
            // No host inventory: the request is approved as the host id named.
            &[],
        )
        .expect("approved");
    assert_eq!(approved.host_id.as_str(), HOST);

    let Outcome::Enrolled(first) = enroller.run_once().expect("enrolled") else {
        panic!("not enrolled");
    };
    let first_der = assert_issued(&enrollment, &first, 1);
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);

    world.set_now(first.not_before + (first.not_after - first.not_before) / 2);
    let Outcome::Renewed(second) = enroller.run_once().expect("renewed") else {
        panic!("not renewed");
    };
    let second_der = assert_issued(&enrollment, &second, 2);
    assert_ne!(first_der, second_der);
}

/// The serial of the tests' Mac.
const SERIAL: &str = "C02XK1ABJG5J";

/// The gate of the tests: every device enrolled and supervised.
struct Supervised;

impl DeviceState for Supervised {
    fn status(&self, _serial: &str) -> Result<DeviceStatus, DeviceStateError> {
        Ok(DeviceStatus {
            enrolled: true,
            supervised: true,
            enrolled_since_unix: None,
        })
    }
}

/// Catches a Mac nonce the coordinator does not approve on: a Mac posts and waits; the
/// coordinator issues a nonce for its pending request, which reaches the Mac through its
/// managed preferences; the next step reposts with it, the coordinator's Mac path approves the
/// request as its allowlist entry says, and the same step's poll brings that host's
/// certificate, with no operator.
#[test]
fn a_mac_is_approved_by_its_nonce_with_the_coordinators_own_enrollment() {
    let world = World::new("own-coordinator-mac");
    *world.claims.lock().expect("claims") = hostkeeper_enroll::Claims {
        mac_serial: Some(SERIAL.to_owned()),
        platform_uuid: Some("4C4C4544-0042-5A10-8048-B7C04F4E3332".to_owned()),
        ..hostkeeper_enroll::Claims::default()
    };
    let allowlist =
        format!("[[mac]]\nserial = \"{SERIAL}\"\nhost_id_prefix = \"mac-\"\nrole = \"{ROLE}\"\n");
    let enrollment = Arc::new(started(&world).with_mac(MacPolicy {
        device_state: Arc::new(Supervised),
        allowlist: MacAllowlist::parse(&allowlist).expect("an allowlist"),
    }));
    let mut enroller = enroller_of(&world, &enrollment);
    assert_eq!(enroller.run_once().expect("posted"), Outcome::Pending);
    assert_eq!(enroller.run_once().expect("polled"), Outcome::Pending);

    let [pending] = enrollment
        .store()
        .pending()
        .expect("pending")
        .try_into()
        .expect("one");
    assert!(!pending.mac_nonce_presented);
    let id = pending.request_id.to_hex();
    let nonce = enrollment
        .issue_mac_nonce(SERIAL, &KeyHash::from_hex(&id).expect("a key hash"))
        .expect("a nonce");
    world.set_managed(Managed::Bytes(nonce_plist(&id, nonce.as_bytes())));

    let Outcome::Enrolled(identity) = enroller.run_once().expect("enrolled") else {
        panic!("not enrolled");
    };
    let host = format!("mac-{}", SERIAL.to_ascii_lowercase());
    assert_eq!(identity.host_id.as_str(), host);
    let der = stored(&identity).to_der().expect("DER");
    let known = enrollment.identity_of(&der).expect("a known certificate");
    assert_eq!(
        (known.host_id.as_str(), known.role.as_str()),
        (host.as_str(), ROLE)
    );
    assert_eq!(enroller.run_once().expect("idle"), Outcome::Idle);
}
