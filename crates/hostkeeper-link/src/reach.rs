//! [`Reach`]: the addresses the coordinator's name is dialed at, and the one it was last reached
//! at.
//!
//! The coordinator is the name of the daemon config's `coordinator`; its certificate is checked
//! against that name whatever address is dialed. Each call asks [`Reach::candidates`] for the
//! addresses to try, in order, and tells [`Reach::reached`] the one whose TLS handshake completed:
//!
//! 1. the addresses the name looks up to, through the [`Lookup`] the reach was given, in the
//!    lookup's order but for the address remembered, which goes first when it is one of them (a
//!    name of several addresses, which a resolver may give in another order at each lookup,
//!    keeps being reached at the one it was reached at);
//! 2. the address remembered: the last one whose handshake completed as the coordinator, in this
//!    run or (given by the daemon with [`Reach::remembering`]) in an earlier one;
//! 3. the daemon config's `coordinator_ip`, at the URL's port.
//!
//! Each address is given once, at its first place. A lookup that fails leaves the other two, so a
//! host whose resolver cannot answer (an uplink that is down, for a name of a public zone) keeps
//! reaching a coordinator on its own network. A new address of the lookup is tried first, and is
//! remembered only once its handshake completes; one that does not answer as the coordinator
//! leaves the remembered one in use. A URL whose host is an address is that address, and nothing
//! is looked up.
//!
//! One log line is written at each change of how the coordinator is reached: from its name to
//! the remembered address or the configured one, from one of those to the other, and back to the
//! name. Reaching it through its name, at whichever address, is the state a daemon starts in and
//! writes nothing. A call that reaches nothing writes nothing here; the heartbeat loop says that
//! the coordinator could not be reached.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_config::daemon::DaemonConfig;

use crate::dial::{IpAddr, SocketAddr};
use crate::mtls::{Endpoint, EndpointError};
use crate::ports::Log;

/// How a name is looked up.
pub trait Lookup: Send + Sync {
    /// The addresses of `host` on `port`.
    ///
    /// # Errors
    /// The name cannot be looked up.
    fn lookup(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>>;
}

/// Where an address the coordinator is dialed at came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Source {
    /// The lookup of the coordinator's name, or the URL's address.
    #[default]
    Name,
    /// The last address whose handshake completed as the coordinator.
    Remembered,
    /// The daemon config's `coordinator_ip`.
    Configured,
}

/// An address to dial, and where it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) addr: SocketAddr,
    pub(crate) source: Source,
}

/// What the reach knows between calls.
#[derive(Default)]
struct State {
    /// The last address whose handshake completed as the coordinator.
    remembered: Option<SocketAddr>,
    /// Where the address last reached came from: the name until another is reached.
    via: Source,
    /// Why the last lookup failed; none when it answered.
    lookup_failed: Option<String>,
}

/// The coordinator's name, and where it is dialed.
pub struct Reach {
    endpoint: Endpoint,
    configured: Option<SocketAddr>,
    lookup: Arc<dyn Lookup>,
    log: Arc<dyn Log>,
    state: Mutex<State>,
}

impl Reach {
    /// The reach of the coordinator of `config`: its name, looked up through `lookup`, and its
    /// `coordinator_ip`; logging to `log`. Nothing is remembered yet.
    ///
    /// # Errors
    /// [`EndpointError`]: the config's coordinator address is not one the links dial.
    pub fn new(
        config: &DaemonConfig,
        lookup: Arc<dyn Lookup>,
        log: Arc<dyn Log>,
    ) -> Result<Reach, EndpointError> {
        let endpoint = Endpoint::of(&config.coordinator)?;
        let configured = config
            .coordinator_ip
            .map(|ip| SocketAddr::new(ip, endpoint.port));
        Ok(Reach {
            endpoint,
            configured,
            lookup,
            log,
            state: Mutex::new(State::default()),
        })
    }

    /// The same reach, remembering `addr` as the address last reached: the one an earlier run
    /// kept. It is dialed as any remembered address is.
    #[must_use]
    pub fn remembering(self, addr: SocketAddr) -> Reach {
        self.lock().remembered = Some(addr);
        self
    }

    /// The coordinator's name, as the URL writes it (an address without brackets when the URL's
    /// host is one).
    pub fn name(&self) -> &str {
        &self.endpoint.host
    }

    /// The port the coordinator is dialed on.
    pub fn port(&self) -> u16 {
        self.endpoint.port
    }

    /// The last address whose handshake completed as the coordinator; none before the first.
    pub fn remembered(&self) -> Option<SocketAddr> {
        self.lock().remembered
    }

    pub(crate) fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The addresses to dial, in order: the name's, the remembered one, the configured one.
    ///
    /// # Errors
    /// The lookup failed and there is no other address; its error.
    pub(crate) fn candidates(&self) -> io::Result<Vec<Candidate>> {
        let looked_up = match self.endpoint.host.parse::<IpAddr>() {
            Ok(ip) => Ok(vec![SocketAddr::new(ip, self.endpoint.port)]),
            Err(_) => self.lookup.lookup(&self.endpoint.host, self.endpoint.port),
        };
        let mut state = self.lock();
        let (mut named, failed) = match looked_up {
            Ok(addrs) => (addrs, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        state.lookup_failed = failed.as_ref().map(ToString::to_string);
        if let Some(at) = named
            .iter()
            .position(|addr| Some(*addr) == state.remembered)
        {
            named[..=at].rotate_right(1);
        }
        // A remembered address that is the configured one is the configured one: where it came
        // from does not change from one call to the next.
        let remembered = state
            .remembered
            .map(|addr| match self.configured == Some(addr) {
                true => (addr, Source::Configured),
                false => (addr, Source::Remembered),
            });
        let others = [
            remembered,
            self.configured.map(|addr| (addr, Source::Configured)),
        ];
        let mut candidates: Vec<Candidate> = Vec::new();
        let all = named
            .into_iter()
            .map(|addr| (addr, Source::Name))
            .chain(others.into_iter().flatten());
        for (addr, source) in all {
            if candidates.iter().all(|known| known.addr != addr) {
                candidates.push(Candidate { addr, source });
            }
        }
        match (candidates.is_empty(), failed) {
            (true, Some(error)) => Err(error),
            (true, None) => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the name has no address",
            )),
            (false, _) => Ok(candidates),
        }
    }

    /// Records that `candidate`'s handshake completed as the coordinator: it is remembered, and
    /// a line is logged when that changes where the address came from.
    pub(crate) fn reached(&self, candidate: Candidate) {
        let mut state = self.lock();
        let changed = state.via != candidate.source;
        state.remembered = Some(candidate.addr);
        state.via = candidate.source;
        if !changed {
            return;
        }
        let why = state.lookup_failed.as_ref().map_or_else(
            || "no address its name looks up to answered as it".to_owned(),
            |error| format!("its name cannot be looked up: {error}"),
        );
        let name = &self.endpoint.host;
        let addr = candidate.addr;
        let line = match candidate.source {
            Source::Name => format!("the coordinator {name} answers at {addr}"),
            Source::Remembered => format!(
                "the coordinator {name} answers at {addr}, the last address that answered as it \
                 ({why})"
            ),
            Source::Configured => format!(
                "the coordinator {name} answers at {addr}, the daemon config's coordinator_ip \
                 ({why})"
            ),
        };
        drop(state);
        self.log.line(&line);
    }
}
