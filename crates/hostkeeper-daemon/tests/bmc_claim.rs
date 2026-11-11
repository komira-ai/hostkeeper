//! The BMC MAC claim of the daemon's enrollment request (`hostkeeper_daemon::adapters::host_claims`
//! and `hostkeeper_daemon::ipmi`): a Linux host's claims ask the BMC they are given, so the request
//! the enroller posts carries the MAC a BMC answers with; a host with no IPMI device claims no BMC
//! MAC and still posts its request; a Mac asks no BMC.

mod support;

use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_daemon::adapters::host_claims;
use hostkeeper_daemon::ipmi::{DevIpmi, IPMI_DEVICE};
use hostkeeper_enroll::{
    Claims, ClaimsSource, CoordinatorEnroll, EnrollAnswer, EnrollRequest, EnrollState, Enroller,
    Ioreg, Ipmi, LinuxClaims, OsEntropy, Outcome, PkiDir, Poll, PollAnswer, RenewAnswer,
    RenewRequest, RequestId, Settings as EnrollSettings, SystemWallClock, TransportError,
};
use hostkeeper_launchd::PlatformKind;
use sha2::{Digest, Sha256};
use support::Scratch;
use x509_cert::der::{Decode, Encode};
use x509_cert::request::CertReq;

/// The CA certificate the fake coordinator presents; the enroller is pinned to its SHA-256.
const CA_DER: &[u8] = b"the coordinator's CA";

/// The MAC the fake BMC answers with, and its spelling in the claim.
const BMC_MAC: [u8; 6] = [0x0c, 0xc4, 0x7a, 0x00, 0x00, 0x01];
const BMC_MAC_TEXT: &str = "0c:c4:7a:00:00:01";

const MACHINE_ID: &str = "0123456789abcdef0123456789abcdef";
const DATA_MAC: &str = "0c:c4:7a:10:20:30";

/// A coordinator that keeps every enrollment request it is sent and leaves each pending.
#[derive(Clone, Default)]
struct Recording(Arc<Mutex<Vec<EnrollRequest>>>);

impl Recording {
    fn enrolls(&self) -> Vec<EnrollRequest> {
        self.0.lock().expect("the requests").clone()
    }
}

impl CoordinatorEnroll for Recording {
    fn presented_ca_der(&mut self) -> Result<Vec<u8>, TransportError> {
        Ok(CA_DER.to_vec())
    }

    fn enroll(&mut self, request: &EnrollRequest) -> Result<EnrollAnswer, TransportError> {
        self.0.lock().expect("the requests").push(request.clone());
        let csr = CertReq::from_der(&request.csr_der).expect("a CSR");
        let spki = csr.info.public_key.to_der().expect("the public key's DER");
        Ok(EnrollAnswer {
            request_id: RequestId::of_spki(&spki),
            state: EnrollState::Pending,
        })
    }

    fn poll(&mut self, poll: &Poll) -> Result<PollAnswer, TransportError> {
        if poll.signature.is_empty() {
            Ok(PollAnswer::Challenge(vec![7; 32]))
        } else {
            Ok(PollAnswer::Pending)
        }
    }

    fn renew(&mut self, _request: &RenewRequest) -> Result<RenewAnswer, TransportError> {
        Err(TransportError("not asked".to_owned()))
    }
}

/// A BMC that answers parameter 5 on channel 1 with [`BMC_MAC`], refuses every other question,
/// and keeps each (channel, parameter) it was asked.
#[derive(Default)]
struct FakeBmc {
    asked: Mutex<Vec<(u8, u8)>>,
}

impl FakeBmc {
    fn asked(&self) -> Vec<(u8, u8)> {
        self.asked.lock().expect("the questions").clone()
    }
}

impl Ipmi for FakeBmc {
    fn lan_parameter(&self, channel: u8, parameter: u8) -> io::Result<Vec<u8>> {
        self.asked
            .lock()
            .expect("the questions")
            .push((channel, parameter));
        if (channel, parameter) != (1, 5) {
            return Err(io::Error::other("no such channel"));
        }
        let mut answer = vec![0x00, 0x11];
        answer.extend_from_slice(&BMC_MAC);
        Ok(answer)
    }
}

