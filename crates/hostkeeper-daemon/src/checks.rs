//! The daemon's own checks of an order (design section 5.1), each a function that answers with
//! a closed [`Refusal`] token.
//!
//! The grammars are those of `hostkeeper_config::names` (design section 11.8); the package
//! digest is the form the protobuf files fix. The manifest's `name`, `version` and
//! `min_hostkeeper` are checked by the manifest's loader, `hostkeeper_config::manifest`, which
//! [`crate::verbs::update`] and [`crate::verbs::force`] call with the order's service and
//! version. Which paths a package places and whether they conflict are checked by the placed-path
//! linker, `hostkeeper_placed::check`, which `force` calls before it changes anything.
//!
//! [`Checks`] says which of these an order is held to: every one for a hooked order, every one
//! but `busy` and the standing halt for a `force` ([`Checks::for_force`]).

use std::cmp::Ordering;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::Verdict;
use hostkeeper_steps::Finished;
use hostkeeper_steps::record::{Record, State};

use crate::packages::PackageError;
use crate::refusal::Refusal;
use crate::verb::{HookedVerb, PackageDigest};

/// A service name: [`hostkeeper_config::names::is_service_name`].
pub fn service_name(text: &str) -> Result<ServiceName, Refusal> {
    ServiceName::new(text).map_err(|_| Refusal::InvalidOrder)
}

/// A version: [`hostkeeper_config::names::is_version`].
pub fn version(text: &str) -> Result<Version, Refusal> {
    Version::new(text).map_err(|_| Refusal::InvalidOrder)
}

/// A package digest: 64 lower-case hex digits.
pub fn digest(text: &str) -> Result<PackageDigest, Refusal> {
    PackageDigest::new(text).ok_or(Refusal::InvalidOrder)
}

/// The order of versions the daemon compares a target with the running version by: the
/// `.`-separated parts in turn, two parts of digits only as whole numbers (`10` above `9`), any
/// other two parts as bytes (`0-rc1` above `0`); when every part is equal, the version with
/// fewer parts is the lower (`1.0` below `1.0.0`).
///
/// The coordinator's own order (its rollout file's `version_scheme`) is not in this repository
/// yet; until it is, this is the order, and the two must agree before the coordinator sends
/// `update` to this daemon.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let mut left = a.split('.');
    let mut right = b.split('.');
    loop {
        let order = match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(x), Some(y)) => compare_parts(x, y),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
}

fn compare_parts(x: &str, y: &str) -> Ordering {
    let digits = |part: &str| !part.is_empty() & part.bytes().all(|b| b.is_ascii_digit());
    match digits(x) & digits(y) {
        true => {
            let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
            x.len().cmp(&y.len()).then_with(|| x.cmp(y))
        }
        false => x.cmp(y),
    }
}

/// One check of design section 5.1 the daemon makes of an order that names a package. Refusing a
/// version below the running one is not among them: it is the rule of the verb `update` alone
/// ([`not_below_running`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Check {
    /// The service name and the version keep their grammars.
    Grammar,
    /// The package digest is 64 lower-case hex digits.
    Digest,
    /// The manifest's `name` is the order's service.
    ManifestName,
    /// The manifest's `version` is the order's version.
    ManifestVersion,
    /// The manifest's `min_hostkeeper` is not above this daemon.
    MinHostkeeper,
    /// The package's shape: its archive bounds and entries, and its manifest's rules.
    Shape,
    /// No path the package places is another service's, or occupied by what `placed.json` does
    /// not list (`path_conflict`).
    PathClaim,
    /// No unfinished order of another package of the service (`busy`).
    Busy,
    /// No standing halt of the service (`halted`).
    Halt,
}

impl Check {
    /// Every check.
    pub const ALL: [Check; 9] = [
        Check::Grammar,
        Check::Digest,
        Check::ManifestName,
        Check::ManifestVersion,
        Check::MinHostkeeper,
        Check::Shape,
        Check::PathClaim,
        Check::Busy,
        Check::Halt,
    ];
}

/// Which checks an order is held to: every one but those it skips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checks {
    skipped: &'static [Check],
}

impl Checks {
    /// A hooked order (`update`, `force_rollback`): every check.
    pub const fn hooked() -> Checks {
        Checks { skipped: &[] }
    }

    /// A `force` (design section 5.1): every check but `busy`, since a force stops the running
    /// step of its service instead, and the standing halt, which its halt generation lifts.
    pub const fn for_force() -> Checks {
        Checks {
            skipped: &[Check::Busy, Check::Halt],
        }
    }

