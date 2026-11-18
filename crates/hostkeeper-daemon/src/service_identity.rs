//! The daemon's service identities (docs/design/service-identity.md, section 4): the third worker
//! of [`crate::links::Links`], which keeps a key and an 8-hour certificate for each service that
//! asks for one, and the report of them that every heartbeat carries.
//!
//! The keeping is `hostkeeper_enroll::ServiceIdentities`; what is here is what it is told:
//!
//! - [`wanted`]: the services it is to keep an identity for, read at every pass. A service is one
//!   whose package directory has a `current` version whose sealed entry (`target.json`)
//!   gives a manifest with `identity`, and whose uninstall has no tombstone in `placed.json`. A
//!   `placed.json` that cannot be read names no service, as no order of a service is run while it
//!   cannot be read. The account of each is the manifest's `identity_account`, `root` when it has
//!   none, resolved by the host's [`ServiceAccounts`]; one it cannot resolve is that service's
//!   reported error.
//! - [`installed`]: the services the host has installed at all (a `current` version, no
//!   tombstone), read after each pass; the keeper removes the identity directory of every other
//!   service, so a pass that began before an uninstall leaves nothing behind it.
//! - [`DaemonIdentities`]: the heartbeat's [`ServiceIdentitySource`], over the keeper's
//!   [`IdentityState`]; it reports the first [`MAX_IDENTITY_SERVICES`] services by name, as the
//!   coordinator drops a longer list whole.
//! - [`identity_loop`]: the worker and the source made together. The worker makes the keeper's
//!   ports on its own thread ([`MakePorts`]: the issuer owns an async runtime, as the enroller's
//!   transport does), logs why when they cannot be made ([`NO_IDENTITY`]) and ends, and otherwise
//!   keeps passes until the stop flag is set. [`identity_ports`] makes the shipped ports: the
//!   coordinator over mutual TLS pinned to the daemon config's `ca_sha256`, the identity directory
//!   of the daemon's directories, the id and CA bundle of the pki directory's current generation, the
//!   operating system's entropy, the wall clock for validity alone and the monotonic clock.

use std::path::Path;
use std::sync::Arc;

use hostkeeper_config::manifest::Expected;
use hostkeeper_config::names::ServiceName;
use hostkeeper_config::platform::Platform;
use hostkeeper_enroll::{
    GrpcIssuer, IdentityPorts, IdentityState, OsEntropy, PkiDir, PkiTrust, ServiceIdentities,
    SystemWallClock, Wanted,
};
use hostkeeper_link::{
    Log, MAX_IDENTITY_SERVICES, ServiceIdentityReport, ServiceIdentitySource, StopFlag,
};

use crate::adapters::host_clock;
use crate::cli::ENROLL_TIMEOUT;
use crate::executor::{Ctx, Daemon};
use crate::installed::{self, CURRENT};
use crate::service_account::{DEFAULT_ACCOUNT, ServiceAccounts};
use crate::target;

/// The line the worker logs when it cannot make its ports, before the reason.
pub const NO_IDENTITY: &str = "error: the service identity loop cannot start: ";

/// The line the worker logs when it cannot remove the identity directory of a service that is not
/// installed, before the reason.
pub const NO_SWEEP: &str =
    "error: the identity directory of an uninstalled service could not be removed: ";

/// Makes the keeper's ports, on the worker's own thread. `Err` says why there are none.
pub type MakePorts = Box<dyn FnOnce() -> Result<IdentityPorts, String> + Send>;

/// The loop's body, run on a worker of the links' dispatch.
pub type IdentityWorker = Box<dyn FnOnce() + Send>;

/// The worker of the service identity loop and the source its heartbeats report from.
pub struct IdentityLoop {
    pub worker: IdentityWorker,
    pub source: Arc<dyn ServiceIdentitySource>,
}

/// The heartbeat's source of the services' identities: what the keeper last reported of each.
pub struct DaemonIdentities {
    state: IdentityState,
}

impl DaemonIdentities {
    pub fn new(state: IdentityState) -> DaemonIdentities {
        DaemonIdentities { state }
    }
}

impl ServiceIdentitySource for DaemonIdentities {
    fn reports(&self) -> Vec<ServiceIdentityReport> {
        self.state
            .get()
            .into_iter()
            .take(MAX_IDENTITY_SERVICES)
            .map(|report| {
                ServiceIdentityReport::new(report.service, report.not_after, report.last_error)
            })
            .collect()
    }
}

