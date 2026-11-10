//! Which entry of a release a host is ordered (docs/design/packages.md, section 3.3).
//!
//! A version set with targets ([`crate::releases`]) names, for each platform, an ordered list of
//! entries, one per installer. For each host of the service the coordinator picks one entry of
//! its platform's list, by the equality of installer names and nothing else:
//!
//! 1. a host that runs the service takes the entry of the installer it runs it with
//!    (`ServiceBeat.installer`), wherever it stands in the list, while its daemon still lists that
//!    installer; else it is picked nothing, [`Pick::InstallerDropped`]. The one change of
//!    installer ever picked is the move (packages-native.md section 10.6): a host that runs the
//!    service from archives (`ServiceBeat.installer` absent, or `hostkeeper-archive`), whose
//!    daemon still lists `hostkeeper-archive` (a daemon from before the archives were deleted,
//!    which makes the move) and the installer of a native entry of the list
//!    ([`is_native_entry`]), takes the first such entry in list order. A host that runs it from
//!    archives and lists no native installer of the list, or whose daemon no longer lists
//!    `hostkeeper-archive` (it can no longer move a service off archives), is picked nothing.
//!    No change between native installers, and none back to archives, is ever picked;
//! 2. any other host takes the first entry whose installer its daemon lists
//!    (`HeartbeatRequest.installers`); else it is picked nothing, [`Pick::NoBuildForHost`];
//! 3. a host of a platform the release has no list for is picked nothing, [`Pick::NoTarget`].
//!
//! An entry the host would be picked is private ([`Picks::is_private`]) when it is a repository
//! entry whose channel is one of a private repository of the registry, or an artifact entry all
//! of whose locations are in the store [`COORDINATOR_STORE`] (a `.pkg` the coordinator serves from
//! a private repository). A daemon that did not announce [`PRIVATE_FEATURE`] cannot read it, so
//! the host is picked nothing, [`Pick::NoTarget`] (an older daemon is told `no_target`).
//!
//! No pick ever returns an entry of the installer `hostkeeper-archive`, which no daemon from this
//! coordinator's release installs with; a daemon that still lists it is picked another entry or
//! none. The move is kept until no host runs a service from archives (the two counts of
//! docs/operations/move-to-native.md, stage 9), and is deleted after. A daemon that did not announce [`TARGET_FEATURE`] is picked nothing,
//! [`Pick::NoBuildForHost`]. One that did is picked from the installers it said
//! in its last heartbeat over gRPC ([`Heard`]), which the fleet keeps in memory, as it keeps
//! features: a heartbeat over the JSON route, which carries none, leaves the list as it was, and
//! until the coordinator has read one (after its start, or a daemon's first heartbeat) the host is
//! picked nothing, [`Pick::Unheard`], and keeps the other rules' reason.
//!
//! The fleet also keeps, under its lock, the releases its decisions read ([`Picks::keep`]): those
//! of every service's desired version and of every standing force's version, refreshed each time
//! the lock is taken. A version with no release kept (one set with a digest or an index, or none)
//! picks every host [`Pick::NoBuildForHost`]: nothing is ordered it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_release::ARCHIVE_INSTALLER;
use hostkeeper_release::grammar::is_installer_name;
use hostkeeper_release::target::is_native_entry;
use hostkeeper_wire::HostId;
use hostkeeper_wire::version::Version;
use hostkeeper_wire::wire::{Health, is_drift_version};

use crate::converge::PickCounts;
use crate::decide::Skip;
use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::identity::Identity;
use crate::membership::HEARTBEAT_V2;
use crate::orders::PickedTarget;
use crate::registry::{Digest, Package, index_platform};
use crate::releases::Release;
use crate::repository_push::PRIVATE_FEATURE;
use crate::service::Coordinator;
use crate::services::ServiceDef;

/// The feature a daemon announces to be sent the picked entry in its orders, and to have its
/// `installers` read.
pub const TARGET_FEATURE: &str = "target-v1";

/// The store of a location the coordinator serves from a private repository.
pub const COORDINATOR_STORE: &str = "coordinator";

/// How many installers a daemon may list.
pub const MAX_INSTALLERS: usize = 16;

/// What a daemon said of its installers in a heartbeat over gRPC.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Heard {
    /// The installers it lists, in its order.
    listed: Vec<String>,
    /// The installer of each service it named one for; a service it names none for runs from
    /// archives ([`ARCHIVE_INSTALLER`]).
    running: BTreeMap<String, String>,
    /// The drift each service reported, for the services that reported one in a good grammar.
    drift: BTreeMap<String, agent::DriftReport>,
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
        // A drift whose string is not a package version is left out and the rest is kept: no
        // pick reads a drift, so it must not cost the list.
        let drift = request
            .services
            .iter()
            .filter_map(|one| Some((one.name.clone(), one.drift.clone()?)))
            .filter(|(_, report)| {
                let fits = |text: &Option<String>| text.as_deref().is_none_or(is_drift_version);
                fits(&report.found) && fits(&report.recorded)
            })
            .collect();
        good.then_some(Self {
            listed,
            running,
            drift,
        })
    }
}

/// What the fleet keeps for the pick: the installers each host's daemon said over gRPC, and the
/// releases the decisions read.
#[derive(Clone, Debug, Default)]
pub struct Picks {
    heard: BTreeMap<HostId, Heard>,
    releases: BTreeMap<(String, String), Arc<Release>>,
    /// The channels of the registry's private repositories.
    private: BTreeSet<String>,
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

