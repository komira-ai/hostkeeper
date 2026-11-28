//! The hardware event log over its in-memory store, and the coordinator's two calls over it:
//! `Hardware.Log` (its order, filters, pages and refusals) and the write of a host's records
//! (stamped with the host, the boot, the time and the machine's serial). Each test's coordinator
//! reads a fixed time; none reads a clock.

use std::sync::Arc;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::HostId;

use super::{
    HardwareLog, HardwareLogError, MAX_LOG_PAGE, MemoryLog, Place, Query, Record, query_of,
};
use crate::operator::Caller;
use crate::service::Coordinator;
use crate::testutil::{Rig, ops_of};

/// The time the coordinators of these tests read.
const NOW: u64 = 1_800_000_000;

/// The operator, with the token named `operator`.
fn operator() -> Caller {
    crate::marks_support::operator()
}

/// The host `name`.
fn host(name: &str) -> HostId {
    name.parse().expect("a host id")
}

/// A coordinator on a directory of its own, reading [`NOW`], its log in memory.
fn coordinator(name: &str) -> (Rig, Coordinator) {
    let rig = Rig::new(name);
    let unix: crate::audit_file::UnixClock = Arc::new(|| NOW);
    let coordinator = Rig::start(&rig.dir, &rig.clock).with_ops(ops_of(&rig.dir).with_unix(unix));
    (rig, coordinator)
}

/// A record of `host` with `event_id` at `time_unix`, of `component` and `severity`, every other
/// field set.
pub(crate) fn record(
    host: &str,
    event_id: &str,
    time_unix: u64,
    component: &str,
    severity: &str,
) -> Record {
    Record {
        host_id: host.to_owned(),
        event_id: event_id.to_owned(),
        machine_serial: format!("SN-{host}"),
        time_unix,
        observed_unix: time_unix.saturating_add(60),
        boot_id: "b7c1".to_owned(),
        of_previous_boot: true,
        component: component.to_owned(),
        subject: "DIMM D1".to_owned(),
        severity: severity.to_owned(),
        state: "event".to_owned(),
        source: "sel".to_owned(),
        code: "memory_uncorrectable".to_owned(),
        message: format!("{event_id} on {host}"),
        raw: "01a4 | Memory | Uncorrectable ECC | Asserted".to_owned(),
        about: String::new(),
        by: String::new(),
    }
}

/// A mix of hosts, times, parts and severities: two records share a time, two a host.
pub(crate) fn mixed() -> Vec<Record> {
    vec![
        record("host-a", "sel:01", 100, "memory", "critical"),
        record("host-b", "smart:nvme0", 300, "disk", "warning"),
        record("host-a", "journal:7", 300, "cpu", "info"),
        record("host-c", "baseline:mem", 200, "memory", "warning"),
        record("host-a", "sel:02", 300, "memory", "critical"),
    ]
}

/// The keys of `records`, in their order.
pub(crate) fn keys(records: &[Record]) -> Vec<(String, String)> {
    records
        .iter()
        .map(|r| (r.host_id.clone(), r.event_id.clone()))
        .collect()
}

/// `(host, event_id)` as owned text.
fn key(host: &str, event: &str) -> (String, String) {
    (host.to_owned(), event.to_owned())
}

/// The whole log, newest first.
fn every(log: &dyn HardwareLog) -> Vec<Record> {
    log.page(&Query {
        limit: usize::MAX,
        ..Query::default()
    })
    .expect("read")
}

/// Proves: the log lists newest first by the time the source gives, and records of one time by
/// host id then `event_id`, both descending; a record written again with the same host and
/// `event_id` (even with another message) is left as first written, and a key twice in one write
/// is written once.
///
/// Catches: a log ordered by when it was written, a tie broken another way than the page token
/// assumes, and a record replaced or written twice by a repeat.
#[test]
fn the_log_is_newest_first_and_a_record_is_written_once() {
    let log = MemoryLog::default();
    log.write(&mixed()).expect("written");
    let mut again = record("host-a", "sel:01", 100, "memory", "critical");
    again.message = "a second reading".to_owned();
    log.write(&[again.clone(), again]).expect("written again");
    let held = every(&log);
    assert_eq!(
        keys(&held),
        [
            key("host-b", "smart:nvme0"),
            key("host-a", "sel:02"),
            key("host-a", "journal:7"),
            key("host-c", "baseline:mem"),
            key("host-a", "sel:01"),
        ]
    );
    assert_eq!(held[4], mixed()[0]);
}

