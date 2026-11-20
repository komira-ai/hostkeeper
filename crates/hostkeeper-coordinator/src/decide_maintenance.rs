//! Which host starts its turn of a maintenance run, and when (`docs/design/maintenance-updates.md`,
//! section 5.1): maintenance's own rules of the rollout gate, decided per heartbeat with the
//! packages' ([`crate::decide`]), in the same transaction under the gate.
//!
//! One rule is shared with packages: **one thing at a time on a host**. A host in its turn (from
//! the order that starts it until it is done, deferred, or put back: [`Step::in_turn`]) is given no
//! package order, and a host with a package update running, or a package order outstanding
//! ([`Fleet::is_busy_besides`]), is not started. The other rules are maintenance's own, checked
//! for a host whose turn waits ([`Step::Waiting`]), or deferred ([`Step::Deferred`]), in a run that
//! has not ended, in this order:
//!
//! 1. **Quiet start and window:** no host starts while the coordinator is in its quiet start
//!    (`--quiet-secs`: the fleet it counts is still being rebuilt from heartbeats, [`Hold::Quiet`]),
//!    nor before the run's window opens or once it has ended ([`Hold::Window`]). A host already
//!    started finishes: nothing below is asked of it.
//! 2. **Halt:** once a host of the run halted or is not back in a group, no further host of that
//!    group starts ([`Hold::Halted`]); the run's other groups go on. The halted host keeps its
//!    batch place.
//! 3. **Backoff:** a host that deferred its turn (its stage failed, or its drain deadline
//!    passed) waits its group's `defer_backoff` since, the n-th deferral of the turn longer than
//!    the one before ([`crate::defer::DeferBackoff::wait`], [`Hold::Backoff`]).
//! 4. **The host itself** ([`eligible`]): it is live, its daemon announces `maintenance-v1`
//!    ([`Hold::NoFeature`]), no hold stands on it and it is excluded from no package ([`Hold::Held`],
//!    [`Hold::Excluded`]), its group is still the one the run took it in ([`Hold::GroupChanged`]),
//!    and it runs no package update and has no package order outstanding
//!    ([`Hold::InPackageUpdate`]). These are read afresh at each heartbeat and stored nowhere: a
//!    hold released, or a package update ended, lets the host go at a later heartbeat.
//! 5. **Batch:** at most `batch` hosts of a group (the run's, else the group's) hold a place, over
//!    every run; groups go in parallel ([`Hold::Batch`]). A host holds its place in its turn (a
//!    halted or not back host included) and, once done, until it has sent its group's
//!    `bake_beats` healthy heartbeats in a row ([`baked`], design section 5.6); a deferred host
//!    holds none.
//! 6. **Order of hosts:** the free places go to the run's hosts of the group that wait, or deferred
//!    and have backed off, and are eligible now (rule 4): first the hosts that wait, by node id,
//!    then the deferred ones, the fewest offers first, then by node id, so a deferred host is
//!    offered again after the group's other hosts; a host past them waits ([`Hold::NotNext`]).
//! 7. **Serving count:** a host is not taken while fewer than `min_serving + 1` hosts of its group
//!    are serving ([`Hold::Serving`]): live, no service reporting unhealthy, and not in a turn of any
//!    run, so a host staging, draining, rebooting, halted or not back never counts. In a run that
//!    may take a group's last host (`take_last_host`, which `Schedule` asks for whenever the run
//!    takes every host of a group), the count needed is at most the number of the group's hosts
//!    the coordinator has heard: a host is then also taken when every host of its group serves, so
//!    a group of one is taken, and a larger group still keeps `min_serving` while its hosts' turns
//!    overlap.
//!
//! A host that passes every rule is sent a `maintain` with the action `RUN`, the order id of its
//! offer ([`RunHost::next_order_id`]: `maintain-<run id>` the first time, `maintain-<run id>-<n>`
//! for the n-th offer after a deferral, since a daemon answers an order it holds with the end of
//! that order's turn) and try 1 ([`Ordered`]); the caller stores its step as
//! [`Step::Staging`] in the same change, so it holds its batch place and is out of the serving
//! count from the order on. The step then follows what the host's daemon reports of the run
//! ([`folded`]): a report that names the run and the order id of the host's offer moves it (so the
//! record of an earlier offer, deferred, does not take a host offered again back); a daemon's
//! `MAINTENANCE_STEP_WAITING` (ordered, not started) keeps it at staging. Until the daemon reports
//! the order id, each heartbeat of the host is sent the same order again (the same id and try), so
//! an answer that was lost is not a run that never starts, and a daemon that holds the order
//! recognises it and does not start it twice; once it reports the id, nothing more is sent, also
//! after a coordinator restart, since the step is stored with the run.
//!
//! A run whose window ended, or that was cancelled, starts no host: its hosts that had not started
//! (waiting, or deferred) are not reached ([`Run::shown`], [`Run::cancel`]).

