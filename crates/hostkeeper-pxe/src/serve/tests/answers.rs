//! The answer set as a pure function of one read and of what the run knows.

use std::collections::{BTreeMap, BTreeSet};

use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_pxe_packets::dhcp::Mode;
use hostkeeper_pxe_seed::inputs::{LinuxPlatform, Record as MenuRecord};
use hostkeeper_wire::{HostId, Platform};

use crate::serve::answers::{Answers, LeftOut, RunFacts, answers, packet_mac};
use crate::serve::inventory::{Pending, Record, Snapshot};

fn mac(text: &str) -> MacAddress {
    MacAddress::parse(text).expect("a MAC")
}

fn id(text: &str) -> HostId {
    text.parse().expect("a host id")
}

fn record(host: &str, macs: &[&str], platform: Option<LinuxPlatform>) -> Record {
    Record {
        host_id: id(host),
        role: None,
        data_macs: macs.iter().map(|m| mac(m)).collect(),
        enrolled: false,
        platform,
    }
}

/// The answers of `snapshot` in `mode`, with boot sets for `platforms`, `--reinstall` of
/// `reinstall` and the MACs `seeded` fetched their seed.
fn run(
    snapshot: &Snapshot,
    mode: Mode,
    platforms: &[LinuxPlatform],
    reinstall: &[&str],
    seeded: &[&str],
) -> Answers {
    let platforms: BTreeSet<LinuxPlatform> = platforms.iter().copied().collect();
    let reinstall: BTreeSet<HostId> = reinstall.iter().map(|h| id(h)).collect();
    let seeded: BTreeSet<MacAddress> = seeded.iter().map(|m| mac(m)).collect();
    answers(
        snapshot,
        &RunFacts {
            mode,
            platforms: &platforms,
            reinstall: &reinstall,
            seeded: &seeded,
        },
    )
}

const BOTH: [LinuxPlatform; 2] = LinuxPlatform::ALL;

/// The inventory of the tests: `new-1` free, `enr-1` enrolled, `known-1` a host the
/// coordinator knows (in any state, retired included: it is in `Hosts.List`), `pend-1` matched
/// by a pending request, `amb-1` a candidate of an ambiguous one, and `arm-1` free with the
/// arm64 platform recorded.
fn inventory() -> Snapshot {
    let mut enrolled = record("enr-1", &["02:00:00:00:00:02"], None);
    enrolled.enrolled = true;
    Snapshot {
        records: vec![
            record("new-1", &["02:00:00:00:00:01", "02:00:00:00:00:11"], None),
            enrolled,
            record("known-1", &["02:00:00:00:00:03"], None),
            record("pend-1", &["02:00:00:00:00:04"], None),
            record("amb-1", &["02:00:00:00:00:05"], None),
            record("arm-1", &["02:00:00:00:00:06"], Some(LinuxPlatform::Arm64)),
        ],
        hosts: ["known-1".to_owned()].into(),
        pending: vec![
            Pending {
                request_id: "req-p".to_owned(),
                host_ids: vec!["pend-1".to_owned()],
            },
            Pending {
                request_id: "req-a".to_owned(),
                host_ids: vec!["other-1".to_owned(), "amb-1".to_owned()],
            },
        ],
    }
}

/// In proxy mode, every data MAC of the one free record is in the set with no platform, the
/// record with arm64 with its platform; an enrolled record, one whose host id the coordinator
/// knows, and one a pending request matches (as MATCHED or as a candidate of AMBIGUOUS) are
/// left out, each with its reason; every record's platform is known by its MACs, in the set or
/// not; and a MAC no record holds is "no record holds it".
///
/// Catches an enrolled, retired or waiting machine installed at its next network boot, and a
/// record platform lost for `hostkeeper/boot.efi`.
#[test]
fn only_free_records_are_in_the_answer_set_and_each_left_out_mac_has_its_reason() {
    let answers = run(&inventory(), Mode::Proxy, &BOTH, &[], &[]);
    let packet = |m: &str| packet_mac(mac(m));
    assert_eq!(
        answers.dhcp,
        BTreeMap::from([
            (packet("02:00:00:00:00:01"), None),
            (packet("02:00:00:00:00:06"), Some(Platform::LinuxArm64)),
            (packet("02:00:00:00:00:11"), None),
        ])
    );
    assert_eq!(
        answers.menus,
        BTreeMap::from([
            (
                mac("02:00:00:00:00:01"),
                MenuRecord {
                    host_id: id("new-1"),
                    role: None,
                    platform: None
                }
            ),
            (
                mac("02:00:00:00:00:06"),
                MenuRecord {
                    host_id: id("arm-1"),
                    role: None,
                    platform: Some(LinuxPlatform::Arm64)
                }
            ),
            (
                mac("02:00:00:00:00:11"),
                MenuRecord {
                    host_id: id("new-1"),
                    role: None,
                    platform: None
                }
            ),
        ])
    );
    assert_eq!(
        answers.record_platforms,
        BTreeMap::from([(packet("02:00:00:00:00:06"), Platform::LinuxArm64)])
    );
    assert_eq!(
        answers.left_out,
        BTreeMap::from([
            (
                mac("02:00:00:00:00:02").octets(),
                LeftOut::Enrolled {
                    host_id: id("enr-1")
                }
            ),
            (
                mac("02:00:00:00:00:03").octets(),
                LeftOut::HostKnown {
                    host_id: id("known-1")
                }
            ),
            (
                mac("02:00:00:00:00:04").octets(),
                LeftOut::Pending {
                    host_id: id("pend-1"),
                    request_id: "req-p".to_owned()
                }
            ),
            (
                mac("02:00:00:00:00:05").octets(),
                LeftOut::Pending {
                    host_id: id("amb-1"),
                    request_id: "req-a".to_owned()
                }
            ),
        ])
    );
    let why = |m: &str| answers.why_not(mac(m).octets());
    assert_eq!(
        why("02:00:00:00:00:02"),
        "the record of host enr-1: a host is enrolled against it"
    );
    assert_eq!(
        why("02:00:00:00:00:03"),
        "the record of host known-1: the coordinator knows a host by that id"
    );
    assert_eq!(
        why("02:00:00:00:00:04"),
        "the record of host pend-1: the pending request req-p matches it"
    );
    assert_eq!(why("02:00:00:00:00:99"), "no record holds it");
}