/// Proves: each filter narrows the page to the records that match it, and filters set together
/// match all of them: the host, the machine's serial, the time (at or after `since_unix`), the
/// component, and the severities given.
///
/// Catches: a filter ignored, one that compares another field, and `since` taken as after.
#[test]
fn each_filter_narrows_the_page() {
    let log = MemoryLog::default();
    log.write(&mixed()).expect("written");
    let page = |query: Query| keys(&log.page(&Query { limit: 10, ..query }).expect("read"));
    let a = || Some("host-a".to_owned());
    assert_eq!(
        page(Query {
            host_id: a(),
            ..Query::default()
        }),
        [
            key("host-a", "sel:02"),
            key("host-a", "journal:7"),
            key("host-a", "sel:01")
        ]
    );
    assert_eq!(
        page(Query {
            serial: Some("SN-host-c".to_owned()),
            ..Query::default()
        }),
        [key("host-c", "baseline:mem")]
    );
    assert_eq!(
        page(Query {
            since_unix: 200,
            component: Some("memory".to_owned()),
            ..Query::default()
        }),
        [key("host-a", "sel:02"), key("host-c", "baseline:mem")]
    );
    assert_eq!(
        page(Query {
            host_id: a(),
            severities: vec!["critical".to_owned()],
            ..Query::default()
        }),
        [key("host-a", "sel:02"), key("host-a", "sel:01")]
    );
    assert_eq!(
        page(Query {
            serial: Some("SN-nobody".to_owned()),
            ..Query::default()
        }),
        Vec::<(String, String)>::new()
    );
}

/// Proves: `Hardware.Log` pages through the whole log in its order, every record once: pages of
/// 2 of 5 records give 2, 2 and 1, each with the token of its last record but the last page,
/// whose token is empty; a page whose token names a time shared by other records goes on with the
/// lower keys of that time.
///
/// Catches: a record lost or listed twice at a page's edge, above all between records of one
/// time, and a last page that still gives a token.
#[test]
fn pages_walk_the_whole_log_once_in_order() {
    let (_rig, coordinator) = coordinator("hardware-log-pages");
    let log = Arc::new(MemoryLog::default());
    log.write(&mixed()).expect("written");
    let coordinator = coordinator.with_hardware_log(log.clone());
    let mut token = String::new();
    let mut seen = Vec::new();
    let mut sizes = Vec::new();
    loop {
        let page = coordinator
            .hardware_log(
                &operator(),
                &proto::HardwareLogRequest {
                    limit: 2,
                    page_token: token.clone(),
                    ..proto::HardwareLogRequest::default()
                },
            )
            .expect("a page");
        sizes.push(page.records.len());
        for listed in &page.records {
            let event = listed.event.as_ref().expect("the event");
            seen.push(key(&listed.host_id, &event.event_id));
        }
        if page.next_page_token.is_empty() {
            break;
        }
        token = page.next_page_token;
    }
    assert_eq!(sizes, [2, 2, 1]);
    assert_eq!(seen, keys(&every(log.as_ref())));
    let first = coordinator
        .hardware_log(&operator(), &proto::HardwareLogRequest::default())
        .expect("a page");
    assert_eq!(first.records.len(), 5);
    assert_eq!(first.next_page_token, "");
}

/// Proves: a page asks for at most [`MAX_LOG_PAGE`] records, for a limit of 0 or above it, and
/// a limit below it is kept; the token of a page is the place of its last record.
///
/// Catches: an unbounded page, and a limit of 0 read as none.
#[test]
fn the_limit_is_capped_and_zero_is_the_cap() {
    for (limit, asked) in [
        (0, MAX_LOG_PAGE),
        (7, 7),
        (256, 256),
        (u32::MAX, MAX_LOG_PAGE),
    ] {
        let query = query_of(&proto::HardwareLogRequest {
            limit,
            ..proto::HardwareLogRequest::default()
        })
        .expect("a query");
        assert_eq!(query.limit, asked, "{limit}");
    }
    assert_eq!(
        record("host-a", "sel:01@17", 100, "memory", "info")
            .place()
            .token(),
        "100.host-a.sel:01@17"
    );
}

