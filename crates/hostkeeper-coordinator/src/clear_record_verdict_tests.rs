//! The verdict of a `clear_record` order in a heartbeat (`docs/design/two-programs.md` section
//! 3.2): acknowledged and shown for the host, but no word on any release. It does not stand for
//! the host in place of the failure the gate reads, and it holds no update slot. Over real files;
//! every time is explicit.

use hostkeeper_wire::wire::{
    Health, Heartbeat, OrderId, Outcome, ServiceReply, Token, UpdateResult, VerdictAck,
};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;

use crate::marks_support::operator;
use crate::rollback_tests::{failure, release, rig, who};
use crate::testutil::{LEGACY, Rig, beat, id, version};
use crate::verdict_file::FEATURE;

/// The definition line that rolls the service in one stage.
const ONE_STAGE: &str = "stages = [100]";

/// 2.0 desired, one daemon of the service updating at a time.
const TWO_SERIAL: &str =
    r#"{"version":"2.0","max_updating":{"daemon":1},"min_up":{"storage":0,"frontend":0}}"#;

/// `b` from an agent that announced `verdict-acks-v1` and `heartbeat-v2`.
fn acking(mut b: Heartbeat) -> Heartbeat {
    b.agent_features = vec![FEATURE.to_owned(), "heartbeat-v2".to_owned()];
    b
}

fn ack(order: &str, attempt: u64) -> VerdictAck {
    VerdictAck {
        service: LEGACY.to_owned(),
        order_id: OrderId::parse(order).expect("an order id"),
        order_try: attempt,
    }
}

/// The answer to `b` from `host` at `at`: its reply and its acks.
fn ask(rig: &Rig, host: &str, b: Heartbeat, at: u64) -> (ServiceReply, Vec<VerdictAck>) {
    rig.at(at);
    let answer = rig
        .coordinator
        .heartbeat_native(who(host), b)
        .expect("a heartbeat");
    (answer.reply, answer.acks)
}

/// A release failure of 2.0 that ends try 1 of `o-1`, from a host at 1.0.
fn failed_two() -> Heartbeat {
    let (phase, class) = release();
    let mut b = failure(phase, class, "2.0");
    let last = b.last_update.as_mut().expect("a verdict");
    last.order_id = OrderId::parse("o-1");
    last.order_try = Some(1);
    b
}

/// The verdict of the `clear_record` order `clear-record-7`, try 1, from a host at 1.0:
/// `outcome` with `reason`, naming 1.0, the version the host runs.
fn cleared(outcome: Outcome, reason: &str) -> UpdateResult {
    UpdateResult {
        target: version("1.0"),
        outcome,
        phase: None,
        class: None,
        reason: Token::parse(reason),
        order_id: OrderId::parse("clear-record-7"),
        order_try: Some(1),
    }
}

/// A heartbeat of a host at 1.0 that carries `verdict`.
fn carrying(verdict: UpdateResult) -> Heartbeat {
    let mut b = beat("1.0");
    b.last_update = Some(verdict);
    b
}

/// What the fleet reads as the host's verdict of the service.
fn read_as(rig: &Rig, host: &str) -> Option<UpdateResult> {
    rig.coordinator
        .lock_fleet()
        .get_in(LEGACY, &id(host))
        .and_then(|h| h.beat.last_update.clone())
}