/// `--reinstall` of a host id sets aside the enrolled, known-host and pending rules for that
/// record only; a MAC that fetched its seed in this run is left out, `--reinstall` or not.
///
/// Catches a reinstall that never installs, one that leaks to records it does not name, and a
/// reinstalled machine installed again at the reboot after its install.
#[test]
fn reinstall_sets_aside_the_host_rules_of_its_records_but_not_the_seed_fetch() {
    let answers = run(
        &inventory(),
        Mode::Proxy,
        &BOTH,
        &["enr-1", "known-1", "pend-1"],
        &["02:00:00:00:00:04", "02:00:00:00:00:11"],
    );
    let menus: Vec<String> = answers.menus.keys().map(ToString::to_string).collect();
    assert_eq!(
        menus,
        [
            "02:00:00:00:00:01",
            "02:00:00:00:00:02",
            "02:00:00:00:00:03",
            "02:00:00:00:00:06"
        ]
    );
    assert_eq!(
        answers.left_out,
        BTreeMap::from([
            (
                mac("02:00:00:00:00:04").octets(),
                LeftOut::Seeded {
                    host_id: id("pend-1")
                }
            ),
            (
                mac("02:00:00:00:00:05").octets(),
                LeftOut::Pending {
                    host_id: id("amb-1"),
                    request_id: "req-a".to_owned()
                }
            ),
            (
                mac("02:00:00:00:00:11").octets(),
                LeftOut::Seeded {
                    host_id: id("new-1")
                }
            ),
        ])
    );
    assert_eq!(
        answers.why_not(mac("02:00:00:00:00:11").octets()),
        "the record of host new-1: its seed was fetched in this run, so its install started"
    );
}

/// In router mode a record with no platform is left out (the router names one file for every
/// client); a record whose platform has no boot set is left out in either mode; a record with a
/// platform that has one is in the set in router mode.
///
/// Catches router mode installing a platform no one stated, and an install entry for a platform
/// whose kernel this server does not hold.
#[test]
fn the_platform_rules_of_router_mode_and_of_the_boot_sets() {
    let router = run(&inventory(), Mode::Router, &BOTH, &[], &[]);
    let menus: Vec<String> = router.menus.keys().map(ToString::to_string).collect();
    assert_eq!(menus, ["02:00:00:00:00:06"]);
    assert_eq!(
        router.left_out.get(&mac("02:00:00:00:00:01").octets()),
        Some(&LeftOut::NoPlatform {
            host_id: id("new-1")
        })
    );
    assert_eq!(
        router.why_not(mac("02:00:00:00:00:01").octets()),
        "the record of host new-1 has no platform, which router mode needs"
    );
    let x86_only = run(
        &inventory(),
        Mode::Proxy,
        &[LinuxPlatform::X86_64],
        &[],
        &[],
    );
    assert_eq!(
        x86_only.left_out.get(&mac("02:00:00:00:00:06").octets()),
        Some(&LeftOut::NoBootSet {
            host_id: id("arm-1"),
            platform: LinuxPlatform::Arm64
        })
    );
    assert_eq!(
        x86_only.why_not(mac("02:00:00:00:00:06").octets()),
        "the record of host arm-1 is linux-arm64, which has no boot set here"
    );
    // The record's platform is still known for `hostkeeper/boot.efi`.
    assert_eq!(
        x86_only
            .record_platforms
            .get(&packet_mac(mac("02:00:00:00:00:06"))),
        Some(&Platform::LinuxArm64)
    );
}

/// A MAC two records hold is left out, naming both, even when one is free and seeded nowhere.
///
/// Catches a machine installed as one of two records the inventory confuses.
#[test]
fn a_mac_two_records_hold_is_left_out() {
    let snapshot = Snapshot {
        records: vec![
            record("a-1", &["02:00:00:00:00:21"], Some(LinuxPlatform::X86_64)),
            record("b-1", &["02:00:00:00:00:21"], None),
        ],
        ..Snapshot::default()
    };
    let answers = run(&snapshot, Mode::Proxy, &BOTH, &[], &[]);
    assert_eq!(answers.dhcp, BTreeMap::new());
    assert_eq!(
        answers.left_out,
        BTreeMap::from([(
            mac("02:00:00:00:00:21").octets(),
            LeftOut::SharedMac {
                host_ids: vec![id("a-1"), id("b-1")]
            }
        )])
    );
    assert_eq!(
        answers.why_not(mac("02:00:00:00:00:21").octets()),
        "the records of hosts a-1, b-1 hold the same MAC"
    );
}

/// The empty read gives the empty answer set: nothing in it, nothing left out.
///
/// Catches an answer made from something other than this read.
#[test]
fn an_empty_read_gives_the_empty_answer_set() {
    assert_eq!(
        run(&Snapshot::default(), Mode::Proxy, &BOTH, &[], &[]),
        Answers::default()
    );
}
