//! The maintenance runs the coordinator stores (docs/design/maintenance-updates.md, sections 4.2,
//! 5.1 and 5.7): what an operator scheduled, and each host's turn of it.
//!
//! A run is kept in the ops state ([`crate::ops::OpsState::maintenance_runs`]): over the files a
//! top-level key of the ops file, written only when a run is held; over a database the rows of
//! `maintenance_runs` and `maintenance_hosts` (migration `0029`), which no release before reads.
//! A coordinator that starts again reads its runs from there.
//!
//! What a run holds is what `Maintenance.Schedule` decided: its kinds, where (hosts, or a host
//! group), its window, its batch and drain deadline (absent: each group's), whether it may take a
//! group's last host, its reason, who scheduled it and when, and its hosts, each with its group,
//! its step and since when, the kinds its turn applies and, for a host left out, why. The one
//! ending it stores is a cancel ([`Run::cancelled_unix`]); where it stands otherwise
//! ([`Run::state_at`]) is read from its hosts' steps and the clock, so a window that ends needs
//! no write. A host's turn is started, and its step then follows what its daemon reports, by
//! [`crate::decide_maintenance`].

use hostkeeper_proto::hostkeeper::agent::v1::MaintenanceKind;
use hostkeeper_proto::hostkeeper::operator::v1::{
    MaintenanceHostStep, MaintenanceSkip, MaintenanceState,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// How many runs are kept: past it, the oldest runs that have ended are dropped when a run is
/// scheduled (their audit entries stay).
pub(crate) const MAX_RUNS: usize = 256;

/// Each name of an enum of the runs, as the ops file and the database's rows write it, with the
/// proto value it stands for.
macro_rules! named {
    (
        $(#[$doc:meta])* $ty:ident: $proto:ident {
            $($(#[$vdoc:meta])* $variant:ident = $name:literal => $value:ident,)*
        }
    ) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub(crate) enum $ty {
            $($(#[$vdoc])* $variant,)*
        }

        impl $ty {
            /// Every value, in order.
            pub(crate) const ALL: &[$ty] = &[$($ty::$variant,)*];

            /// The name the ops file and the rows write.
            pub(crate) fn as_str(self) -> &'static str {
                match self {
                    $($ty::$variant => $name,)*
                }
            }

            /// The value named `name`, if it is one.
            pub(crate) fn parse(name: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|value| value.as_str() == name)
            }

            /// The proto value it stands for.
            pub(crate) fn proto(self) -> $proto {
                match self {
                    $($ty::$variant => $proto::$value,)*
                }
            }
        }

        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let name = String::deserialize(deserializer)?;
                Self::parse(&name).ok_or_else(|| {
                    serde::de::Error::custom(format!(
                        "{name:?} is not a {}",
                        stringify!($ty)
                    ))
                })
            }
        }
    };
}

named! {
    /// A kind of maintenance a run applies.
    Kind: MaintenanceKind {
        /// A macOS update.
        Macos = "macos" => Macos,
        /// An Xcode install.
        Xcode = "xcode" => Xcode,
        /// Linux security or kernel updates.
        Linux = "linux" => Linux,
        /// A reboot the host needs.
        Reboot = "reboot" => Reboot,
    }
}

named! {
    /// Where a host's turn of a run stands.
    Step: MaintenanceHostStep {
        /// In the run, not started.
        Waiting = "waiting" => Waiting,
        /// Staging the update while it serves.
        Staging = "staging" => Staging,
        /// Draining.
        Draining = "draining" => Draining,
        /// Applying the update.
        Applying = "applying" => Applying,
        /// Rebooting.
        Rebooting = "rebooting" => Rebooting,
        /// Checking.
        Checking = "checking" => Checking,
        /// Putting the drained services back.
        PuttingBack = "putting_back" => PuttingBack,
        /// Done.
        Done = "done" => Done,
        /// Deferred: back in service with nothing changed, offered again while the window is open.
        Deferred = "deferred" => Deferred,
        /// Halted: cordoned until an operator acts.
        Halted = "halted" => Halted,
        /// Silent past its group's `return_secs` while rebooting.
        NotBack = "not_back" => NotBack,
        /// Not started before the run was cancelled or its window ended.
        NotReached = "not_reached" => NotReached,
        /// Left out of the run: never ordered.
        Skipped = "skipped" => Skipped,
    }
}

named! {
    /// Why a host is left out of a run.
    Skip: MaintenanceSkip {
        /// Its daemon does not announce `maintenance-v1`.
        NoMaintenanceV1 = "no_maintenance_v1" => NoMaintenanceV1,
        /// A hold stands on it for a package.
        Held = "held" => Held,
        /// It is excluded from a package.
        Excluded = "excluded" => Excluded,
    }
}

