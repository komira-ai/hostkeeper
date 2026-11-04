//! The daemon's own checks of an order (design section 5.1), each a function that answers with
//! a closed [`Refusal`] token.
//!
//! The grammars are those of `hostkeeper_config::names` (design section 11.8); the package
//! digest is the form the protobuf files fix. The manifest's `name`, `version` and
//! `min_hostkeeper` are checked by the manifest's loader, `hostkeeper_config::manifest`, which
//! [`crate::verbs::update`] calls with the order's service and version. Which paths a package
//! places and whether they conflict are not checked here.

use std::cmp::Ordering;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::Verdict;
use hostkeeper_steps::Finished;
use hostkeeper_steps::record::{Record, State};

use crate::packages::PackageError;
use crate::refusal::Refusal;
use crate::verb::{HookedVerb, PackageDigest};

/// A service name: 1 to 64 bytes of `A-Za-z0-9_-`.
pub fn service_name(text: &str) -> Result<ServiceName, Refusal> {
    ServiceName::new(text).map_err(|_| Refusal::InvalidOrder)
}

/// A version: 1 to 128 bytes of `A-Za-z0-9._+-`, not beginning with `.`.
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

/// An `update` is refused below the running version (`below_running`); a `force_rollback` is
/// not. A service that runs no version has nothing to be below.
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

/// Whether `rec` is the step record of an order for the package `key` names.
fn same_package(rec: &Record, key: &OrderKey) -> bool {
    (rec.target_version == key.version.as_str()) & (rec.tarball_digest == key.digest.as_str())
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
    if halted {
        return TryRule::Refuse(Refusal::Halted);
    }
    if same_try {
        return TryRule::Refuse(Refusal::TryConflict);
    }
    if key.order_try < held {
        return TryRule::Refuse(Refusal::StaleTry);
    }
    let in_the_way = finished.is_none() & rec.progress.watching.is_none() & !same;
    match in_the_way {
        true => TryRule::Refuse(Refusal::Busy),
        false => TryRule::Run,
    }
}
