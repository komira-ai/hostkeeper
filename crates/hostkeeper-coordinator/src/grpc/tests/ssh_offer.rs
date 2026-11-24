//! A daemon's heartbeat loop from its first heartbeat (`hostkeeper_link::Heartbeater`, with an
//! sshd setup) against this coordinator's `Coordinator.Heartbeat`, through `crate::grpc::route`:
//! how `ssh-v1` comes to be agreed when neither side names it first ([`crate::features::OFFERED`]).
//! No socket: each heartbeat of the loop is sent in process, as the host `node-07`.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_config::names::Version;
use hostkeeper_link::testing::World as Host;
use hostkeeper_link::{
    ANNOUNCED, BeatOutcome, ClientIdentity, CoordinatorLink, DEFAULT_HEARTBEAT_SECS, Feature,
    Heartbeat, Heartbeater, LinkError, LoopConfig, Platform, PushedSsh, Reply, SSH, SshReport,
    SshSink,
};
use hostkeeper_proto::hostkeeper::agent::v1 as agent;

use super::heartbeat::{beat, proto_message, svc, updater};
use crate::http::Peer;
use crate::service::Coordinator;
use crate::ssh_heartbeat::FEATURE;
use crate::ssh_sign::tests::{OPERATORS, Tune, World, world};

/// The `SHA256:` fingerprint of the host key the setup reports.
const HOST_KEY: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

/// The daemon's sshd setup, as the loop sees it: every access it is given, and a report of the
/// last one applied (none before the first), as the daemon's setup reports a record it wrote.
#[derive(Default)]
struct Setup {
    applied: Mutex<Vec<PushedSsh>>,
}

