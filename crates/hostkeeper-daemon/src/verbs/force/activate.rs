//! `hostkeeper activate`: the host-local switch of a service to a version the host already keeps
//! (docs/design/hostkeeper-design.md, Q16), run by root with the daemon stopped and the
//! coordinator and the network out of reach ([`crate::local`]).
//!
//! It is a [`force`](super) that fetches nothing and stages nothing:
//!
//! - Refused before anything changes: the service `hostkeeper` (`self_service`); a service whose
//!   first install or uninstall is unfinished (`installing`, `uninstalling`); a version the host
//!   does not keep whole, a directory and its seal (`not_kept`, with the versions it keeps); a
//!   version whose tree no longer has the digest its seal holds, checked again now
//!   (`release_unreadable`); a step record that cannot be read. A manifest that does not read for
//!   the service and version is the verdict `Failed` before the commit, as for a force.
//! - With `--pin`, the pin ([`crate::pin`]) is written next, before the switch, so the daemon
//!   started after a crash holds the coordinator's orders all the same.
//! - Then the force's steps 4 to 18: an unfinished hooked update is closed `forced_over` and the
//!   unit of its running step stopped by name, the force record is journaled, and the switch runs
//!   from the target's manifest and its installer alone. Nothing of the version left is run: its
//!   manifest is read for the names of its units, which the service manager stops. A record left
//!   with no verdict by a crash is the journal the daemon's start pass runs again from its top
//!   ([`super::resume`]); an `activate` run before that starts a force record of its own over it.
//!
//! The switch is final, as a force's is: no hook runs, nothing is reverted when the target's
//! units do not start (`start_failed`), and the repair is another `activate`. The force record
//! names no order id and keeps the try of the service's last record, so the coordinator's tries
//! go on past it.

use std::io;
use std::path::Path;
use std::time::SystemTime;

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_steps::{Layout, Protected, Seal, read_seal};

use super::{Journal, SELF_SERVICE, Target, begin, lifecycle_check, locked, read_manifest, switch};
use crate::executor::{Ctx, Reply};
use crate::installed::{self, CURRENT};
use crate::locks::Package;
use crate::pin;
use crate::refusal::Refusal;
use crate::verb::{HookedVerb, PackageDigest};
use crate::verbs::update::last_record;

/// The versions of the package directory `layout` the host keeps whole, a directory and its seal,
/// newest staged first (by the time its seal was written, then by name). None when the directory
/// cannot be read.
pub fn kept(layout: &Layout) -> io::Result<Vec<String>> {
    let inputs = layout.prune_inputs(&Protected::default())?;
    let mut whole: Vec<(SystemTime, String)> = inputs
        .seals
        .into_iter()
        .filter(|(version, _)| inputs.versions.contains(version))
        .map(|(version, staged)| (staged, version))
        .collect();
    whole.sort_by(|a, b| b.cmp(a));
    Ok(whole.into_iter().map(|(_, version)| version).collect())
}

/// The kept versions of `service` in the packages directory `packages`, as a refusal names them:
/// `2.0.0 (current), 1.0.0`, or `none`.
pub fn listing(packages: &Path, service: &ServiceName) -> String {
    let layout = Layout::package(&packages.join(service.as_str()));
    let current = installed::linked_version(packages, service, CURRENT);
    let named: Vec<String> = kept(&layout)
        .unwrap_or_default()
        .into_iter()
        .map(
            |version| match current.as_ref().map(Version::as_str) == Some(version.as_str()) {
                true => format!("{version} (current)"),
                false => version,
            },
        )
        .collect();
    match named.is_empty() {
        true => "none".to_owned(),
        false => named.join(", "),
    }
}

/// `hostkeeper activate`: `service` switched to `version`, which the host keeps, pinned there
/// first when `pin` is set.
pub fn activate(ctx: &Ctx, service: &ServiceName, version: &Version, pin: bool) -> Reply {
    if service.as_str() == SELF_SERVICE {
        return Reply::refused(
            Refusal::SelfService,
            format!("{service} updates itself by its own path, never by an activate"),
        );
    }
    let checked = kept_target(ctx, service, version).and_then(|kept| {
        lifecycle_check(ctx, service)?;
        last_record(ctx, service)?;
        Ok(kept)
    });
    let (target, digest) = match checked {
        Ok(checked) => checked,
        Err(refused) => return refused,
    };
    if pin && let Err(error) = pin::write(&ctx.dirs.state, service, version) {
        return Reply::refused(Refusal::StoreFailed, format!("the pin: {error}"));
    }
    let package = Package {
        verb: HookedVerb::Force,
        version: version.clone(),
        digest,
    };
    let journal = Journal {
        service,
        version,
        digest: &package.digest,
        order_try: None,
        halt_generation: 0,
        order_id: None,
    };
    let guard = match locked(ctx, service, &package, None) {
        Ok(guard) => guard,
        Err(reply) => return reply,
    };
    match begin(ctx, journal, &target) {
        Ok((rec, force)) => switch::run(ctx, &guard, rec, force, &target),
        Err(refused) => refused,
    }
}

/// The target of an activate of `service` to `version`, and its package digest, from what the
/// host keeps: refused `not_kept` when there is no directory and seal of it, `release_unreadable`
/// when its tree is not as its seal says.
fn kept_target(
    ctx: &Ctx,
    service: &ServiceName,
    version: &Version,
) -> Result<(Target, PackageDigest), Reply> {
    let layout = Layout::package(&ctx.dirs.packages.join(service.as_str()));
    let kept = kept(&layout).unwrap_or_default();
    if !kept.iter().any(|kept| kept == version.as_str()) {
        return Err(Reply::refused(
            Refusal::NotKept,
            format!(
                "{service} {version} is not kept on this host, which keeps {}",
                listing(&ctx.dirs.packages, service)
            ),
        ));
    }
    let sealed = layout
        .check_tree(version.as_str())
        .and_then(|()| read_seal(&layout.seal_path(version.as_str())))
        .and_then(|bytes| Seal::parse(&bytes).map_err(io::Error::other))
        .and_then(|seal| {
            PackageDigest::new(seal.package_digest())
                .map(|digest| (seal.tree_digest().to_owned(), digest))
                .ok_or_else(|| io::Error::other("its seal names no package digest"))
        });
    let (tree_digest, digest) = sealed.map_err(|error| {
        Reply::refused(
            Refusal::ReleaseUnreadable,
            format!("{service} {version} is not as its seal says: {error}"),
        )
    })?;
    let dir = layout.version_path(version.as_str());
    let (entry, manifest) = read_manifest(ctx, service, version, &dir)?;
    let target = Target {
        manifest,
        entry,
        tree_digest,
    };
    Ok((target, digest))
}
