//! How the console finds a Linux server's BMC with no targets file (`super::address`): at the
//! address its daemon last reported, moved by a heartbeat that reports another, marked stale while
//! the host is not heartbeating, then at the one the neighbour table holds; and the targets file
//! as an override. The MAC check at every address still decides what is sent.

use std::sync::Arc;
use std::time::Duration;

use hostkeeper_console::bmc::{MAC_READ, POWER_STATUS};
use hostkeeper_console::scripted::Seen;
use hostkeeper_enrollment::names::HostId;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ConsoleRefusal as Refusal;

use crate::marks_support::start;
use crate::ops::OpsStore;

use super::support::{
    Fixture, MAC, NODE, NODE_BMC, NODE_BMC_ADDRESS, NODE_BMC_ANSWER, caller, console, file_address,
    line,
};

/// The answer of a session that reads the MAC of `node-02`'s BMC and its power, which is on.
const STATUS_ON: [&str; 2] = [NODE_BMC_ANSWER, "Chassis Power is on\n"];

/// What a session of the tool says when no BMC answers at its address, on both of its runs.
const NO_SESSION: &str = "Error: Unable to establish IPMI v2 / RMCP+ session\n";

/// A `status` of `node-02`'s power, by `alice`.
fn status(fixture: &Fixture) -> Result<proto::ConsolePowerResponse, Refusal> {
    fixture.rig.coordinator.console_power(
        &caller("alice"),
        &proto::ConsolePowerRequest {
            host_id: NODE.to_owned(),
            action: proto::PowerAction::Status.into(),
            reason: "is it up".to_owned(),
        },
    )
}

/// The addresses the tool was opened at, in order (each run of a session, `-v` runs included).
fn dialled(fixture: &Fixture) -> Vec<String> {
    fixture
        .tool
        .seen()
        .into_iter()
        .filter_map(|seen| match seen {
            Seen::Open { args, .. } => {
                let at = args.iter().position(|arg| arg == "-H")?;
                args.get(at + 1).cloned()
            }
            _ => None,
        })
        .collect()
}

/// When the coordinator first accepted the report of `node-02`'s BMC it keeps.
fn reported_unix(fixture: &Fixture) -> u64 {
    let host = HostId::parse(NODE).expect("a host id");
    let enrolled = fixture.enrollment.enrollment.store().enrolled_host(&host);
    let kept = enrolled.expect("read").expect("enrolled").bmc;
    kept.expect("a BMC kept").at_unix
}

/// The answer's account of a heartbeat's `address`.
fn reported(fixture: &Fixture, address: &str, stale: bool) -> proto::ConsoleAddress {
    proto::ConsoleAddress {
        address: address.to_owned(),
        source: proto::ConsoleAddressSource::Heartbeat.into(),
        stale,
        reported_unix: reported_unix(fixture),
    }
}

/// The answer's account of the neighbour table's `address`.
fn neighbor(address: &str) -> proto::ConsoleAddress {
    proto::ConsoleAddress {
        address: address.to_owned(),
        source: proto::ConsoleAddressSource::NeighborTable.into(),
        stale: false,
        reported_unix: 0,
    }
}

/// Proves: with no targets file, a power call reaches the BMC at the address the host's
/// heartbeat reported, and a later heartbeat that reports another address moves the call there;
/// each answer names the address and that it came from a heartbeat of a live host.
///
/// Catches: a BMC still dialled at the address of the heartbeat before (the lease moved and the
/// console did not follow), an address taken from somewhere else than the latest report, and an
/// answer that does not say where the address came from.
#[test]
fn a_heartbeat_carrying_a_new_address_moves_the_target() {
    let fixture = Fixture::without_targets("console-bmc-moved");
    fixture.rig.at(2_010);
    fixture.report_bmc(NODE, NODE_BMC_ADDRESS, NODE_BMC);
    fixture.tool.session(&STATUS_ON);
    let answer = status(&fixture).expect("answered");
    assert_eq!(
        answer.address,
        Some(reported(&fixture, NODE_BMC_ADDRESS, false))
    );

    fixture.rig.at(2_020);
    fixture.report_bmc(NODE, "192.0.2.40", NODE_BMC);
    fixture.tool.session(&STATUS_ON);
    let answer = status(&fixture).expect("answered");
    assert_eq!(
        answer.address,
        Some(reported(&fixture, "192.0.2.40", false))
    );
    assert_eq!(dialled(&fixture), [NODE_BMC_ADDRESS, "192.0.2.40"]);
    assert_eq!(fixture.tool.unused(), 0);
}

