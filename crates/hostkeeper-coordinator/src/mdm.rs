//! The MDM policy gate in the coordinator (docs/design/hostkeeper-mdm.md sections 3 and 6.1):
//! its flags, its start, and the coordinator's [`DeviceState`] port answered by it.
//!
//! The gate is `hostkeeper_mdm_gate`, linked in, so the enrollment's question about a Mac
//! serial is a library call and no RPC. With `--mdm-api-url`, `--mdm-api-key-file` and
//! `--mdm-webhook-listen` (all three, and `--enrollment-dir`) the coordinator:
//!
//! - opens the gate's device records, a file per device in `mdm-devices` under the enrollment
//!   store (so `backup` and `restore` copy them); a record that does not read stops the start;
//! - reads NanoHUB's API key from its file, which must pass the secret file rule (the error
//!   names the flag, never the path or the key);
//! - serves NanoHUB's webhook on the loopback address `--mdm-webhook-listen`
//!   ([`hostkeeper_mdm_gate::webhook`]);
//! - answers [`DeviceState::status`] from the gate's records, so a Mac whose serial is on the
//!   allowlist is approved by its nonce only when the gate says it is enrolled and supervised.
//!
//! Without the three flags the coordinator runs no gate: its port is [`NoDeviceState`], and
//! `Mdm.ListDevices` answers no device.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Args;
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_mdm_gate::{
    ApiKey, ApiKeyError, ApiUrl, ApiUrlError, FileDeviceStore, Gate, NanoHub, Random, StoreError,
    webhook,
};

use crate::enrollment::{DeviceState, DeviceStateError, DeviceStatus, NoDeviceState};

/// The directory of the gate's device records in the enrollment store.
pub const DEVICES_DIR: &str = "mdm-devices";

/// The flags of the MDM policy gate, all three or none.
#[derive(Clone, Debug, Args)]
pub struct MdmFlags {
    /// NanoHUB's API, on loopback: `http://<loopback address>:<port>`, as
    /// `http://127.0.0.1:9004`. With it the coordinator runs the MDM policy gate: it keeps a
    /// record per MDM device and answers whether a Mac is enrolled and supervised, so a Mac on
    /// --mac-serial-allowlist can be approved by its nonce. Requires --mdm-api-key-file,
    /// --mdm-webhook-listen and --enrollment-dir.
    #[arg(
        long,
        requires_all = ["mdm_api_key_file", "mdm_webhook_listen", "enrollment_dir"]
    )]
    pub mdm_api_url: Option<String>,
    /// NanoHUB's API key (its -api-key): a file of one key, mode 0600 or 0400, owned by the
    /// coordinator's user (a link is refused).
    #[arg(long, requires = "mdm_api_url")]
    pub mdm_api_key_file: Option<PathBuf>,
    /// Where to serve NanoHUB's webhook: a loopback address and port, as `127.0.0.1:9100`.
    /// NanoHUB is started with `-webhook-url http://<this address>/webhook`.
    #[arg(long, requires = "mdm_api_url")]
    pub mdm_webhook_listen: Option<SocketAddr>,
}

/// Why the gate did not start.
#[derive(Debug, thiserror::Error)]
pub enum MdmError {
    /// `--mdm-api-url` is not a loopback HTTP URL.
    #[error(transparent)]
    Url(#[from] ApiUrlError),
    /// `--mdm-api-key-file` is refused.
    #[error(transparent)]
    Key(#[from] ApiKeyError),
    /// `--mdm-webhook-listen` is not a loopback address.
    #[error(
        "--mdm-webhook-listen: {0} is not a loopback address; NanoHUB posts to it from the same host"
    )]
    Listen(SocketAddr),
    /// A device record cannot be read.
    #[error("the MDM device records: {0}")]
    Store(#[from] StoreError),
}

/// The running gate, and where its webhook is served.
#[derive(Debug)]
pub struct Mdm {
    /// The gate.
    pub gate: Arc<Gate>,
    /// Where NanoHUB's webhook is served.
    pub webhook_listen: SocketAddr,
}

impl Mdm {
    /// The gate the flags name, its records in `enrollment_dir`, its times from `clock` and
    /// its command UUIDs from `random`; `None` without the flags.
    ///
    /// # Errors
    /// [`MdmError`]: the URL is not loopback HTTP, the key file is refused, the webhook address
    /// is not loopback, or a device record does not read.
    pub fn open(
        flags: &MdmFlags,
        enrollment_dir: &Path,
        clock: Arc<dyn UnixClock>,
        random: Random,
    ) -> Result<Option<Self>, MdmError> {
        let (Some(url), Some(key), Some(listen)) = (
            &flags.mdm_api_url,
            &flags.mdm_api_key_file,
            flags.mdm_webhook_listen,
        ) else {
            // clap requires the three together: without them there is no gate.
            return Ok(None);
        };
        let url = ApiUrl::parse(url)?;
        let key = ApiKey::read(key)?;
        if !webhook::is_loopback(&listen) {
            return Err(MdmError::Listen(listen));
        }
        let store = FileDeviceStore::open(Arc::new(RealFs), &enrollment_dir.join(DEVICES_DIR))?;
        let gate = Gate::open(Box::new(store), NanoHub::new(url, key), clock, random)?;
        Ok(Some(Self {
            gate: Arc::new(gate),
            webhook_listen: listen,
        }))
    }

    /// The coordinator's port onto the gate of `mdm`, or [`NoDeviceState`] without one.
    #[must_use]
    pub fn device_state(mdm: Option<&Self>) -> Arc<dyn DeviceState> {
        mdm.map_or_else(
            || Arc::new(NoDeviceState) as Arc<dyn DeviceState>,
            |mdm| Arc::new(GateDeviceState(Arc::clone(&mdm.gate))),
        )
    }
}

/// The gate as the enrollment asks it.
struct GateDeviceState(Arc<Gate>);

impl DeviceState for GateDeviceState {
    fn status(&self, serial: &str) -> Result<DeviceStatus, DeviceStateError> {
        self.0
            .status(serial)
            .map(|status| DeviceStatus {
                enrolled: status.enrolled,
                supervised: status.supervised,
                enrolled_since_unix: status.enrolled_since_unix,
            })
            .map_err(|why| DeviceStateError(why.to_string()))
    }
}

#[cfg(test)]
#[path = "mdm_tests.rs"]
pub(crate) mod tests;
