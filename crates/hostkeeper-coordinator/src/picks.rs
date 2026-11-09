//! Which entry of a release a host is ordered (docs/design/packages.md, section 3.3).
//!
//! A version set with targets ([`crate::releases`]) names, for each platform, an ordered list of
//! entries, one per installer. For each host of the service the coordinator picks one entry of
//! its platform's list, by the equality of installer names and nothing else:
//!
//! 1. a host that runs the service takes the entry of the installer it runs it with
//!    (`ServiceBeat.installer`, absent for `hostkeeper-archive`), wherever it stands in the list,
//!    while its daemon still lists that installer; else it is picked nothing,
//!    [`Pick::InstallerDropped`]. Two exceptions make the move to native packages
//!    (packages-native.md section 10.6): a host that runs it from `hostkeeper-archive` takes the
//!    first native entry ([`is_native_entry`]) whose installer its daemon lists, wherever it
//!    stands, before the archive entry; and a host that runs it with a native installer, going
//!    back to a version lower than the one it runs (a rollback, automatic or an operator's set or
//!    force) whose list has no native entry, takes that list's `hostkeeper-archive` entry while
//!    its daemon lists it: the move's rollback, which the daemon makes from the archive version
//!    it keeps. No other change of installer is ever picked;
//! 2. any other host takes the first entry whose installer its daemon lists
//!    (`HeartbeatRequest.installers`); else it is picked nothing, [`Pick::NoBuildForHost`];
//! 3. a host of a platform the release has no list for is picked nothing, [`Pick::NoTarget`].
//!
//! A daemon that did not announce [`TARGET_FEATURE`] counts as listing `hostkeeper-archive` alone
//! and running every service from archives. One that did is picked from the installers it said
//! in its last heartbeat over gRPC ([`Heard`]), which the fleet keeps in memory, as it keeps
//! features: a heartbeat over the JSON route, which carries none, leaves the list as it was, and
//! until the coordinator has read one (after its start, or a daemon's first heartbeat) the host is
//! picked nothing, [`Pick::Unheard`], and keeps the other rules' reason.
//!
//! The fleet also keeps, under its lock, the releases its decisions read ([`Picks::keep`]): those
//! of every service's desired version and of every standing force's version, refreshed each time
//! the lock is taken. A version with no release kept (one set with a digest or an index, or none)
//! is ordered as before: every function here answers `None` for it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_release::Installer;
use hostkeeper_release::grammar::is_installer_name;
use hostkeeper_release::target::is_native_entry;
use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::Health;

use crate::converge::PickCounts;
use crate::decide::Skip;
use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::identity::Identity;
use crate::membership::HEARTBEAT_V2;
use crate::orders::PickedTarget;
use crate::registry::{
    Digest, IndexKey, IndexRecord, Package, Release as Registered, index_platform,
};
use crate::releases::Release;
use crate::service::Coordinator;
use crate::services::ServiceDef;

/// The feature a daemon announces to be sent the picked entry in its orders, and to have its
/// `installers` read.
pub const TARGET_FEATURE: &str = "target-v1";

/// How many installers a daemon may list.
pub const MAX_INSTALLERS: usize = 16;

/// The installer of today's archives: the one every daemon runs.
const ARCHIVE: &str = Installer::HostkeeperArchive.as_str();

/// What a daemon said of its installers in a heartbeat over gRPC.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Heard {
    /// The installers it lists, in its order.
    listed: Vec<String>,
    /// The installer of each service it named one for; a service it names none for runs from
    /// archives.
    running: BTreeMap<String, String>,
}

impl Heard {
    /// What `request` says, or `None` when a name breaks a rule: more than [`MAX_INSTALLERS`]
    /// installers, one listed twice, or one that is not 1 to 32 bytes of `[a-z0-9-]`, listed or
    /// named by a service. Such a heartbeat is answered, and the list read before it is kept.
    pub(crate) fn of(request: &agent::HeartbeatRequest) -> Option<Self> {
        let listed = request.installers.clone();
        let running: BTreeMap<String, String> = request
            .services
            .iter()
            .filter_map(|one| Some((one.name.clone(), one.installer.clone()?)))
            .collect();
        let mut sorted: Vec<&String> = listed.iter().collect();
        sorted.sort();
        sorted.dedup();
        let good = listed.len() <= MAX_INSTALLERS
            && sorted.len() == listed.len()
            && listed
                .iter()
                .chain(running.values())
                .all(|n| is_installer_name(n));
        good.then_some(Self { listed, running })
    }
}

/// What the fleet keeps for the pick: the installers each host's daemon said over gRPC, and the
/// releases the decisions read.
#[derive(Clone, Debug, Default)]
pub struct Picks {
    heard: BTreeMap<HostId, Heard>,
    releases: BTreeMap<(String, String), Arc<Release>>,
}