    /// Replaces the channels of the private repositories the picks read with `channels`.
    pub(crate) fn keep_private(&mut self, channels: BTreeSet<String>) {
        self.private = channels;
    }

    /// Whether `entry` needs a daemon that reads private repositories.
    fn is_private(&self, entry: &Target) -> bool {
        match &entry.repository {
            Some(channel) => self.private.contains(channel),
            None => {
                !entry.locations.is_empty()
                    && entry.locations.iter().all(|l| l.store == COORDINATOR_STORE)
            }
        }
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

    /// The drift the daemon of `id` last reported for `service` over gRPC, if its last heartbeat
    /// that was read reported one.
    pub(crate) fn drift(&self, id: &HostId, service: &str) -> Option<&agent::DriftReport> {
        self.heard.get(id)?.drift.get(service)
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

/// The pick for the host `id` of `service` at `version`. A version with no release kept (one set
/// with a package digest or a release index, or none) picks every host [`Pick::NoBuildForHost`].
pub(crate) fn pick<'a>(
    fleet: &'a Fleet,
    service: &str,
    version: &Version,
    id: &HostId,
) -> Pick<'a> {
    let Some(release) = fleet.picks.release(service, version) else {
        return Pick::NoBuildForHost;
    };
    // A platform with no list is the release's choice, whatever the daemon lists.
    let Some(list) = fleet
        .platform_of(service, id)
        .and_then(|platform| release.targets().get(index_platform(platform).as_str()))
    else {
        return Pick::NoTarget;
    };
    // A daemon that did not announce the feature lists nothing a pick can name.
    if !fleet.announced(id, TARGET_FEATURE) {
        return Pick::NoBuildForHost;
    }
    let Some(listed) = fleet.picks.listed(id) else {
        return Pick::Unheard;
    };
    // No pick returns an entry of the archive installer: a daemon from before the archives were
    // taken out may still list it, and a release kept may still name it.
    let installable =
        |entry: &&Target| entry.installer != ARCHIVE_INSTALLER && listed.contains(&entry.installer);
    let picked = if runs(fleet, service, id) {
        // A service that runs from archives names no installer, or names the archive's.
        let running = fleet
            .picks
            .running(id, service)
            .unwrap_or(ARCHIVE_INSTALLER);
        let mut entries = list.entries.iter().filter(installable);
        let found = if running == ARCHIVE_INSTALLER {
            // The move: only a daemon that still lists the archive installer runs a service from
            // archives and moves it; one that no longer does refuses the order.
            let moves = listed.iter().any(|name| name == ARCHIVE_INSTALLER);
            entries.find(|entry| moves && is_native_entry(entry))
        } else {
            entries.find(|entry| entry.installer == running)
        };
        found.map_or(Pick::InstallerDropped, Pick::Entry)
    } else {
        list.entries
            .iter()
            .find(installable)
            .map_or(Pick::NoBuildForHost, Pick::Entry)
    };
    let unreadable =
        |entry: &Target| fleet.picks.is_private(entry) && !fleet.announced(id, PRIVATE_FEATURE);
    match picked {
        Pick::Entry(entry) if unreadable(entry) => Pick::NoTarget,
        other => other,
    }
}

/// Why the host `id` is ordered nothing of `service` at `version` though every other rule lets
/// it be: the gate reason of a pick that found no entry ([`Skip::NotAMember`] for a daemon whose
/// list is not read yet: the coordinator has no word of what it can install). `None` when an
/// entry is picked.
pub(crate) fn skip(fleet: &Fleet, service: &str, version: &Version, id: &HostId) -> Option<Skip> {
    match pick(fleet, service, version, id) {
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

/// Whether the host `id` is picked an entry of `service` at `version`.
pub(crate) fn picked(fleet: &Fleet, service: &str, version: &Version, id: &HostId) -> bool {
    matches!(pick(fleet, service, version, id), Pick::Entry(_))
}

/// What an order of `service` at `version` names for the host `id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ordered {
    /// The package the order names: the entry's digest, and the release index set with the
    /// targets, if one was.
    pub(crate) package: Package,
    /// The entry the order carries.
    pub(crate) target: PickedTarget,
}

/// What an order of `service` at `version` names for the host `id`: `None` when no entry is
/// picked for the host.
pub(crate) fn order_of(
    fleet: &Fleet,
    service: &str,
    version: &Version,
    id: &HostId,
) -> Option<Ordered> {
    let Pick::Entry(entry) = pick(fleet, service, version, id) else {
        return None;
    };
    let digest = Digest::parse(&entry.digest).ok()?;
    let release = fleet.picks.release(service, version)?;
    Some(Ordered {
        package: Package {
            digest,
            index_digest: release.index_digest().cloned(),
        },
        target: PickedTarget::new(entry.clone()),
    })
}

/// How many hosts of `def` the release of `desired`'s version picks an entry for, and picks none
/// for by why; `None` when the version has no release kept, which the status leaves out as it
/// always has (the convergence of the JSON route is byte for byte what it was). The hosts are those the decision asks
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
        let found = Some(pick(fleet, service, &desired.version, id)).filter(|_| asked);
        match found {
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
/// in the heartbeat put back together, and the installer and the drift of each service it named one for. A
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
        one.drift = fleet.picks.drift(id, &one.name).cloned();
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
