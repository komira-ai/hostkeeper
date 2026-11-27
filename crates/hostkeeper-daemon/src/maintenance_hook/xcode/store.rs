//! The Xcode store, as the stage reads it: `GET /v1/xcode/<sha256>` on the coordinator's
//! heartbeat surface, over the host's mutual TLS (its enrollment certificate, the CA the daemon
//! config pins), from a byte on so that a broken copy resumes ([`CoordinatorGet::get_from`]).
//!
//! On a host the client is made for each read from the daemon config and the pki directory
//! ([`HostStore`]): the hook is its own process, and a config that cannot be read or a
//! coordinator address that cannot be dialed is [`StoreError::Unusable`], which the stage defers.
//! The coordinator's name is dialed where a reach of the read's own says
//! ([`crate::coordinator_address::host_reach`]): the addresses it looks up to, then the config's
//! `coordinator_ip`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_link::{CoordinatorGet, GetError, Got, Limit, Log, PkiCertSource, TcpTransport};
use hostkeeper_steps::Clock;

use crate::coordinator_address::{host_lookup, host_reach};

/// Why the store gave no body.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// The client of the coordinator could not be made: the daemon config, or its address.
    #[error("the coordinator cannot be reached: {0}")]
    Unusable(String),
    /// The read itself failed.
    #[error("{0}")]
    Get(GetError),
}

/// The coordinator's store, read from a byte on.
pub trait Store {
    /// Reads `path` from its byte `first` on, giving the bytes to `sink`; see
    /// [`CoordinatorGet::get_from`].
    ///
    /// # Errors
    /// [`StoreError`]: why the store gave no body.
    fn get_from(
        &self,
        path: &str,
        first: u64,
        limit: Limit,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Got, StoreError>;
}

impl Store for CoordinatorGet {
    fn get_from(
        &self,
        path: &str,
        first: u64,
        limit: Limit,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Got, StoreError> {
        CoordinatorGet::get_from(self, path, first, limit, sink).map_err(StoreError::Get)
    }
}

/// The store of the coordinator the daemon config at `config` names, with the certificate of the
/// pki directory `pki`, over TCP, each connection bounded by its read's deadline on `clock`.
pub struct HostStore {
    config: PathBuf,
    trusted_uid: u32,
    pki: PathBuf,
    clock: Arc<dyn Clock>,
}

impl HostStore {
    /// The store of the config at `config`, which must be owned by `trusted_uid` (root on a
    /// host), with the certificate of the pki directory `pki`.
    #[must_use]
    pub fn new(config: &Path, trusted_uid: u32, pki: &Path, clock: Arc<dyn Clock>) -> HostStore {
        HostStore {
            config: config.to_owned(),
            trusted_uid,
            pki: pki.to_owned(),
            clock,
        }
    }

    /// The client of the coordinator, made from the config as it is now.
    fn client(&self) -> Result<CoordinatorGet, StoreError> {
        let config = DaemonConfig::load(&self.config, self.trusted_uid)
            .map_err(|error| StoreError::Unusable(format!("{}: {error}", self.config.display())))?;
        let reach = host_reach(&config, None, host_lookup(), Arc::new(Quiet))
            .map_err(|error| StoreError::Unusable(error.to_string()))?;
        let certs = Arc::new(PkiCertSource::new(&self.pki, Arc::new(Quiet)));
        let transport = Arc::new(TcpTransport::new(Arc::clone(&self.clock)));
        Ok(CoordinatorGet::new(
            &config,
            &self.pki,
            certs,
            Arc::new(reach),
            transport,
            Arc::clone(&self.clock),
        ))
    }
}

impl Store for HostStore {
    fn get_from(
        &self,
        path: &str,
        first: u64,
        limit: Limit,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Got, StoreError> {
        Store::get_from(&self.client()?, path, first, limit, sink)
    }
}

/// A log that keeps nothing: the heartbeat's certificate source says what is wrong with the pki
/// directory.
struct Quiet;

impl Log for Quiet {
    fn line(&self, _text: &str) {}
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