/// Proves: a page token is read back as the place it names, an `event_id` holding dots and
/// colons included, and every text that is not one this coordinator gives is none: no dots, a
/// time that is not decimal digits (empty, signed, past `u64`), a host id that is no node id, an
/// empty `event_id` and one over 64 bytes.
///
/// Catches: a token split at the wrong dot, and a malformed token read as the first page.
#[test]
fn a_page_token_reads_back_and_a_malformed_one_is_none() {
    let place = Place {
        time_unix: 1_795_662_012,
        host_id: "host-a".to_owned(),
        event_id: "sel:01a4@1790000000.x".to_owned(),
    };
    assert_eq!(Place::parse(&place.token()), Some(place));
    let long = format!("1.host-a.{}", "e".repeat(65));
    let longest = format!("1.host-a.{}", "e".repeat(64));
    assert!(Place::parse(&longest).is_some());
    for bad in [
        "",
        "100",
        "100.host-a",
        ".host-a.e",
        "+1.host-a.e",
        "18446744073709551616.host-a.e",
        "1.Host_A.e",
        "1..e",
        "1.host-a.",
        long.as_str(),
    ] {
        assert_eq!(Place::parse(bad), None, "{bad}");
    }
}

/// Proves: the filters of a request become the query's: a host id, a serial, the time, the
/// component's token and every severity at least as bad as the one asked (`info` all three,
/// `warning` two, `critical` one), unspecified ones none.
///
/// Catches: a severity filter that lists only the one asked, or the ones below it.
#[test]
fn a_request_becomes_its_query() {
    let request = |severity: agent::HardwareSeverity| proto::HardwareLogRequest {
        host_id: Some("host-a".to_owned()),
        serial: Some("SN-1".to_owned()),
        since_unix: 5,
        component: agent::HardwareComponent::Pcie.into(),
        severity: severity.into(),
        limit: 3,
        page_token: "9.host-b.e".to_owned(),
    };
    let query = query_of(&request(agent::HardwareSeverity::Warning)).expect("a query");
    assert_eq!(
        query,
        Query {
            host_id: Some("host-a".to_owned()),
            serial: Some("SN-1".to_owned()),
            since_unix: 5,
            component: Some("pcie".to_owned()),
            severities: vec!["warning".to_owned(), "critical".to_owned()],
            after: Some(Place {
                time_unix: 9,
                host_id: "host-b".to_owned(),
                event_id: "e".to_owned(),
            }),
            limit: 3,
        }
    );
    let severities = |severity| query_of(&request(severity)).expect("a query").severities;
    assert_eq!(
        severities(agent::HardwareSeverity::Info),
        ["info", "warning", "critical"]
    );
    assert_eq!(severities(agent::HardwareSeverity::Critical), ["critical"]);
    assert!(severities(agent::HardwareSeverity::Unspecified).is_empty());
    assert_eq!(
        query_of(&proto::HardwareLogRequest::default()),
        Some(Query {
            limit: MAX_LOG_PAGE,
            ..Query::default()
        })
    );
}

