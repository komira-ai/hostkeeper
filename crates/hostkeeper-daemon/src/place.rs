//! The switch of a hooked order (design section 5.2, steps 5 and 6): the step runner calls it
//! immediately before every run of `cutover` and of `rollback`, and after a `cleanup` that
//! succeeded ([`hostkeeper_steps::StepRunner::with_switch`]). It acts for every hooked order: an
//! `install`, which has no release to replace, and an `update`, a `force_rollback` and a `force`
//! that has a hook, which have one. Every version it switches was staged by a repository
//! installer: its version directory holds `target.json` ([`crate::target`]), and the switch is
//! the installer's ([`repository_switch`]).
//!
//! - [`Toward::Target`], before `cutover`: the `activate` of the target ([`Repository::activate`]):
//!   the package installed, its `run` units enabled and `current` switched to the target
//!   (`previous` becomes the release it replaces).
//! - [`Toward::Back`], before `rollback`: for an update, the release it replaced is activated
//!   again from its sealed directory ([`Repository::back`]); for a first install, the package is
//!   removed and `current` with it, when it names the target; the version directory stays, as a
//!   staged version a retry uses.
//! - [`Toward::Confirmed`], after `cleanup` and before the verdict is written: nothing, as
//!   nothing was placed.
//!
//! Each is idempotent, so a resumed step switches again, and so a switch that failed is tried
//! again ([`retried`]): up to [`SWITCH_ATTEMPTS`] times, [`SWITCH_RETRY_SECS`] apart on the
//! daemon's clock, each failure but the last logged as `switch_retried` with what failed. A
//! service manager can refuse a call for a moment (launchd, asked to boot out or print a job it
//! is spawning that instant), and the placed lock is taken without waiting; one such refusal must
//! not end the update without its step. Every failure is tried again, one that cannot pass on a
//! second try (an unreadable `target.json`) too: the tries cost seconds, and telling the two
//! apart would need every error typed. The last failure is the switch's error, which the runner
//! stops on.
//!
//! `current` and `previous` are written by [`Placer::switch_current`], not by the package
//! layout's own switch, which reads a seal and writes through `std::fs`: this path goes through
//! the [`Fs`] port the daemon's links do.

use std::sync::Arc;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_placed::{Fs, Placer, Roots, UnitControl};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_steps::{Clock, Switch, SwitchError, Toward, UpdateOrder};

use crate::boot::{WARN_SWITCH_RETRIED, warn_line};
use crate::own_package;
use crate::repository::Repository;
use crate::target;
use crate::verbs::force::SELF_SERVICE;

/// How many times a switch is tried before its error stops the update.
pub(crate) const SWITCH_ATTEMPTS: u32 = 5;
/// The wait between two tries of a switch, in seconds of the daemon's clock.
pub(crate) const SWITCH_RETRY_SECS: u64 = 2;

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// The switch of a hooked order on the host of `roots`, through `fs` and `units`. The target's
/// version directory holds `target.json`, which `repository` switches by ([`repository_switch`]);
/// one that holds none is no version this daemon can switch. One try: the daemon runs it
/// [`retried`].
pub fn hooked_switch(
    roots: Roots,
    fs: Arc<dyn Fs + Send + Sync>,
    units: Arc<dyn UnitControl + Send + Sync>,
    repository: Repository,
) -> Switch {
    Arc::new(move |toward: Toward, order: &UpdateOrder| {
        let staged = target::read(order.staged.dir()).map_err(|e| SwitchError::Failed(text(e)))?;
        if order.service == SELF_SERVICE {
            let placer = Placer::new(roots.clone(), &*fs, &*units);
            let record = placer
                .store()
                .load()
                .map_err(|e| SwitchError::Failed(text(e)))?;
            if own_package::deferred(staged.as_ref(), &record) {
                // Its new binary switches, once it has started ([`crate::own_package`]).
                return Ok(());
            }
        }
        let Some(entry) = staged else {
            return Err(SwitchError::Failed(format!(
                "{} holds no {}: it was not staged by an installer of this daemon",
                order.staged.dir().display(),
                target::TARGET_FILE
            )));
        };
        repository_switch(&repository, toward, order, &entry)
    })
}

/// `switch` tried up to [`SWITCH_ATTEMPTS`] times: a failed try but the last is logged through
/// `log` (one line, `alert=switch_retried`, with what failed) and tried again after
/// [`SWITCH_RETRY_SECS`] of `clock`; the last try's result is the switch's.
pub fn retried(
    switch: Switch,
    clock: Arc<dyn Clock>,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) -> Switch {
    Arc::new(move |toward: Toward, order: &UpdateOrder| {
        let mut attempt = 1;
        loop {
            match switch(toward, order) {
                Err(SwitchError::Failed(why)) if attempt < SWITCH_ATTEMPTS => {
                    log(&warn_line(
                        WARN_SWITCH_RETRIED,
                        &format!(
                            "service={}: the switch {toward:?} failed (try {attempt} of \
                             {SWITCH_ATTEMPTS}), tried again in {SWITCH_RETRY_SECS}s: {why}",
                            order.service
                        ),
                    ));
                    clock.sleep_secs(SWITCH_RETRY_SECS);
                    attempt += 1;
                }
                // Success, the last failure, or an installer's verdict (`Ended`): not retried.
                ended => return ended,
            }
        }
    })
}

/// The switch of an order whose target the repository installer staged for `entry`: before
/// `cutover` the installer's `activate` of the target; before `rollback` the previous version's
/// `activate` from its own sealed `target.json`, or for a first install the package's removal;
/// nothing to confirm, as nothing was placed. A previous version with no `target.json` is none
/// this daemon can activate again.
fn repository_switch(
    repository: &Repository,
    toward: Toward,
    order: &UpdateOrder,
    entry: &Target,
) -> Result<(), SwitchError> {
    match toward {
        Toward::Target => repository.activate(order, entry),
        Toward::Back => match (&order.previous, order.previous_version.as_deref()) {
            (Some(previous), Some(version)) => {
                let version = Version::new(version)
                    .map_err(text)
                    .map_err(SwitchError::Failed)?;
                let sealed = target::read(previous.dir())
                    .map_err(text)
                    .map_err(SwitchError::Failed)?
                    .ok_or_else(|| {
                        SwitchError::Failed(format!(
                            "{} holds no {}: it cannot be activated again",
                            previous.dir().display(),
                            target::TARGET_FILE
                        ))
                    })?;
                repository.back(order, entry, Some((&version, previous.dir(), &sealed)))
            }
            (None, None) => repository.back(order, entry, None),
            _ => Err(SwitchError::Failed(half(order))),
        },
        Toward::Confirmed => Ok(()),
    }
}

/// Why an order that names only half of the release it replaces cannot be switched back.
fn half(order: &UpdateOrder) -> String {
    format!(
        "the order for {} names only half of the release it replaces",
        order.service
    )
}

/// The service and the target of `order`, in their grammars.
pub(crate) fn names(order: &UpdateOrder) -> Result<(ServiceName, Version), String> {
    let service = ServiceName::new(&order.service).map_err(text)?;
    let version = Version::new(&order.target_version).map_err(text)?;
    Ok((service, version))
}
