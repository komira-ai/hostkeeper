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
//! With `--mdm-profile-identifier` and `--mdm-daemon-config` as well, the gate runs its
//! enrollment side ([`hostkeeper_mdm_gate::enroll`]): it installs the config profile on each
//! allowlisted Mac, and pushes the one-time nonce once the Mac's daemon has posted its request.
//! The daemon config file is checked at start by the daemon's own loader. The gate asks the
//! coordinator's enrollment through [`hostkeeper_mdm_gate::Enrollments`], attached once the
//! enrollment is built ([`Mdm::attach`]), is told of each new request, and looks at every
//! device once a minute besides ([`hostkeeper_mdm_gate::enroll::run`]).
//!
//! Without the three flags the coordinator runs no gate: its port is [`NoDeviceState`], and
//! `Mdm.ListDevices` answers no device.

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::{Arc, OnceLock, Weak};

use futures::future::{Either, select};
use tokio::io::{AsyncRead, AsyncWrite};

use clap::Args;
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::names::KeyHash;
use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_mdm_gate::{
    ApiKey, ApiKeyError, ApiUrl, ApiUrlError, CONFIG_FLAG, DaemonProfile, DaemonProfileError,
    EnrollSide, Enrollments, EnrollmentsError, FileDeviceStore, Gate, IdentifierError, NanoHub,
    Nonce, ProfileIdentifier, Random, StoreError, Waiting, enroll, webhook,
};