use std::collections::BTreeMap;
use std::time::Duration;

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_wire::HostId;
use hostkeeper_wire::wire::Health;

use crate::defer::DeferBackoff;
use crate::fleet::{Fleet, Node};
use crate::maintenance::FEATURE;
use crate::maintenance_effective::effective_of;
use crate::maintenance_policy::{LinuxScope, Maintenance as Policy};
use crate::maintenance_runs::{Kind, MAX_OFFERED_AGAIN, Run, RunHost, Skip, Step};
use crate::ops::OpsState;
use crate::orders::Maintain;

/// Why a host whose turn waits is not started at this heartbeat. Nothing of it is stored: the host
/// is asked again at its next heartbeat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hold {
    /// The coordinator is in its quiet start.
    Quiet,
    /// The run's window has not opened, or has ended.
    Window,
    /// A host of the run halted or is not back in the host's group.
    Halted,
    /// The host deferred its turn, and its group's `defer_backoff` since has not passed.
    Backoff,
    /// The host's daemon does not announce `maintenance-v1`.
    NoFeature,
    /// A hold stands on the host for a package.
    Held,
    /// The host is excluded from a package.
    Excluded,
    /// The host's group is not the one the run took it in.
    GroupChanged,
    /// A package update of the host runs, or a package order of it is outstanding.
    InPackageUpdate,
    /// The coordinator has not heard the host within its liveness bound.
    NotLive,
    /// As many hosts of its group are in their turn as the batch lets.
    Batch,
    /// Hosts of its group with a lower node id take the free places.
    NotNext,
    /// Fewer than `min_serving + 1` hosts of its group are serving.
    Serving,
}

/// What the rules make of one host at one heartbeat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// No run holds a turn of the host that is open: packages are decided as ever.
    Free,
    /// The host is in its turn: it is given no package order, and `resend`, when its daemon has not
    /// reported the order, is sent again.
    InTurn {
        /// The order to send again, when the host has not reported it.
        resend: Option<Ordered>,
    },
    /// The host waits in a run, held back by the rule named; packages are decided as ever.
    Waits(Hold),
    /// Start the host's turn: send it the order and store its step as staging.
    Start(Ordered),
}

/// A `maintain` decided for a host, with where its turn is held.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Ordered {
    /// The run's id.
    pub(crate) run_id: String,
    /// The group the run took the host in; empty: none.
    pub(crate) host_group: String,
    /// The order's id ([`order_id`]).
    pub(crate) order_id: String,
    /// The arguments.
    pub(crate) maintain: Maintain,
}

/// What the rules read.
pub(crate) struct View<'a> {
    /// The ops state: the runs, the holds and the exclusions.
    pub(crate) state: &'a OpsState,
    /// The fleet the heartbeats built.
    pub(crate) fleet: &'a Fleet,
    /// The group of each enrolled host now, by node id (a host not named is in none).
    pub(crate) groups: &'a BTreeMap<String, String>,
    /// The maintenance table of the group the host's turn is in ([`turn_group`]): the group's,
    /// or every default for a host in none.
    pub(crate) policy: &'a Policy,
    /// That group's `bake_beats`: the healthy heartbeats a done host holds its batch place for
    /// (0 for a host in none).
    pub(crate) bake_beats: u32,
    /// That group's `defer_backoff`: how long a deferred host waits before it is offered again.
    pub(crate) defer_backoff: DeferBackoff,
    /// The ops store's clock, in unix seconds: what the runs' windows are read against.
    pub(crate) unix_now: u64,
    /// The coordinator's clock: what the fleet's liveness and orders are read against.
    pub(crate) now: Duration,
}

