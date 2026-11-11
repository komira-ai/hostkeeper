//! The service an agent is ([`crate::agent_service`]): a host whose agent names itself
//! `hostkeeper` runs the service `hostkeeper`, at the agent's version, whether or not its
//! heartbeat lists it; and the order path that brings such a host to the desired release, from
//! a daemon the deb or the pkg installed (no package of it listed) and from a daemon that
//! installed its own release without switching to it. The heartbeats are what the deployed
//! daemons send: every feature of their `ANNOUNCED`, the agent, and only the services whose
//! package they installed.

use hostkeeper_wire::wire::{
    Agent, AgentName, Health, Heartbeat, ORDER_FEATURE, Outcome, ServiceBeat, ServiceReply,
    UpdateResult,
};
use serde_json::{Value, json};

use crate::agent_service::{own_beat, runs_itself};
use crate::assign_support::why_of;
use crate::decide::{Skip, Why};
use crate::fleet::ORDER_AGE;
use crate::marks_support::{ask_typed, digest, native_target, who};
use crate::orders::{Order, install_order_id};
use crate::registry::Digest;
use crate::service::Coordinator;
use crate::services::Services;
use crate::testutil::{OPERATOR, Rig, beat, id, version};

const HK: &str = "hostkeeper";
const WEB: &str = "web";

/// What a 0.1.83 and a 0.1.92 daemon announce, in their order, and `target-v1` of a daemon of this
/// release.
const DEPLOYED: &[&str] = &[
    "deferred-v1",
    "halt-v1",
    "heartbeat-v2",
    "forced-v1",
    "order-v1",
    "verdict-acks-v1",
    "grpc-v1",
    "install-v1",
    "abort-v1",
    "target-v1",
];

/// hostkeeper and web on the role `daemon`, with `selector` lines under hostkeeper's entry.
fn services(selector: &str) -> Services {
    Services::parse(&format!(
        "[[service]]\nname = \"{HK}\"\nroles = [\"daemon\"]\n{selector}\n\n\
         [[service]]\nname = \"{WEB}\"\nroles = [\"daemon\"]\n"
    ))
    .expect("the services")
}

/// A rig past the quiet start running [`services`] with no legacy service, whose CI may set
/// both.
fn rig(test: &str, selector: &str) -> Rig {
    let mut rig = Rig::new(test);
    let list = format!(
        "{}services = [\"{HK}\", \"{WEB}\"]\n",
        hostkeeper_oidc::testkit::config_text(hostkeeper_oidc::testkit::STATIC_ISSUER)
    );
    rig.coordinator = Rig::start_with_allow_list(&rig.dir, &rig.clock, &list)
        .with_services(services(selector))
        .with_legacy_service(None);
    rig.at(2_000);
    rig
}

/// CI sets `service` to `to`, with the package digest [`digest`] 7.
async fn set(rig: &Rig, service: &str, to: &str) {
    let body = format!(
        r#"{{"service":"{service}","version":"{to}","release_digest":"{}"}}"#,
        digest(7)
    );
    rig.set(&body).await;
}

fn agent(running: &str) -> Agent {
    Agent {
        name: AgentName::parse(HK).expect("an agent name"),
        version: version(running),
    }
}

/// The heartbeat of a deployed daemon of version `running` that lists `services`.
fn daemon(running: &str, services: Vec<ServiceBeat>) -> Heartbeat {
    let mut b = beat(running);
    b.agent_features = DEPLOYED.iter().map(|f| (*f).to_owned()).collect();
    b.agent = Some(agent(running));
    b.services = Some(services);
    b
}

/// An entry of `service` at `listed`, healthy, with `last` as its last update.
fn entry(service: &str, listed: &str, last: Option<UpdateResult>) -> ServiceBeat {
    let mut one: ServiceBeat =
        serde_json::from_value(json!({"name": service, "version": listed, "health": "healthy"}))
            .expect("a service beat");
    one.last_update = last;
    one
}

/// A verdict of `outcome` of the order of try `order_try` to `target`.
fn verdict(target: &str, outcome: Outcome, order_try: u64) -> UpdateResult {
    UpdateResult {
        order_try: Some(order_try),
        ..UpdateResult::new(version(target), outcome)
    }
}

/// A refusal of an install of `target` for `reason`: failed, no phase, no class.
fn refusal(target: &str, reason: &str) -> UpdateResult {
    UpdateResult {
        reason: hostkeeper_wire::wire::Token::parse(reason),
        ..UpdateResult::new(version(target), Outcome::Failed)
    }
}

/// An entry of `service` at `listed` with `health` and `last` as its last update.
fn entry_with(health: Health, last: Option<UpdateResult>) -> ServiceBeat {
    let mut one = entry(HK, "0.2.0", last);
    one.health = health;
    one
}

