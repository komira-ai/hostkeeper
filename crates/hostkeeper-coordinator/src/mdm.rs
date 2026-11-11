//! The MDM policy gate in the coordinator (docs/design/hostkeeper-mdm.md sections 3 and 6.1):
//! its flags, its start, and the coordinator's [`DeviceState`] port answered by it.
//!
//! The gate is `hostkeeper_mdm_gate`, linked in, so the enrollment's question about a Mac
//! serial is a library call and no RPC. With `--mdm-api-url`, `--mdm-api-key-file` and
//! `--mdm-webhook-listen` (all three, and enrollment: `--ca-key-file` with `--enrollment-dir` or
//! `--database-dsn-file`) the coordinator:
//!
//! - opens the gate's device records where it keeps its other state ([`Devices`]): with the file
//!   store a file per device in `mdm-devices` under the enrollment store (so `backup` and
//!   `restore` copy them), with the database the table `mdm_devices`; a record that does not read
//!   stops the start;
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
//! The daemon config file is checked at start by the daemon's own loader. With `--mdm-stores`
//! and `--mdm-macos-pkg` the config profile also carries the domains `dev.hostkeeper.stores` and
//! `dev.hostkeeper.macos-pkg`, each file checked at start by the loader that reads it on the
//! Mac; a stores file that names a credential file is refused. The gate sends the config
//! profile again to a Mac whose settings it has not acknowledged. The gate asks the
//! coordinator's enrollment through [`hostkeeper_mdm_gate::Enrollments`], attached once the
//! enrollment is built ([`Mdm::attach`]), is told of each new request, and looks at every
//! device once a minute besides ([`hostkeeper_mdm_gate::enroll::run`]).
//!
//! With `--mdm-host` as well, the enrollment side installs the hostkeeper pkg
//! ([`hostkeeper_mdm_gate::install`]): the install named `hostkeeper` of the MDM setting
//! `installs`, by `InstallEnterpriseApplication` with an inline manifest naming its digest,
//! fetched from `https://<--mdm-host>/pkg/<sha256>.pkg`. The old `--mdm-pkg-allowlist` seeds the
//! setting once, while it was never set: its newest pkg, which must be root's alone, becomes the
//! install `hostkeeper`, and the file is not read again. The other installs of the setting are
//! sent by the baseline.
//!
//! With `--mdm-baseline-file` as well (and `--mac-serial-allowlist`), the enrollment side also
//! sets up each Mac at enrollment ([`hostkeeper_mdm_gate::baseline`]): per host group, Screen
//! Sharing for a supervised Mac, the group's profiles, the Mac's name and the installs, kept by a `ProfileList` check every
//! `--mdm-baseline-check-s` seconds. The file and its profiles must be root's alone; each of its
//! host groups must be the role of some allowlist entry; `Mdm.ListDevices` shows each Mac's state.
//!
//! With `--mdm-listen` instead (`docs/design/coordinator-mdm.md`, CM11a) the gate's commands go
//! into the coordinator's own queue ([`crate::mdm_server`]) and the Macs' check-ins and results
//! reach the gate from the listener; NanoHUB's three flags are not read and no webhook is served.
//!
//! Without the three flags or `--mdm-listen` the coordinator runs no gate: its port is
//! [`NoDeviceState`], and `Mdm.ListDevices` answers no device.

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::{Arc, OnceLock, Weak};

use futures::future::{Either, select};
use tokio::io::{AsyncRead, AsyncWrite};

use clap::Args;
use hostkeeper_enrollment::fs::{Fs, RealFs};
use hostkeeper_enrollment::names::KeyHash;
use hostkeeper_enrollment::validity::UnixClock;
use hostkeeper_mdm_gate::{
    ApiKey, ApiKeyError, ApiUrl, ApiUrlError, Baseline, BaselineError, CONFIG_FLAG, CommandQueue,
    ConfigProfile, DEFAULT_CHECK_SECS, DaemonProfile, DaemonProfileError, DeviceStore, EnrollSide,
    Enrollments, EnrollmentsError, FileDeviceStore, Gate, IdentifierError, Install, Installs,
    InstallsError, MACOS_PKG_FLAG, MacosPkgProfile, MacosPkgProfileError, MdmHost, MdmHostError,
    NanoHub, Nonce, PkgAllowlist, PkgAllowlistError, ProfileIdentifier, Random, SETTING_NAME,
    STORES_FLAG, StoreError, StoresProfile, StoresProfileError, Waiting, enroll, webhook,
};