impl View<'_> {
    /// The group of the host `id` now; empty: none.
    fn group_of(&self, id: &str) -> &str {
        self.groups.get(id).map_or("", String::as_str)
    }
}

/// Where the host `id` stands in the runs of `state`, without reading its group: in its turn
/// (and whether its order is to be sent again), waiting in a run whose window may be open, or in
/// none.
fn turn(state: &OpsState, fleet: &Fleet, id: &HostId, unix_now: u64) -> Turn {
    let runs = &state.maintenance_runs;
    for (r, run) in runs.iter().enumerate() {
        if let Some(h) = run
            .hosts
            .iter()
            .position(|host| host.host_id == id.as_str())
            && run.hosts[h].step.in_turn()
        {
            let unreported =
                run.hosts[h].step == Step::Staging && !reported(fleet, id, run, &run.hosts[h]);
            return Turn::In {
                run: r,
                host: h,
                unreported,
            };
        }
    }
    runs.iter()
        .enumerate()
        .filter(|(_, run)| !run.ended_at(unix_now))
        .find_map(|(r, run)| {
            run.hosts
                .iter()
                .position(|host| host.host_id == id.as_str() && offered(host))
                .map(|h| Turn::Waiting { run: r, host: h })
        })
        .unwrap_or(Turn::None)
}

/// Whether `host` has not started and may be offered its turn: it waits, or it deferred and has
/// not been offered again [`MAX_OFFERED_AGAIN`] times.
fn offered(host: &RunHost) -> bool {
    match host.step {
        Step::Waiting => true,
        Step::Deferred => host.offered_again < MAX_OFFERED_AGAIN,
        _ => false,
    }
}

/// [`turn`]'s answer: indices into the ops state's runs and the run's hosts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Turn {
    /// In no open turn.
    None,
    /// In its turn; `unreported` while its order was sent and its daemon has not named it.
    In {
        run: usize,
        host: usize,
        unreported: bool,
    },
    /// Waiting, or deferred, in a run that has not ended.
    Waiting { run: usize, host: usize },
}

/// Whether the host's daemon reports a run record of `run` taken by the order of `host`'s turn.
fn reported(fleet: &Fleet, id: &HostId, run: &Run, host: &RunHost) -> bool {
    reported_run(fleet.node(id))
        .is_some_and(|said| said.run_id == run.id && said.order_id == host.order_id(&run.id))
}

/// The run record the host's last kept report names, if any.
fn reported_run(node: Option<&Node>) -> Option<&agent::MaintenanceRunState> {
    node?.maintenance.as_ref()?.report().run.as_ref()
}

/// The group the host `id`'s open turn was taken in ([`turn`]), and whether that turn waits (or
/// deferred); none
/// when it has none: what the caller reads the group's table for before it asks [`decide`].
pub(crate) fn turn_group(
    state: &OpsState,
    fleet: &Fleet,
    id: &HostId,
    unix_now: u64,
) -> Option<(String, bool)> {
    let runs = &state.maintenance_runs;
    match turn(state, fleet, id, unix_now) {
        Turn::None => None,
        Turn::In { run, host, .. } => Some((runs[run].hosts[host].host_group.clone(), false)),
        Turn::Waiting { run, host } => Some((runs[run].hosts[host].host_group.clone(), true)),
    }
}