/// The convergence of `service`, as JSON.
fn convergence(coordinator: &Coordinator, service: &str) -> Value {
    let status = coordinator.status(Some(OPERATOR)).expect("status");
    serde_json::to_value(&status.per_service[service].convergence).expect("json")
}

/// The version the fleet counts `host` at for `service`.
fn counted_at(rig: &Rig, service: &str, host: &str) -> Option<String> {
    rig.coordinator
        .lock_fleet()
        .get_in(service, &id(host))
        .map(|h| h.beat.version.as_str().to_owned())
}

/// The install of `service` at `to` from the package of [`digest`] 7, at `order_try`.
fn install(service: &str, to: &str, order_try: u64) -> Order {
    let digest = Digest::parse(&digest(7)).expect("a digest");
    Order::Install {
        target: Some(native_target(service, 7)),
        service: service.to_owned(),
        version: version(to),
        order_id: install_order_id(&digest),
        digest,
        index_digest: None,
        order_try,
    }
}

/// Catches a fleet built by bootstrap that never self-updates: a daemon the deb or the pkg
/// installed lists no service `hostkeeper` (no package of it is installed), and the coordinator
/// counted it `missing`, never at the version it runs. It is one of hostkeeper's hosts at its
/// agent's version (0.1.83), the convergence leaves no host out (no `hosts` block), the
/// service is in progress while the host is behind, and the host is ordered the install of the
/// desired release, the order a daemon with no package of hostkeeper takes.
#[tokio::test]
async fn a_bootstrapped_host_runs_its_agents_service_at_the_agents_version_and_is_installed_it() {
    let rig = rig("agent-bootstrapped", "");
    set(&rig, HK, "0.2.0").await;

    let (reply, order) = ask_typed(&rig, "d1", "daemon", daemon("0.1.83", vec![]));
    assert_eq!(reply, ServiceReply::Nothing);
    assert_eq!(order, Some(install(HK, "0.2.0", 1)));
    assert_eq!(counted_at(&rig, HK, "d1").as_deref(), Some("0.1.83"));
    let c = convergence(&rig.coordinator, HK);
    assert_eq!(c["state"], "in_progress", "{c}");
    assert_eq!(c["hosts"], Value::Null, "{c}");
}

/// Catches a selector that leaves the bootstrapped host `missing` (or `unmanaged`) and drops it
/// from the service's hosts: with a selector naming its role, the host is counted with no host
/// left out, the `hosts` block saying so, and is ordered the install.
#[tokio::test]
async fn a_selector_counts_a_bootstrapped_host_it_selects() {
    let rig = rig(
        "agent-bootstrapped-selector",
        "[service.selector]\nroles = [\"daemon\"]\n",
    );
    set(&rig, HK, "0.2.0").await;

    let (_, order) = ask_typed(&rig, "d1", "daemon", daemon("0.1.92", vec![]));
    assert_eq!(order, Some(install(HK, "0.2.0", 1)));
    assert_eq!(counted_at(&rig, HK, "d1").as_deref(), Some("0.1.92"));
    let c = convergence(&rig.coordinator, HK);
    assert_eq!(
        c["hosts"],
        json!({"missing": 0, "absent": 0, "excluded": 0, "unmanaged": 0, "deferred": 0}),
        "{c}"
    );
}

/// Catches an old daemon ordered, again and again, the update to the release it installed but
/// cannot switch to: a daemon before the install of hostkeeper ran the self-update installs the
/// package and keeps running its binary, and its `update` to the release `current` already
/// names replaces no release, so its success is written at once and the binary never moves.
/// After the install of 0.2.0 succeeded (its verdict reported), the daemon lists hostkeeper at
/// 0.2.0 while its agent is 0.1.83: the host is counted at 0.1.83, is told nothing, and the gate
/// says `unsupported`; the service stays in progress. The next release (0.3.0) is ordered as an
/// update, one try past the install, which such a daemon runs as its self-update, since it
/// replaces the release installed.
#[tokio::test]
async fn an_agent_that_installed_its_release_without_running_it_waits_for_the_next() {
    let rig = rig("agent-installed-not-run", "");
    set(&rig, HK, "0.2.0").await;
    let installed = || {
        daemon(
            "0.1.83",
            vec![entry(
                HK,
                "0.2.0",
                Some(verdict("0.2.0", Outcome::Succeeded, 1)),
            )],
        )
    };

    let (reply, order) = ask_typed(&rig, "d1", "daemon", installed());
    assert_eq!((reply, order), (ServiceReply::Nothing, None));
    assert_eq!(why_of(&rig, HK, "d1"), Some(Why::Skip(Skip::Unsupported)));
    assert_eq!(counted_at(&rig, HK, "d1").as_deref(), Some("0.1.83"));
    assert_eq!(convergence(&rig.coordinator, HK)["state"], "in_progress");

    set(&rig, HK, "0.3.0").await;
    let answer = rig
        .coordinator
        .heartbeat_with_orders_native(
            who("d1", "daemon"),
            installed(),
            crate::trusted_root::TrustedRootSaid::default(),
        )
        .expect("answered")
        .0;
    assert_eq!(
        answer.reply,
        ServiceReply::Update {
            service: Some(HK.to_owned()),
            version: version("0.3.0"),
        }
    );
    assert_eq!(answer.order_try, Some(2));
}