/// Proves: `Hardware.Log` refuses a caller that is no operator (`unauthorized`, and `forbidden`
/// for CI) and a request it cannot read as a query (`invalid_request`): a host id that is no node
/// id, a serial an inventory record cannot hold (empty, a space, over 64 bytes), a component or a
/// severity this release does not know, and a page token it did not give.
///
/// Catches: a log read for a caller that may not read it, and a filter it does not know taken as
/// none, which would list every record.
#[test]
fn the_log_refuses_a_caller_and_a_request_it_does_not_take() {
    let (_rig, coordinator) = coordinator("hardware-log-refusals");
    let ask = |caller: &Caller, request: proto::HardwareLogRequest| {
        coordinator.hardware_log(caller, &request).map(|_| ())
    };
    let any = proto::HardwareLogRequest::default;
    assert_eq!(
        ask(&Caller::Anonymous, any()),
        Err(proto::HardwareRefusal::Unauthorized)
    );
    assert_eq!(
        ask(&Caller::Ci, any()),
        Err(proto::HardwareRefusal::Forbidden)
    );
    let invalid = Err(proto::HardwareRefusal::InvalidRequest);
    let bad = [
        proto::HardwareLogRequest {
            host_id: Some("Host_A".to_owned()),
            ..any()
        },
        proto::HardwareLogRequest {
            serial: Some(String::new()),
            ..any()
        },
        proto::HardwareLogRequest {
            serial: Some("SN 1".to_owned()),
            ..any()
        },
        proto::HardwareLogRequest {
            serial: Some("S".repeat(65)),
            ..any()
        },
        proto::HardwareLogRequest {
            component: 99,
            ..any()
        },
        proto::HardwareLogRequest {
            severity: 99,
            ..any()
        },
        proto::HardwareLogRequest {
            page_token: "not a token".to_owned(),
            ..any()
        },
    ];
    for request in bad {
        assert_eq!(ask(&operator(), request.clone()), invalid, "{request:?}");
    }
    assert_eq!(ask(&operator(), any()), Ok(()));
}

/// A log whose every call fails.
struct Broken;

impl HardwareLog for Broken {
    fn write(&self, _records: &[Record]) -> Result<(), String> {
        Err("the disk is gone".to_owned())
    }

    fn page(&self, _query: &Query) -> Result<Vec<Record>, String> {
        Err("the disk is gone".to_owned())
    }
}

/// Proves: a log that cannot be read is `store_failed` to `Hardware.Log`, and one that cannot
/// be written is [`HardwareLogError::Store`] to a write.
///
/// Catches: a failed read answered as an empty log, and a failed write answered as written (the
/// daemon would drop records the log never held).
#[test]
fn a_log_that_fails_is_store_failed_and_a_write_to_it_fails() {
    let (_rig, coordinator) = coordinator("hardware-log-broken");
    let coordinator = coordinator.with_hardware_log(Arc::new(Broken));
    assert_eq!(
        coordinator
            .hardware_log(&operator(), &proto::HardwareLogRequest::default())
            .map(|_| ()),
        Err(proto::HardwareRefusal::StoreFailed)
    );
    let written = coordinator.write_hardware_events(&host("host-a"), &health(vec![event("e1")]));
    assert!(
        matches!(&written, Err(HardwareLogError::Store(why)) if why == "the disk is gone"),
        "{written:?}"
    );
}

/// The event `event_id` of the incident's DIMM, every field set.
pub(crate) fn event(event_id: &str) -> agent::HardwareEvent {
    agent::HardwareEvent {
        event_id: event_id.to_owned(),
        time_unix: 1_795_662_012,
        of_previous_boot: true,
        component: agent::HardwareComponent::Memory.into(),
        subject: "DIMM D1".to_owned(),
        severity: agent::HardwareSeverity::Critical.into(),
        state: agent::HardwareEventState::Event.into(),
        source: "sel".to_owned(),
        code: "memory_uncorrectable".to_owned(),
        message: "Uncorrectable ECC error on DIMM D1".to_owned(),
        raw: "01a4 | Memory #0x53 DIMM_D1 | Uncorrectable ECC | Asserted".to_owned(),
        about: String::new(),
    }
}

/// A host's health in the boot `b7c1` holding `events`.
pub(crate) fn health(events: Vec<agent::HardwareEvent>) -> agent::HostHealth {
    agent::HostHealth {
        events,
        boot_id: "b7c1".to_owned(),
        ..agent::HostHealth::default()
    }
}