/// What the rules make of the host `id` at this heartbeat.
pub(crate) fn decide(view: &View<'_>, id: &HostId) -> Verdict {
    let runs = &view.state.maintenance_runs;
    match turn(view.state, view.fleet, id, view.unix_now) {
        Turn::None => Verdict::Free,
        Turn::In {
            run,
            host,
            unreported,
        } => {
            let (run, host) = (&runs[run], &runs[run].hosts[host]);
            Verdict::InTurn {
                resend: unreported.then(|| ordered(view, run, host, host.order_id(&run.id))),
            }
        }
        Turn::Waiting { run, host } => {
            let (run, host) = (&runs[run], &runs[run].hosts[host]);
            match start(view, run, host) {
                Ok(()) => Verdict::Start(ordered(view, run, host, host.next_order_id(&run.id))),
                Err(hold) => Verdict::Waits(hold),
            }
        }
    }
}

/// Whether `host`, which waits in `run`, starts now, or the first rule that holds it back.
fn start(view: &View<'_>, run: &Run, host: &RunHost) -> Result<(), Hold> {
    if view.fleet.is_quiet(view.now) {
        return Err(Hold::Quiet);
    }
    if view.unix_now < run.window_start_unix || run.window_ended(view.unix_now) {
        return Err(Hold::Window);
    }
    let group = host.host_group.as_str();
    if run.halted_groups().iter().any(|halted| halted == group) {
        return Err(Hold::Halted);
    }
    if !backed_off(view, host) {
        return Err(Hold::Backoff);
    }
    eligible(view, host)?;
    let policy = view.policy;
    let batch = run.batch.unwrap_or(policy.batch) as usize;
    let taken = places_of(&view.state.maintenance_runs, group, view.bake_beats);
    let free = batch.saturating_sub(taken);
    if free == 0 {
        return Err(Hold::Batch);
    }
    // The hosts that wait go first, by node id; then the deferred ones, the fewest offers first.
    let mut next: Vec<&RunHost> = run
        .hosts
        .iter()
        .filter(|other| offered(other) && other.host_group == group)
        .filter(|other| backed_off(view, other) && eligible(view, other).is_ok())
        .collect();
    next.sort_by_key(|other| {
        (
            other.step == Step::Deferred,
            other.offered_again,
            other.host_id.as_str(),
        )
    });
    if !next
        .iter()
        .take(free)
        .any(|other| other.host_id == host.host_id)
    {
        return Err(Hold::NotNext);
    }
    let mut needed = policy.min_serving as usize + 1;
    if run.take_last_host {
        needed = needed.min(of_group(view, group).count());
    }
    if serving(view, group) < needed {
        return Err(Hold::Serving);
    }
    Ok(())
}

/// Whether `host`, when it deferred, has waited its group's `defer_backoff` since: the n-th
/// deferral of its turn waits `initial_secs * factor^(n-1)`, capped at `max_secs`
/// ([`DeferBackoff::wait`]). A host that waits has nothing to wait for.
fn backed_off(view: &View<'_>, host: &RunHost) -> bool {
    if host.step != Step::Deferred {
        return true;
    }
    let wait = view
        .defer_backoff
        .wait(host.offered_again.saturating_add(1), 0);
    view.unix_now >= host.since_unix.saturating_add(wait.as_secs())
}

/// Whether the waiting `host` may be started at all now, by what it is and does, whatever its
/// group's other hosts do: the first rule of section 5.1 it fails, if any.
fn eligible(view: &View<'_>, host: &RunHost) -> Result<(), Hold> {
    let fleet = view.fleet;
    let heard = host.host_id.parse::<HostId>().ok().and_then(|id| {
        let node = fleet.node(&id)?;
        Some((id, node))
    });
    let Some((id, node)) = heard.filter(|(_, node)| node.is_live(view.now)) else {
        return Err(Hold::NotLive);
    };
    if !node.features.iter().any(|feature| feature == FEATURE) {
        return Err(Hold::NoFeature);
    }
    let group = view.group_of(&host.host_id);
    let role = if group.is_empty() {
        node.role.as_str()
    } else {
        group
    };
    match package_skip(view.state, &host.host_id, role) {
        Some(Skip::Held) => return Err(Hold::Held),
        Some(_) => return Err(Hold::Excluded),
        None => {}
    }
    if group != host.host_group {
        return Err(Hold::GroupChanged);
    }
    // No service is named the empty string, so every service of the host counts.
    if fleet.is_busy_besides("", &id, view.now) {
        return Err(Hold::InPackageUpdate);
    }
    Ok(())
}