/// An `ioreg` a Linux host never runs.
struct NoIoreg;

impl Ioreg for NoIoreg {
    fn platform_expert(&self) -> io::Result<String> {
        Err(io::Error::other("no ioreg on Linux"))
    }
}

/// An `ioreg` that prints one platform expert device.
struct OneMac;

impl Ioreg for OneMac {
    fn platform_expert(&self) -> io::Result<String> {
        Ok(concat!(
            "    \"IOPlatformSerialNumber\" = \"C02TEST0ABCD\"\n",
            "    \"IOPlatformUUID\" = \"12345678-90AB-CDEF-1234-567890ABCDEF\"\n",
        )
        .to_owned())
    }
}

/// The files of a Linux host under `root`: a machine id and one network interface with a MAC;
/// no DMI data.
fn linux_files(root: &Path) -> LinuxClaims {
    std::fs::write(root.join("machine-id"), format!("{MACHINE_ID}\n")).expect("the machine id");
    let eth0 = root.join("net").join("eth0");
    std::fs::create_dir_all(&eth0).expect("the interface");
    std::fs::write(eth0.join("address"), format!("{DATA_MAC}\n")).expect("its address");
    LinuxClaims::new(
        &root.join("machine-id"),
        &root.join("product_serial"),
        &root.join("product_uuid"),
        &root.join("net"),
    )
}

/// The enrollment requests a first enrollment step posts, with the claims of `claims`: the step
/// must end pending.
fn requests_posted_with(scratch: &Scratch, claims: Box<dyn ClaimsSource>) -> Vec<EnrollRequest> {
    let coordinator = Recording::default();
    let ports = hostkeeper_enroll::Ports {
        coordinator: Box::new(coordinator.clone()),
        store: Box::new(PkiDir::new(&scratch.path().join("pki"))),
        claims,
        entropy: Box::new(OsEntropy),
        wall: Box::new(SystemWallClock),
        clock: Box::new(hostkeeper_link::testing::FakeClock::new(0, 0)),
        managed: None,
    };
    let settings = EnrollSettings {
        ca_sha256: Sha256Hex::parse(&hex::encode(Sha256::digest(CA_DER))).expect("a SHA-256"),
        image: "hostkeeper-0.1.0".to_owned(),
    };
    let mut enroller = Enroller::open(settings, ports).expect("the enroller opens");
    assert_eq!(enroller.run_once().expect("the step"), Outcome::Pending);
    coordinator.enrolls()
}

/// Catches a daemon whose Linux claims are made without the BMC reader it is given (the
/// request then carries no BMC MAC), a BMC asked for another parameter than 5 or past the
/// channel that answered, and the BMC MAC taking the place of the host's other claims.
#[test]
fn a_linux_hosts_enrollment_request_carries_the_mac_its_bmc_answers_with() {
    let scratch = Scratch::new("bmc-claim-fake");
    let bmc = Arc::new(FakeBmc::default());
    let claims = host_claims(
        PlatformKind::Linux,
        linux_files(scratch.path()),
        Box::new(NoIoreg),
        Arc::clone(&bmc) as Arc<dyn Ipmi>,
    );
    let enrolls = requests_posted_with(&scratch, claims);
    assert_eq!(enrolls.len(), 1);
    assert_eq!(
        enrolls[0].claims,
        Claims {
            machine_id: Some(MACHINE_ID.to_owned()),
            network_addresses: vec![DATA_MAC.to_owned()],
            bmc_mac: Some(BMC_MAC_TEXT.to_owned()),
            ..Claims::default()
        }
    );
    assert_eq!(bmc.asked(), [(1, 5)]);
}