impl Picks {
    /// Notes what the daemon of `id` said over gRPC, replacing what it said before.
    pub(crate) fn hear(&mut self, id: HostId, heard: Heard) {
        self.heard.insert(id, heard);
    }

    /// Forgets what the daemon of `id` said: the host was released ([`Fleet::release_if_final`]).
    pub(crate) fn forget(&mut self, id: &HostId) {
        self.heard.remove(id);
    }

    /// Replaces the releases the decisions read with `releases`, by service and version.
    pub(crate) fn keep(&mut self, releases: BTreeMap<(String, String), Arc<Release>>) {
        self.releases = releases;
    }

    /// Adds `release` as the release of `service` at `version` (a force's, before its plan).
    pub(crate) fn keep_one(&mut self, service: &str, version: &Version, release: Arc<Release>) {
        self.releases
            .insert((service.to_owned(), version.as_str().to_owned()), release);
    }

    /// The installers the daemon of `id` listed in its last heartbeat over gRPC, if one was read.
    pub(crate) fn listed(&self, id: &HostId) -> Option<&[String]> {
        self.heard.get(id).map(|heard| heard.listed.as_slice())
    }

    /// The installer the daemon of `id` said it runs `service` with, over gRPC.
    pub(crate) fn running(&self, id: &HostId, service: &str) -> Option<&str> {
        self.heard.get(id)?.running.get(service).map(String::as_str)
    }

    fn release(&self, service: &str, version: &Version) -> Option<&Arc<Release>> {
        self.releases
            .get(&(service.to_owned(), version.as_str().to_owned()))
    }
}

/// What the pick found for one host.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Pick<'a> {
    /// The entry the host is ordered.
    Entry(&'a Target),
    /// The release has no list for the host's platform.
    NoTarget,
    /// The host does not run the service, and no entry is of an installer its daemon lists.
    NoBuildForHost,
    /// The host runs the service with an installer the list has no entry of, or its daemon no
    /// longer lists.
    InstallerDropped,
    /// The daemon announced [`TARGET_FEATURE`] and the coordinator has read no list of it yet.
    Unheard,
}

/// Whether the host `id` runs `service`, as the decision reads it: it has a word of the service
/// that its last heartbeat still lists, and it is not `absent`.
fn runs(fleet: &Fleet, service: &str, id: &HostId) -> bool {
    fleet
        .get_in(service, id)
        .filter(|_| {
            fleet
                .node(id)
                .is_none_or(|node| node.announced.contains_key(service))
        })
        .is_some_and(|host| host.beat.health != Health::Absent)
}

/// Whether `installer` is native ([`Installer::is_native`]) by its exact spelling.
fn is_native(installer: &str) -> bool {
    installer.parse().is_ok_and(Installer::is_native)
}

/// Whether ordering the host `id` `service` at `version` goes back: the host's last word of the
/// service names a later version, by [`Version::at_least`].
fn rolls_back(fleet: &Fleet, service: &str, version: &Version, id: &HostId) -> bool {
    fleet
        .get_in(service, id)
        .is_some_and(|host| !version.at_least(&host.beat.version))
}

/// The pick for the host `id` of `service` at `version`, with the release it is of; `None` when
/// the version has no release kept.
pub(crate) fn pick<'a>(
    fleet: &'a Fleet,
    service: &str,
    version: &Version,
    id: &HostId,
) -> Option<(Pick<'a>, &'a Release)> {
    let release = fleet.picks.release(service, version)?;
    // A platform with no list is the release's choice, whatever the daemon lists.
    let Some(list) = fleet
        .platform_of(service, id)
        .and_then(|platform| release.targets().get(index_platform(platform).as_str()))
    else {
        return Some((Pick::NoTarget, release));
    };
    let archive = [ARCHIVE.to_owned()];
    let (listed, running) = if fleet.announced(id, TARGET_FEATURE) {
        let Some(listed) = fleet.picks.listed(id) else {
            return Some((Pick::Unheard, release));
        };
        (listed, fleet.picks.running(id, service).unwrap_or(ARCHIVE))
    } else {
        (&archive[..], ARCHIVE)
    };
    let lists = |installer: &str| listed.iter().any(|name| name == installer);
    let picked = if runs(fleet, service, id) {
        let listed_of = |installer: &str| {
            list.entries
                .iter()
                .find(|entry| entry.installer == installer && lists(installer))
        };
        let native = || {
            list.entries
                .iter()
                .find(|entry| is_native_entry(entry) && lists(&entry.installer))
        };
        let found = if running == ARCHIVE {
            // The move: a native entry its daemon lists before the archive it runs.
            native().or_else(|| listed_of(ARCHIVE))
        } else {
            listed_of(running).or_else(|| {
                // The move's rollback: back to a version whose list has no native entry.
                let back = rolls_back(fleet, service, version, id)
                    && is_native(running)
                    && !list.entries.iter().any(is_native_entry);
                back.then(|| listed_of(ARCHIVE)).flatten()
            })
        };
        found.map_or(Pick::InstallerDropped, Pick::Entry)
    } else {
        list.entries
            .iter()
            .find(|entry| lists(&entry.installer))
            .map_or(Pick::NoBuildForHost, Pick::Entry)
    };
    Some((picked, release))
}