use crate::enrollment::{
    DeviceState, DeviceStateError, DeviceStatus, Enrollment, MacAllowlist, NoDeviceState, Posted,
};
use crate::mdm_setup::{MdmSetup, SetupFailure};

/// How long a command waits for the Mac's answer before it is queued again, in seconds.
pub const DEFAULT_COMMAND_TIMEOUT_S: u64 = 86_400;

/// How often the queue looks at what is due, in seconds.
pub const DEFAULT_QUEUE_SWEEP_S: u64 = 60;

/// The directory of the gate's device records in the enrollment store.
pub const DEVICES_DIR: &str = "mdm-devices";

/// Where the gate's device records are kept.
pub enum Devices<'a> {
    /// A file per device in [`DEVICES_DIR`] under the file store's enrollment store, this
    /// directory (`--enrollment-dir`), made when missing.
    Files(&'a Path),
    /// A store the coordinator opened, the database's (`--database-dsn-file`).
    Store(Box<dyn DeviceStore>),
}

/// The flags of the MDM policy gate: NanoHUB's three, all or none, and the listener of the Macs.
#[derive(Clone, Debug, Args)]
#[command(group(clap::ArgGroup::new("mdm_gate").args(["mdm_api_url", "mdm_listen"]).multiple(true)))]
pub struct MdmFlags {
    /// The MDM listener of the Macs and of a host's apt: the address of one interface of the LAN
    /// (no wildcard) and its port, as `192.0.2.7:443`. It serves `PUT /checkin` and `PUT /mdm`
    /// over TLS with the certificate of the MDM name that the setup holds
    /// (`docs/design/coordinator-mdm.md` section 4.1), and answers nothing until that certificate
    /// is stored. With it the commands of the gate are queued in the coordinator and the Macs'
    /// check-ins reach the gate directly: NanoHUB, its flags and its webhook are not used. The
    /// port is the default 443 when it is omitted from the address by the held script that sets it.
    /// Requires the enrollment store, as the gate does.
    #[arg(long, requires_all = ["ca_key_file", "records_store"])]
    pub mdm_listen: Option<SocketAddr>,
    /// How long, in seconds, a command sent to a Mac that has not answered waits before it is
    /// queued again and pushed (`docs/design/coordinator-mdm.md` section 5.2). Delivery is at
    /// least once: a Mac that answers later sees the command twice.
    #[arg(
        long,
        default_value_t = DEFAULT_COMMAND_TIMEOUT_S,
        value_parser = clap::value_parser!(u64).range(1..),
        requires = "mdm_listen"
    )]
    pub mdm_command_timeout_s: u64,
    /// How often, in seconds, the coordinator looks for commands whose timeout passed and for
    /// pushes whose floor elapsed.
    #[arg(
        long,
        default_value_t = DEFAULT_QUEUE_SWEEP_S,
        value_parser = clap::value_parser!(u64).range(1..),
        requires = "mdm_listen"
    )]
    pub mdm_queue_sweep_s: u64,
    /// NanoHUB's API, on loopback: `http://<loopback address>:<port>`, as
    /// `http://127.0.0.1:9004`. With it the coordinator runs the MDM policy gate: it keeps a
    /// record per MDM device and answers whether a Mac is enrolled and supervised, so a Mac on
    /// --mac-serial-allowlist can be approved by its nonce. Its records are kept with the
    /// coordinator's other state: in --enrollment-dir, or in the database of
    /// --database-dsn-file. Requires --mdm-api-key-file, --mdm-webhook-listen and
    /// --ca-key-file, with --enrollment-dir or --database-dsn-file.
    #[arg(
        long,
        requires_all = ["mdm_api_key_file", "mdm_webhook_listen", "ca_key_file", "records_store"]
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
    #[arg(long, requires_all = ["mdm_gate", "mdm_daemon_config"])]
    pub mdm_profile_identifier: Option<String>,
    /// The daemon config the config profile carries: a `hostkeeper.toml` of the farm
    /// (coordinator, ca_sha256, bounds, and release_channel and trusted_signers, which the
    /// loader still requires and the daemon no longer reads), checked at start by the daemon's
    /// own loader. It holds no secret. Requires --mdm-profile-identifier.
    #[arg(long, requires = "mdm_profile_identifier")]
    pub mdm_daemon_config: Option<PathBuf>,
    /// The stores the config profile carries, as the domain `dev.hostkeeper.stores`: the keys of
    /// a `stores.toml`, checked at start by the daemon's own loader. The daemon replaces its
    /// stores.toml from it at its start. It holds no secret: a store that names a credential
    /// file (`basic_auth_file`, `bearer_file`) is refused, since the file is not on the Mac.
    /// Requires --mdm-profile-identifier.
    #[arg(long, requires = "mdm_profile_identifier")]
    pub mdm_stores: Option<PathBuf>,
    /// The macos-pkg signers the config profile carries, as the domain
    /// `dev.hostkeeper.macos-pkg`: the key of a `macos-pkg.toml` (`signers`, 1 to 4 SHA-256
    /// fingerprints of leaf certificates), checked at start by the daemon's own loader. The
    /// daemon replaces its macos-pkg.toml from it at its start. Requires
    /// --mdm-profile-identifier.
    #[arg(long, requires = "mdm_profile_identifier")]
    pub mdm_macos_pkg: Option<PathBuf>,
    /// The pkg allowlist: a TOML file of root's that no one else can write, of `[[pkg]]`
    /// entries, each the pkg's `sha256`, its `version` and its `path` (`<sha256>.pkg` in the
    /// MDM kit's pkg directory). With it the gate sends each Mac on --mac-serial-allowlist the
    /// newest pkg on the list by `InstallEnterpriseApplication`, once the Mac has its config
    /// profile and until its daemon has posted, again an hour after the Mac's last answer.
    /// It is the old form of the setting `installs` (the MDM setup's named installs): read once,
    /// only when that setting was never set, as its install named `hostkeeper`; after that the
    /// setting rules and the file is not read. Requires --mdm-profile-identifier and --mdm-host.
    #[arg(long, requires_all = ["mdm_profile_identifier", "mdm_host"])]
    pub mdm_pkg_allowlist: Option<PathBuf>,
    /// The MDM host's DNS name, as its proxy's certificate names it, as `mdm.example.com`:
    /// the Mac fetches a pkg from `https://<this name>/pkg/<sha256>.pkg`. With it the gate
    /// sends the installs of the setting `installs`: the install named `hostkeeper` as above,
    /// and each other one to the Macs of a host group that has a baseline. Requires
    /// --mdm-profile-identifier.
    #[arg(long, requires = "mdm_profile_identifier")]
    pub mdm_host: Option<String>,
    /// The enrollment baseline: a TOML file of root's that no one else can write, of `[[group]]`
    /// rows, each a `host_group` and `[[group.entry]]` rows, an entry being `remote_desktop =
    /// true` (Screen Sharing, `EnableRemoteDesktop`, sent only to a supervised Mac) or `profile
    /// = "<path>"` (an unsigned `.mobileconfig` of root's, installed by `InstallProfile`). A
    /// Mac's host group is the role of its --mac-serial-allowlist entry, and each `host_group`
    /// must be the role of some entry. The gate sends the group's entries once the Mac has
    /// acknowledged its config profile, and sends again what a `ProfileList` check finds
    /// missing; a failure never fails the enrollment, and `Mdm.ListDevices` shows it. Read at
    /// start. Requires --mdm-profile-identifier and --mac-serial-allowlist.
    #[arg(long, requires_all = ["mdm_profile_identifier", "mac_serial_allowlist"])]
    pub mdm_baseline_file: Option<PathBuf>,
    /// How often, in seconds, a Mac's baseline is checked, and `EnableRemoteDesktop` sent again
    /// (the Mac answers no read-back of it): at least 1. A Mac is also checked at each of its
    /// `TokenUpdate`s. Requires --mdm-baseline-file.
    #[arg(
        long,
        requires = "mdm_baseline_file",
        default_value_t = DEFAULT_CHECK_SECS,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub mdm_baseline_check_s: u64,
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
    /// A file of `--mdm-daemon-config`, `--mdm-stores` or `--mdm-macos-pkg` cannot be read.
    #[error("{flag} {path}: {source}")]
    ConfigRead {
        /// The flag.
        flag: &'static str,
        /// The file.
        path: String,
        /// What the system said.
        #[source]
        source: std::io::Error,
    },
    /// `--mdm-daemon-config` is refused by the daemon's loader.
    #[error(transparent)]
    Config(#[from] DaemonProfileError),
    /// `--mdm-stores` is refused: a credential file, or the daemon's loader refuses it.
    #[error(transparent)]
    Stores(#[from] StoresProfileError),
    /// `--mdm-macos-pkg` is refused by the daemon's loader.
    #[error(transparent)]
    MacosPkg(#[from] MacosPkgProfileError),
    /// `--mdm-pkg-allowlist` is refused.
    #[error(transparent)]
    PkgAllowlist(#[from] PkgAllowlistError),
    /// `--mdm-host` is not a DNS name.
    #[error(transparent)]
    Host(#[from] MdmHostError),
    /// The setting `installs` does not parse: the store was edited by hand.
    #[error(transparent)]
    Installs(#[from] InstallsError),
    /// The MDM setup's settings cannot be read, or the installs cannot be stored.
    #[error(transparent)]
    Setup(#[from] SetupFailure),
    /// `--mdm-baseline-file` is refused.
    #[error(transparent)]
    Baseline(#[from] BaselineError),
    /// `--mdm-baseline-file` names a host group no entry of `--mac-serial-allowlist` has.
    #[error(
        "--mdm-baseline-file names host group {0}, which no entry of --mac-serial-allowlist has (a Mac's host group is its entry's role)"
    )]
    BaselineGroup(String),
}

/// The running gate, and where its webhook is served.
#[derive(Debug)]
pub struct Mdm {
    /// The gate.
    pub gate: Arc<Gate>,
    /// Where NanoHUB's webhook is served; none when the gate's commands go through the
    /// coordinator's own queue (`--mdm-listen`), where the Macs' check-ins reach the gate directly.
    pub webhook_listen: Option<SocketAddr>,
    /// The enrollment the gate's enrollment side asks, once attached; `None` without the side.
    attached: Option<Arc<Attached>>,
}

impl Mdm {
    /// The gate the flags name, its records in `devices`, its times from `clock` and its
    /// command UUIDs from `random`; `None` without the flags, and then `devices` is not opened.
    ///
    /// # Errors
    /// [`MdmError`]: the URL is not loopback HTTP, the key file is refused, the webhook address
    /// is not loopback, or a device record does not read.
    pub fn open(
        flags: &MdmFlags,
        devices: Devices<'_>,
        clock: Arc<dyn UnixClock>,
        random: Random,
        setup: &MdmSetup,
    ) -> Result<Option<Self>, MdmError> {
        Self::open_with(flags, devices, clock, random, None, setup)
    }

    /// [`Self::open`], with the gate's commands queued in `queue` (the coordinator's own, with
    /// `--mdm-listen`) in place of NanoHUB's: the NanoHUB flags are not read, and no webhook is
    /// served. `None` is [`Self::open`].
    ///
    /// # Errors
    /// As [`Self::open`].
    pub fn open_with(
        flags: &MdmFlags,
        devices: Devices<'_>,
        clock: Arc<dyn UnixClock>,
        random: Random,
        queue: Option<Arc<dyn CommandQueue>>,
        setup: &MdmSetup,
    ) -> Result<Option<Self>, MdmError> {
        let (queue, webhook_listen): (Arc<dyn CommandQueue>, Option<SocketAddr>) = match queue {
            Some(queue) => (queue, None),
            None => {
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
                (Arc::new(NanoHub::new(url, key)), Some(listen))
            }
        };
        let side = side_of(flags)?;
        let install = install_of(flags, Arc::new(RealFs), setup)?;
        let baseline = baseline_of(
            flags,
            &RealFs,
            side.as_ref().map(|(identifier, _)| identifier),
        )?;
        let store: Box<dyn DeviceStore> = match devices {
            Devices::Files(dir) => Box::new(FileDeviceStore::open(
                Arc::new(RealFs),
                &dir.join(DEVICES_DIR),
            )?),
            Devices::Store(store) => store,
        };
        let gate = Gate::open(store, queue, clock, random)?
            .with_install(install)
            .with_baseline(baseline);
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
            webhook_listen,
            attached,
        }))
    }

    /// Refuses the baseline of `mdm`, when it has one, if it lists a host group that is no
    /// entry's role in `allowlist`: the group would never be picked, which is a mistake in
    /// either file (a Mac's host group is its allowlist entry's role).
    ///
    /// # Errors
    /// [`MdmError::BaselineGroup`], naming the first such group.
    pub fn check_baseline_groups(
        mdm: Option<&Self>,
        allowlist: &MacAllowlist,
    ) -> Result<(), MdmError> {
        let roles = allowlist.host_groups();
        let groups = mdm
            .map(|mdm| mdm.gate.baseline_groups())
            .unwrap_or_default();
        match groups
            .into_iter()
            .find(|group| !roles.contains(group.as_str()))
        {
            Some(unknown) => Err(MdmError::BaselineGroup(unknown)),
            None => Ok(()),
        }
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

    /// Has `gate` look at every device once a minute and at each wake
    /// ([`hostkeeper_mdm_gate::enroll::run`]), forever: the gate of the coordinator's own queue,
    /// which hears no webhook.
    pub async fn looks(gate: Arc<Gate>) -> Infallible {
        enroll::run(gate, enroll::TICK).await
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

/// The enrollment side's identifier and config profile, when its two flags are given: the
/// daemon config, with the stores and the signers when their flags are.
fn side_of(flags: &MdmFlags) -> Result<Option<(ProfileIdentifier, ConfigProfile)>, MdmError> {
    let (Some(identifier), Some(config)) =
        (&flags.mdm_profile_identifier, &flags.mdm_daemon_config)
    else {
        // clap requires the two together: without them there is no enrollment side.
        return Ok(None);
    };
    let identifier = ProfileIdentifier::parse(identifier)?;
    let mut profile = ConfigProfile::new(DaemonProfile::from_toml(&text_of(CONFIG_FLAG, config)?)?);
    if let Some(path) = &flags.mdm_stores {
        profile = profile.with_stores(StoresProfile::from_toml(&text_of(STORES_FLAG, path)?)?);
    }
    if let Some(path) = &flags.mdm_macos_pkg {
        let text = text_of(MACOS_PKG_FLAG, path)?;
        profile = profile.with_macos_pkg(MacosPkgProfile::from_toml(&text)?);
    }
    Ok(Some((identifier, profile)))
}

/// The text of the file `path`, which the flag `flag` names.
fn text_of(flag: &'static str, path: &Path) -> Result<String, MdmError> {
    std::fs::read_to_string(path).map_err(|source| MdmError::ConfigRead {
        flag,
        path: path.display().to_string(),
        source,
    })
}

/// The pkg install, when `--mdm-host` is given: the installs of the setting `installs`. When the
/// setting was never set, the old file `--mdm-pkg-allowlist` names, if given, is read from `fs`
/// (it must be root's alone) and its newest pkg becomes the install `hostkeeper`, which is
/// stored as the setting: that is the one time the file is read, so a later start never opens
/// it, and a change of the setting is not undone by it.
fn install_of(
    flags: &MdmFlags,
    fs: Arc<dyn Fs>,
    setup: &MdmSetup,
) -> Result<Option<Install>, MdmError> {
    let Some(host) = &flags.mdm_host else {
        // Without the host there is no URL for a pkg: the gate sends none.
        return Ok(None);
    };
    let host = MdmHost::parse(host)?;
    let set = setup.settings()?;
    let installs = match set.get(SETTING_NAME) {
        Some(text) => Installs::parse(text)?,
        None => {
            let seeded = match &flags.mdm_pkg_allowlist {
                Some(path) => PkgAllowlist::load(&*fs, path)?.installs(),
                None => Installs::empty(),
            };
            if flags.mdm_pkg_allowlist.is_some() {
                setup.set_setting(SETTING_NAME, &seeded.to_json())?;
            }
            seeded
        }
    };
    Ok(Some(Install::new(installs, host, fs)))
}

/// The enrollment baseline, when `--mdm-baseline-file` is given: the file read from `fs`, which
/// must find it and its profiles root's alone, checked every `--mdm-baseline-check-s` seconds.
fn baseline_of(
    flags: &MdmFlags,
    fs: &dyn Fs,
    identifier: Option<&ProfileIdentifier>,
) -> Result<Option<Baseline>, MdmError> {
    let (Some(path), Some(identifier)) = (&flags.mdm_baseline_file, identifier) else {
        // clap requires the identifier with the file: without the two there is no baseline.
        return Ok(None);
    };
    Ok(Some(
        Baseline::load(fs, path, identifier)?.with_check_secs(flags.mdm_baseline_check_s),
    ))
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

    fn host_group(&self, serial: &str) -> Option<String> {
        self.get()
            .ok()
            .and_then(|enrollment| enrollment.host_group(serial))
    }

    fn host_name(&self, serial: &str) -> Option<String> {
        self.get()
            .ok()
            .and_then(|enrollment| enrollment.host_name(serial))
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
                enrolled_via_dep: status.enrolled_via_dep,
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
