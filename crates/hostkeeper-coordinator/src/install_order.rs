//! The order a host group's packages are installed on a host in (`docs/design/hostgroups.md`
//! section 1.4): the order of the group's list.
//!
//! A host is offered the install of a package only when every package listed before it is
//! installed on the host, at any version, or cannot be: the question of that package is one of
//! the four that say it is no word for this host to act on, [`Skip::NoDesired`],
//! [`Skip::NoTarget`], [`Skip::NoPackageForPlatform`] and [`Skip::NoBuildForHost`]. Until then the
//! later package waits, told [`crate::decide::Gate::InstallOrder`], and takes no place of its plan: the check is
//! made before the plan's gate, so a config package never reaches a host before the service it
//! depends on, whichever stage each plan is in. Updates are not ordered by the list.
//!
//! A package that is not installed and is in any other state holds the later ones back: a halt, a
//! hold, a version marked bad, the quiet start, a gate of its plan or an install the host is
//! ordered next. A coordinator without host groups orders nothing by the list.
//!
//! Uninstalls go the other way: the intents a `Put` wrote for one host have generations in the
//! reverse of the old list, and the uninstall of a package waits, told
//! [`crate::decide::Gate::UninstallOrder`], while a package whose intent has a lower generation is
//! still on the host, whatever gate holds that one ([`uninstall_waits`]).

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_wire::HostId;
use hostkeeper_wire::wire::Health;

use crate::decide::{Decision, Skip, consider, installed_on};
use crate::desired::Desired;
use crate::fleet::Fleet;
use crate::services::{ServiceDef, Services};

/// Whether the install of `def` on the host `id` waits for a package listed before it, in the
/// group `def` was made for. False for a service of the rollout file.
pub(crate) fn waits(
    services: &Services,
    def: &ServiceDef,
    all: &BTreeMap<String, Desired>,
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> bool {
    def.scope.is_some()
        && services
            .defs()
            .filter(|earlier| earlier.priority > def.priority && !earlier.roles.is_empty())
            .any(|earlier| holds_back(earlier, all, fleet, id, now))
}

/// Whether the uninstall of `def` on the host `id` waits for the uninstall of a package that goes
/// before it: a package a `Put` took out that the host still runs and whose intent has a lower
/// generation ([`crate::group_scope`]), so that a config package listed after its service goes
/// first whichever gate holds it. False for a service of the rollout file.
pub(crate) fn uninstall_waits(
    services: &Services,
    def: &ServiceDef,
    fleet: &Fleet,
    id: &HostId,
) -> bool {
    def.scope.is_some()
        && services.defs().any(|earlier| {
            earlier.roles.is_empty()
                && earlier.priority > def.priority
                && fleet.lists(id, &earlier.name)
                && fleet
                    .occupant(&earlier.name, id)
                    .is_some_and(|host| host.beat.health != Health::Absent)
        })
}

/// Whether `earlier`, which the host `id` is not past, holds the packages listed after it back:
/// it is not installed on the host and its own decision is not one of the four that say it
/// cannot be.
fn holds_back(
    earlier: &ServiceDef,
    all: &BTreeMap<String, Desired>,
    fleet: &Fleet,
    id: &HostId,
    now: Duration,
) -> bool {
    let name = earlier.name.as_str();
    if installed_on(fleet, name, id) {
        return false;
    }
    if fleet.is_halted_in(earlier) {
        return true;
    }
    let Some(want) = all.get(name) else {
        return false;
    };
    if fleet.is_excluded(name, id) {
        return true;
    }
    let considered = consider(earlier, want, fleet.bad_versions(name), fleet, id, now);
    !matches!(
        considered.hold,
        Some(Decision::Skip(
            Skip::NoTarget | Skip::NoPackageForPlatform | Skip::NoBuildForHost
        ))
    )
}