/// Why the host `id` is ordered nothing of `service` at `version` though every other rule lets
/// it be: the gate reason of a pick that found no entry ([`Skip::NotAMember`] for a daemon whose
/// list is not read yet: the coordinator has no word of what it can install). `None` when an
/// entry is picked or the version has no release kept.
pub(crate) fn skip(fleet: &Fleet, service: &str, version: &Version, id: &HostId) -> Option<Skip> {
    match pick(fleet, service, version, id)?.0 {
        Pick::Entry(_) => None,
        Pick::NoTarget => Some(Skip::NoTarget),
        Pick::NoBuildForHost => Some(Skip::NoBuildForHost),
        Pick::InstallerDropped => Some(Skip::InstallerDropped),
        Pick::Unheard => Some(Skip::NotAMember),
    }
}

/// Whether the release picked the host nothing by its own choice (no list for its platform, no
/// entry it can install, the installer it runs dropped): such a host is not counted, not missing
/// and not failed. A host whose list is not read yet is not one.
pub(crate) fn unpicked(fleet: &Fleet, service: &str, version: &Version, id: &HostId) -> bool {
    skip(fleet, service, version, id).is_some_and(|why| why != Skip::NotAMember)
}

/// Whether the host `id` is picked an entry of `service` at `version`; `None` when the version
/// has no release kept.
pub(crate) fn picked(fleet: &Fleet, service: &str, version: &Version, id: &HostId) -> Option<bool> {
    pick(fleet, service, version, id).map(|(found, _)| matches!(found, Pick::Entry(_)))
}

/// What an order of `service` at `version` names for the host `id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ordered {
    /// The package the order names: the entry's digest, and the release index set with the
    /// targets, if one was.
    pub(crate) package: Package,
    /// The entry, for a daemon that announced [`TARGET_FEATURE`].
    pub(crate) target: Option<PickedTarget>,
    /// Whether the entry is an archive: the only one an order on the JSON route may name.
    pub(crate) archive: bool,
}

/// What an order of `service` at `version` names for the host `id`: `None` when the version has
/// no release kept (it is ordered as before), `Some(None)` when no entry is picked for the host.
pub(crate) fn order_of(
    fleet: &Fleet,
    service: &str,
    version: &Version,
    id: &HostId,
) -> Option<Option<Ordered>> {
    let (found, release) = pick(fleet, service, version, id)?;
    let Pick::Entry(entry) = found else {
        return Some(None);
    };
    Some(Digest::parse(&entry.digest).ok().map(|digest| {
        Ordered {
            package: Package {
                digest,
                index_digest: release.index_digest().cloned(),
            },
            target: fleet
                .announced(id, TARGET_FEATURE)
                .then(|| PickedTarget::new(entry.clone())),
            archive: entry.installer == ARCHIVE,
        }
    }))
}

/// The package digest an install of `service` at `version` names for the host `id`: the picked
/// entry's; `None` when the version has no release kept, `Some(None)` when nothing is picked.
pub(crate) fn package_for(
    fleet: &Fleet,
    service: &str,
    version: &Version,
    id: &HostId,
) -> Option<Option<Package>> {
    order_of(fleet, service, version, id).map(|ordered| ordered.map(|o| o.package))
}

/// The release a force of a version set with targets records in the ops file, which names one
/// digest: the digest of the release index set with the targets, else the SHA-256 of the
/// release's stored form, with the archive entries as the index's packages. Only this
/// coordinator's pick reads the entries; an older coordinator, which reads the record alone,
/// orders a daemon an archive entry of its platform under that digest.
pub(crate) fn recorded(release: &Release) -> Option<Registered> {
    let stored = || crate::ops_file::digest(release.to_json().to_string().as_bytes());
    let packages = release
        .targets()
        .iter()
        .filter_map(|(platform, list)| {
            let entry = list.entries.iter().find(|e| e.installer == ARCHIVE);
            let key = platform.parse().ok().map(IndexKey::of);
            key.zip(entry.and_then(|e| Digest::parse(&e.digest).ok()))
        })
        .collect();
    release
        .index_digest()
        .cloned()
        .or_else(|| Digest::parse(&stored()).ok())
        .map(|digest| Registered::Index(IndexRecord { digest, packages }))
}

