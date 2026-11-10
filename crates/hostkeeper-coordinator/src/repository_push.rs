//! The repositories the coordinator sends a host (docs/design/packages-repositories.md,
//! section 3.2): what its daemon reported of them, the set the registry makes for it, and what
//! `Hosts.List` and `Hosts.Get` show.
//!
//! A daemon that announces [`FEATURE`] (a Linux daemon) reports, in every heartbeat over gRPC, the
//! set in force on the host and the last set it refused ([`Report`]). The coordinator keeps the
//! last report it read of each host in memory, never durably: a report that breaks a rule is
//! dropped, counted and logged as a dropped fact ([`Coordinator::note_extras_dropped`]), and the
//! one read before is kept.
//!
//! The set a host needs is the repositories of the services assigned to it and not excluded, and
//! of those it announced ([`needed`]). It is sent, whole, in the answer to a heartbeat over gRPC
//! ([`Coordinator::repository_set`]) when it names at least one repository and at most
//! [`MAX_SET`], the registry holds every one, and the host's report holds neither the set's
//! digest in force nor that digest refused. A host that does not announce the feature, a host
//! that beats over the JSON route and a Mac are sent nothing and shown nothing.
//!
//! The set is made for the host, by its group ([`host_group`]): each entry goes through
//! `registry::for_host`, so a private repository reaches a host only when its suites list the
//! host's group, with that group as its one suite and without its credential file; one that does
//! not list it is left out and the rest is sent. A private repository is sent only to a daemon
//! that announced [`PRIVATE_FEATURE`] (it mirrors); a set made of nothing a host may be sent is
//! not sent. The digest is the digest of the host's own set.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_proto::hostkeeper::operator::v1 as operator;
use hostkeeper_proto::hostkeeper::release::v1::Repository;
use hostkeeper_release::grammar::is_sha256;
use hostkeeper_release::registry::{self, MAX_SET};
use hostkeeper_wire::HostId;

use crate::fleet::Fleet;
use crate::identity::Identity;
use crate::repository_serve::host_group;
use crate::service::Coordinator;

/// The feature a daemon announces to be sent the repositories its services name and to report
/// them; the coordinator advertises it ([`crate::service::FEATURES`]).
pub const FEATURE: &str = "repositories-v1";

/// The feature a daemon announces to be sent the private repositories of its group: it reads them
/// from the coordinator ([`crate::repository_serve`]) into a mirror. The coordinator reads it
/// and does not advertise it.
pub const PRIVATE_FEATURE: &str = "private-repository-v1";

/// The most bytes of the reason of a refusal a report keeps.
pub(crate) const MAX_REASON: usize = 256;

/// A set a daemon refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refused {
    /// The set's digest.
    pub(crate) digest: String,
    /// Why, as the daemon worded it: at most [`MAX_REASON`] bytes.
    pub(crate) reason: String,
}

/// What a daemon reported of the repositories: the generation and digest of the set in force
/// (0 and empty while it holds none the coordinator sent), and the last set it refused.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) generation: u64,
    pub(crate) digest: String,
    pub(crate) refused: Option<Refused>,
}

impl Report {
    /// The report `said` holds, or `None` when it breaks a rule: a digest that is neither empty
    /// nor 64 lower-case hex characters, a generation that is 0 with a digest or not 0 without
    /// one, or a refusal whose digest is no digest or whose reason is empty. A reason longer than
    /// [`MAX_REASON`] bytes is cut at a character boundary.
    pub(crate) fn of(said: &agent::RepositoriesReport) -> Option<Self> {
        let digest_ok = said.digest.is_empty() || is_sha256(&said.digest);
        if !digest_ok || (said.generation == 0) != said.digest.is_empty() {
            return None;
        }
        let refused = match &said.refused {
            None => None,
            Some(refusal) => {
                if !is_sha256(&refusal.digest) || refusal.reason.is_empty() {
                    return None;
                }
                Some(Refused {
                    digest: refusal.digest.clone(),
                    reason: cut(&refusal.reason, MAX_REASON).to_owned(),
                })
            }
        };
        Some(Self {
            generation: said.generation,
            digest: said.digest.clone(),
            refused,
        })
    }

    /// Whether the set of `digest` is in force on the host or was refused by it: it is not sent
    /// again.
    fn holds(&self, digest: &str) -> bool {
        self.digest == digest || self.refused.as_ref().is_some_and(|r| r.digest == digest)
    }
}

/// `text` cut to at most `max` bytes, at a character boundary.
fn cut(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The last report of each host that announced [`FEATURE`], in memory under a lock of its own,
/// taken after the fleet's and holding nothing else.
#[derive(Debug, Default)]
pub(crate) struct Reports {
    seen: Mutex<BTreeMap<HostId, Report>>,
}

impl Reports {
    /// Notes `report` as the host `id`'s last, or, with none, that it announced the feature: a
    /// host that has not reported yet holds no set.
    fn note(&self, id: &HostId, report: Option<Report>) {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        match report {
            Some(report) => {
                seen.insert(id.clone(), report);
            }
            None => {
                seen.entry(id.clone()).or_default();
            }
        }
    }

    /// The last report of the host `id`; none for a host that never announced the feature.
    pub(crate) fn of(&self, id: &HostId) -> Option<Report> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
    }
}