/// Why the host `id`, whose holds of a role are read for `role`, is left out of a maintenance run
/// by the packages of `state`: a hold that covers it, then an exclusion from any package.
pub(crate) fn package_skip(state: &OpsState, id: &str, role: &str) -> Option<Skip> {
    if state.holds.iter().any(|hold| hold.scope.covers(id, role)) {
        Some(Skip::Held)
    } else if state
        .excluded
        .iter()
        .any(|exclusion| exclusion.node_id == id)
    {
        Some(Skip::Excluded)
    } else {
        None
    }
}

/// How many batch places of `group` are held, over every run of `runs`: one by each host in its
/// turn, or done and still baking for the group's `bake_beats`, however many runs hold it so.
fn places_of(runs: &[Run], group: &str, bake_beats: u32) -> usize {
    let mut holders: Vec<&str> = runs
        .iter()
        .flat_map(|run| &run.hosts)
        .filter(|host| host.host_group == group)
        .filter(|host| host.step.in_turn() || host.bakes(bake_beats))
        .map(|host| host.host_id.as_str())
        .collect();
    holders.sort_unstable();
    holders.dedup();
    holders.len()
}

/// The hosts of `group` the coordinator has heard, live or not.
fn of_group<'a>(view: &View<'a>, group: &'a str) -> impl Iterator<Item = (&'a HostId, &'a Node)> {
    let groups = view.groups;
    view.fleet
        .nodes()
        .filter(move |(id, _)| groups.get(id.as_str()).map_or("", String::as_str) == group)
}

/// How many hosts of `group` are serving: heard within the liveness bound, no service of theirs
/// reporting unhealthy, and in no turn of any run.
fn serving(view: &View<'_>, group: &str) -> usize {
    let runs = &view.state.maintenance_runs;
    of_group(view, group)
        .filter(|(_, node)| node.is_live(view.now))
        .filter(|(_, node)| node.announced.values().all(|h| *h != Health::Unhealthy))
        .filter(|(id, _)| {
            !runs
                .iter()
                .flat_map(|run| &run.hosts)
                .any(|host| host.host_id == id.as_str() && host.step.in_turn())
        })
        .count()
}

/// The `maintain` of `host`'s turn in `run` under the order id `order_id`: its kinds, the Linux
/// scope, `macos_major` and `return_secs` of its group's table ([`View::policy`]) for the host's
/// platform, and the run's drain deadline, else the group's.
fn ordered(view: &View<'_>, run: &Run, host: &RunHost, order_id: String) -> Ordered {
    let platform = host
        .host_id
        .parse::<HostId>()
        .ok()
        .and_then(|id| view.fleet.node(&id).map(|node| node.platform))
        .unwrap_or(hostkeeper_wire::Platform::LinuxX86_64);
    let effective = effective_of(view.policy, platform);
    let drain_deadline_secs = match run.drain_deadline_secs {
        Some(0) => None,
        Some(secs) => Some(secs),
        None => effective.drain_deadline_secs,
    };
    let kinds = host.kinds.iter().map(|kind| kind.proto()).collect();
    Ordered {
        run_id: run.id.clone(),
        host_group: host.host_group.clone(),
        order_id,
        maintain: Maintain {
            run_id: run.id.clone(),
            kinds,
            linux_scope: match effective.linux_scope {
                LinuxScope::Security => agent::LinuxScope::Security,
                LinuxScope::All => agent::LinuxScope::All,
            },
            drain_deadline_secs,
            macos_path: agent::MacosPath::Install,
            macos_major: effective.macos_major,
            return_secs: effective.return_secs,
        },
    }
}