/// Catches a `clear_record` verdict kept as the host's standing verdict, or read by the gate in
/// place of the failure it fell back from: d1 failed 2.0 and runs 1.0, and the failure stands
/// for it once acknowledged. Its succeeded `clear_record` verdict is acknowledged by its order
/// id and try and is the one shown for the host, but the failure of 2.0 still stands, the fleet
/// still reads it, and d1 is not ordered 2.0, at that heartbeat or after; d2, which never
/// failed, is.
#[tokio::test]
async fn a_clear_record_verdict_leaves_the_failure_the_gate_reads() {
    let rig = rig("clear-record-verdict-gate", ONE_STAGE, TWO_SERIAL).await;
    let failed = failed_two().last_update;
    let (reply, acks) = ask(&rig, "d1", acking(failed_two()), 2_000);
    assert_eq!((reply, acks), (ServiceReply::Nothing, vec![ack("o-1", 1)]));

    let verdict = cleared(Outcome::Succeeded, "record_cleared");
    let (reply, acks) = ask(&rig, "d1", acking(carrying(verdict.clone())), 2_015);
    assert_eq!(
        (reply, acks),
        (ServiceReply::Nothing, vec![ack("clear-record-7", 1)])
    );
    let d1 = id("d1");
    assert_eq!(rig.coordinator.verdicts.standing(LEGACY, &d1), failed);
    assert_eq!(read_as(&rig, "d1"), failed);
    assert_eq!(
        rig.coordinator
            .lock_fleet()
            .clear_records
            .shown(&d1, LEGACY),
        Some(&verdict)
    );
    let page = rig
        .coordinator
        .get_host(
            &operator(),
            &proto::GetHostRequest {
                node_id: "d1".to_owned(),
            },
        )
        .expect("the host page")
        .host
        .expect("a host");
    let shown = page
        .services
        .iter()
        .find(|one| one.service == LEGACY)
        .and_then(|one| one.last_update.as_ref())
        .expect("the verdict of the service");
    assert_eq!(shown.order_id.as_deref(), Some("clear-record-7"));
    assert_eq!(shown.reason, "record_cleared");

    let (reply, acks) = ask(&rig, "d1", acking(beat("1.0")), 2_030);
    assert_eq!((reply, acks), (ServiceReply::Nothing, vec![]));
    assert_eq!(read_as(&rig, "d1"), failed);
    let (reply, _) = ask(&rig, "d2", acking(beat("1.0")), 2_030);
    assert!(
        matches!(&reply, ServiceReply::Update { version: v, .. } if *v == version("2.0")),
        "{reply:?}"
    );
}

/// Catches a failed `clear_record` verdict taken as a failed update: an unhealthy host at 1.0
/// with no update behind it reports `record_reads` for its `clear_record`; it is acknowledged,
/// stands for nothing, the fleet reads no verdict for the host, and the host holds no update
/// slot. Its next verdict of another order ends the one shown.
#[tokio::test]
async fn a_failed_clear_record_verdict_on_an_unhealthy_host_holds_no_update_slot() {
    let rig = rig("clear-record-verdict-slot", ONE_STAGE, TWO_SERIAL).await;
    let mut sick = carrying(cleared(Outcome::Failed, "record_reads"));
    sick.health = Health::Unhealthy;
    let (_, acks) = ask(&rig, "d1", acking(sick), 2_000);
    assert_eq!(acks, [ack("clear-record-7", 1)]);
    let d1 = id("d1");
    assert_eq!(rig.coordinator.verdicts.standing(LEGACY, &d1), None);
    assert_eq!(read_as(&rig, "d1"), None);
    let holds = rig
        .coordinator
        .lock_fleet()
        .get_in(LEGACY, &d1)
        .map(crate::fleet::Host::holds_slot);
    assert_eq!(holds, Some(false));

    ask(&rig, "d1", acking(failed_two()), 2_015);
    assert_eq!(
        rig.coordinator
            .lock_fleet()
            .clear_records
            .shown(&d1, LEGACY),
        None
    );
}

/// Catches an order id taken for a `clear_record`'s by its prefix alone: `clear-record-` with
/// no number, or with more after the number, is any order's verdict, and stands.
#[test]
fn only_an_order_id_of_clear_record_and_a_number_is_a_clear_record_verdict() {
    let with = |order: &str| UpdateResult {
        order_id: OrderId::parse(order),
        ..cleared(Outcome::Failed, "record_reads")
    };
    assert!(crate::clear_record::is_verdict(&with("clear-record-7")));
    assert!(crate::clear_record::is_verdict(&with(
        &crate::clear_record::order_id(12)
    )));
    for other in [
        "clear-record-",
        "clear-record-7a",
        "o-clear-record-7",
        "o-1",
    ] {
        assert!(!crate::clear_record::is_verdict(&with(other)), "{other}");
    }
    let none = UpdateResult {
        order_id: None,
        ..cleared(Outcome::Failed, "record_reads")
    };
    assert!(!crate::clear_record::is_verdict(&none));
}
