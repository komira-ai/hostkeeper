//! The switch of a hooked order (design sections 4.7, "Placed paths", and 5.2, steps 5 and 6):
//! the step runner calls it immediately before every run of `cutover` and of `rollback`, and
//! after a `cleanup` that succeeded ([`hostkeeper_steps::StepRunner::with_switch`]). It acts for
//! every hooked order: an `install`, which has no release to replace, and an `update`, a
//! `force_rollback` and a `force` that has a hook, which have one.
//!
//! - [`Toward::Target`], before `cutover`: the target's manifest read from its version
//!   directory, [`Placer::desired`], [`Placer::begin`] (its paths `pending`), [`Placer::link`],
//!   [`Placer::switch_current`] to the target (`previous` becomes the release it replaces) and
//!   [`Placer::reload`]. A first install also commits its paths here ([`Placer::commit`]: there
//!   is nothing to undo but the install, which `rollback` takes off the host); an update leaves
//!   them `pending` until it is confirmed.
//! - [`Toward::Back`], before `rollback`: for an update, `current` is switched back to the
//!   release it replaced, and [`Placer::relink_to`] that release's manifest links what it
//!   places, removes what only the target placed and reloads. For a first install, the install
//!   is taken off the host: [`Placer::remove_all`] (the `run` units stopped and disabled, every
//!   placed link removed, `daemon-reload`) and then `current`, when it names the target; the
//!   version directory stays, as a staged version a retry uses.
//! - [`Toward::Confirmed`], after `cleanup` and before the verdict is written: the target's
//!   paths are committed (`placed`) and the paths of the service the target does not list are
//!   removed ([`Placer::unlink_unlisted`]).
//!
//! Each is idempotent, so a resumed step switches again. `current` and `previous` are written
//! by [`Placer::switch_current`], not by the package layout's own switch, which reads a seal and
//! writes through `std::fs`: this path goes through the [`Fs`] port the placed paths do.

use std::path::Path;
use std::sync::Arc;

use hostkeeper_config::manifest::{Expected, HostkeeperVersion, Manifest};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_placed::{Fs, Placer, Roots, UnitControl};
use hostkeeper_steps::{Switch, Toward, UpdateOrder};

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// The switch of a hooked order on the host of `roots`, through `fs` and `units`; a manifest is
/// read against the daemon's version `own`.
pub fn hooked_switch(
    roots: Roots,
    fs: Arc<dyn Fs + Send + Sync>,
    units: Arc<dyn UnitControl + Send + Sync>,
    own: HostkeeperVersion,
) -> Switch {
    Arc::new(move |toward: Toward, order: &UpdateOrder| {
        let placer = Placer::new(roots.clone(), &*fs, &*units);
        match toward {
            Toward::Target => forward(&placer, order, &own),
            Toward::Back => back(&placer, &*fs, order, &own),
            Toward::Confirmed => confirmed(&placer, order, &own),
        }
    })
}

/// The service and the target of `order`, in their grammars.
fn names(order: &UpdateOrder) -> Result<(ServiceName, Version), String> {
    let service = ServiceName::new(&order.service).map_err(text)?;
    let version = Version::new(&order.target_version).map_err(text)?;
    Ok((service, version))
}

/// The manifest of the version directory `dir` as the release `version` of `service`.
fn manifest_of(
    dir: &Path,
    service: &ServiceName,
    version: &Version,
    own: &HostkeeperVersion,
) -> Result<Manifest, String> {
    let expected = Expected {
        name: service.clone(),
        version: version.clone(),
        platform: hostkeeper_config::platform::Platform::current(),
    };
    Manifest::load(dir, &expected, own).map_err(text)
}

/// Places the target: its paths `pending`, its links, `current`, `daemon-reload`; and, for a
/// first install, its paths `placed`.
fn forward(
    placer: &Placer<'_>,
    order: &UpdateOrder,
    own: &HostkeeperVersion,
) -> Result<(), String> {
    let (service, version) = names(order)?;
    let manifest = manifest_of(order.staged.dir(), &service, &version, own)?;
    let claims = placer.desired(&manifest).map_err(text)?;
    let linking = placer.begin(&claims).map_err(text)?;
    placer.link(&linking).map_err(text)?;
    // Negative control: link switch stubbed (an update only; a first install still switches)
    if order.previous.is_none() {
        placer.switch_current(&service, &version).map_err(text)?;
    }
    placer.reload().map_err(text)?;
    if order.previous.is_none() {
        placer.commit(linking).map_err(text)?;
    }
    Ok(())
}

/// Undoes the switch: an update goes back to the release it replaced, a first install is taken
/// off the host.
fn back(
    placer: &Placer<'_>,
    fs: &dyn Fs,
    order: &UpdateOrder,
    own: &HostkeeperVersion,
) -> Result<(), String> {
    match (&order.previous, order.previous_version.as_deref()) {
        (Some(previous), Some(version)) => {
            let (service, _) = names(order)?;
            let version = Version::new(version).map_err(text)?;
            let manifest = manifest_of(previous.dir(), &service, &version, own)?;
            placer.switch_current(&service, &version).map_err(text)?;
            placer.relink_to(&manifest).map_err(text)?;
            Ok(())
        }
        (None, None) => uninstall_first(placer, fs, order),
        _ => Err(format!(
            "the order for {} names only half of the release it replaces",
            order.service
        )),
    }
}

/// Commits the target's paths and removes what it does not list.
fn confirmed(
    placer: &Placer<'_>,
    order: &UpdateOrder,
    own: &HostkeeperVersion,
) -> Result<(), String> {
    let (service, version) = names(order)?;
    let manifest = manifest_of(order.staged.dir(), &service, &version, own)?;
    let claims = placer.desired(&manifest).map_err(text)?;
    let linking = placer.begin(&claims).map_err(text)?;
    placer.commit(linking).map_err(text)?;
    placer.unlink_unlisted(&manifest).map_err(text)?;
    Ok(())
}

/// Takes the install back off the host: every placed path of the service (its units stopped
/// first), then `current` when it names the target.
fn uninstall_first(placer: &Placer<'_>, fs: &dyn Fs, order: &UpdateOrder) -> Result<(), String> {
    let (service, version) = names(order)?;
    placer.remove_all(&service).map_err(text)?;
    let named = placer.current(service.as_str()).map_err(text)?;
    match named.as_deref() == Some(version.as_str()) {
        true => {
            let current = placer.roots().packages().current(service.as_str());
            fs.remove_file(&current).map_err(text)?;
            fs.sync_dir(current.parent().unwrap_or(Path::new("/")))
                .map_err(text)
        }
        false => Ok(()),
    }
}
