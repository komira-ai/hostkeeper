//! The answer set (docs/design/network-install.md, section 3, "Which machines may be
//! installed"), a pure function of one read of the coordinator ([`Snapshot`]) and of what this
//! run knows: its mode, the platforms it has a boot set for, the host ids `--reinstall` names,
//! and the MACs that fetched their seed in this run.
//!
//! A data MAC of a record is in the set when, in this order:
//!
//! 1. no other record holds the same MAC (the coordinator refuses that; were it to happen, the
//!    MAC is left out rather than given one record's install);
//! 2. its machine has not fetched its seed in this run (a MAC leaves the set when its install
//!    starts, `--reinstall` or not);
//! 3. unless `--reinstall` names the record's host id: the record is not enrolled, its
//!    `host_held` is `NONE` (the enrollment store holds no host by its id: any other value,
//!    `UNSPECIFIED` included, leaves it out), the record's approval window is `OPEN` (`CLOSED`,
//!    `USED` and `UNSPECIFIED`, a coordinator before windows, leave it out), and no pending
//!    request matches it;
//! 4. in router mode, the record states a platform (the router names one file for every
//!    client, so the record is where the install's platform is stated);
//! 5. the record's platform, when it states one, has a boot set.
//!
//! Every MAC of a record that is left out is kept with the reason ([`LeftOut`]), which `serve`
//! logs when that MAC asks for an answer. Whether the firmware's or GRUB's architecture agrees
//! with the record's platform is judged per answer, by `hostkeeper-pxe-packets` and
//! `hostkeeper-pxe-seed`'s menu choice, from what the answer set holds.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use hostkeeper_enrollment::names::MacAddress;
use hostkeeper_proto::hostkeeper::operator::v1::HostHeld as HostHeldValue;
use hostkeeper_proto::hostkeeper::operator::v1::RecordState;
use hostkeeper_pxe_packets::Mac;
use hostkeeper_pxe_packets::dhcp::{AnswerSet, Mode};
use hostkeeper_pxe_packets::tftp::RecordPlatforms;
use hostkeeper_pxe_seed::inputs::{LinuxPlatform, Record as MenuRecord};
use hostkeeper_wire::HostId;

use crate::serve::inventory::{Record, Snapshot};
use crate::serve::platforms::wire_of;

/// What a run knows besides the read.
#[derive(Clone, Debug)]
pub struct RunFacts<'a> {
    /// `--mode`.
    pub mode: Mode,
    /// The platforms with a boot set.
    pub platforms: &'a BTreeSet<LinuxPlatform>,
    /// `--reinstall`.
    pub reinstall: &'a BTreeSet<HostId>,
    /// The MACs that fetched their seed in this run.
    pub seeded: &'a BTreeSet<MacAddress>,
}

/// Why a recorded MAC is not in the answer set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeftOut {
    /// It fetched its seed in this run.
    Seeded { host_id: HostId },
    /// Another record holds the same MAC.
    SharedMac { host_ids: Vec<HostId> },
    /// A host is enrolled against the record.
    Enrolled { host_id: HostId },
    /// The enrollment store holds a host by the record's host id, or the coordinator does not
    /// say whether it does (`host_held` is not `NONE`).
    HostHeld {
        host_id: HostId,
        held: HostHeldValue,
    },
    /// The record's approval window is not open (`state` is not `OPEN`): it ended or never
    /// opened, an approval used the record, or the coordinator does not say.
    Closed { host_id: HostId, state: RecordState },
    /// A pending request matches the record.
    Pending { host_id: HostId, request_id: String },
    /// Router mode, and the record states no platform.
    NoPlatform { host_id: HostId },
    /// The record's platform has no boot set.
    NoBootSet {
        host_id: HostId,
        platform: LinuxPlatform,
    },
}

impl fmt::Display for LeftOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LeftOut::Seeded { host_id } => write!(
                f,
                "the record of host {host_id}: its seed was fetched in this run, so its install started"
            ),
            LeftOut::SharedMac { host_ids } => {
                let ids: Vec<&str> = host_ids.iter().map(HostId::as_str).collect();
                write!(
                    f,
                    "the records of hosts {} hold the same MAC",
                    ids.join(", ")
                )
            }
            LeftOut::Enrolled { host_id } => {
                write!(
                    f,
                    "the record of host {host_id}: a host is enrolled against it"
                )
            }
            LeftOut::HostHeld { host_id, held } => {
                let what = match held {
                    HostHeldValue::Approved => "a host is approved as that id",
                    HostHeldValue::Denied => "that id is denied, its retirement not final yet",
                    HostHeldValue::Retired => "that id is retired",
                    HostHeldValue::Unspecified | HostHeldValue::None => {
                        "the coordinator does not say whether a host holds that id"
                    }
                };
                write!(f, "the record of host {host_id}: {what}")
            }
            LeftOut::Closed { host_id, state } => {
                let what = match state {
                    RecordState::Closed => "its approval window is closed",
                    RecordState::Used => "an approval used it",
                    RecordState::Unspecified | RecordState::Open => {
                        "the coordinator does not say whether its approval window is open"
                    }
                };
                write!(f, "the record of host {host_id}: {what}")
            }
            LeftOut::Pending {
                host_id,
                request_id,
            } => write!(
                f,
                "the record of host {host_id}: the pending request {request_id} matches it"
            ),
            LeftOut::NoPlatform { host_id } => write!(
                f,
                "the record of host {host_id} has no platform, which router mode needs"
            ),
            LeftOut::NoBootSet { host_id, platform } => write!(
                f,
                "the record of host {host_id} is {platform}, which has no boot set here"
            ),
        }
    }
}

