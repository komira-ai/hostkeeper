//! The switch of a first install (design sections 4.7, "Placed paths", and 5.2, steps 5 and
//! 6): the step runner calls it immediately before every run of `cutover` and of `rollback`
//! ([`hostkeeper_steps::StepRunner::with_switch`]).
//!
//! It acts only for an order with no release to replace, which on this daemon is an `install`
//! (an `update` and a `force_rollback` need a release installed, [`crate::checks::installed_rule`]);
//! for every other order it does nothing, and the placement of an update is left to a later
//! change. Before `cutover` it places the package by the write-ahead order of `placed.json`:
//! [`Placer::begin`] (the paths `pending`), [`Placer::link`], [`Placer::switch_current`]
//! (`current` only: there is no `previous`), [`Placer::reload`] and [`Placer::commit`]. Before
//! `rollback` it takes the install back off the host: [`Placer::remove_all`] (the `run` units
//! stopped and disabled, every placed link removed, `daemon-reload`) and then `current`, when it
//! names the target; the version directory stays, as a staged version a retry uses. Each is
//! idempotent, so a resumed step switches again.

use std::path::Path;
use std::sync::Arc;

use hostkeeper_config::manifest::{Expected, HostkeeperVersion, Manifest};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_placed::{Fs, Placer, Roots, UnitControl};
use hostkeeper_steps::{Switch, Toward, UpdateOrder};

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// The switch of a first install on the host of `roots`, through `fs` and `units`; a manifest
/// is read against the daemon's version `own`.
pub fn first_install(
    roots: Roots,
    fs: Arc<dyn Fs + Send + Sync>,
    units: Arc<dyn UnitControl + Send + Sync>,
    own: HostkeeperVersion,
) -> Switch {
    Arc::new(move |toward: Toward, order: &UpdateOrder| {
        let placer = Placer::new(roots.clone(), &*fs, &*units);
        match (&order.previous, toward) {
            (Some(_), _) => Ok(()),
            (None, Toward::Target) => forward(&placer, order, &own),
            (None, Toward::Back) => back(&placer, &*fs, order),
        }
    })
}

/// The service and the target of `order`, in their grammars.
fn names(order: &UpdateOrder) -> Result<(ServiceName, Version), String> {
    let service = ServiceName::new(&order.service).map_err(text)?;
    let version = Version::new(&order.target_version).map_err(text)?;
    Ok((service, version))
}

/// Places the target: its paths `pending`, its links, `current`, `daemon-reload`, its paths
/// `placed`.
fn forward(
    placer: &Placer<'_>,
    order: &UpdateOrder,
    own: &HostkeeperVersion,
) -> Result<(), String> {
    let (service, version) = names(order)?;
    let expected = Expected {
        name: service.clone(),
        version: version.clone(),
    };
    let manifest = Manifest::load(order.staged.dir(), &expected, own).map_err(text)?;
    let claims = placer.desired(&manifest).map_err(text)?;
    let linking = placer.begin(&claims).map_err(text)?;
    placer.link(&linking).map_err(text)?;
    placer.switch_current(&service, &version).map_err(text)?;
    placer.reload().map_err(text)?;
    placer.commit(linking).map_err(text)?;
    Ok(())
}

/// Takes the install back off the host: every placed path of the service (its units stopped
/// first), then `current` when it names the target.
fn back(placer: &Placer<'_>, fs: &dyn Fs, order: &UpdateOrder) -> Result<(), String> {
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
