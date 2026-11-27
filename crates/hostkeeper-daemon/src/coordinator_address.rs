//! The coordinator's reach as the daemon makes it ([`host_reach`]), and the file that keeps the
//! address it was last reached at across a restart ([`ADDRESS_FILE`] in the state directory).
//!
//! The reach looks the coordinator's name up through a `BackgroundLookup` over the system
//! resolver that waits `LOOKUP_SECS` (`hostkeeper_link::dial`), and dials the daemon config's
//! `coordinator_ip` after the addresses it knows. The address of the file, when it was written
//! for the same name and port, is the one remembered at the start: a daemon restarted while its
//! name cannot be looked up (an update of hostkeeper itself, a reboot) reaches the coordinator
//! where it last did.
//!
//! The file is a hint and nothing trusts it: the address in it is dialed as any other, and the
//! coordinator's certificate is checked against its name whatever address answered. A file that
//! is absent, cannot be read or parsed, or was written for another name or port, is read as
//! none; the next address reached writes it whole.
//!
//! ```json
//! {"name": "coordinator.example", "address": "192.0.2.10:8443"}
//! ```

use std::fs;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_link::{
    BackgroundLookup, EndpointError, LOOKUP_SECS, Log, Lookup, Reach, SystemLookup,
};
use serde::{Deserialize, Serialize};

use crate::files::write_atomic;

/// The file of the state directory that keeps the address the coordinator was last reached at.
pub const ADDRESS_FILE: &str = "coordinator-address.json";

/// What [`ADDRESS_FILE`] holds: the coordinator's name, as the daemon config's URL writes it,
/// and the address (with its port) whose handshake last completed as it.
#[derive(Debug, Deserialize, Serialize)]
struct AddressFile {
    name: String,
    address: SocketAddr,
}

/// The production lookup of the coordinator's name: the system resolver, run on a thread of its
/// own and waited for at most [`LOOKUP_SECS`].
#[must_use]
pub fn host_lookup() -> Arc<dyn Lookup> {
    Arc::new(BackgroundLookup::new(
        Arc::new(SystemLookup),
        Duration::from_secs(LOOKUP_SECS),
    ))
}

/// The reach of the coordinator of `config`, looked up through `lookup`, logging to `log`, and
/// remembering the address of [`ADDRESS_FILE`] in `state_dir` (when one is given and the file
/// was written for its name and port).
///
/// # Errors
/// [`EndpointError`]: the config's coordinator address is not one the links dial.
pub fn host_reach(
    config: &DaemonConfig,
    state_dir: Option<&Path>,
    lookup: Arc<dyn Lookup>,
    log: Arc<dyn Log>,
) -> Result<Reach, EndpointError> {
    let reach = Reach::new(config, lookup, log)?;
    Ok(match state_dir.and_then(|dir| read(dir, &reach)) {
        Some(addr) => reach.remembering(addr),
        None => reach,
    })
}

/// The address [`ADDRESS_FILE`] in `state_dir` keeps for `reach`'s name and port; none when the
/// file is absent, cannot be read or parsed, or is another name's or port's.
#[must_use]
pub fn read(state_dir: &Path, reach: &Reach) -> Option<SocketAddr> {
    let bytes = fs::read(state_dir.join(ADDRESS_FILE)).ok()?;
    let file: AddressFile = serde_json::from_slice(&bytes).ok()?;
    (file.name == reach.name() && file.address.port() == reach.port()).then_some(file.address)
}

/// Writes `addr` as the address `reach`'s coordinator was last reached at, to [`ADDRESS_FILE`]
/// in `state_dir`, whole, by a rename ([`write_atomic`]).
///
/// # Errors
/// The write's own.
pub fn write(state_dir: &Path, reach: &Reach, addr: SocketAddr) -> io::Result<()> {
    let file = AddressFile {
        name: reach.name().to_owned(),
        address: addr,
    };
    let bytes = serde_json::to_vec(&file).map_err(io::Error::other)?;
    write_atomic(state_dir, ADDRESS_FILE, &bytes, &|_| false)
}

#[cfg(test)]
#[path = "coordinator_address_tests.rs"]
mod tests;