/// The answer set of one read, in the three shapes its users take.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answers {
    /// For the DHCP answers: each MAC in the set, with its record's platform.
    pub dhcp: AnswerSet,
    /// For the menus and seeds: each MAC in the set, with its record.
    pub menus: BTreeMap<MacAddress, MenuRecord>,
    /// For `hostkeeper/boot.efi`: the platform of every record that states one, by each of its
    /// data MACs, in the set or not.
    pub record_platforms: RecordPlatforms,
    /// Each recorded MAC left out of the set, by its octets, with the reason.
    pub left_out: BTreeMap<[u8; 6], LeftOut>,
}

impl Answers {
    /// The reason the MAC of `octets` is not in the set: its record's, or that no record holds
    /// it.
    pub fn why_not(&self, octets: [u8; 6]) -> String {
        match self.left_out.get(&octets) {
            Some(why) => why.to_string(),
            None => "no record holds it".to_owned(),
        }
    }
}

/// The packet crate's spelling of `mac`.
pub fn packet_mac(mac: MacAddress) -> Mac {
    Mac::new(mac.octets())
}

/// The answer set of `snapshot`, for a run that knows `facts`.
pub fn answers(snapshot: &Snapshot, facts: &RunFacts<'_>) -> Answers {
    let mut holders: BTreeMap<MacAddress, Vec<&Record>> = BTreeMap::new();
    for record in &snapshot.records {
        for mac in &record.data_macs {
            holders.entry(*mac).or_default().push(record);
        }
    }
    let mut out = Answers::default();
    for (mac, records) in holders {
        for record in &records {
            if let Some(platform) = record.platform {
                out.record_platforms
                    .insert(packet_mac(mac), wire_of(platform));
            }
        }
        match judge(mac, &records, snapshot, facts) {
            Ok(record) => {
                out.dhcp
                    .insert(packet_mac(mac), record.platform.map(wire_of));
                out.menus.insert(
                    mac,
                    MenuRecord {
                        host_id: record.host_id.clone(),
                        role: record.role.clone(),
                        platform: record.platform,
                    },
                );
            }
            Err(why) => {
                out.left_out.insert(mac.octets(), why);
            }
        }
    }
    out
}

/// The one record `mac` may be installed as, or why it is left out.
fn judge<'r>(
    mac: MacAddress,
    records: &[&'r Record],
    snapshot: &Snapshot,
    facts: &RunFacts<'_>,
) -> Result<&'r Record, LeftOut> {
    let [record] = records else {
        return Err(LeftOut::SharedMac {
            host_ids: records.iter().map(|r| r.host_id.clone()).collect(),
        });
    };
    let host_id = || record.host_id.clone();
    if facts.seeded.contains(&mac) {
        return Err(LeftOut::Seeded { host_id: host_id() });
    }
    if !facts.reinstall.contains(&record.host_id) {
        if record.enrolled {
            return Err(LeftOut::Enrolled { host_id: host_id() });
        }
        if record.host_held != HostHeldValue::None {
            return Err(LeftOut::HostHeld {
                host_id: host_id(),
                held: record.host_held,
            });
        }
        if record.state != RecordState::Open {
            return Err(LeftOut::Closed {
                host_id: host_id(),
                state: record.state,
            });
        }
        let matching = snapshot.pending.iter().find(|pending| {
            pending
                .host_ids
                .iter()
                .any(|id| id == record.host_id.as_str())
        });
        if let Some(pending) = matching {
            return Err(LeftOut::Pending {
                host_id: host_id(),
                request_id: pending.request_id.clone(),
            });
        }
    }
    match (record.platform, facts.mode) {
        (None, Mode::Router) => Err(LeftOut::NoPlatform { host_id: host_id() }),
        (Some(platform), _) if !facts.platforms.contains(&platform) => Err(LeftOut::NoBootSet {
            host_id: host_id(),
            platform,
        }),
        _ => Ok(record),
    }
}