/// Catches an OpenRC host (Alpine) claimed as a Mac or with nothing: its claims are a Linux
/// host's, the machine id, the addresses and the MAC its BMC answers with.
#[test]
fn an_openrc_host_claims_as_a_linux_host() {
    let scratch = Scratch::new("bmc-claim-openrc");
    let bmc = Arc::new(FakeBmc::default());
    let claims = host_claims(
        PlatformKind::Openrc,
        linux_files(scratch.path()),
        Box::new(OneMac),
        Arc::clone(&bmc) as Arc<dyn Ipmi>,
    )
    .claims()
    .expect("the claims");
    assert_eq!(
        claims,
        Claims {
            machine_id: Some(MACHINE_ID.to_owned()),
            network_addresses: vec![DATA_MAC.to_owned()],
            bmc_mac: Some(BMC_MAC_TEXT.to_owned()),
            ..Claims::default()
        }
    );
    assert_eq!(bmc.asked(), [(1, 5)]);
}

/// Catches a host with no IPMI device whose enrollment fails or stops instead of posting its
/// request without a BMC MAC, and its other claims lost with the BMC's.
#[test]
fn a_linux_host_with_no_ipmi_device_enrolls_without_a_bmc_mac() {
    let scratch = Scratch::new("bmc-claim-no-device");
    let absent = scratch.path().join("ipmi0");
    let claims = host_claims(
        PlatformKind::Linux,
        linux_files(scratch.path()),
        Box::new(NoIoreg),
        Arc::new(DevIpmi::new(&absent)),
    );
    let enrolls = requests_posted_with(&scratch, claims);
    assert_eq!(enrolls.len(), 1);
    assert_eq!(
        enrolls[0].claims,
        Claims {
            machine_id: Some(MACHINE_ID.to_owned()),
            network_addresses: vec![DATA_MAC.to_owned()],
            bmc_mac: None,
            ..Claims::default()
        }
    );
    assert!(!absent.exists(), "asking an absent device creates nothing");
}

/// Catches a missing device, or a file that is no IPMI device, taken for a BMC that answered:
/// both are an error for every question, the missing one naming that it is not found.
#[test]
fn a_path_with_no_ipmi_device_behind_it_answers_nothing() {
    let scratch = Scratch::new("bmc-claim-not-a-device");
    let missing = DevIpmi::new(&scratch.path().join("ipmi0"));
    let error = missing.lan_parameter(1, 5).expect_err("no device");
    assert_eq!(error.kind(), io::ErrorKind::NotFound);

    let plain = scratch.path().join("plain-file");
    std::fs::write(&plain, [0u8; 64]).expect("a plain file");
    let not_a_device = DevIpmi::new(&plain);
    assert!(
        not_a_device.lan_parameter(1, 5).is_err(),
        "a file refuses the ioctl"
    );
    assert_eq!(std::fs::read(&plain).expect("the file"), [0u8; 64]);
}

/// Catches the host's reader pointed at another device than the kernel's first IPMI interface.
#[test]
fn the_hosts_bmc_is_behind_dev_ipmi0() {
    assert_eq!(IPMI_DEVICE, "/dev/ipmi0");
    assert_eq!(DevIpmi::host(), DevIpmi::new(Path::new("/dev/ipmi0")));
    assert_ne!(DevIpmi::host(), DevIpmi::new(Path::new("/dev/ipmi1")));
    assert!(format!("{:?}", DevIpmi::host()).contains("/dev/ipmi0"));
}

/// Catches Linux collectors run on a Mac: a Mac's claims ask no BMC and read none of the Linux
/// files they are given.
#[test]
fn a_mac_asks_no_bmc() {
    let scratch = Scratch::new("bmc-claim-mac");
    let bmc = Arc::new(FakeBmc::default());
    let claims = host_claims(
        PlatformKind::Macos,
        linux_files(scratch.path()),
        Box::new(OneMac),
        Arc::clone(&bmc) as Arc<dyn Ipmi>,
    )
    .claims()
    .expect("the claims");
    assert_eq!(
        claims,
        Claims {
            mac_serial: Some("C02TEST0ABCD".to_owned()),
            platform_uuid: Some("12345678-90AB-CDEF-1234-567890ABCDEF".to_owned()),
            ..Claims::default()
        }
    );
    assert_eq!(bmc.asked(), []);
}