impl Step {
    /// Whether a host at this step is in its turn: started and not ended. Such a host finishes
    /// its turn whatever becomes of its run, and a halted or silent host stays so until an
    /// operator acts.
    pub(crate) fn in_turn(self) -> bool {
        matches!(
            self,
            Self::Staging
                | Self::Draining
                | Self::Applying
                | Self::Rebooting
                | Self::Checking
                | Self::PuttingBack
                | Self::Halted
                | Self::NotBack
        )
    }

    /// Whether a host at this step has not started and may still start: it waits, or was
    /// deferred and is offered again while the run's window is open.
    pub(crate) fn may_start(self) -> bool {
        matches!(self, Self::Waiting | Self::Deferred)
    }
}

/// One host's turn of a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunHost {
    /// The host, by node id.
    pub(crate) host_id: String,
    /// Its host group when the run took it; empty: none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) host_group: String,
    /// Where its turn stands.
    pub(crate) step: Step,
    /// When it entered its step.
    pub(crate) since_unix: u64,
    /// The kinds its turn applies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) kinds: Vec<Kind>,
    /// The kinds applied so far, in the order applied, as its daemon reports them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) applied: Vec<Kind>,
    /// Why it is left out, with [`Step::Skipped`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) skipped_because: Option<Skip>,
    /// Why it halted or deferred, as its daemon words it; empty otherwise.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) reason: String,
    /// How many times it was offered its turn again after a deferral: its order id
    /// ([`RunHost::order_id`]) is the run's first while 0, and a new one for each offer after.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub(crate) offered_again: u32,
    /// Done: the healthy heartbeats in a row it has sent since, toward its group's `bake_beats`;
    /// 0 otherwise.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub(crate) baked_beats: u32,
}

impl RunHost {
    /// The id of the order that took the turn it is in, or last was in, in the run `run_id`:
    /// `maintain-<run id>` for the first offer, then `maintain-<run id>-<n>` for the n-th
    /// ([`Self::offered_again`] + 1). A daemon answers an order it already holds with the end of
    /// that order's turn, whatever its try, so a turn offered again after a deferral takes a new
    /// id: at most 9 + 45 + 10 bytes for a run id [`new_id`] makes, within an order id's 64, since
    /// a host is offered again at most [`MAX_OFFERED_AGAIN`] times.
    pub(crate) fn order_id(&self, run_id: &str) -> String {
        order_id_of(run_id, self.offered_again)
    }

    /// The id of the order that starts its turn now: [`Self::order_id`] of the next offer when it
    /// was deferred, its own otherwise.
    pub(crate) fn next_order_id(&self, run_id: &str) -> String {
        let again = match self.step {
            Step::Deferred => self.offered_again.saturating_add(1),
            _ => self.offered_again,
        };
        order_id_of(run_id, again)
    }

    /// Whether, done in a group that bakes for `bake_beats` healthy heartbeats, it still bakes:
    /// it holds its batch place until it has sent them.
    pub(crate) fn bakes(&self, bake_beats: u32) -> bool {
        self.step == Step::Done && self.baked_beats < bake_beats
    }
}

/// The order id of the `maintain` of the run `run_id` offered `again` times after a deferral.
pub(crate) fn order_id_of(run_id: &str, again: u32) -> String {
    match again {
        0 => format!("{ORDER_PREFIX}{run_id}"),
        n => format!("{ORDER_PREFIX}{run_id}-{}", u64::from(n) + 1),
    }
}

/// What the order id of a run's `maintain` begins with: then the run's id.
pub(crate) const ORDER_PREFIX: &str = "maintain-";

/// The most times a host is offered its turn again after a deferral, so that its order id's
/// `-<n>` stays within 10 bytes.
pub(crate) const MAX_OFFERED_AGAIN: u32 = 999_999_998;

/// A maintenance run, as the coordinator stores it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Run {
    /// Its id: the `run_id` of each host's `maintain` order.
    pub(crate) id: String,
    /// The kinds it applies, as scheduled; empty: every kind due on each host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) kinds: Vec<Kind>,
    /// The hosts it was scheduled on, as named; empty when it names a host group.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) host_ids: Vec<String>,
    /// The host group it was scheduled on; empty when it names hosts.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) host_group: String,
    /// When its window opens; 0: when it was scheduled.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) window_start_unix: u64,
    /// When its window ends; 0: no end.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) window_end_unix: u64,
    /// How many hosts of each group at once; none: each group's `batch`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) batch: Option<u32>,
    /// How long a drain may wait; 0: no deadline; none: each group's `drain_deadline_secs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) drain_deadline_secs: Option<u64>,
    /// Whether it may take every host of a group.
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) take_last_host: bool,
    /// Why it was scheduled.
    pub(crate) reason: String,
    /// Who scheduled it, as the audit entry names them.
    pub(crate) by: String,
    /// When it was scheduled.
    pub(crate) scheduled_unix: u64,
    /// When an operator cancelled it; none while no one has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cancelled_unix: Option<u64>,
    /// Its hosts, by node id.
    #[serde(default)]
    pub(crate) hosts: Vec<RunHost>,
}

