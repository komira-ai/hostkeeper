//! `Hosts.ClearRecord` from the outside: refused `daemon_too_old` to a daemon that did not
//! announce `clear-record-v1`, with nothing ordered or audited; ordered once, on a typed
//! heartbeat, to one that did, with the audit line `record_cleared`; `unknown_host`,
//! `unknown_service`, `invalid_request` and the callers that are not the operator refused; and a
//! call whose audit line cannot be written refused with nothing ordered. Every time is the rig's
//! clock; nothing sleeps.

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Heartbeat, ServiceReply};

use crate::audit_refusal_tests::full_disk;
use crate::clear_record::{FEATURE, order_id};
use crate::marks_support::{
    NEW_DAEMON, WEB, ask, ask_json, ask_typed, beat_of, operator, rig, runs, set, start, update,
};
use crate::operator::Caller;
use crate::ops::OpsState;
use crate::orders::Order;
use crate::rollback_tests::unwritable;
use crate::testutil::Rig;

/// The features of a daemon that takes the `clear_record` order: [`NEW_DAEMON`]'s and
/// [`FEATURE`].
fn clearer_features() -> Vec<&'static str> {
    let mut features = NEW_DAEMON.to_vec();
    features.push(FEATURE);
    features
}

/// A heartbeat of a daemon that announces [`FEATURE`], running `running` of `web`.
fn clearer(running: &str) -> Heartbeat {
    beat_of(&clearer_features(), vec![runs(WEB, running)])
}

/// A heartbeat of a daemon of today, which does not announce [`FEATURE`], running `running` of
/// `web`.
fn daemon(running: &str) -> Heartbeat {
    beat_of(NEW_DAEMON, vec![runs(WEB, running)])
}

/// The request that clears the record of `service` on `node`.
fn clear_of(node: &str, service: &str) -> proto::ClearRecordRequest {
    proto::ClearRecordRequest {
        node_id: node.to_owned(),
        service: service.to_owned(),
    }
}

/// The operator's call of `node` and `service` on the rig's coordinator.
fn clear(rig: &Rig, node: &str, service: &str) -> Result<String, proto::HostsRefusal> {
    rig.coordinator
        .clear_record(&operator(), &clear_of(node, service))
        .map(|answer| answer.order_id)
}

/// Every entry of the audit log, oldest first.
fn audit(rig: &Rig) -> Vec<proto::AuditEntry> {
    rig.coordinator
        .audit_list(&operator(), proto::ListAuditRequest::default())
        .expect("the audit log")
        .entries
}

/// The entries of the audit log whose action is `record_cleared`.
fn cleared(rig: &Rig) -> Vec<proto::AuditEntry> {
    audit(rig)
        .into_iter()
        .filter(|entry| entry.action == "record_cleared")
        .collect()
}

/// Proves: a host whose daemon did not announce `clear-record-v1` in its last heartbeat is
/// refused `daemon_too_old`; nothing is audited; and nothing is ordered, not at its next typed
/// heartbeat and not after its daemon later announces the feature, because nothing was queued.
/// A host whose daemon announced it and then a heartbeat without it is refused too: the last
/// heartbeat decides.
///
/// Catches: a refusal that queues the order all the same (sent once the daemon can read it, or
/// at once to a daemon that cannot), that writes the audit line, that is answered with another
/// token, or a feature read from any heartbeat but the last.
#[tokio::test]
async fn clear_record_is_refused_daemon_too_old_and_nothing_is_ordered() {
    let rig = rig("clear-record-too-old", "");
    assert_eq!(
        ask(&rig, "d1", "daemon", daemon("1.0")),
        ServiceReply::Nothing
    );
    let before = audit(&rig);

    assert_eq!(
        clear(&rig, "d1", WEB),
        Err(proto::HostsRefusal::DaemonTooOld)
    );
    assert_eq!(audit(&rig), before, "nothing is audited");
    assert_eq!(
        ask_typed(&rig, "d1", "daemon", daemon("1.0")),
        (ServiceReply::Nothing, None)
    );
    assert_eq!(
        ask_typed(&rig, "d1", "daemon", clearer("1.0")),
        (ServiceReply::Nothing, None),
        "nothing waited for a daemon that can read it"
    );

    // The daemon announced the feature in the heartbeat before; its last one does not.
    assert_eq!(
        ask_typed(&rig, "d1", "daemon", daemon("1.0")),
        (ServiceReply::Nothing, None)
    );
    assert_eq!(
        clear(&rig, "d1", WEB),
        Err(proto::HostsRefusal::DaemonTooOld)
    );
    assert_eq!(audit(&rig), before, "nothing is audited");
}