impl Setup {
    fn applied(&self) -> Vec<PushedSsh> {
        self.applied
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl SshSink for Setup {
    fn report(&self) -> Option<SshReport> {
        self.applied().last().map(|access| SshReport {
            applied_generation: access.generation,
            state: "applied".to_owned(),
            host_key_fingerprints: vec![HOST_KEY.to_owned()],
            detail: String::new(),
        })
    }

    fn apply(&self, access: &PushedSsh) {
        self.applied
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(access.clone());
    }
}

/// The loop's link to `coordinator`, in process: each heartbeat goes to `Coordinator.Heartbeat`
/// as the host `node-07`, carrying the loop's features and SSH report; the answer comes back with
/// its features and SSH access. Each request and answer is kept, in order.
struct InProcess {
    coordinator: Arc<Coordinator>,
    runtime: tokio::runtime::Handle,
    peer: Peer,
    exchanged: Mutex<Vec<(agent::HeartbeatRequest, agent::HeartbeatResponse)>>,
}

impl InProcess {
    fn exchanged(&self) -> Vec<(agent::HeartbeatRequest, agent::HeartbeatResponse)> {
        self.exchanged
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl CoordinatorLink for InProcess {
    fn heartbeat(&self, _identity: &ClientIdentity, sent: &Heartbeat) -> Result<Reply, LinkError> {
        let mut request = proto_message(&[svc("web", "1.0")]);
        request.features = sent
            .features
            .iter()
            .map(|feature| feature.as_str().to_owned())
            .collect();
        request.ssh = sent.ssh.as_ref().map(|report| agent::SshReport {
            applied_generation: report.applied_generation,
            state: report.state.clone(),
            host_key_fingerprints: report.host_key_fingerprints.clone(),
            detail: report.detail.clone(),
        });
        let answer: agent::HeartbeatResponse = self
            .runtime
            .block_on(beat(&self.coordinator, &self.peer, &request))
            .decode();
        self.exchanged
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((request, answer.clone()));
        Ok(Reply {
            features: answer
                .features
                .iter()
                .filter_map(|name| Feature::parse(name))
                .collect(),
            ssh: answer.ssh.map(|access| PushedSsh {
                generation: access.generation,
                enabled: access.enabled,
                ca_public_keys: access.ca_public_keys,
                login_user: access.login_user,
                principal: access.principal,
                revoked_serials: access.revoked_serials,
                password_auth: access.password_auth,
            }),
            ..Reply::default()
        })
    }
}

/// A daemon on Linux x86-64 whose state directory is under `dir`, with an sshd setup and no other
/// optional port, beating to `coordinator` from its first heartbeat.
struct Daemon {
    heartbeater: Heartbeater,
    link: Arc<InProcess>,
    setup: Arc<Setup>,
    _host: Host,
}

impl Daemon {
    fn new(coordinator: Coordinator, runtime: &tokio::runtime::Runtime, dir: &Path) -> Daemon {
        let host = Host::default();
        let link = Arc::new(InProcess {
            coordinator: Arc::new(coordinator),
            runtime: runtime.handle().clone(),
            peer: updater("node-07", "web"),
            exchanged: Mutex::default(),
        });
        let setup = Arc::new(Setup::default());
        let mut ports = host.ports();
        ports.link = Arc::clone(&link) as Arc<dyn CoordinatorLink>;
        ports.ssh = Some(Arc::clone(&setup) as Arc<dyn SshSink>);
        let config = LoopConfig {
            platform: Platform::LinuxX86_64,
            agent_version: Version::new("0.9.0").expect("a version"),
            state_dir: dir.join("daemon-state"),
            period_secs: DEFAULT_HEARTBEAT_SECS,
            installers: Vec::new(),
            private_repository: false,
        };
        Daemon {
            heartbeater: Heartbeater::new(config, ports),
            link,
            setup,
            _host: host,
        }
    }

    /// Beats `n` times, each heartbeat answered.
    fn beat(&mut self, n: usize) {
        for _ in 0..n {
            assert_eq!(self.heartbeater.beat_once(), BeatOutcome::Sent);
        }
    }
}

/// The SSH signing world `name`: a coordinator that holds an SSH user CA and enrolled `node-07`.
fn signing(name: &str) -> World {
    world(name, Some(OPERATORS), Tune::default())
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("a runtime")
}

/// Whether `features` holds `ssh-v1`.
fn has_ssh(features: &[String]) -> bool {
    features.iter().any(|name| name == FEATURE)
}

/// Proves: a daemon with an sshd setup, from its first heartbeat to a coordinator that holds an
/// SSH user CA, names no `ssh-v1` in that heartbeat and is offered it in the answer, which carries
/// no access; names it in the second and is answered the access of its host (enabled, the fleet's
/// two CA keys, its principal), which the setup applies once; and reports that generation applied
/// in the third, whose answer carries no access and still agrees `ssh-v1`, while the coordinator
/// keeps the report.
///
/// Catches: a coordinator that echoes `ssh-v1` only when named, so that a daemon, which names it
/// only once a reply advertised it, never sets sshd up.
#[test]
fn a_daemon_from_its_first_heartbeat_reaches_ssh_v1_and_applies_the_access() {
    let world = signing("grpc-ssh-offer-cold");
    let runtime = runtime();
    let mut daemon = Daemon::new(world.coordinator, &runtime, world._rig.dir.path());

    daemon.beat(3);

    let exchanged = daemon.link.exchanged();
    let (first, first_answer) = &exchanged[0];
    let announced: Vec<String> = ANNOUNCED.iter().map(|f| f.as_str().to_owned()).collect();
    assert_eq!(
        first.features, announced,
        "the first heartbeat names no ssh-v1"
    );
    assert_eq!(first.ssh, None);
    assert!(
        has_ssh(&first_answer.features),
        "{:?}",
        first_answer.features
    );
    assert_eq!(
        first_answer.ssh, None,
        "no access to a daemon that did not name it"
    );

    let (second, second_answer) = &exchanged[1];
    assert_eq!(
        second.features.last().map(String::as_str),
        Some(SSH.as_str())
    );
    assert_eq!(second.ssh, None, "nothing applied yet");
    assert!(has_ssh(&second_answer.features));
    let access = second_answer.ssh.clone().expect("the access");
    assert!(access.enabled);
    assert_eq!(access.principal, "hk-host-node-07");
    assert_eq!(access.ca_public_keys, world.fleet);
    assert_eq!(access.login_user, "root");

    let applied = daemon.setup.applied();
    assert_eq!(applied.len(), 1, "applied once");
    assert_eq!(applied[0].generation, access.generation);
    assert_eq!(applied[0].ca_public_keys, access.ca_public_keys);
    assert_eq!(applied[0].principal, access.principal);

    let (third, third_answer) = &exchanged[2];
    assert_eq!(
        third.ssh.as_ref().map(|report| report.applied_generation),
        Some(access.generation)
    );
    assert!(has_ssh(&third_answer.features));
    assert_eq!(
        third_answer.ssh, None,
        "the applied generation is not sent again"
    );
    let kept = daemon
        .link
        .coordinator
        .ssh_reports
        .report_of(&crate::testutil::id("node-07"))
        .expect("the report is kept");
    assert_eq!(kept.applied_generation, access.generation);
    assert_eq!(kept.state, "applied");
}

/// Proves: the same daemon against the same coordinator without an SSH user CA is never offered
/// `ssh-v1`, never names it, is sent no access and applies none, while `maintenance-v1` is still
/// offered to it.
///
/// Catches: `ssh-v1` offered by a coordinator that can send no access, so that every daemon names
/// it, and reports to it, for nothing.
#[test]
fn a_coordinator_without_an_ssh_ca_offers_no_ssh_v1() {
    let world = signing("grpc-ssh-offer-no-ca");
    let mut coordinator = world.coordinator;
    coordinator.ssh = None;
    let runtime = runtime();
    let mut daemon = Daemon::new(coordinator, &runtime, world._rig.dir.path());

    daemon.beat(3);

    for (request, answer) in daemon.link.exchanged() {
        assert!(!has_ssh(&request.features), "{:?}", request.features);
        assert_eq!(request.ssh, None);
        assert!(!has_ssh(&answer.features), "{:?}", answer.features);
        assert!(
            answer
                .features
                .iter()
                .any(|name| name == crate::maintenance::FEATURE),
            "{:?}",
            answer.features
        );
        assert_eq!(answer.ssh, None);
    }
    assert!(daemon.setup.applied().is_empty());
}

/// Proves the features a gRPC heartbeat is answered beside those it named: with an SSH user CA,
/// `ssh-v1` and `maintenance-v1`, in [`crate::features::FEATURES`]' order, to an agent that named
/// neither; without the CA `maintenance-v1` alone, yet `ssh-v1` still to an agent that named it; to
/// a coordinator that does not advertise `ssh-v1`, never `ssh-v1`, CA or not; to an agent that
/// announced no feature, nothing.
///
/// Catches: `ssh-v1` offered unnamed without a CA, or by a coordinator that does not advertise it,
/// and a daemon that named it refused it when the CA is gone.
#[test]
fn ssh_v1_is_offered_unnamed_only_with_a_ca_and_while_advertised() {
    let names = |list: &[&str]| -> Vec<String> { list.iter().map(|n| (*n).to_owned()).collect() };
    let announced = names(&["halt-v1"]);
    let maintenance = crate::maintenance::FEATURE;
    let world = signing("grpc-ssh-offer-rule");
    let mut coordinator = world.coordinator;

    assert_eq!(
        coordinator.offering(&announced, &[]),
        names(&[FEATURE, maintenance])
    );
    assert_eq!(
        coordinator.offering(&announced, &names(&["halt-v1"])),
        names(&["halt-v1", FEATURE, maintenance])
    );
    assert_eq!(coordinator.offering(&[], &[]), Vec::<String>::new());

    coordinator.ssh = None;
    assert_eq!(coordinator.offering(&announced, &[]), names(&[maintenance]));
    assert_eq!(
        coordinator.offering(&names(&[FEATURE]), &names(&[FEATURE])),
        names(&[FEATURE, maintenance])
    );

    let world = signing("grpc-ssh-offer-unadvertised");
    let unadvertised = world.coordinator.with_features(&["halt-v1", maintenance]);
    assert_eq!(
        unadvertised.offering(&announced, &[]),
        names(&[maintenance])
    );
    assert!(crate::features::offered(FEATURE, true));
    assert!(!crate::features::offered(FEATURE, false));
    assert!(crate::features::offered(maintenance, false));
    assert!(!crate::features::offered("halt-v1", true));
}
