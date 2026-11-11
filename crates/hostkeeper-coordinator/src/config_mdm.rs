//! The start's wiring of the MDM listener (`--mdm-listen`, `docs/design/coordinator-mdm.md`
//! section 4.1): the queue the gate puts its commands in, the server of the routes, and the
//! listener that serves them with the TLS of the store.

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_enrollment::validity::{SystemClock, UnixClock};
use hostkeeper_mdm_gate::CommandQueue;
use hostkeeper_mdm_push::clock::SystemClock as PushClock;
use tokio::io::{AsyncRead, AsyncWrite};

use super::Serve;
use crate::enrollment::os_random;
use crate::listen::{CLIENT_PATIENCE, MAX_CONNECTIONS, Remote, serve_mdm};
use crate::mdm_acme::Clock;
use crate::mdm_acme::SystemClock as AcmeClock;
use crate::mdm_acme::godaddy::GoDaddyProviders;
use crate::mdm_acme::https::{Https, RealHttps};
use crate::mdm_acme::renewal::{Parts as RenewerParts, TlsRenewer, renew_every};
use crate::mdm_lease::Lease;
use crate::mdm_server::pushing::{SetupPushers, production_sender};
use crate::mdm_server::queue::{MdmQueue, Pushers};
use crate::mdm_server::tls::{MdmTls, Trust};
use crate::mdm_server::{MdmServer, Parts};
use crate::mdm_setup::MdmSetup;
use crate::service::Coordinator;
use crate::store::mdm::MdmStore;

/// How long a request to the ACME directory or the DNS provider, and its answer, may take.
const ACME_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// What `--mdm-listen` makes at start, before the gate: the queue, which the gate is opened over.
pub(super) struct Wiring {
    address: SocketAddr,
    sweep: Duration,
    store: Arc<dyn MdmStore>,
    queue: Arc<MdmQueue>,
    pushers: Pushers,
    tls: Arc<MdmTls>,
    renewer: Arc<TlsRenewer>,
    pkg_dir: Option<std::path::PathBuf>,
}

/// The issuers a client certificate of the MDM listener may chain to: the CA of the Macs'
/// identities once it exists (it is made when the first Mac needs it). Until then no client
/// certificate is trusted at the handshake, and a Mac authenticates by its `Mdm-Signature` in
/// any case.
fn trusted_issuers(setup: Arc<MdmSetup>) -> Trust {
    Arc::new(move || {
        crate::mdm_enroll::ca::held_certificate(&setup)
            .into_iter()
            .collect()
    })
}

/// The renewer of the certificate for the MDM name that reaches the real ACME directory and the
/// real DNS provider: HTTPS to the public web, GoDaddy, the wall clock.
fn production_renewer(setup: &Arc<MdmSetup>) -> TlsRenewer {
    let https: Arc<dyn Https> = Arc::new(RealHttps::production(ACME_REQUEST_TIMEOUT));
    let clock: Arc<dyn Clock> = Arc::new(AcmeClock);
    TlsRenewer::new(RenewerParts {
        setup: Arc::clone(setup),
        providers: Arc::new(GoDaddyProviders::new(
            Arc::clone(&https),
            Arc::clone(&clock),
        )),
        https,
        clock,
        status: Arc::clone(setup.tls_status()),
    })
}

/// The wiring of `flags`' MDM listener over `store` and `setup`; none without `--mdm-listen`.
pub(super) fn wire(
    flags: &Serve,
    store: &Arc<dyn MdmStore>,
    setup: &Arc<MdmSetup>,
    lease: &Lease,
) -> Option<Wiring> {
    let address = flags.mdm.mdm_listen?;
    let pushing = Arc::new(SetupPushers::new(
        Arc::clone(setup),
        Arc::new(PushClock),
        production_sender(),
    ));
    let pushers: Pushers = Arc::new(move || pushing.get());
    let clock: Arc<dyn UnixClock> = Arc::new(SystemClock);
    let queue = Arc::new(MdmQueue::new(
        Arc::clone(store),
        clock,
        Arc::clone(&pushers),
        lease.clone(),
        Arc::clone(setup),
        flags.mdm.mdm_command_timeout_s,
    ));
    Some(Wiring {
        address,
        sweep: Duration::from_secs(flags.mdm.mdm_queue_sweep_s),
        store: Arc::clone(store),
        queue,
        pushers,
        tls: Arc::new(MdmTls::new(
            Arc::clone(setup),
            trusted_issuers(Arc::clone(setup)),
        )),
        renewer: Arc::new(production_renewer(setup)),
        pkg_dir: flags.mdm.mdm_pkg_dir.clone(),
    })
}

impl Wiring {
    /// The queue, as the gate's port.
    pub(super) fn queue(&self) -> Arc<dyn CommandQueue> {
        Arc::clone(&self.queue) as Arc<dyn CommandQueue>
    }

    fn attach_to(self, coordinator: Coordinator) -> (Coordinator, SocketAddr) {
        let gate = coordinator.mdm().map(|mdm| Arc::clone(&mdm.gate));
        let server = MdmServer::new(Parts {
            store: self.store,
            clock: Arc::new(SystemClock),
            random: os_random(),
            gate,
            queue: self.queue,
            pushers: self.pushers,
            tls: self.tls,
            sweep: self.sweep,
            abm: None,
            renewer: self.renewer,
            pkg_dir: self.pkg_dir,
        });
        (
            coordinator.with_mdm_server(Some(Arc::new(server))),
            self.address,
        )
    }
}

/// `coordinator` serving the Macs' listener when `wiring` is given, with the gate when it runs
/// one, and the address to bind.
pub(super) fn attach(
    wiring: Option<Wiring>,
    coordinator: Coordinator,
) -> (Coordinator, Option<SocketAddr>) {
    match wiring {
        Some(wiring) => {
            let (coordinator, address) = wiring.attach_to(coordinator);
            (coordinator, Some(address))
        }
        None => (coordinator, None),
    }
}

/// Serves the Macs' listener on the connections `accept` gives, and looks at what the queue has
/// due every `--mdm-queue-sweep-s` seconds, forever.
pub(super) async fn serve<A, F, S>(accept: A, coordinator: Arc<Coordinator>) -> Infallible
where
    A: FnMut() -> F + Send,
    F: Future<Output = std::io::Result<S>> + Send,
    S: AsyncRead + AsyncWrite + Remote + Unpin + Send + 'static,
{
    let Some(server) = coordinator.mdm_server().cloned() else {
        return std::future::pending().await;
    };
    let serving = serve_mdm(accept, coordinator, CLIENT_PATIENCE, MAX_CONNECTIONS);
    let sweeping = crate::mdm_server::sweep_every(Arc::clone(&server));
    let queue = crate::listen::together(serving, sweeping);
    let renewing = renew_every(Arc::clone(&server.renewer));
    crate::listen::together(queue, renewing).await
}

#[cfg(test)]
#[path = "config_mdm_tests.rs"]
mod tests;
