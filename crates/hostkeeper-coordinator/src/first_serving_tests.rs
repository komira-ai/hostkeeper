//! When an enrolled host first served: the coordinator keeps, with the host's enrollment, the
//! time by the enrollment's clock of the first heartbeat it accepted from the host since the
//! host's approval that reported a service it manages for the host (one that counts the host as
//! present, not the agent itself) healthy with no update of it under way, and `Hosts.Get`
//! answers it as `first_serving_unix`: the "serving" of the measurement of network install
//! (docs/design/network-install.md section 11, item N11). Later heartbeats leave it; a new
//! approval clears it until the host serves again.

use std::sync::Arc;

use hostkeeper_enrollment::fs::FaultFs;
use hostkeeper_proto::hostkeeper::agent::v1::EnrollState;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Agent, AgentName, Health, Heartbeat, ServiceBeat};

use crate::enrollment::os_random;
use crate::enrollment::tests::support::{HostKey, NOW, Rig as EnrollmentRig, enroll_request};
use crate::marks_support::{ask, operator, runs};
use crate::services::Services;
use crate::testutil::{Log, Rig, beat, version};

/// The agent's own service, and a service it runs.
const HK: &str = "hostkeeper";
const WEB: &str = "web";

/// When the operator approves `db-1`, by the enrollment's clock.
const APPROVED: u64 = NOW + 1_000;
/// When `db-1` heartbeats first.
const FIRST: u64 = NOW + 1_090;

/// What a deployed daemon announces.
const DEPLOYED: &[&str] = &[
    "deferred-v1",
    "halt-v1",
    "heartbeat-v2",
    "forced-v1",
    "order-v1",
    "verdict-acks-v1",
    "install-v1",
    "abort-v1",
];

/// hostkeeper and web, each on the role `storage`.
fn services() -> Services {
    Services::parse(&format!(
        "[[service]]\nname = \"{HK}\"\nroles = [\"storage\"]\n\n\
         [[service]]\nname = \"{WEB}\"\nroles = [\"storage\"]\n"
    ))
    .expect("the services")
}

/// A coordinator of hostkeeper and web, past its quiet start, over its own enrollment.
struct Fixture {
    enrollment: EnrollmentRig,
    rig: Rig,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        Fixture::over(EnrollmentRig::new(&format!("{name}-enrollment")), name)
    }

    fn over(enrollment: EnrollmentRig, name: &str) -> Fixture {
        let mut rig = Rig::new(name);
        let list = format!(
            "{}services = [\"{HK}\", \"{WEB}\"]\n",
            hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER)
        );
        rig.coordinator = Rig::start_with_allow_list(&rig.dir, &rig.clock, &list)
            .with_services(services())
            .with_legacy_service(None)
            .with_enrollment(Arc::clone(&enrollment.enrollment));
        rig.at(2_000);
        Fixture { enrollment, rig }
    }

    /// A new key that posted from machine `machine`, approved by the operator as `host` at
    /// `at` by the enrollment's clock, taking the id over when `replace`.
    fn approve(&self, machine: &str, host: &str, at: u64, replace: bool) {
        let key = HostKey::new();
        let posted = self
            .enrollment
            .enrollment
            .enroll(enroll_request(&key, machine))
            .expect("posted");
        assert_eq!(posted.state, EnrollState::Pending as i32);
        self.enrollment.clock.set(at);
        self.rig
            .coordinator
            .enrollment_approve(
                &operator(),
                proto::ApproveRequest {
                    request_id: key.key_hash().to_hex(),
                    key_sha256: key.key_hash().to_hex(),
                    host_id: host.to_owned(),
                    role: "storage".to_owned(),
                    reason: "racked".to_owned(),
                    replace,
                    ..proto::ApproveRequest::default()
                },
            )
            .expect("approved");
    }

    /// A heartbeat of `host` of `role` at `at` by the enrollment's clock, from a deployed
    /// daemon (the agent hostkeeper) listing `services`, which the coordinator accepts.
    fn beat(&self, host: &str, role: &str, at: u64, services: Vec<ServiceBeat>) {
        self.enrollment.clock.set(at);
        ask(&self.rig, host, role, daemon(services));
    }

    /// `host`'s first heartbeat and first serving times, as `Hosts.Get` answers them.
    fn times(&self, host: &str) -> (Option<u64>, Option<u64>) {
        let got = self
            .rig
            .coordinator
            .get_host(
                &operator(),
                &proto::GetHostRequest {
                    node_id: host.to_owned(),
                },
            )
            .expect("answered")
            .host
            .expect("a host");
        (got.first_heartbeat_unix, got.first_serving_unix)
    }
}

/// The heartbeat of a deployed daemon, the agent hostkeeper at 0.1.100, that lists `services`.
fn daemon(services: Vec<ServiceBeat>) -> Heartbeat {
    let mut b = beat("0.1.100");
    b.agent_features = DEPLOYED.iter().map(|f| (*f).to_owned()).collect();
    b.agent = Some(Agent {
        name: AgentName::parse(HK).expect("an agent name"),
        version: version("0.1.100"),
    });
    b.services = Some(services);
    b
}

/// `service` at 1.0, unhealthy.
fn unhealthy(service: &str) -> ServiceBeat {
    ServiceBeat {
        health: Health::Unhealthy,
        ..runs(service, "1.0")
    }
}