/// The services that want an identity, with the owner of their files; see the module
/// documentation.
pub fn wanted(ctx: &Ctx, accounts: &dyn ServiceAccounts) -> Vec<Wanted> {
    let Ok(listed) = installed::service_dirs(&ctx.dirs.packages) else {
        return Vec::new();
    };
    let Ok(record) = ctx.placer().store().load() else {
        return Vec::new();
    };
    listed
        .into_values()
        .filter(|service| !record.uninstalling.contains_key(service.as_str()))
        .filter_map(|service| asks(ctx, service))
        .map(|(service, account)| Wanted {
            account: accounts.lookup(&account),
            service,
        })
        .collect()
}

/// The names of the services the host has installed at all: a `current` version and no tombstone
/// in `placed.json`. None when the packages directory or `placed.json` cannot be read, as then
/// nothing is known to be absent.
pub fn installed(ctx: &Ctx) -> Option<Vec<String>> {
    let listed = installed::service_dirs(&ctx.dirs.packages).ok()?;
    let record = ctx.placer().store().load().ok()?;
    Some(
        listed
            .into_values()
            .filter(|service| !record.uninstalling.contains_key(service.as_str()))
            .filter(|service| {
                installed::linked_version(&ctx.dirs.packages, service, CURRENT).is_some()
            })
            .map(|service| service.to_string())
            .collect(),
    )
}

/// `service` and its account, when the entry of its `current` version asks for an identity.
fn asks(ctx: &Ctx, service: ServiceName) -> Option<(ServiceName, String)> {
    let version = installed::linked_version(&ctx.dirs.packages, &service, CURRENT)?;
    let dir = ctx
        .dirs
        .packages
        .join(service.as_str())
        .join(version.as_str());
    let expected = Expected {
        name: service.clone(),
        version,
        platform: Platform::current(),
    };
    let manifest = target::manifest(&dir, &expected, &ctx.own_version).ok()?;
    manifest.identity.then(|| {
        let account = manifest
            .identity_account
            .unwrap_or_else(|| DEFAULT_ACCOUNT.to_owned());
        (service, account)
    })
}

/// The worker that keeps the identities of `daemon`'s services over the ports `make` makes, and the
/// source that reports them. The worker passes every minute until `stop` is set.
pub fn identity_loop(
    daemon: Arc<Daemon>,
    make: MakePorts,
    accounts: Arc<dyn ServiceAccounts>,
    log: Arc<dyn Log>,
    stop: StopFlag,
) -> IdentityLoop {
    let state = IdentityState::default();
    let source = Arc::new(DaemonIdentities::new(state.clone()));
    let worker = Box::new(move || {
        let ports = match make() {
            Ok(ports) => ports,
            Err(why) => {
                log.line(&format!("{NO_IDENTITY}{why}"));
                return;
            }
        };
        let mut keeper = ServiceIdentities::new(ports, state);
        keeper.run_loop(
            &mut || wanted(daemon.ctx(), accounts.as_ref()),
            &mut || installed(daemon.ctx()),
            &mut |error| log.line(&format!("{NO_SWEEP}{error}")),
            &mut || !stop.is_stopped(),
        );
    });
    IdentityLoop { worker, source }
}

/// The shipped ports: see the module documentation. `pki` is the pki directory, whose current
/// generation holds the certificate the issuer presents.
pub fn identity_ports(daemon: &Daemon, pki: &Path) -> MakePorts {
    let ctx = daemon.ctx();
    let endpoint = ctx.config.coordinator.as_str().to_owned();
    let pin = ctx.config.ca_sha256.as_str().to_owned();
    let files = ctx.dirs.identity.clone();
    let current = PkiDir::new(pki).current_paths();
    Box::new(move || {
        let made = GrpcIssuer::new(&endpoint, current.clone(), ENROLL_TIMEOUT, &pin);
        let issuer = made.map_err(|e| e.to_string())?;
        Ok(IdentityPorts {
            issuer: Box::new(issuer),
            files: Box::new(files),
            trust: Box::new(PkiTrust::new(current)),
            entropy: Box::new(OsEntropy),
            wall: Box::new(SystemWallClock),
            clock: Box::new(host_clock()),
        })
    })
}