/// What the registry makes of a host's needs.
struct Plan {
    /// The repositories its services name.
    named: BTreeSet<String>,
    /// Those the registry does not hold.
    unregistered: Vec<String>,
    /// The set it would be sent, if it needs one the registry can make: its digest and entries.
    set: Option<(String, Vec<Repository>)>,
    /// The registry's generation the set is made at.
    generation: u64,
}

/// The names of the repositories the host `id` needs: those of the services assigned to it and
/// not excluded, and of the services it announced. In group mode the services placed on it are
/// the packages of its group, not what the rollout file and the assignments place.
fn needed(coordinator: &Coordinator, fleet: &Fleet, id: &HostId) -> BTreeSet<String> {
    let grouped = coordinator.desired.in_group_mode();
    let placed = grouped.then(|| coordinator.basis_of(id));
    let of_group = placed.iter().flat_map(|basis| basis.services.defs());
    let of_file = coordinator.services.defs().filter(|def| {
        (!grouped && fleet.is_assigned(def, id) && !fleet.is_excluded(&def.name, id))
            || fleet.lists(id, &def.name)
    });
    of_group
        .chain(of_file)
        .map(|def| coordinator.repository_of(&def.name))
        .collect()
}

/// The entries of `entries` the host of `group` is sent: every public one, and the private ones
/// when its daemon reads them (`private`), as `registry::for_host` makes them.
fn for_group(entries: &[Repository], group: &str, private: bool) -> Vec<Repository> {
    entries
        .iter()
        .filter(|entry| private || !registry::is_private(entry))
        .filter_map(|entry| registry::for_host(entry, group))
        .collect()
}

impl Coordinator {
    /// Notes what a heartbeat over gRPC from `who` said of the repositories, when its daemon
    /// announced [`FEATURE`]: the report, or, when it breaks a rule, nothing (counted and logged
    /// as a dropped fact; the report read before is kept).
    pub(crate) fn hear_repositories(&self, who: &Identity, request: &agent::HeartbeatRequest) {
        if !request.features.iter().any(|f| f == FEATURE) {
            return;
        }
        let report = match &request.repositories {
            None => Some(Report::default()),
            Some(said) => Report::of(said),
        };
        if report.is_none() {
            self.note_extras_dropped(
                who,
                "the repositories report breaks a rule",
                &["repositories"],
            );
        }
        self.repository_reports.note(&who.host_id, report);
    }

    fn repository_plan(&self, fleet: &Fleet, id: &HostId) -> Plan {
        let named = needed(self, fleet, id);
        let group = host_group(id, fleet.role_in("", id));
        let private = fleet.announced(id, PRIVATE_FEATURE);
        let (unregistered, set, generation) = self.desired.with_repositories(|registry| {
            let unregistered: Vec<String> = named
                .iter()
                .filter(|name| registry.get(name).is_none())
                .cloned()
                .collect();
            let set = (unregistered.is_empty() && !named.is_empty() && named.len() <= MAX_SET)
                .then(|| registry.entries_of(named.iter().map(String::as_str)))
                .flatten()
                .map(|entries| for_group(&entries, &group, private))
                .filter(|entries| !entries.is_empty())
                .map(|entries| (registry::digest(&entries), entries));
            (unregistered, set, registry.generation)
        });
        Plan {
            named,
            unregistered,
            set,
            generation,
        }
    }

    /// The set the answer to a heartbeat over gRPC from the host `id` carries, if it carries
    /// one: the registry's generation, the digest of the set and its entries, when the host
    /// announced [`FEATURE`], needs a set the registry can make, and its report holds neither the
    /// set's digest in force nor refused.
    pub(crate) fn repository_set(&self, id: &HostId) -> Option<agent::RepositorySet> {
        let report = self.repository_reports.of(id)?;
        let plan = {
            let fleet = self.lock_fleet();
            fleet
                .announced(id, FEATURE)
                .then(|| self.repository_plan(&fleet, id))?
        };
        let (digest, entries) = plan.set.filter(|(digest, _)| !report.holds(digest))?;
        Some(agent::RepositorySet {
            generation: plan.generation,
            digest,
            entries,
        })
    }

    /// Fills in `host`, as `Hosts.List` and `Hosts.Get` made it for the host `id` (`fleet` is
    /// the fleet as they read it), with the repositories it needs and what its daemon reported
    /// of them, when the daemon announced [`FEATURE`].
    pub(crate) fn show_repositories(&self, fleet: &Fleet, id: &HostId, host: &mut operator::Host) {
        let Some(report) = self.repository_reports.of(id) else {
            return;
        };
        if !fleet.announced(id, FEATURE) {
            return;
        }
        let plan = self.repository_plan(fleet, id);
        let digest = plan
            .set
            .as_ref()
            .map(|(digest, _)| digest.clone())
            .unwrap_or_default();
        let refused = report
            .refused
            .as_ref()
            .filter(|r| r.digest == digest)
            .map(|r| r.reason.clone())
            .unwrap_or_default();
        host.repositories = Some(operator::HostRepositories {
            named: plan.named.into_iter().collect(),
            unregistered: plan.unregistered,
            current: !digest.is_empty() && report.digest == digest,
            digest,
            generation: report.generation,
            refused,
        });
    }
}