/// Proves: a host's records are written with the host, the boot of its health, the coordinator's
/// time and no serial for a host no inventory record matches (no enrollment here), and are
/// answered by `Hardware.Log` with every field the daemon sent; written again, they are the same
/// records, first had at the first time.
///
/// Catches: a field of the daemon's record dropped or moved on its way through the log, and a
/// repeat that moves `observed_unix`.
#[test]
fn a_hosts_records_are_written_stamped_and_answered_whole() {
    let (rig, coordinator) = coordinator("hardware-log-write");
    let log = Arc::new(MemoryLog::default());
    let coordinator = coordinator.with_hardware_log(log.clone());
    let sent = health(vec![event("sel:01a4@1790000000")]);
    coordinator
        .write_hardware_events(&host("host-a"), &sent)
        .expect("written");
    let expected = proto::HardwareRecord {
        host_id: "host-a".to_owned(),
        machine_serial: String::new(),
        observed_unix: NOW,
        boot_id: "b7c1".to_owned(),
        by: String::new(),
        event: Some(event("sel:01a4@1790000000")),
    };
    let listed = |coordinator: &Coordinator| {
        coordinator
            .hardware_log(&operator(), &proto::HardwareLogRequest::default())
            .expect("listed")
            .records
    };
    assert_eq!(listed(&coordinator), std::slice::from_ref(&expected));
    let later: crate::audit_file::UnixClock = Arc::new(|| NOW + 99);
    let later = Rig::start(&rig.dir, &rig.clock)
        .with_ops(ops_of(&rig.dir).with_unix(later))
        .with_hardware_log(log);
    later
        .write_hardware_events(&host("host-a"), &sent)
        .expect("written again");
    assert_eq!(listed(&later), [expected]);
}

/// Proves: a health that breaks a bound (7 records, one more than a heartbeat holds, or a record
/// with no `event_id`) writes none of its records, and one with no record writes nothing.
///
/// Catches: a refused health partly written, which the daemon would send again whole.
#[test]
fn a_health_that_breaks_a_bound_writes_nothing() {
    let (_rig, coordinator) = coordinator("hardware-log-bounds");
    let log = Arc::new(MemoryLog::default());
    let coordinator = coordinator.with_hardware_log(log.clone());
    let seven = (0..7).map(|n| event(&format!("e{n}"))).collect();
    for refused in [health(seven), health(vec![event("e1"), event("")])] {
        assert!(matches!(
            coordinator.write_hardware_events(&host("host-a"), &refused),
            Err(HardwareLogError::Refused(_))
        ));
    }
    coordinator
        .write_hardware_events(&host("host-a"), &health(Vec::new()))
        .expect("nothing to write");
    assert!(every(log.as_ref()).is_empty());
}

/// Proves: every component, severity and state the daemon can send is kept as its token and
/// answered as the value it was, and a value this release does not know (a later daemon's) is
/// kept as `unspecified`; a token this release does not know (a later coordinator's) is answered
/// unspecified, with the record's message.
///
/// Catches: a token that reads back as another value, and a record of a later release refused
/// or dropped instead of shown by its message.
#[test]
fn every_value_is_kept_as_its_token_and_an_unknown_one_reads_unspecified() {
    let one = |component: i32, severity: i32, state: i32| {
        let sent = agent::HardwareEvent {
            component,
            severity,
            state,
            ..event("e")
        };
        let kept = Record::of(&host("host-a"), "", "", 0, &sent);
        let answered = kept.to_proto().event.expect("the event");
        (kept, answered)
    };
    for component in 0..=8 {
        for severity in 0..=3 {
            for state in 0..=3 {
                let (_, answered) = one(component, severity, state);
                assert_eq!(
                    (answered.component, answered.severity, answered.state),
                    (component, severity, state)
                );
            }
        }
    }
    let (kept, _) = one(
        agent::HardwareComponent::Memory.into(),
        agent::HardwareSeverity::Warning.into(),
        agent::HardwareEventState::Asserted.into(),
    );
    assert_eq!(
        (
            kept.component.as_str(),
            kept.severity.as_str(),
            kept.state.as_str()
        ),
        ("memory", "warning", "asserted")
    );
    let (kept, _) = one(42, 42, 42);
    assert_eq!(
        (
            kept.component.as_str(),
            kept.severity.as_str(),
            kept.state.as_str()
        ),
        ("unspecified", "unspecified", "unspecified")
    );
    let later = Record {
        component: "gpu".to_owned(),
        severity: "fatal".to_owned(),
        state: "flapping".to_owned(),
        ..record("host-a", "e", 1, "memory", "info")
    };
    let answered = later.to_proto();
    let event = answered.event.expect("the event");
    assert_eq!((event.component, event.severity, event.state), (0, 0, 0));
    assert_eq!(event.message, "e on host-a");
}

