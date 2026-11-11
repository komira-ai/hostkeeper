//! A move and the host's facts ([`crate::moving`], [`crate::facts`]): the move raises the host's
//! facts generation, the facts sent name the new group, and a host whose daemon announces
//! `facts-v1` is ordered nothing, after its admission, until its heartbeat reports the
//! generation. The tests heartbeat over the route that carries the facts: the daemon's words
//! about the facts are heard first ([`crate::service::Coordinator::hear_facts`]), then the
//! heartbeat is decided.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1::HostGroupsRefusal as Refusal;

use crate::facts::FEATURE;
use crate::group_decision_support::Farm;
use crate::marks_support::who;
use crate::membership_tests::labels;
use crate::moving_tests::{MOVING, OLD, farm_of, membership, say, told, wide};
use crate::store::audit_failure;
use crate::testutil::{Rig, id};

/// What a daemon says of the facts in a heartbeat over gRPC: it announces `facts-v1` or not, and
/// reports the generation it wrote, if any.
fn request(announces: bool, reported: Option<u64>) -> agent::HeartbeatRequest {
    agent::HeartbeatRequest {
        features: announces.then(|| FEATURE.to_owned()).into_iter().collect(),
        facts_generation: reported,
        ..agent::HeartbeatRequest::default()
    }
}

/// The generation of the facts of `m1`.
fn generation(farm: &Farm) -> u64 {
    farm.world.coordinator().ops().facts_generation("m1")
}

/// The facts the answer to a heartbeat of `m1` carries, for a daemon that says `say`.
fn facts_to(farm: &Farm, say: &agent::HeartbeatRequest) -> Option<agent::HostFacts> {
    farm.world
        .coordinator()
        .host_facts(&who("m1", "storage"), say)
}

/// What `m1` is told by a heartbeat that lists `running`, from a daemon that says `say`.
fn told_by(farm: &Farm, running: &[(&str, &str)], say: &agent::HeartbeatRequest) -> Option<String> {
    farm.world
        .coordinator()
        .hear_facts(&who("m1", "storage"), say);
    told(farm, "m1", running)
}

/// The number of the newest audit entry.
fn newest(farm: &Farm) -> u64 {
    farm.world.audit().last().expect("an entry").seq
}

/// Proves: a move raises the generation of the moved host's facts to the number of its
/// `host_move` entry and no other host's; the facts a daemon that reported the generation before
/// is sent carry the new generation and name the new group, and none are sent to a daemon that
/// reported the new one. Before the move the facts name the group of the host's record.
///
/// Catches: a move that leaves the host's facts naming the old group, or one that does not
/// resend them.
#[tokio::test]
async fn a_move_raises_the_generation_and_the_facts_name_the_new_group() {
    let farm = farm_of("mvf-raise", wide()).await;
    let before = generation(&farm);
    let others = farm.world.coordinator().ops().facts_generation("m2");
    let first = facts_to(&farm, &request(true, None)).expect("facts");
    assert_eq!(
        (first.generation, first.host_group.as_str()),
        (before, "old")
    );
    assert_eq!(facts_to(&farm, &request(true, Some(before))), None);

    farm.world.move_to("m1", "new").expect("moved");

    let raised = newest(&farm);
    assert!(raised > before);
    assert_eq!(generation(&farm), raised);
    assert_eq!(
        farm.world.coordinator().ops().facts_generation("m2"),
        others
    );
    assert_eq!(
        facts_to(&farm, &request(true, Some(before))),
        Some(agent::HostFacts {
            generation: raised,
            host_id: "m1".to_owned(),
            host_group: "new".to_owned(),
            labels: Default::default(),
        })
    );
    assert_eq!(facts_to(&farm, &request(true, Some(raised))), None);
}

/// Proves: a move that is refused because its commit fails leaves the host's generation where it
/// was and its facts naming the old group, and the same move, once the store takes commits again,
/// raises it.
///
/// Catches: a generation raised, or a group in the facts, for a move that did not happen.
#[tokio::test]
async fn a_commit_that_fails_leaves_the_generation_and_the_facts_as_they_were() {
    let farm = farm_of("mvf-refused", wide()).await;
    let before = generation(&farm);
    farm.world.fake.refuse(|_| Some(audit_failure()));

    assert_eq!(
        farm.world.move_to("m1", "new").expect_err("refused"),
        Refusal::AuditUnavailable
    );

    assert_eq!(generation(&farm), before);
    assert_eq!(facts_to(&farm, &request(true, Some(before))), None);
    assert_eq!(
        facts_to(&farm, &request(true, None)).map(|facts| facts.host_group),
        Some("old".to_owned())
    );
    assert_eq!(farm.world.membership("m1"), membership("old", None));
    farm.world.fake.heal();
    farm.world.move_to("m1", "new").expect("moved");
    assert_eq!(generation(&farm), newest(&farm));
    assert!(generation(&farm) > before);
}