/// Catches an old daemon taken for one that installed its release when its last verdict is of
/// another release or is no success: a daemon whose agent runs 0.1.83 and lists hostkeeper at
/// 0.2.0 after a verdict of 0.1.9, succeeded or failed, is still ordered the update to 0.2.0.
#[tokio::test]
async fn a_verdict_of_another_release_or_no_success_does_not_hold_an_old_daemon() {
    for (name, outcome) in [
        ("agent-verdict-other-succeeded", Outcome::Succeeded),
        ("agent-verdict-other-failed", Outcome::Failed),
    ] {
        let rig = rig(name, "");
        set(&rig, HK, "0.2.0").await;
        let (reply, _) = ask_typed(
            &rig,
            "d1",
            "daemon",
            daemon(
                "0.1.83",
                vec![entry(HK, "0.2.0", Some(verdict("0.1.9", outcome, 1)))],
            ),
        );
        assert_eq!(
            reply,
            ServiceReply::Update {
                service: Some(HK.to_owned()),
                version: version("0.2.0"),
            },
            "{name}"
        );
    }
}

/// Catches a host counted at the version of the package its daemon lists instead of the binary
/// that runs (after a self-update fell back, or before an old daemon switched), and the rule of
/// the agent's own service applied to another service: a daemon whose agent runs 0.2.0 and lists
/// hostkeeper at 0.2.0 is up to date, one whose agent runs 0.1.83 and lists it at 0.2.0 with no
/// verdict is counted at 0.1.83 and ordered the update, and `web`, listed at 1.0 with a
/// succeeded install of 2.0 on the same host, is counted at 1.0 and ordered its update.
#[tokio::test]
async fn the_agents_own_service_is_counted_at_the_agents_version_and_no_other() {
    let rig = rig("agent-version", "");
    set(&rig, HK, "0.2.0").await;
    set(&rig, WEB, "2.0").await;

    let (reply, _) = ask_typed(
        &rig,
        "d1",
        "daemon",
        daemon(
            "0.2.0",
            vec![entry(HK, "0.2.0", None), entry(WEB, "2.0", None)],
        ),
    );
    assert_eq!(reply, ServiceReply::Nothing);
    assert_eq!(why_of(&rig, HK, "d1"), Some(Why::Skip(Skip::UpToDate)));

    let (reply, _) = ask_typed(
        &rig,
        "d2",
        "daemon",
        daemon("0.1.83", vec![entry(HK, "0.2.0", None)]),
    );
    assert_eq!(
        reply,
        ServiceReply::Update {
            service: Some(HK.to_owned()),
            version: version("0.2.0"),
        }
    );
    assert_eq!(counted_at(&rig, HK, "d2").as_deref(), Some("0.1.83"));

    rig.at(2_000 + ORDER_AGE.as_secs() * 10);
    let (reply, _) = ask_typed(
        &rig,
        "d3",
        "daemon",
        daemon(
            "0.1.83",
            vec![entry(
                WEB,
                "1.0",
                Some(verdict("2.0", Outcome::Succeeded, 1)),
            )],
        ),
    );
    assert_eq!(counted_at(&rig, WEB, "d3").as_deref(), Some("1.0"));
    assert_eq!(
        reply,
        ServiceReply::Update {
            service: Some(WEB.to_owned()),
            version: version("2.0"),
        }
    );
}