/// The step a daemon's `step` stands for in a host's turn: its waiting (ordered, not started) is
/// staging, which holds the batch place the order took; none for one it did not name.
fn step_of(step: i32) -> Option<Step> {
    use agent::MaintenanceStep as Said;
    Some(match Said::try_from(step).ok()? {
        Said::Unspecified => return None,
        Said::Waiting | Said::Staging => Step::Staging,
        Said::Draining => Step::Draining,
        Said::Applying => Step::Applying,
        Said::Rebooting => Step::Rebooting,
        Said::Checking => Step::Checking,
        Said::PuttingBack => Step::PuttingBack,
        Said::Done => Step::Done,
        Said::Deferred => Step::Deferred,
        Said::Halted => Step::Halted,
    })
}

/// The kind a daemon's `kind` names, if it names one.
fn kind_of(kind: i32) -> Option<Kind> {
    Kind::ALL
        .iter()
        .copied()
        .find(|known| i32::from(known.proto()) == kind)
}

/// The host's turn as its daemon's report says it now, when that differs from what `runs` hold:
/// the index of the run and of the host, and the host's turn with the step, the kinds applied and
/// the reason the report gives (the step's time `unix_now` when the step changed). None when the
/// host reports no run, a run of none of `runs`, a run taken by another order than the one of the
/// host's turn ([`RunHost::order_id`]: a deferred host offered again is not read back to deferred
/// by the record of its earlier offer), a step it does not name, or what is held already; and none
/// for a turn that ended without the host (skipped, not reached) or is done.
pub(crate) fn folded(
    runs: &[Run],
    node: Option<&Node>,
    id: &HostId,
    unix_now: u64,
) -> Option<(usize, usize, RunHost)> {
    let said = reported_run(node)?;
    let r = runs.iter().position(|run| run.id == said.run_id)?;
    let h = runs[r]
        .hosts
        .iter()
        .position(|host| host.host_id == id.as_str())?;
    let held = &runs[r].hosts[h];
    if said.order_id != held.order_id(&said.run_id)
        || matches!(held.step, Step::Skipped | Step::NotReached | Step::Done)
    {
        return None;
    }
    let step = step_of(said.step)?;
    let applied: Vec<Kind> = said.applied.iter().filter_map(|k| kind_of(*k)).collect();
    let mut turn = held.clone();
    if turn.step != step {
        turn.step = step;
        turn.since_unix = unix_now;
    }
    turn.applied = applied;
    turn.reason.clone_from(&said.halted_reason);
    (turn != *held).then_some((r, h, turn))
}

/// The bake of the host `id` after its heartbeat, kept as `node`: for each turn of `runs` that
/// is done and still bakes for its group's `bake_beats` (`bake_beats_of` the turn's group), the
/// index of the run and of the host and the healthy heartbeats in a row it has sent since it was
/// done: one more for a heartbeat in which every service it lists is healthy, none after any
/// other (the count starts again); only the turns whose count changes. Nothing while the host is
/// in a turn of any run: its heartbeats then prove that turn, not the one it is done with.
pub(crate) fn baked(
    runs: &[Run],
    node: Option<&Node>,
    id: &HostId,
    bake_beats_of: impl Fn(&str) -> u32,
) -> Vec<(usize, usize, u32)> {
    let of_host = || {
        runs.iter().enumerate().flat_map(|(r, run)| {
            run.hosts
                .iter()
                .enumerate()
                .filter(|(_, host)| host.host_id == id.as_str())
                .map(move |(h, host)| (r, h, host))
        })
    };
    if of_host().any(|(_, _, host)| host.step.in_turn()) {
        return Vec::new();
    }
    let healthy = node.is_some_and(|node| node.announced.values().all(|h| *h == Health::Healthy));
    of_host()
        .filter(|(_, _, host)| host.bakes(bake_beats_of(&host.host_group)))
        .filter_map(|(r, h, host)| {
            let held = host.baked_beats;
            let beats = if healthy { held.saturating_add(1) } else { 0 };
            (beats != held).then_some((r, h, beats))
        })
        .collect()
}

#[cfg(test)]
#[path = "decide_maintenance_tests.rs"]
pub(crate) mod tests;