/// Proves: to a daemon that announced `clear-record-v1`, the call answers the order id
/// `clear-record-<seq>` of its `record_cleared` entry (by the operator's token, naming the host
/// and the service, with no reason); the JSON heartbeat never carries the order; the next typed
/// heartbeat carries it once, as the `clear_record` case of `Order` with the try 1; the heartbeat
/// after carries nothing. A second call while the order waits answers the same id and audits
/// nothing more; a call after it was sent orders again, with its own entry and id. The order
/// takes no update slot: the host behind is still ordered.
///
/// Catches: an order sent on every heartbeat or twice, sent before the audit line names it,
/// with an id that is not its entry's, lost to a JSON heartbeat, queued twice with two ids, a
/// repeat audited, or an order that holds the update slot.
#[tokio::test]
async fn clear_record_is_ordered_once_to_a_daemon_that_announced_it() {
    let rig = rig("clear-record-once", "");
    assert_eq!(
        ask(&rig, "d1", "daemon", clearer("2.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        ask(&rig, "d2", "daemon", daemon("1.0")),
        ServiceReply::Nothing
    );
    set(&rig, WEB, "2.0", 1).await;

    let id = clear(&rig, "d1", WEB).expect("ordered");
    let entries = cleared(&rig);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let entry = &entries[0];
    assert_eq!(id, order_id(entry.seq));
    assert_eq!(id, format!("clear-record-{}", entry.seq));
    assert_eq!(entry.by, proto::Actor::Operator as i32);
    assert_eq!(entry.by_name.as_deref(), Some("operator"));
    assert_eq!(entry.node_id.as_deref(), Some("d1"));
    assert_eq!(entry.service.as_deref(), Some(WEB));
    assert_eq!(entry.reason, None);

    assert_eq!(clear(&rig, "d1", WEB), Ok(id.clone()), "the same order");
    assert_eq!(cleared(&rig).len(), 1, "a repeat is not audited");

    // The JSON route has nowhere to put an order: it waits for a typed heartbeat.
    assert_eq!(
        ask_json(&rig, "d1", "daemon", clearer("2.0")),
        ServiceReply::Nothing
    );
    let sent = Order::ClearRecord {
        service: WEB.to_owned(),
        order_id: id.clone(),
    };
    assert_eq!(
        ask_typed(&rig, "d1", "daemon", clearer("2.0")),
        (ServiceReply::Nothing, Some(sent.clone()))
    );
    assert_eq!(
        ask_typed(&rig, "d1", "daemon", clearer("2.0")),
        (ServiceReply::Nothing, None),
        "sent once"
    );
    assert_eq!(
        sent.to_proto(),
        agent::Order {
            order_id: id.clone(),
            r#try: 1,
            verb: Some(agent::order::Verb::ClearRecord(agent::ClearRecord {
                service: WEB.to_owned(),
            })),
        }
    );

    // The order took no slot: the host behind is still ordered.
    assert_eq!(ask(&rig, "d2", "daemon", daemon("1.0")), update(WEB, "2.0"));

    // Asked again once sent: a new order, with its own entry.
    let again = clear(&rig, "d1", WEB).expect("ordered");
    let entries = cleared(&rig);
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert_eq!(again, order_id(entries[1].seq));
    assert_ne!(again, id);
    let (_, order) = ask_typed(&rig, "d1", "daemon", clearer("2.0"));
    assert_eq!(
        order,
        Some(Order::ClearRecord {
            service: WEB.to_owned(),
            order_id: again,
        })
    );
}

/// Proves: an order queued while the daemon announced the feature waits, unsent, through a
/// heartbeat of the host without it, and goes at the next heartbeat with it; an `abort` queued
/// for the same host goes first and the `clear_record` at the heartbeat after; two services'
/// orders go one per heartbeat, by service name.
///
/// Catches: an order sent to a daemon that no longer announces the feature, lost when it cannot
/// be sent, an `abort` held behind it, or two orders in one answer.
#[tokio::test]
async fn a_queued_clear_record_waits_for_a_daemon_that_announces_it_and_after_an_abort() {
    let rig = rig("clear-record-waits", "");
    let both = || {
        beat_of(
            &clearer_features(),
            vec![runs(WEB, "1.0"), runs(crate::marks_support::DB, "1.0")],
        )
    };
    assert_eq!(ask(&rig, "d1", "daemon", both()), ServiceReply::Nothing);

    let web = clear(&rig, "d1", WEB).expect("ordered");
    let db = clear(&rig, "d1", crate::marks_support::DB).expect("ordered");
    rig.coordinator
        .abort(
            &operator(),
            proto::AbortRequest {
                node_id: "d1".to_owned(),
                service: WEB.to_owned(),
                reason: "stuck step".to_owned(),
            },
        )
        .expect("queued");

    let (_, order) = ask_typed(&rig, "d1", "daemon", daemon("1.0"));
    assert!(matches!(order, Some(Order::Abort { .. })), "{order:?}");
    assert_eq!(
        ask_typed(&rig, "d1", "daemon", daemon("1.0")),
        (ServiceReply::Nothing, None),
        "not to a daemon without the feature"
    );
    let (_, first) = ask_typed(&rig, "d1", "daemon", both());
    let (_, second) = ask_typed(&rig, "d1", "daemon", both());
    assert_eq!(
        (first, second),
        (
            Some(Order::ClearRecord {
                service: crate::marks_support::DB.to_owned(),
                order_id: db,
            }),
            Some(Order::ClearRecord {
                service: WEB.to_owned(),
                order_id: web,
            }),
        )
    );
    assert_eq!(
        ask_typed(&rig, "d1", "daemon", both()),
        (ServiceReply::Nothing, None)
    );
}

/// Proves: a host never heard from is `unknown_host`, a service the coordinator does not manage
/// `unknown_service`, a node id that is no host id `invalid_request`, a CI token `forbidden`
/// and no token `unauthorized`; each audits nothing and orders nothing.
///
/// Catches: an order queued for a host nobody heard from or a service nobody runs, a malformed
/// id accepted, a caller other than the operator let through, or a refusal that audits.
#[tokio::test]
async fn clear_record_refuses_an_unknown_host_service_or_id_and_other_callers() {
    let rig = rig("clear-record-refusals", "");
    assert_eq!(
        ask(&rig, "d1", "daemon", clearer("1.0")),
        ServiceReply::Nothing
    );
    let before = audit(&rig);

    assert_eq!(
        clear(&rig, "d9", WEB),
        Err(proto::HostsRefusal::UnknownHost)
    );
    assert_eq!(
        clear(&rig, "d1", "nope"),
        Err(proto::HostsRefusal::UnknownService)
    );
    for bad in ["", "D1", "-d1", "d_1"] {
        assert_eq!(
            clear(&rig, bad, WEB),
            Err(proto::HostsRefusal::InvalidRequest),
            "{bad:?}"
        );
    }
    for (caller, refusal) in [
        (Caller::Ci, proto::HostsRefusal::Forbidden),
        (Caller::Anonymous, proto::HostsRefusal::Unauthorized),
    ] {
        assert_eq!(
            rig.coordinator.clear_record(&caller, &clear_of("d1", WEB)),
            Err(refusal)
        );
    }
    assert_eq!(audit(&rig), before, "nothing is audited");
    assert_eq!(
        ask_typed(&rig, "d1", "daemon", clearer("1.0")),
        (ServiceReply::Nothing, None),
        "nothing is ordered"
    );
}

/// Proves: with an ops store that cannot be written the call is refused `store_failed`, and
/// with an audit file that takes no lines `audit_unavailable`; in each case nothing is ordered,
/// and once the audit file takes lines again the same call orders.
///
/// Catches: an order queued before its audit line is written (sent with no `record_cleared`
/// entry naming it), or a failed write answered as an accepted order.
#[tokio::test]
async fn clear_record_whose_audit_line_cannot_be_written_orders_nothing() {
    let mut rig = rig("clear-record-store-failed", "");
    rig.coordinator = start(&rig.dir, &rig.clock, "").with_ops(unwritable(OpsState::default()));
    assert_eq!(
        ask(&rig, "d1", "daemon", clearer("1.0")),
        ServiceReply::Nothing
    );
    assert_eq!(
        clear(&rig, "d1", WEB),
        Err(proto::HostsRefusal::StoreFailed)
    );
    assert_eq!(
        ask_typed(&rig, "d1", "daemon", clearer("1.0")),
        (ServiceReply::Nothing, None)
    );

    let (rig, down) = full_disk("clear-record-audit-unavailable").await;
    let both = || {
        beat_of(
            &clearer_features(),
            vec![runs(WEB, "1.0"), runs(crate::marks_support::DB, "1.0")],
        )
    };
    ask(&rig, "d1", "daemon", both());
    assert_eq!(
        clear(&rig, "d1", WEB),
        Err(proto::HostsRefusal::AuditUnavailable)
    );
    // `web` 2.0 is desired here, so the reply may be the gate's update; no operator's order.
    assert_eq!(ask_typed(&rig, "d1", "daemon", both()).1, None);
    down.store(false, std::sync::atomic::Ordering::SeqCst);
    let id = clear(&rig, "d1", WEB).expect("ordered");
    let (_, order) = ask_typed(&rig, "d1", "daemon", both());
    assert_eq!(
        order,
        Some(Order::ClearRecord {
            service: WEB.to_owned(),
            order_id: id,
        })
    );
}