/// Proves: the BMC at the reported address that answers with another MAC than the record's is
/// refused `address_mismatch` after the one MAC read, with nothing else sent and the refusal
/// audited.
///
/// Catches: a reported address trusted for what is at it, so that a power command goes to the
/// wrong server's BMC.
#[test]
fn a_mac_mismatch_at_the_reported_address_still_refuses() {
    let fixture = Fixture::without_targets("console-bmc-mismatch");
    fixture.report_bmc(NODE, NODE_BMC_ADDRESS, NODE_BMC);
    fixture.tool.session(&[" 11 0c c4 7a 00 00 09\n"]);
    assert_eq!(status(&fixture), Err(Refusal::AddressMismatch));
    let asked: Vec<Seen> = fixture
        .tool
        .seen()
        .into_iter()
        .filter(|seen| matches!(seen, Seen::Ask(_)))
        .collect();
    assert_eq!(asked, [Seen::Ask(MAC_READ.to_owned())]);
    assert_eq!(
        fixture.lines(),
        [
            line("alice", "power_action", NODE, "is it up; action=status"),
            line(
                "alice",
                "console_denied",
                NODE,
                "is it up; call=power; cause=address_mismatch"
            ),
        ]
    );
}

/// Proves: once the host stops heartbeating, its last reported address is still tried first and
/// is answered as stale; when no BMC answers there, the call is tried at the address the
/// neighbour table holds for the BMC's MAC, and the answer names that one. While the host
/// heartbeats, the same address is not stale.
///
/// Catches: a down host's BMC given up on (the console's main use is a host that is down), the
/// stale address answered as current, the neighbour table never tried, or tried before the
/// heartbeat's address.
#[test]
fn a_host_not_heartbeating_has_a_stale_address_then_the_neighbor_table() {
    let fixture = Fixture::without_targets("console-bmc-stale");
    fixture.rig.at(2_010);
    fixture.report_bmc(NODE, NODE_BMC_ADDRESS, NODE_BMC);
    fixture.neighbors.put(NODE_BMC, "192.0.2.77");
    fixture.rig.at(2_010 + 44);
    let open = |reason: &str| {
        fixture.rig.coordinator.console_open(
            &caller("alice"),
            &proto::OpenConsoleRequest {
                host_id: NODE.to_owned(),
                reason: reason.to_owned(),
            },
        )
    };
    let opened = open("live").expect("opened");
    assert_eq!(
        opened.address,
        Some(reported(&fixture, NODE_BMC_ADDRESS, false))
    );
    fixture
        .rig
        .coordinator
        .console_close(
            &caller("alice"),
            &proto::CloseConsoleRequest {
                host_id: NODE.to_owned(),
            },
        )
        .expect("closed");

    fixture.rig.at(2_010 + 45);
    let opened = open("down").expect("opened");
    assert_eq!(
        opened.address,
        Some(reported(&fixture, NODE_BMC_ADDRESS, true))
    );

    fixture.tool.end_at_start(NO_SESSION);
    fixture.tool.end_at_start(NO_SESSION);
    fixture.tool.session(&[NODE_BMC_ANSWER]);
    fixture.tool.session(&STATUS_ON);
    let answer = status(&fixture).expect("answered");
    assert_eq!(answer.address, Some(neighbor("192.0.2.77")));
    assert_eq!(
        dialled(&fixture),
        [
            NODE_BMC_ADDRESS,
            NODE_BMC_ADDRESS,
            "192.0.2.77",
            "192.0.2.77"
        ],
        "the stale address, then the neighbour's checked, then the call there"
    );
    let asked: Vec<Seen> = fixture
        .tool
        .seen()
        .into_iter()
        .filter(|seen| matches!(seen, Seen::Ask(_)))
        .collect();
    assert_eq!(
        asked,
        [
            Seen::Ask(MAC_READ.to_owned()),
            Seen::Ask(MAC_READ.to_owned()),
            Seen::Ask(POWER_STATUS.to_owned()),
        ],
        "the check sends the MAC read alone"
    );
}

/// Proves: a coordinator that never heard the host (another coordinator on the same store, or
/// one restarted since) finds the address the host reported through another, and answers it as
/// stale, since it knows of no heartbeat within the last 45 seconds.
///
/// Catches: a host this coordinator has no fleet entry for taken as live, so that a possibly
/// moved address is answered as current; and a report kept in one coordinator's memory only.
#[test]
fn a_coordinator_that_never_heard_the_host_finds_its_address_as_stale() {
    let fixture = Fixture::without_targets("console-bmc-other-coordinator");
    fixture.report_bmc(NODE, NODE_BMC_ADDRESS, NODE_BMC);
    let state = fixture.rig.coordinator.ops().state();
    let other = start(&fixture.rig.dir, &fixture.rig.clock, "")
        .with_ops(OpsStore::on_fake(&fixture.fake, state))
        .with_enrollment(Arc::clone(&fixture.enrollment.enrollment))
        .with_console(
            console(&fixture.files, &fixture.tool, 8, Duration::from_secs(30))
                .with_neighbors(fixture.neighbors.lookup()),
        );
    let opened = other.console_open(
        &caller("alice"),
        &proto::OpenConsoleRequest {
            host_id: NODE.to_owned(),
            reason: "down".to_owned(),
        },
    );
    assert_eq!(
        opened.expect("opened").address,
        Some(reported(&fixture, NODE_BMC_ADDRESS, true))
    );
}