// ---- the contract every store of the log keeps --------------------------------------------------

/// A store of the log under [`conformance`]: the one live, and one started again on what the live
/// one made durable.
pub(crate) trait Subject {
    /// The store that writes.
    fn live(&self) -> Arc<dyn HardwareLog>;
    /// A store started again on what the live one holds.
    fn restarted(&self) -> Arc<dyn HardwareLog>;
}

/// The records of the contract: [`mixed`], with keys that sort apart byte by byte and by a
/// language's collation (upper case before lower, `-` before `_`), and a time past `i32`.
pub(crate) fn contract_records() -> Vec<Record> {
    let mut records = mixed();
    records.extend([
        record("host-a", "SEL:9", 300, "memory", "critical"),
        record("host-a", "sel_10", 300, "memory", "critical"),
        record("host-a", "sel-10", 300, "memory", "critical"),
        record(
            "host-b",
            "panic:2b6f9c1e",
            4_102_444_800,
            "board",
            "critical",
        ),
    ]);
    records
}

/// The queries of the contract: every filter alone and together, and pages after a place.
fn contract_queries() -> Vec<Query> {
    let after = |time_unix, host: &str, event: &str| {
        Some(Place {
            time_unix,
            host_id: host.to_owned(),
            event_id: event.to_owned(),
        })
    };
    let all = Query {
        limit: 100,
        ..Query::default()
    };
    vec![
        all.clone(),
        Query {
            limit: 3,
            ..all.clone()
        },
        Query {
            host_id: Some("host-a".to_owned()),
            ..all.clone()
        },
        Query {
            serial: Some("SN-host-b".to_owned()),
            ..all.clone()
        },
        Query {
            since_unix: 300,
            ..all.clone()
        },
        Query {
            component: Some("memory".to_owned()),
            severities: vec!["warning".to_owned(), "critical".to_owned()],
            ..all.clone()
        },
        Query {
            after: after(300, "host-a", "sel:02"),
            ..all.clone()
        },
        Query {
            after: after(300, "host-a", "sel-10"),
            limit: 2,
            ..all.clone()
        },
        Query {
            host_id: Some("host-c".to_owned()),
            after: after(200, "host-c", "baseline:mem"),
            ..all
        },
    ]
}

/// Asserts that `subject` keeps the contract of [`HardwareLog`]: an empty store lists nothing;
/// each query of [`contract_queries`] answers exactly the records [`Query::page_of`] picks of
/// [`contract_records`], in its order, field for field; a record written again, alone or in a
/// write with new ones, is left as first written and the new ones are written; and a store started
/// again lists the same.
pub(crate) fn conformance(subject: &dyn Subject) {
    let live = subject.live();
    assert_eq!(every(live.as_ref()), Vec::new(), "an empty store");
    let records = contract_records();
    let (first, rest) = records.split_at(4);
    live.write(first).expect("the first write");
    let mut changed = first[0].clone();
    changed.message = "a second reading".to_owned();
    let mut again = vec![changed];
    again.extend_from_slice(rest);
    again.push(rest[0].clone());
    live.write(&again).expect("the second write");
    let check = |store: &dyn HardwareLog| {
        for query in contract_queries() {
            assert_eq!(
                store.page(&query).expect("read"),
                query.page_of(records.iter()),
                "{query:?}"
            );
        }
    };
    // The live store first: starting another may stop it (the database's lock moves).
    check(live.as_ref());
    check(subject.restarted().as_ref());
}

/// The in-memory store: started again, it is the same store.
struct OnMemory(Arc<MemoryLog>);

impl Subject for OnMemory {
    fn live(&self) -> Arc<dyn HardwareLog> {
        self.0.clone()
    }

    fn restarted(&self) -> Arc<dyn HardwareLog> {
        self.0.clone()
    }
}

/// Proves: the in-memory store keeps the contract every store of the log keeps.
///
/// Catches: a store whose order, filters or repeats differ from the reference the database and
/// the file are held to.
#[test]
fn the_memory_store_keeps_the_contract() {
    conformance(&OnMemory(Arc::new(MemoryLog::default())));
}