/// Whether `n` is 0, for a key written only when it is not.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes the field by reference"
)]
fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// Whether `n` is 0, for a key written only when it is not.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes the field by reference"
)]
fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

/// Whether `b` is false, for a key written only when it is true.
#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes the field by reference"
)]
fn is_false(b: &bool) -> bool {
    !*b
}

impl Run {
    /// Whether its window has ended at `now`.
    pub(crate) fn window_ended(&self, now: u64) -> bool {
        self.window_end_unix != 0 && now >= self.window_end_unix
    }

    /// Where the run stands at `now`, and when it ended (0 while it has not): cancelled, when
    /// an operator cancelled it; done, when every host's turn has ended (at the latest step's
    /// time); its window ended, when it has and no host is in its turn; halted, when a host of it
    /// halted or is not back; scheduled, before its window opens; running otherwise.
    pub(crate) fn state_at(&self, now: u64) -> (MaintenanceState, u64) {
        if let Some(at) = self.cancelled_unix {
            return (MaintenanceState::Cancelled, at);
        }
        let open = |host: &&RunHost| host.step.in_turn() || host.step.may_start();
        if !self.hosts.iter().any(|host| open(&host)) {
            let last = self.hosts.iter().map(|host| host.since_unix).max();
            return (MaintenanceState::Done, last.unwrap_or(self.scheduled_unix));
        }
        let in_turn = self.hosts.iter().any(|host| host.step.in_turn());
        if self.window_ended(now) && !in_turn {
            return (MaintenanceState::WindowEnded, self.window_end_unix);
        }
        if !self.halted_groups().is_empty() {
            return (MaintenanceState::Halted, 0);
        }
        if now < self.window_start_unix {
            return (MaintenanceState::Scheduled, 0);
        }
        (MaintenanceState::Running, 0)
    }

    /// Whether the run has ended at `now`: cancelled, done, or past its window with no host in
    /// its turn.
    pub(crate) fn ended_at(&self, now: u64) -> bool {
        matches!(
            self.state_at(now).0,
            MaintenanceState::Cancelled | MaintenanceState::Done | MaintenanceState::WindowEnded
        )
    }

    /// The groups in which a host halted or is not back, by name, each once, in order.
    pub(crate) fn halted_groups(&self) -> Vec<String> {
        let mut groups: Vec<String> = self
            .hosts
            .iter()
            .filter(|host| matches!(host.step, Step::Halted | Step::NotBack))
            .map(|host| host.host_group.clone())
            .collect();
        groups.sort();
        groups.dedup();
        groups
    }

    /// Whether the host `host_id` has a turn of this run that has not ended at `now`: one it is
    /// in, or one it waits for while the run goes on.
    pub(crate) fn holds_turn(&self, host_id: &str, now: u64) -> bool {
        self.hosts.iter().any(|host| {
            host.host_id == host_id
                && (host.step.in_turn() || (host.step.may_start() && !self.ended_at(now)))
        })
    }

    /// `host`'s turn as it stands at `now`: a host that had not started (waiting, or deferred and
    /// not offered again) once the run's window has ended is not reached, since the window
    /// ended, as a cancel would end it; any other as held. A window that ends needs no write.
    pub(crate) fn shown(&self, host: &RunHost, now: u64) -> RunHost {
        let mut shown = host.clone();
        if host.step.may_start() && self.window_ended(now) {
            shown.step = Step::NotReached;
            shown.since_unix = self.window_end_unix;
        }
        shown
    }

    /// Cancels the run at `now`: no host of it starts from now on, so each host that had not
    /// started ends as not reached; a host in its turn is left as it is.
    pub(crate) fn cancel(&mut self, now: u64) {
        self.cancelled_unix = Some(now);
        for host in &mut self.hosts {
            if host.step.may_start() {
                host.step = Step::NotReached;
                host.since_unix = now;
            }
        }
    }
}

/// Keeps at most [`MAX_RUNS`] of `runs`: the oldest that have ended at `now` and hold no host in
/// its turn are dropped first; a run that has not ended, or that holds a host still in its turn (a
/// cancelled run's halted host), is never dropped.
pub(crate) fn prune(runs: &mut Vec<Run>, now: u64) {
    let mut over = runs.len().saturating_sub(MAX_RUNS);
    runs.retain(|run| {
        let in_turn = run.hosts.iter().any(|host| host.step.in_turn());
        if over > 0 && run.ended_at(now) && !in_turn {
            over -= 1;
            return false;
        }
        true
    });
}

/// The id of a run scheduled at `unix`, given the runs held: `run-<unix>-<n>`, `n` the smallest
/// from 1 that no run holds: an order id's characters, well within its 64 bytes.
pub(crate) fn new_id(runs: &[Run], unix: u64) -> String {
    let mut n: u64 = 1;
    loop {
        let id = format!("run-{unix}-{n}");
        if !runs.iter().any(|run| run.id == id) {
            return id;
        }
        n += 1;
    }
}

#[cfg(test)]
#[path = "maintenance_runs_tests.rs"]
pub(crate) mod tests;