    /// The checks skipped.
    pub const fn skipped(&self) -> &'static [Check] {
        self.skipped
    }

    /// Whether `check` is skipped.
    pub fn skips(&self, check: Check) -> bool {
        self.skipped.contains(&check)
    }
}

/// An `update` is refused below the running version (`below_running`); a `force_rollback` and a
/// `force` are not. A service that runs no version has nothing to be below.
pub fn not_below_running(
    verb: HookedVerb,
    target: &Version,
    running: Option<&Version>,
) -> Result<(), Refusal> {
    let below = running.is_some_and(|running| {
        compare_versions(target.as_str(), running.as_str()) == Ordering::Less
    });
    match (verb, below) {
        (HookedVerb::Update, true) => Err(Refusal::BelowRunning),
        _ => Ok(()),
    }
}

/// The token of a package error.
pub fn package_refusal(error: &PackageError) -> Refusal {
    match error {
        PackageError::NotFound(_) => Refusal::PackageNotFound,
        PackageError::DigestMismatch(_) => Refusal::DigestMismatch,
        PackageError::NoSpace(_) => Refusal::NoSpace,
        PackageError::Other(_) => Refusal::PackageFailed,
    }
}

/// What a hooked order is, for the try rules: the verb, the package and the try.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderKey {
    pub verb: HookedVerb,
    pub version: Version,
    pub digest: PackageDigest,
    pub order_try: u64,
}

/// What the try rules say of an order, given the service's step record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TryRule {
    /// Run it: a new order, or the unfinished one the record holds, which the runner resumes.
    Run,
    /// A resend of an order that ended: answer with the record's verdict and run nothing.
    Answer(Finished),
    /// Refuse it.
    Refuse(Refusal),
}

/// How the order of `rec` ended, `None` while it has not.
pub fn finished_of(rec: &Record) -> Option<Finished> {
    match &rec.state {
        State::Finished { verdict, reason } => Some(Finished {
            verdict: verdict.clone(),
            reason: reason.clone(),
        }),
        _ => None,
    }
}

/// Whether `rec` is the step record of an order for the package `key` names, of the same kind:
/// the record of a force answers only a `force`, and a hooked record only a hooked order.
fn same_package(rec: &Record, key: &OrderKey) -> bool {
    let same_kind = rec.force.is_some() == (key.verb == HookedVerb::Force);
    (rec.target_version == key.version.as_str())
        & (rec.tarball_digest == key.digest.as_str())
        & same_kind
}

/// The try rules of design section 8.3, in this order, for the service whose step record is
/// `last` (`None` when it has none):
///
/// 1. the same try and the same package as the record: a resend, answered from the record when
///    the order ended, and resumed when it did not;
/// 2. a `Halt` verdict holds every other order, of any version, until `clear_halt`;
/// 3. the same try with another version or digest: `try_conflict`;
/// 4. a lower try: `stale_try`;
/// 5. a higher try is a new order once the last has ended, and `busy` while an unfinished order
///    of another package is in the record (a post-success watcher is not in the way: the runner
///    stops it for a new order). An unfinished order of the same package is resumed.
///
/// A record written without a try (before the daemon kept one) counts as try 0.
pub fn try_rule(last: Option<&Record>, key: &OrderKey) -> TryRule {
    try_rule_with(last, key, Checks::hooked())
}

/// [`try_rule`] for an order held to `checks`. A `force` ([`Checks::for_force`]) skips rule 2,
/// and rule 5 for an unfinished hooked update, which it stops instead; an unfinished force of
/// another package is `busy` for it all the same.
pub fn try_rule_with(last: Option<&Record>, key: &OrderKey, checks: Checks) -> TryRule {
    let Some(rec) = last else {
        return TryRule::Run;
    };
    let finished = finished_of(rec);
    let halted = matches!(
        &rec.state,
        State::Finished {
            verdict: Verdict::Halt,
            ..
        }
    );
    let same = same_package(rec, key);
    let held = rec.order_try.unwrap_or(0);
    let same_try = key.order_try == held;
    if same_try & same {
        return finished.map_or(TryRule::Run, TryRule::Answer);
    }
    if halted & !checks.skips(Check::Halt) {
        return TryRule::Refuse(Refusal::Halted);
    }
    if same_try {
        return TryRule::Refuse(Refusal::TryConflict);
    }
    if key.order_try < held {
        return TryRule::Refuse(Refusal::StaleTry);
    }
    let in_the_way = finished.is_none() & rec.progress.watching.is_none() & !same;
    let stopped_instead = checks.skips(Check::Busy) & rec.force.is_none();
    match in_the_way & !stopped_instead {
        true => TryRule::Refuse(Refusal::Busy),
        false => TryRule::Run,
    }
}