/// Catches a host taken for its agent's service when the agent is not that service: a heartbeat
/// with no agent, an agent that did not announce `order-v1` (its top level is the legacy
/// service's), and an agent whose name is no service the coordinator manages leave the heartbeat
/// as it was, and their node does not run the service.
#[test]
fn only_an_order_v1_agent_named_after_a_managed_service_runs_it() {
    let defined = services("");
    let listed = || daemon("0.1.83", vec![entry(HK, "0.2.0", None)]);

    let mut no_agent = listed();
    no_agent.agent = None;
    let mut no_order = listed();
    no_order.agent_features.retain(|f| f != ORDER_FEATURE);
    let mut unmanaged = listed();
    unmanaged.agent = Some(Agent {
        name: AgentName::parse("updater").expect("a name"),
        version: version("0.1.83"),
    });
    for mut b in [no_agent, no_order, unmanaged] {
        let before = b.clone();
        assert_eq!(own_beat(&defined, &mut b), None);
        assert_eq!(b, before);
    }

    let mut b = daemon("0.1.83", vec![]);
    let own = own_beat(&defined, &mut b).expect("the agent's own entry");
    assert_eq!(own, entry(HK, "0.1.83", None));
    assert_eq!(b.services, Some(vec![]));

    let mut b = listed();
    assert_eq!(own_beat(&defined, &mut b), None);
    assert_eq!(b.services, Some(vec![entry(HK, "0.1.83", None)]));

    let mut node = crate::fleet::Node {
        role: "daemon".to_owned(),
        features: DEPLOYED.iter().map(|f| (*f).to_owned()).collect(),
        agent: Some(agent("0.1.83")),
        platform: hostkeeper_wire::Platform::LinuxX86_64,
        seen: std::time::Duration::ZERO,
        announced: Default::default(),
        sent_unix: None,
        cert_not_after_unix: None,
        last_renew_error: None,
        trusted_root: crate::trusted_root::TrustedRootSaid::default(),
        skewed_beats: 0,
    };
    assert!(runs_itself(&node, HK));
    assert!(!runs_itself(&node, WEB));
    node.features.retain(|f| f != ORDER_FEATURE);
    assert!(!runs_itself(&node, HK));
    node.features.push(ORDER_FEATURE.to_owned());
    node.agent = None;
    assert!(!runs_itself(&node, HK));
}

/// Catches an install ordered at a try the host already holds, for ever: a host that never
/// reports a verdict of an install (refused `try_conflict` because its step record holds try 1
/// of an earlier install, for a service its heartbeat does not list) was ordered the next
/// install at try 1 again, since the coordinator numbered tries from verdicts it heard only. An
/// order asked again within [`ORDER_AGE`] keeps its try; once it aged out with no word, the next
/// is one try past it; and an order replaced by one of another version (a new desired version
/// while the first is outstanding) is one past it too.
#[tokio::test]
async fn an_order_with_no_verdict_is_followed_one_try_past_it() {
    let rig = rig("agent-tries", "");
    set(&rig, HK, "0.2.0").await;
    let ordered = |rig: &Rig| ask_typed(rig, "d1", "daemon", daemon("0.1.83", vec![])).1;

    assert_eq!(ordered(&rig), Some(install(HK, "0.2.0", 1)));
    assert_eq!(ordered(&rig), Some(install(HK, "0.2.0", 1)));
    rig.at(2_000 + ORDER_AGE.as_secs());
    assert_eq!(ordered(&rig), Some(install(HK, "0.2.0", 2)));

    set(&rig, HK, "0.2.1").await;
    assert_eq!(ordered(&rig), Some(install(HK, "0.2.1", 3)));
}

/// Catches an order given the try of an outstanding order of the other kind to the same version:
/// an uninstall's target is the version the host runs, so an install of that version given while
/// the uninstall is outstanding (the service assigned again) would go at the uninstall's try,
/// which the daemon's step record holds for another order and refuses `try_conflict`. The
/// uninstall asked again keeps its try; an install or an update of that version takes the next.
#[test]
fn an_order_of_the_other_kind_to_the_same_version_takes_the_next_try() {
    let mut fleet = crate::fleet::Fleet::new(std::time::Duration::ZERO, std::time::Duration::ZERO);
    let now = std::time::Duration::from_secs(10);
    let running = version("0.2.0");
    fleet.order_uninstall(
        WEB,
        id("d1"),
        running.clone(),
        "uninstall-1".to_owned(),
        now,
    );
    assert_eq!(fleet.order_try(WEB, &id("d1"), &running, true, now), 1);
    assert_eq!(fleet.order_try(WEB, &id("d1"), &running, false, now), 2);
    fleet.order(WEB, id("d1"), running.clone(), now);
    assert_eq!(fleet.order_try(WEB, &id("d1"), &running, false, now), 2);
}

/// Catches an install refused `already_installed` counted as a failed update of a host that runs
/// the desired version: a daemon that lists its own service present at that version, with the
/// refusal as its last update, is ordered nothing, up to date, and the service converged.
#[tokio::test]
async fn an_already_installed_refusal_at_the_desired_version_is_no_failure_of_the_host() {
    let rig = rig("agent-already-installed", "");
    set(&rig, HK, "0.2.0").await;
    let refused = entry_with(Health::Healthy, Some(refusal("0.2.0", "already_installed")));

    let (reply, order) = ask_typed(&rig, "d1", "daemon", daemon("0.2.0", vec![refused]));

    assert_eq!((reply, order), (ServiceReply::Nothing, None));
    assert_eq!(why_of(&rig, HK, "d1"), Some(Why::Skip(Skip::UpToDate)));
    let c = convergence(&rig.coordinator, HK);
    assert_eq!(c["state"], "converged", "{c}");
}