/// `service` at 1.0, healthy, with an update of it to 1.1 under way.
fn updating(service: &str) -> ServiceBeat {
    ServiceBeat {
        updating: Some(version("1.1")),
        ..runs(service, "1.0")
    }
}

/// Proves: the coordinator stamps the first heartbeat since the approval that reports a service
/// it manages for the host installed and healthy, and only that one: not a heartbeat with none
/// (only the agent itself, healthy), nor one where the service is unhealthy, being updated or
/// not the coordinator's; a later heartbeat that reports it serving leaves the time.
///
/// Catches: serving read from the first heartbeat (the agent itself is always healthy), from an
/// unhealthy or still-updating service, from a service the coordinator does not manage, or from
/// the latest heartbeat instead of the first.
#[test]
fn the_first_heartbeat_with_a_managed_service_serving_is_kept_and_later_ones_leave_it() {
    let f = Fixture::new("first-serving");
    f.approve("m-1", "db-1", APPROVED, false);
    f.beat("db-1", "storage", FIRST, vec![runs(HK, "0.1.100")]);
    assert_eq!(f.times("db-1"), (Some(FIRST), None), "only the agent");
    f.beat("db-1", "storage", FIRST + 15, vec![unhealthy(WEB)]);
    f.beat("db-1", "storage", FIRST + 30, vec![updating(WEB)]);
    f.beat("db-1", "storage", FIRST + 45, vec![runs("cache", "2.0")]);
    assert_eq!(f.times("db-1"), (Some(FIRST), None), "not serving yet");
    f.beat("db-1", "storage", FIRST + 60, vec![runs(WEB, "1.0")]);
    assert_eq!(f.times("db-1"), (Some(FIRST), Some(FIRST + 60)));
    f.beat("db-1", "storage", FIRST + 75, vec![runs(WEB, "1.0")]);
    assert_eq!(
        f.times("db-1"),
        (Some(FIRST), Some(FIRST + 60)),
        "a later beat"
    );
}

/// Proves: a service that does not count the host (the host's role is not among its roles) is
/// not the host serving, and a host that is not enrolled answers no time.
///
/// Catches: any healthy service of the coordinator read as one it manages for the host, and a
/// host of the farm CA (no enrollment) answered with a time.
#[test]
fn a_service_that_does_not_count_the_host_and_a_host_not_enrolled_set_nothing() {
    let f = Fixture::new("first-serving-not-counted");
    f.approve("m-1", "db-1", APPROVED, false);
    f.beat("db-1", "edge", FIRST, vec![runs(WEB, "1.0")]);
    assert_eq!(f.times("db-1"), (Some(FIRST), None), "not selected");
    f.beat("web-7", "storage", FIRST + 5, vec![runs(WEB, "1.0")]);
    assert_eq!(f.times("web-7"), (None, None), "not enrolled");
}

/// Proves: a new key approved for the host id (`replace`) clears the serving time, so the host
/// answers none until a heartbeat after that approval reports a managed service serving.
///
/// Catches: a re-installed host answering the serving time of its install before, which would
/// put serving before the approval it is answered with.
#[test]
fn a_new_approval_starts_the_first_serving_heartbeat_over() {
    let f = Fixture::new("first-serving-replaced");
    f.approve("m-1", "db-1", APPROVED, false);
    f.beat("db-1", "storage", FIRST, vec![runs(WEB, "1.0")]);
    assert_eq!(f.times("db-1"), (Some(FIRST), Some(FIRST)));
    let again = FIRST + 500;
    f.approve("m-9", "db-1", again, true);
    assert_eq!(f.times("db-1"), (None, None));
    f.beat("db-1", "storage", again + 30, vec![runs(HK, "0.1.100")]);
    f.beat("db-1", "storage", again + 90, vec![runs(WEB, "1.0")]);
    assert_eq!(f.times("db-1"), (Some(again + 30), Some(again + 90)));
}

/// Proves: when the store cannot record the serving time, the heartbeat is still answered, the
/// failure is logged, and the next heartbeat that reports serving records it.
///
/// Catches: a store failure that refuses the heartbeat, goes unlogged, or marks the host as
/// noted so the time is never recorded.
#[test]
fn a_serving_time_the_store_cannot_write_is_recorded_by_the_next_heartbeat() {
    let fs = Arc::new(FaultFs::new());
    let enrollment = EnrollmentRig::over("first-serving-fault-enrollment", fs.clone(), os_random());
    let f = Fixture::over(enrollment, "first-serving-fault");
    f.approve("m-1", "db-1", APPROVED, false);
    f.beat("db-1", "storage", FIRST, vec![runs(HK, "0.1.100")]);
    let log = Log::default();
    let guard = log.install();
    // Noting takes the store's lock (1), reads the host file (2), then writes it anew (3).
    fs.fail_at(fs.count() + 3);
    f.beat("db-1", "storage", FIRST + 60, vec![runs(WEB, "1.0")]);
    drop(guard);
    assert!(
        log.text()
            .contains("a first serving heartbeat was not recorded"),
        "{}",
        log.text()
    );
    assert_eq!(f.times("db-1"), (Some(FIRST), None));
    f.beat("db-1", "storage", FIRST + 75, vec![runs(WEB, "1.0")]);
    assert_eq!(f.times("db-1"), (Some(FIRST), Some(FIRST + 75)));
}