use crate::enrollment::{
    DeviceState, DeviceStateError, DeviceStatus, Enrollment, NoDeviceState, Posted,
};

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
    /// The farm's MDM profile identifier, a reverse-DNS name, as `com.example.hostkeeper.mdm`
    /// (the kit's settings say it). With it and --mdm-daemon-config the gate installs the
    /// config profile `<identifier>.config` on each Mac on --mac-serial-allowlist and pushes
    /// its one-time nonce in `<identifier>.nonce`. Requires --mdm-api-url and
    /// --mdm-daemon-config.
    #[arg(long, requires_all = ["mdm_api_url", "mdm_daemon_config"])]
    pub mdm_profile_identifier: Option<String>,
    /// The daemon config the config profile carries: a `hostkeeper.toml` of the farm
    /// (coordinator, ca_sha256, release_channel, trusted_signers, bounds), checked at start by
    /// the daemon's own loader. It holds no secret. Requires --mdm-profile-identifier.
    #[arg(long, requires = "mdm_profile_identifier")]
    pub mdm_daemon_config: Option<PathBuf>,
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
    /// `--mdm-profile-identifier` is not a reverse-DNS name.
    #[error(transparent)]
    Identifier(#[from] IdentifierError),
    /// `--mdm-daemon-config` cannot be read.
    #[error("{CONFIG_FLAG} {path}: {source}")]
    ConfigRead {
        /// The file.
        path: String,
        /// What the system said.
        #[source]
        source: std::io::Error,
    },
    /// `--mdm-daemon-config` is refused by the daemon's loader.
    #[error(transparent)]
    Config(#[from] DaemonProfileError),
}

/// The running gate, and where its webhook is served.
#[derive(Debug)]
pub struct Mdm {
    /// The gate.
    pub gate: Arc<Gate>,
    /// Where NanoHUB's webhook is served.
    pub webhook_listen: SocketAddr,
    /// The enrollment the gate's enrollment side asks, once attached; `None` without the side.
    attached: Option<Arc<Attached>>,
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
        let side = side_of(flags)?;
        let store = FileDeviceStore::open(Arc::new(RealFs), &enrollment_dir.join(DEVICES_DIR))?;
        let gate = Gate::open(Box::new(store), NanoHub::new(url, key), clock, random)?;
        let (gate, attached) = match side {
            Some((identifier, config)) => {
                let attached = Arc::new(Attached::default());
                let side = EnrollSide {
                    identifier,
                    config,
                    enrollments: Arc::clone(&attached) as Arc<dyn Enrollments>,
                };
                (gate.with_enrollment(side), Some(attached))
            }
            None => (gate, None),
        };
        Ok(Some(Self {
            gate: Arc::new(gate),
            webhook_listen: listen,
            attached,
        }))
    }

    /// Gives the gate's enrollment side `enrollment` to ask; before this it sends no profile.
    /// Without the side it does nothing.
    pub fn attach(&self, enrollment: &Arc<Enrollment>) {
        // Set once; a second attach keeps the first.
        let _ = self
            .attached
            .as_ref()
            .map(|attached| attached.0.set(Arc::downgrade(enrollment)));
    }

    /// `enrollment` as the coordinator runs it with the gate of `mdm`: it tells the gate of each
    /// new request ([`Enrollment::with_posted`]), and the gate's enrollment side asks it
    /// ([`Mdm::attach`]). Without a gate, `enrollment` as it is.
    #[must_use]
    pub fn wire(mdm: Option<&Self>, enrollment: Enrollment) -> Arc<Enrollment> {
        let enrollment = Arc::new(match Self::posted(mdm) {
            Some(posted) => enrollment.with_posted(posted),
            None => enrollment,
        });
        let _ = mdm.map(|mdm| mdm.attach(&enrollment));
        enrollment
    }

    /// Serves NanoHUB's webhook to `gate` on the connections `accept` gives, and has `gate`
    /// look at every device once a minute and at each wake
    /// ([`hostkeeper_mdm_gate::enroll::run`]), forever.
    pub async fn serve<A, F, S>(gate: Arc<Gate>, accept: A) -> Infallible
    where
        A: FnMut() -> F,
        F: Future<Output = std::io::Result<S>>,
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let webhook = pin!(webhook::serve(accept, Arc::clone(&gate)));
        let looks = pin!(enroll::run(gate, enroll::TICK));
        let (Either::Left((never, _)) | Either::Right((never, _))) = select(webhook, looks).await;
        match never {}
    }

    /// What the enrollment calls after a new request is recorded: a wake of the gate of `mdm`,
    /// or nothing without one.
    #[must_use]
    pub fn posted(mdm: Option<&Self>) -> Option<Posted> {
        mdm.map(|mdm| {
            let gate = Arc::clone(&mdm.gate);
            Arc::new(move || gate.wake()) as Posted
        })
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

/// The enrollment side's identifier and daemon config, when its two flags are given.
fn side_of(flags: &MdmFlags) -> Result<Option<(ProfileIdentifier, DaemonProfile)>, MdmError> {
    let (Some(identifier), Some(config)) =
        (&flags.mdm_profile_identifier, &flags.mdm_daemon_config)
    else {
        // clap requires the two together: without them there is no enrollment side.
        return Ok(None);
    };
    let identifier = ProfileIdentifier::parse(identifier)?;
    let text = std::fs::read_to_string(config).map_err(|source| MdmError::ConfigRead {
        path: config.display().to_string(),
        source,
    })?;
    Ok(Some((identifier, DaemonProfile::from_toml(&text)?)))
}

/// The coordinator's enrollment, as the gate asks it once [`Mdm::attach`] gave it. Held weakly:
/// the enrollment holds the gate (its device state), so a strong hold would be a cycle. Before
/// it is attached, or once it is gone, no serial is allowed and nothing is answered.
#[derive(Debug, Default)]
struct Attached(OnceLock<Weak<Enrollment>>);

impl Attached {
    fn get(&self) -> Result<Arc<Enrollment>, EnrollmentsError> {
        self.0.get().and_then(Weak::upgrade).ok_or_else(|| {
            EnrollmentsError("the coordinator's enrollment is not attached".to_owned())
        })
    }
}

impl Enrollments for Attached {
    fn allowed(&self, serial: &str) -> bool {
        self.get()
            .is_ok_and(|enrollment| enrollment.allowed(serial))
    }

    fn waiting(&self, serial: &str) -> Result<Vec<Waiting>, EnrollmentsError> {
        self.get()?.waiting(serial)
    }

    fn last_approval_unix(&self, serial: &str) -> Result<Option<u64>, EnrollmentsError> {
        self.get()?.last_approval_unix(serial)
    }

    fn issue_nonce(&self, serial: &str, request_id: &KeyHash) -> Result<Nonce, EnrollmentsError> {
        self.get()?.issue_nonce(serial, request_id)
    }

    fn audit(&self, action: &str, reason: &str) {
        // Unattached, the gate has issued nothing to audit.
        let _ = self
            .get()
            .map(|enrollment| Enrollments::audit(&*enrollment, action, reason));
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

#[cfg(test)]
#[path = "mdm_enroll_tests.rs"]
mod enroll_tests;