/// Proves: a duplicate host's report (two daemons on one host id) is not kept.
///
/// Catches: two machines' BMCs written over each other at every heartbeat, and the console sent
/// to whichever spoke last.
#[test]
fn a_duplicate_hosts_report_is_not_kept() {
    let fixture = Fixture::without_targets("console-bmc-duplicate");
    let host = HostId::parse(NODE).expect("a host id");
    let store = fixture.enrollment.enrollment.store();
    store.report_live(&host, "machine-a", 1_000).expect("live");
    store.report_live(&host, "machine-b", 1_001).expect("live");
    fixture.report_bmc(NODE, NODE_BMC_ADDRESS, NODE_BMC);
    let enrolled = store.enrolled_host(&host).expect("read").expect("enrolled");
    assert_eq!(enrolled.bmc, None);
    assert_eq!(status(&fixture), Err(Refusal::NoAddress));
}

/// Proves: a BMC whose host never reported it is found in the neighbour table alone; a neighbour
/// entry at the reported address is not tried twice; and with no report and no entry the host is
/// refused `no_address`, with no tool run.
///
/// Catches: the neighbour table read only as a fallback of a report, the same address tried
/// twice, and a host with no address dialled at some default.
#[test]
fn the_neighbor_table_alone_finds_a_bmc_and_nothing_known_is_no_address() {
    let fixture = Fixture::without_targets("console-bmc-neighbor");
    assert_eq!(status(&fixture), Err(Refusal::NoAddress));
    assert_eq!(fixture.tool.seen(), []);

    fixture.neighbors.put(NODE_BMC, "192.0.2.77");
    fixture.tool.session(&STATUS_ON);
    assert_eq!(
        status(&fixture).expect("answered").address,
        Some(neighbor("192.0.2.77"))
    );

    fixture.report_bmc(NODE, "192.0.2.77", NODE_BMC);
    fixture.tool.end_at_start(NO_SESSION);
    fixture.tool.end_at_start(NO_SESSION);
    assert_eq!(status(&fixture), Err(Refusal::DeviceUnreachable));
    assert_eq!(dialled(&fixture), ["192.0.2.77"; 3]);
}

/// Proves: a report of another MAC than the record's BMC MAC (another BMC, a record not yet
/// updated) gives the console no address for the host.
///
/// Catches: a report trusted over the inventory record, which names the BMC the console may
/// command.
#[test]
fn a_report_of_another_mac_than_the_records_is_not_used() {
    let fixture = Fixture::without_targets("console-bmc-other-mac");
    fixture.report_bmc(NODE, NODE_BMC_ADDRESS, "0c:c4:7a:00:00:09");
    assert_eq!(status(&fixture), Err(Refusal::NoAddress));
    assert_eq!(fixture.tool.seen(), []);
}

/// Proves: a targets file that names the BMC's MAC overrides the heartbeat and the neighbour
/// table: its address alone is tried, and a failure there is the answer.
///
/// Catches: an override that is only a first try, so that a BMC an operator pinned is reached
/// somewhere else.
#[test]
fn the_targets_file_overrides_every_other_source() {
    let fixture = Fixture::new("console-bmc-override");
    fixture.report_bmc(NODE, "192.0.2.40", NODE_BMC);
    fixture.neighbors.put(NODE_BMC, "192.0.2.77");
    fixture.tool.session(&STATUS_ON);
    assert_eq!(
        status(&fixture).expect("answered").address,
        Some(file_address(NODE_BMC_ADDRESS))
    );
    fixture.tool.end_at_start(NO_SESSION);
    fixture.tool.end_at_start(NO_SESSION);
    assert_eq!(status(&fixture), Err(Refusal::DeviceUnreachable));
    assert_eq!(dialled(&fixture), [NODE_BMC_ADDRESS; 3]);
}

/// Proves: with no targets file a Mac has no address (`no_address`): the heartbeat and the
/// neighbour table find BMCs only.
///
/// Catches: a Mac's screen dialled at an address found for another device.
#[test]
fn a_mac_needs_the_targets_file() {
    let fixture = Fixture::without_targets("console-bmc-mac");
    let opened = fixture.rig.coordinator.console_open(
        &caller("alice"),
        &proto::OpenConsoleRequest {
            host_id: MAC.to_owned(),
            reason: "login".to_owned(),
        },
    );
    assert_eq!(opened, Err(Refusal::NoAddress));
}
