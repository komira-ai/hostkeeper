//! What the daemon's loops write for the admin `Status` (`hostkeeper_daemon::node`): the
//! heartbeat loop its link after each heartbeat, at the second of the links' clock, and the
//! enrollment loop the host's enrollment after each step; a loop that does not run writes
//! nothing.

mod support;

use std::io;
use std::sync::Arc;

use hostkeeper_config::daemon::Sha256Hex;
use hostkeeper_daemon::links::Links;
use hostkeeper_daemon::node::LinkHealth;
use hostkeeper_enroll::{
    CoordinatorEnroll, EnrollAnswer, EnrollRequest, Enroller, Enrollment, LinuxClaims, OsEntropy,
    PkiDir, Poll, PollAnswer, RenewAnswer, RenewRequest, Settings as EnrollSettings,
    SystemWallClock, TransportError,
};
use hostkeeper_link::testing::{FakeClock, FakeLink, ManualDispatch, ScriptedSleeper, identity};
use hostkeeper_link::{CertState, LinkError, Reply, StopFlag};
use support::{Rig, idle_links};

const NOW: u64 = 1_900_000_000;

/// Catches a heartbeat whose outcome does not reach `Status`, a second read from another clock,
/// or an enrollment written by a loop that has no enroller: an enrolled host whose first
/// heartbeat is answered and whose second is refused has the first's second as its last
/// success and one failure; the enrollment loop, whose enroller could not be made, leaves no
/// enrollment.
#[test]
fn the_heartbeat_loop_writes_its_link_after_each_heartbeat() {
    let rig = Rig::new("node-beat");
    let daemon = Arc::new(rig.daemon());
    let link = Arc::new(FakeLink::default());
    link.answer(Ok(Reply::default()));
    link.answer(Err(LinkError::Transport(io::ErrorKind::ConnectionRefused)));
    let dispatch = Arc::new(ManualDispatch::default());
    let links = Links {
        link: link.clone(),
        certs: Arc::new(hostkeeper_link::testing::FakeCerts::new(
            CertState::Enrolled(identity("a")),
        )),
        clock: Arc::new(FakeClock::new(NOW, 0)),
        sleeper: Box::new(ScriptedSleeper::new(1)),
        dispatch: dispatch.clone(),
        ..idle_links()
    };
    links.start(&daemon);
    assert!(dispatch.run_next(), "the enrollment loop");
    assert!(dispatch.run_next(), "the heartbeat loop");
    assert_eq!(link.calls().len(), 2);
    let node = &daemon.ctx().node;
    assert_eq!(
        node.link(),
        Some(LinkHealth {
            last_success_unix: Some(NOW),
            consecutive_failures: 1,
        })
    );
    assert_eq!(node.enrollment(), None);
}

/// A coordinator that cannot be reached, and that tells the loop to stop the first time it is
/// asked anything.
struct Down(StopFlag);

impl CoordinatorEnroll for Down {
    fn presented_ca_der(&mut self) -> Result<Vec<u8>, TransportError> {
        self.0.stop();
        Err(TransportError("down".to_owned()))
    }

    fn enroll(&mut self, _request: &EnrollRequest) -> Result<EnrollAnswer, TransportError> {
        Err(TransportError("not asked".to_owned()))
    }

    fn poll(&mut self, _poll: &Poll) -> Result<PollAnswer, TransportError> {
        Err(TransportError("not asked".to_owned()))
    }

    fn renew(&mut self, _request: &RenewRequest) -> Result<RenewAnswer, TransportError> {
        Err(TransportError("not asked".to_owned()))
    }
}

/// Catches a step whose enrollment does not reach `Status`, or a host reported pending before it
/// made a key: a host whose coordinator cannot be reached made no key, and its one step leaves
/// it unenrolled; a heartbeat loop that never beat leaves no link.
#[test]
fn the_enrollment_loop_writes_the_enrollment_after_its_step() {
    let rig = Rig::new("node-enroll");
    let daemon = Arc::new(rig.daemon());
    let stop = StopFlag::default();
    let claims = rig.scratch.path().join("no-claims");
    let ports = hostkeeper_enroll::Ports {
        coordinator: Box::new(Down(stop.clone())),
        store: Box::new(PkiDir::new(&rig.scratch.path().join("pki"))),
        claims: Box::new(LinuxClaims::new(&claims, &claims, &claims, &claims)),
        entropy: Box::new(OsEntropy),
        wall: Box::new(SystemWallClock),
        clock: Box::new(FakeClock::new(0, 0)),
    };
    let settings = EnrollSettings {
        ca_sha256: Sha256Hex::parse(&"00".repeat(32)).expect("a SHA-256"),
        image: "image-1".to_owned(),
    };
    let dispatch = Arc::new(ManualDispatch::default());
    let links = Links {
        enroller: Box::new(move || Enroller::open(settings, ports).map_err(|e| e.to_string())),
        dispatch: dispatch.clone(),
        stop,
        ..idle_links()
    };
    links.start(&daemon);
    assert!(dispatch.run_next(), "the enrollment loop");
    let node = &daemon.ctx().node;
    assert_eq!(node.enrollment(), Some(Enrollment::Unenrolled));
    assert_eq!(node.link(), None);
}
