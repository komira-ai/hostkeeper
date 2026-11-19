//! The BMC a host's heartbeat reports, end to end through the router as the listeners serve it:
//! the gRPC heartbeat on the host's certificate, then the report as the host's enrollment keeps it
//! for the console.

use hostkeeper_enrollment::enroll::KeptBmc;
use hostkeeper_enrollment::names::HostId;
use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use tonic::Code;

use super::heartbeat::{beat, peer_of, proto_message, svc};
use super::hosts::world_over;
use super::operator::World;
use crate::enrollment::tests::support::{HostKey, Rig as EnrollmentRig, enrolled};
use crate::testutil::OPERATOR;

/// The heartbeat of a daemon that runs kbf 1.0 and reports its BMC at `ip` with `mac`.
fn carrying(bmc: Option<(&str, &str)>) -> agent::HeartbeatRequest {
    let mut message = proto_message(&[svc("kbf", "1.0")]);
    message.bmc = bmc.map(|(ip, mac)| agent::BmcReport {
        ip: ip.to_owned(),
        mac: mac.to_owned(),
    });
    message
}

/// What the enrollment keeps of `s1`'s BMC: its address and MAC.
fn kept(enrollment: &EnrollmentRig) -> Option<(String, String)> {
    let host = HostId::parse("s1").expect("a host id");
    let enrolled = enrollment.enrollment.store().enrolled_host(&host);
    let bmc: Option<KeptBmc> = enrolled.expect("read").expect("enrolled").bmc;
    bmc.map(|kept| (kept.bmc.ip.to_string(), kept.bmc.mac.to_string()))
}

/// What the status counts of dropped facts, under `bmc`.
fn dropped(world: &World) -> Option<u64> {
    let status = world.coordinator.status(Some(OPERATOR)).expect("status");
    status.extras_dropped.get("bmc").copied()
}

/// Proves: the BMC a gRPC heartbeat reports is kept with the host's enrollment, from the host's
/// first heartbeat (which records what the host runs, the record the BMC is kept in); a later heartbeat
/// with none leaves it kept; a later one with another address replaces it; a report that breaks a
/// rule is answered OK like any heartbeat, counted under `bmc`, and leaves the one kept before.
///
/// Catches: a report lost between the heartbeat handler and the store (the console would never
/// learn the address), a first heartbeat's report read before what the host runs is recorded and
/// so dropped, a report cleared by a heartbeat without one (a daemon whose read found
/// nothing), a newer address that does not replace the older one, and a broken report kept or
/// refusing the heartbeat.
#[tokio::test]
async fn a_heartbeats_bmc_is_kept_with_the_hosts_enrollment() {
    let enrollment = EnrollmentRig::new("grpc-bmc-enrollment");
    let certificate = enrolled(&enrollment.enrollment, &HostKey::new(), "machine-1", "s1");
    let world = world_over("grpc-bmc", &enrollment);
    let peer = peer_of(&certificate);
    let send = |message: agent::HeartbeatRequest| {
        let coordinator = &world.coordinator;
        let peer = &peer;
        async move {
            assert_eq!(
                beat(coordinator, peer, &message).await.token(),
                (Code::Ok, "")
            );
        }
    };
    let at = |ip: &str| Some((ip.to_owned(), "0c:c4:7a:00:00:01".to_owned()));

    assert_eq!(kept(&enrollment), None);
    send(carrying(Some(("192.0.2.12", "0c:c4:7a:00:00:01")))).await;
    assert_eq!(kept(&enrollment), at("192.0.2.12"));
    send(carrying(None)).await;
    assert_eq!(kept(&enrollment), at("192.0.2.12"));
    send(carrying(Some(("192.0.2.40", "0c:c4:7a:00:00:01")))).await;
    assert_eq!(kept(&enrollment), at("192.0.2.40"));
    assert_eq!(dropped(&world), None);

    send(carrying(Some(("0.0.0.0", "0c:c4:7a:00:00:01")))).await;
    assert_eq!(kept(&enrollment), at("192.0.2.40"));
    assert_eq!(dropped(&world), Some(1));
}