/// Proves: an admitted host whose daemon announces `facts-v1` is ordered nothing, and shown
/// `moving`, while its heartbeat reports no generation or the one before the move, and, at the
/// heartbeat that reports the move's generation, is ordered the uninstalls in reverse order and
/// then the installs in order, and its move ends.
///
/// Catches: a host ordered to uninstall before its daemon wrote the facts that name the new
/// group.
#[tokio::test]
async fn an_admitted_host_waits_for_its_facts_then_runs_the_sequence() {
    let farm = farm_of("mvf-held", wide()).await;
    let before = generation(&farm);
    farm.world.move_to("m1", "new").expect("moved");
    let raised = generation(&farm);

    assert_eq!(told_by(&farm, &OLD, &request(true, Some(before))), None);
    assert!(farm.world.coordinator().lock_fleet().is_admitted(&id("m1")));
    assert_eq!(farm.why("m1", "oldcfg"), MOVING);
    assert_eq!(farm.why("m1", "svc"), MOVING);
    assert_eq!(told_by(&farm, &OLD, &request(true, None)), None);
    assert_eq!(farm.world.membership("m1"), membership("new", Some("old")));

    let reports = request(true, Some(raised));
    assert_eq!(told_by(&farm, &OLD, &reports), say("uninstall oldcfg"));
    assert_eq!(
        told_by(&farm, &[("oldsvc", "1.0")], &reports),
        say("uninstall oldsvc")
    );
    assert_eq!(told_by(&farm, &[], &reports), say("install svc"));
    assert_eq!(
        told_by(&farm, &[("svc", "2.0")], &reports),
        say("install cfg")
    );
    assert_eq!(
        told_by(&farm, &[("svc", "2.0"), ("cfg", "2.0")], &reports),
        None
    );
    assert_eq!(farm.world.membership("m1"), membership("new", None));
}

/// Proves: a daemon that does not announce `facts-v1` is not held for the facts: it is ordered
/// the first uninstall at the heartbeat that admits it, whatever generation it reports, and so is
/// a daemon that announced the feature and no longer does; a coordinator that does not advertise
/// the feature holds no host for them either.
///
/// Catches: a host whose daemon can never report a generation, held moving for good.
#[tokio::test]
async fn a_daemon_without_facts_is_not_held() {
    let farm = farm_of("mvf-old-daemon", wide()).await;
    farm.world.move_to("m1", "new").expect("moved");
    assert_eq!(
        told_by(&farm, &OLD, &request(false, None)),
        say("uninstall oldcfg")
    );

    let farm = farm_of("mvf-stopped", wide()).await;
    farm.world.move_to("m1", "new").expect("moved");
    assert_eq!(told_by(&farm, &OLD, &request(true, None)), None);
    assert_eq!(
        told_by(&farm, &OLD, &request(false, Some(1))),
        say("uninstall oldcfg")
    );

    let rig = Rig::new("mvf-unadvertised");
    let coordinator = Rig::start(&rig.dir, &rig.clock).with_features(&["heartbeat-v2"]);
    coordinator.hear_facts(&who("m1", "storage"), &request(true, None));
    assert!(!coordinator.lock_fleet().facts.awaits(&id("m1"), 7));
}

/// Proves: a change of the host's facts during its move (a label set) holds it again, between
/// two of its uninstalls, until its heartbeat reports the new generation.
///
/// Catches: a sequence that goes on while the host's file is behind the coordinator's facts.
#[tokio::test]
async fn a_change_of_the_facts_during_the_move_holds_the_host_again() {
    let farm = farm_of("mvf-labels", wide()).await;
    farm.world.move_to("m1", "new").expect("moved");
    let raised = generation(&farm);
    assert_eq!(
        told_by(&farm, &OLD, &request(true, Some(raised))),
        say("uninstall oldcfg")
    );

    labels(&farm.world.rig, "m1", r#"{"zone":"a"}"#);
    let relabelled = generation(&farm);
    assert!(relabelled > raised);
    let after_uninstall = [("oldsvc", "1.0")];
    assert_eq!(
        told_by(&farm, &after_uninstall, &request(true, Some(raised))),
        None
    );
    assert_eq!(
        told_by(&farm, &after_uninstall, &request(true, Some(relabelled))),
        say("uninstall oldsvc")
    );
}