/// How many hosts of `def` the release of `desired`'s version picks an entry for, and picks none
/// for by why; `None` when the version has no release kept. The hosts are those the decision asks
/// of: assigned the service, not excluded from it, not held, and with a word of the service or
/// a daemon that lists every service it runs.
pub(crate) fn counts(def: &ServiceDef, desired: &Desired, fleet: &Fleet) -> Option<PickCounts> {
    let service = def.name.as_str();
    fleet.picks.release(service, &desired.version)?;
    let ids: BTreeSet<&HostId> = fleet
        .nodes()
        .map(|(id, _)| id)
        .chain(fleet.hosts_of(service).map(|(id, _)| id))
        .collect();
    let mut counts = PickCounts::default();
    for id in ids {
        let asked = fleet.is_assigned(def, id)
            && !fleet.is_excluded(service, id)
            && !fleet.is_held(service, id)
            && (fleet.get_in(service, id).is_some() || fleet.announced(id, HEARTBEAT_V2));
        let found = pick(fleet, service, &desired.version, id).filter(|_| asked);
        match found.map(|(found, _)| found) {
            Some(Pick::Entry(_)) => counts.picked += 1,
            Some(Pick::NoTarget) => counts.no_target += 1,
            Some(Pick::NoBuildForHost) => counts.no_build_for_host += 1,
            Some(Pick::InstallerDropped) => {
                counts.installer_dropped += 1;
                counts.dropped.push(id.to_string());
            }
            Some(Pick::Unheard) | None => {}
        }
    }
    Some(counts)
}

impl Coordinator {
    /// Notes the installers a heartbeat over gRPC from `who` said ([`Heard`]), when its daemon
    /// announced [`TARGET_FEATURE`]: what the picks of this heartbeat and the next ones read,
    /// until another heartbeat over gRPC says others. A list that breaks a rule is counted and
    /// logged as a dropped fact ([`Coordinator::note_extras_dropped`]), and the one read before
    /// is kept.
    pub(crate) fn hear_installers(&self, who: &Identity, request: &agent::HeartbeatRequest) {
        if !request.features.iter().any(|f| f == TARGET_FEATURE) {
            return;
        }
        match Heard::of(request) {
            // Only the list is written: the marks and releases a decision reads are refreshed when
            // the heartbeat takes the lock to decide.
            Some(heard) => self
                .fleet
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .picks
                .hear(who.host_id.clone(), heard),
            None => {
                self.note_extras_dropped(who, "the installers break a rule", &["installers"]);
            }
        }
    }
}

/// Fills in `host`, as `Hosts.List` and `Hosts.Get` made it for the host `id`, with what the
/// fleet keeps of its installers: the list its daemon last said over gRPC, in `installers` and
/// in the heartbeat put back together, and the installer of each service it named one for. A
/// service of `services` the host does not list, whose desired version (of `desired`) was set
/// with targets that pick the host no entry, is added with that gate reason (`no_target`,
/// `no_build_for_host`), so the host shows why it is not ordered beside the installers it lists.
pub(crate) fn show(
    fleet: &Fleet,
    id: &HostId,
    host: &mut operator::Host,
    services: &crate::services::Services,
    desired: &BTreeMap<String, Desired>,
) {
    let listed = fleet
        .picks
        .listed(id)
        .map(<[String]>::to_vec)
        .unwrap_or_default();
    let mut beat = host.heartbeat.take().unwrap_or_default();
    beat.installers.clone_from(&listed);
    for one in &mut beat.services {
        one.installer = fleet.picks.running(id, &one.name).map(str::to_owned);
    }
    host.heartbeat = Some(beat);
    host.installers = listed;
    for def in services.defs() {
        let name = def.name.as_str();
        let reason = desired
            .get(name)
            .filter(|_| fleet.is_assigned(def, id) && !fleet.is_excluded(name, id))
            .filter(|_| !host.services.iter().any(|one| one.service == name))
            .and_then(|want| skip(fleet, name, &want.version, id))
            .and_then(|why| match why {
                Skip::NoTarget => Some(operator::GateReason::NoTarget),
                Skip::NoBuildForHost => Some(operator::GateReason::NoBuildForHost),
                _ => None,
            });
        host.services
            .extend(reason.map(|reason| operator::HostService {
                service: name.to_owned(),
                gate_reason: reason.into(),
                ..operator::HostService::default()
            }));
    }
    host.services.sort_by(|a, b| a.service.cmp(&b.service));
}
