//! `hostkeeper-coordinator serve`'s flags and start-up: read every file, bind the listeners,
//! print the start line, then serve until SIGTERM or SIGINT. The binary's other verbs: `backup`
//! and `restore` copy the state files `serve` keeps and put a copy back ([`crate::backup`]),
//! under the same lock; `export` writes the state the database holds into files of the file
//! store ([`crate::transfer`]).
//!
//! Everything is a flag; the coordinator reads no environment variable. `serve --config <file>`
//! takes flags from a TOML file, put in place of it before the arguments are parsed
//! ([`config_file`]). Without
//! `--enrollment-dir` it holds no CA key: its certificate and key are files the farm CA issued,
//! and the CA certificate it checks updaters against is a file too. With it, it is also the
//! hosts' CA ([`crate::enrollment`]): `--ca-key-file` is the CA's key (made with `--ca-init`),
//! the heartbeat listener serves the server certificate the start keeps in `--server-cert-file`
//! for the names of `--server-name` (and the reload tick of `--tls-reload-secs` renews while
//! the coordinator runs), lets a client without a certificate through the handshake,
//! and admits a client certificate of its own CA as well as one of `--client-ca`. The handlers
//! of `Enroll`, `Poll` and `Renew` are served as gRPC on the heartbeat listener
//! ([`crate::grpc`]). With enrollment, `--lan-listen` (the address of one interface, which the
//! server certificate names) serves what the heartbeat listener serves, with its TLS, to hosts
//! on the farm LAN. It places hosts by host group (`HostGroups`), whether or not it holds a group
//! yet, and reads no rollout file: `--rollout-file` is refused ([`StartError::RolloutFileRetired`]);
//! `--legacy-service` names the one whose heartbeat is the top level of a heartbeat;
//! the operator's tokens are those of `--tokens-file`, or the one token of
//! `--operator-token-file`. The private listener (`--private-listen`) serves CI's calls and the
//! operator's; it takes the address of one interface, or with `--private-listen-wildcard` a
//! wildcard address. With `--ui-dir` it also serves the static
//! files of that directory under `/ui/` (`crate::ui`); without the flag, the binary serves
//! [`DEFAULT_UI_DIR`], where the container image holds the operator web app, when that path is
//! a directory ([`Serve::with_default_ui_dir`]).
//! With `--health-listen` (a loopback address) it also serves `GET /healthz` in plain HTTP to a
//! probe on the same machine (`crate::health`).
//! How the sockets are bound is a function the caller gives ([`bind_tcp`] in the binary), so
//! the whole start can be driven over in-memory connections.
//!
//! Before it opens any state file, the start takes the lock of `<state-file>.lock`
//! ([`crate::lock`]) and holds it for as long as the coordinator runs, so a second `serve` on
//! the same files is refused. It then refuses to start over a restore that stopped partway
//! (`<state-file>.restore-unfinished`, [`crate::backup`]), opens the state files as one backend
//! ([`crate::store::Files`]) under the three stores and the spent ids of CI's tokens
//! (`<state-file>.jti`, [`crate::jti_file`]), and settles the marker of changes that were in
//! memory and not yet written when the coordinator stopped (`<ops-file>.halts.unfinished`,
//! [`crate::ops::Marker`]): removed when the ops file holds every change it lists, a refusal to
//! start naming those it does not hold, and a refusal, as the release before made, for a marker
//! that lists nothing this build reads.

use std::convert::Infallible;
use std::ffi::OsString;
use std::io;
use std::net::SocketAddr;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use futures::future::{Either, select};
use hostkeeper_enrollment::ca::HOST_VALIDITY_SECS;
use hostkeeper_enrollment::enroll::DEFAULT_MAX_PENDING;
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::validity::SystemClock;
use hostkeeper_oidc::{AllowList, HttpsKeys, Roots, Verifier};
use hostkeeper_tls::{Alpn, ClientAuth, TlsFiles, TlsSource};
use hostkeeper_wire::identity::{
    DEFAULT_HOST_ROLE_URI_PREFIX, DEFAULT_ROLE_URI_PREFIX, SanPrefixes,
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::accounts::AccountStore;
use crate::alerts::{
    DEFAULT_CERT_WINDOW_SECS, DEFAULT_SILENT_HOST_SECS, OBSERVE_EVERY, Thresholds, observe_every,
};
use crate::audit_file::wall_clock;
use crate::backup::{Backup, Report, Restore, backup, restore};
use crate::desired::DesiredStore;
use crate::enrollment::{MacAllowlist, MacPolicy, Setup, StoreAt, os_random, setup};
use crate::fleet::{CLOCK_SKEW_AFTER, QUIET_START};
use crate::health::{self, Serving};
use crate::host_groups::GroupName;
use crate::http::Surface;
use crate::initial_admin::{LoginFlags, watch_every};
use crate::inventory::RecordWindow;
use crate::listen::{
    CLIENT_PATIENCE, Listener, MAX_CONNECTIONS, Remote, reload_every, serve, together,
};
use crate::lock::{FileLock, LockError, LockGuard, StateLock};
use crate::mdm::{Devices, Mdm, MdmFlags};
use crate::ops::OpsStore;
use crate::service::{Clock, Coordinator};
use crate::store::Spent;
use crate::transfer::{Export, export};
use crate::ui::UiDir;
use crate::verdict_file::VerdictFile;

#[path = "config_error.rs"]
mod error;

#[path = "config_file.rs"]
mod config_file;

pub use error::StartError;

/// The times the flags take, in seconds.
#[path = "config_secs.rs"]
mod secs;
use secs::{record_window_secs, request_ttl_secs};

/// The start's checks of the listen addresses.
#[path = "config_listen.rs"]
mod listen;

/// The addresses `serve` bound, the line it prints for them, and the socket it binds with.
#[path = "config_bound.rs"]
mod bound;
pub use bound::{Bound, bind_tcp, start_line};

/// The start's wiring of the MDM listener.
#[path = "config_mdm.rs"]
mod mdm_listener;

/// The start's check of the Mac serial allowlist against the host groups the store holds.
#[path = "config_mac.rs"]
mod mac;
use mac::check_allowlist;

/// The start's state files, opened as one backend once the markers a stop left beside them are
/// settled.
#[path = "config_state.rs"]
mod state;

#[cfg(test)]
pub(crate) use state::{
    refuse_unfinished_halt, refuse_unfinished_migrate, refuse_unfinished_restore, settle_unfinished,
};

/// How long the verifier waits before it fetches the issuer's keys again for an unknown `kid`.
const KEY_REFETCH: Duration = Duration::from_secs(30);

/// How long fetching the issuer's discovery document or keys may take.
const KEY_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The shortest `--host-cert-validity-secs`: a minute, so that half life (when the daemon
/// renews) is six of its 5-second steps away from the issuance.
pub const HOST_CERT_VALIDITY_FLOOR_SECS: u64 = 60;

/// `hostkeeper-coordinator`: tells each host's updater which version to run, each service by
/// its update plan, one service's rollout at a time.
#[derive(Clone, Debug, Parser)]
#[command(
    name = "hostkeeper-coordinator",
    version = crate::stamp::version()
)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The verbs of the binary.
#[derive(Clone, Debug, Subcommand)]
#[expect(
    clippy::large_enum_variant,
    reason = "parsed once per process from its arguments; boxing `Serve` would change every caller for no saving"
)]
pub enum Command {
    /// Serve the heartbeat and private listeners, the LAN listener when `--lan-listen` is
    /// given and the health listener when `--health-listen` is, until SIGTERM or SIGINT.
    Serve(Serve),
    /// Copy the state of a stopped coordinator into a new directory, with the SHA-256 of each
    /// file in its manifest. The CA's key is not copied.
    Backup(Backup),
    /// Put a copy `backup` made back, into targets that hold no state.
    Restore(Restore),
    /// Write the state the database of --database-dsn-file holds into files of the file store
    /// that hold no state, and the rows of each table.
    Export(Export),
}

/// The flags of `serve`.
#[derive(Clone, Debug, Args)]
// The store of the enrollment and the MDM gate's records: the directory or the database (which
// refuses the directory beside it, naming it, at start).
#[group(id = "records_store", args = ["enrollment_dir", "database_dsn_file"], multiple = true)]
pub struct Serve {
    /// A TOML file of flags of `serve`, each key a flag's name without `--` (a string or an
    /// integer is its value, `true` a flag with no value, an array the flag once per element),
    /// put in place of this flag before the others are parsed: a flag it gives may not be given
    /// again.
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// Where updaters dial (mutual TLS).
    #[arg(long)]
    pub heartbeat_listen: SocketAddr,
    /// The private listener (TLS): where CI sets the desired state and reads the convergence
    /// with its OIDC token, and where an operator reads the status and acts with a token. It
    /// takes the address of one interface (loopback, a tunnel or a management network): a
    /// wildcard address is refused at start, unless --private-listen-wildcard is given.
    // The name this flag had, `--control-listen`, is a hidden alias; giving both is refused. It
    // is not in the doc comment above because that is the flag's help, which does not show it.
    #[arg(long, alias = "control-listen")]
    pub private_listen: SocketAddr,
    /// Take a wildcard --private-listen (`0.0.0.0` or `[::]`): the private listener then
    /// listens on every interface of the coordinator's network namespace. Give it only in a
    /// namespace whose interfaces are loopback and one private network, such as a container's
    /// on one overlay network, where the address of that interface is not known before the
    /// container starts. No address is trusted: what needs a token without the flag needs it
    /// with the flag.
    /// An address of one interface is taken with it as without it.
    #[arg(long)]
    pub private_listen_wildcard: bool,
    /// A third listener, on the farm LAN, for network-installed hosts, which have no tailnet:
    /// it serves what the heartbeat listener serves (`Enroll` and `Poll` with no client
    /// certificate, the heartbeat and `Renew` with a host's) and never the operator API, the
    /// JSON gateway or the UI. The server certificate names its IP address, so a daemon dials it
    /// by address. It takes the address of one interface: a wildcard address is refused at
    /// start. Requires --ca-key-file.
    #[arg(long, requires = "ca_key_file")]
    pub lan_listen: Option<SocketAddr>,
    /// A health listener, plain HTTP with no TLS and no token, for a probe on this machine: it
    /// answers `GET` and `HEAD` of `/healthz` only, `200 ok` while every listener is bound and
    /// serving and the store takes writes, and `503` with one reason token otherwise
    /// (`starting`, `not_writer`, `store_unreachable`). It takes a loopback address
    /// (127.0.0.0/8 or ::1): any other, a wildcard among them, is refused at start. Without it
    /// there is no health listener.
    #[arg(long)]
    pub health_listen: Option<SocketAddr>,
    /// The coordinator's certificate chain (PEM), issued by the farm CA with the role
    /// `coordinator`.
    #[arg(long)]
    pub tls_cert: PathBuf,
    /// The coordinator's private key (PEM).
    #[arg(long)]
    pub tls_key: PathBuf,
    /// The farm CA certificate or bundle (PEM) an updater's certificate must chain to.
    #[arg(long)]
    pub client_ca: PathBuf,
    /// The desired-state file of the file store: written by the coordinator, read back at
    /// start. Its directory must be writable by the coordinator. Exactly one of this and
    /// `--database-dsn-file`.
    #[arg(long, required_unless_present = "database_dsn_file")]
    pub state_file: Option<PathBuf>,
    /// The Postgres store: a file holding one libpq connection string in keyword form
    /// (`host=127.0.0.1 port=5432 dbname=hostkeeper user=hostkeeper password=<secret>
    /// sslmode=disable`), a regular file owned by the coordinator's user, mode 0600 or 0400, at
    /// most 4 KiB. The database holds everything the file store's files hold. Not taken beside
    /// --state-file, --ops-file, --enrollment-dir, --audit-max-bytes or
    /// --audit-retained-segments. Requires --halt-marker-file.
    #[arg(long, requires = "halt_marker_file")]
    pub database_dsn_file: Option<PathBuf>,
    /// With --database-dsn-file: the file, on a local disk that stays writable when the database
    /// is not, that lists the changes that stand in memory and are not yet committed. A start
    /// that finds it removes it when the database holds every change it lists, and refuses
    /// otherwise.
    #[arg(long, requires = "database_dsn_file")]
    pub halt_marker_file: Option<PathBuf>,
    /// The allow-list of CI's OIDC token: a TOML file with a `[remote_operator]` table.
    #[arg(long)]
    pub oidc_config: PathBuf,
    /// The operator's tokens: a TOML file with a `[[token]]` table for each, with its `name` (1
    /// to 32 bytes of a-z, 0-9 and -) and the `sha256` of the token as lowercase hex, so the
    /// file holds no secret; mode 0600, owned by the coordinator's user. It is read again
    /// every `--tls-reload-secs`, so a token is revoked or replaced without a restart. Not
    /// beside `--operator-token-file`; one of the two or `--initial-admin-file` is required.
    #[arg(long)]
    pub tokens_file: Option<PathBuf>,
    /// A file holding one operator token, mode 0600, owned by the coordinator's user: the one
    /// token, named `operator`. Read once, at start. Not beside `--tokens-file`.
    #[arg(long)]
    pub operator_token_file: Option<PathBuf>,
    #[command(flatten)]
    pub login: LoginFlags,
    /// The ops file: what operators changed (exclusions, labels) and the audit ring, written
    /// by the coordinator and read back at start; created by the first change. Default: the
    /// state file's name with `.ops` added. Its directory must be writable by the
    /// coordinator. The audit file `<ops-file>.audit` is beside it.
    #[arg(long)]
    pub ops_file: Option<PathBuf>,
    /// The most bytes the audit file `<ops-file>.audit` holds before it is rotated: a line that
    /// would take it past this is written to a fresh file, and the full one is sealed as
    /// `<ops-file>.audit.<n>`. At least 1; 0 is refused. Default 16 MiB.
    #[arg(long)]
    pub audit_max_bytes: Option<NonZeroU64>,
    /// How many sealed segments of the audit file are kept: at each rotation the oldest beyond
    /// this are deleted. At least 1; 0 is refused. Default 8.
    #[arg(long)]
    pub audit_retained_segments: Option<NonZeroUsize>,
    /// Refused: `serve` reads no rollout file. The flag is accepted only to say so, at start,
    /// to an operator whose unit still names it; it is not in the help.
    #[arg(long, hide = true)]
    pub rollout_file: Option<PathBuf>,
    /// The service whose heartbeat is the top level of a heartbeat, which a desired-state body
    /// without `service` sets and an allow-list entry without `services` admits. It names the
    /// service of the single-state desired file. Without it only the `services` entries of a
    /// heartbeat count.
    #[arg(long)]
    pub legacy_service: Option<String>,
    /// The prefix of the certificate URI that names a caller's role (`<prefix>updater`).
    #[arg(long, default_value = DEFAULT_ROLE_URI_PREFIX)]
    pub role_uri_prefix: String,
    /// The prefix of the certificate URI that names the role of the caller's host.
    #[arg(long, default_value = DEFAULT_HOST_ROLE_URI_PREFIX)]
    pub host_role_uri_prefix: String,
    /// How often the TLS files and the tokens file are looked at again, in seconds.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
    pub tls_reload_secs: u64,
    /// The enrollment store of the file store: a directory of the coordinator's own holding the
    /// pending requests, the issued certificates, the approved hosts and the deny rows. With it
    /// (or with --database-dsn-file and --ca-key-file, the store then the database's) the
    /// coordinator is the hosts' CA: the heartbeat listener serves `--server-cert-file`, lets a
    /// client without a certificate through the handshake (each route then decides; only
    /// `Enroll` and `Poll` will answer one), and admits a host certificate of its CA.
    /// Requires --ca-key-file, --server-cert-file and --server-name.
    #[arg(long, requires_all = ["ca_key_file", "server_cert_file", "server_names"])]
    pub enrollment_dir: Option<PathBuf>,
    /// The CA's key: PKCS#8, mode 0600, owned by the coordinator's user (a file open to
    /// group or others, or a symbolic link, is refused). Its certificate is beside it, the same
    /// path with `.cert` added. Back both up once, sealed and offline. Turns enrollment on;
    /// requires --enrollment-dir or --database-dsn-file, --server-cert-file and --server-name.
    #[arg(long, requires_all = ["server_cert_file", "server_names"])]
    pub ca_key_file: Option<PathBuf>,
    /// Make the CA when --ca-key-file does not exist; without it a missing CA stops the start.
    /// The CA is made once: every daemon config pins its certificate's SHA-256.
    #[arg(long, requires = "ca_key_file")]
    pub ca_init: bool,
    /// The coordinator's server certificate, the CA certificate and its key, one PEM file
    /// (mode 0600) the coordinator writes: made at start when missing, within the last third
    /// of its validity, or naming other names than --server-name and --lan-listen.
    #[arg(long, requires = "ca_key_file")]
    pub server_cert_file: Option<PathBuf>,
    /// A DNS name of the coordinator, as daemons dial it: the server certificate names each
    /// one given, and the address of --lan-listen besides. A certificate that names other
    /// names than these is made again at start and at the reload tick.
    #[arg(long = "server-name", requires = "ca_key_file")]
    pub server_names: Vec<String>,
    /// How many enrollment requests may wait for an operator at once (default 256). A new
    /// request over it is refused `pending_full`; no waiting request is dropped for it, and a
    /// host posting its waiting request again is still answered.
    #[arg(long, requires = "ca_key_file", value_parser = clap::value_parser!(u64).range(1..))]
    pub max_pending_enrollments: Option<u64>,
    /// How long each host certificate the CA issues or renews is valid, in seconds: from 60
    /// (the daemon renews at half life, and looks every 5 seconds) to 604800, the CA's cap of 7
    /// days, which is the default. A certificate already issued keeps its validity; the next
    /// issuance or renewal takes the new one.
    #[arg(
        long,
        default_value_t = HOST_VALIDITY_SECS,
        requires = "ca_key_file",
        value_parser = clap::value_parser!(u64).range(HOST_CERT_VALIDITY_FLOOR_SECS..=HOST_VALIDITY_SECS)
    )]
    pub host_cert_validity_secs: u64,
    /// How long an enrollment request waits for an operator before it expires: a whole number
    /// and a unit, `s`, `m`, `h` or `d` (`90m`, `72h`), at least one second. An expired request
    /// is no longer approved, and its host is listed unenrolled; the host's key posted again
    /// waits anew. Requests are looked at every --tls-reload-secs.
    #[arg(long, default_value = "72h", requires = "ca_key_file", value_parser = request_ttl_secs)]
    pub enrollment_request_ttl: u64,
    /// How long the approval window of a new inventory record that names a role lasts when
    /// `Enrollment.AddRecord` names none: a whole number and a unit, `s`, `m`, `h` or `d`
    /// (`90m`, `24h`), at least one second, and no longer than --record-approve-max. While the
    /// window is open a pending request that matches the record exactly is approved by the
    /// coordinator itself. Needs no --enrollment-dir.
    #[arg(long, default_value = "24h", value_parser = record_window_secs)]
    pub record_approve_for: u64,
    /// The longest approval window an inventory record may be given, `Enrollment.AddRecord` and
    /// `Enrollment.OpenRecord` alike: a whole number and a unit, `s`, `m`, `h` or `d` (`7d`),
    /// at least one second. A longer window is refused `invalid_window`.
    #[arg(long, default_value = "7d", value_parser = record_window_secs)]
    pub record_approve_max: u64,
    /// The Mac serial numbers a Mac may be approved for by its MDM nonce, with the host id
    /// prefix and host group of each: a TOML file of `[[mac]]` entries (`serial`,
    /// `host_id_prefix`, `host_group`; an entry written before groups names a `role`, read as
    /// the group, and `services`, which a coordinator that holds a host group refuses at start).
    /// Without it no Mac is approved by its nonce.
    #[arg(long, requires = "ca_key_file")]
    pub mac_serial_allowlist: Option<PathBuf>,
    /// The MDM policy gate: NanoHUB's API, its key and the webhook listener.
    #[command(flatten)]
    pub mdm: MdmFlags,
    /// The file that holds the key the MDM server's secrets (the push key, the ABM token and the
    /// rest of the setup, `docs/design/coordinator-mdm.md` section 4.9) are sealed under in the
    /// store: 64 hex digits, a regular file of the coordinator's user, mode 0600 or 0400. The
    /// coordinator makes it (mode 0600) when it is not there and the store holds no sealed
    /// secret. A file that is not there, or no flag, with sealed secrets in the store, and a key
    /// that opens none of them, refuse the start: a new key would orphan every secret. Keep the
    /// file apart from the store's backups. Without the flag the coordinator keeps no MDM secret
    /// and `Mdm.PutSecret` is refused `secrets_unavailable`.
    #[arg(long)]
    pub mdm_secrets_key_file: Option<PathBuf>,
    /// A directory of static files (the operator web app) that the private listener serves
    /// under `/ui/`, to `GET` and `HEAD` only: `/ui/` is its `index.html`, and only files ending
    /// in .html, .css, .js, .json, .svg, .png or .ico of at most 4 MiB inside it are served.
    /// The files need no token; the operator API they call does. It must be a directory at
    /// start. Without it, /usr/share/hostkeeper-coordinator/ui is served when it is a
    /// directory (the container image holds the web app there), and otherwise no file is
    /// served.
    #[arg(long)]
    pub ui_dir: Option<PathBuf>,
    /// How long after its start the coordinator orders nothing, in seconds, so that it hears
    /// the fleet first and knows which hosts hold an update slot. At least two heartbeat
    /// intervals (30); a lower value is refused at start.
    #[arg(long, default_value_t = QUIET_START.as_secs())]
    pub quiet_secs: u64,
    /// How far, in seconds, the time a host's heartbeat says may be from the coordinator's wall
    /// clock, either way, before the heartbeat counts as skewed: `Hosts.List` and `Hosts.Get`
    /// answer each host's count since the start (`clock_skew_beats`). Nothing else reads it.
    #[arg(long, default_value_t = CLOCK_SKEW_AFTER.as_secs())]
    pub clock_skew_secs: u64,
    /// How many services may have a live rollout at once, 1 to 16: a service whose rollout is
    /// not live (none of its hosts updating or baking, and its update plan not under way) orders
    /// no host while this many others are.
    #[arg(
        long,
        default_value_t = 1,
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=16)
    )]
    pub live_rollouts: usize,
    /// How long a host the coordinator has heard from may go without a heartbeat before it is an
    /// alert (`silent_host`), in seconds.
    #[arg(
        long,
        default_value_t = DEFAULT_SILENT_HOST_SECS,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub alert_silent_host_secs: u64,
    /// How long before the certificate the coordinator serves its hosts expires it is an alert
    /// (`cert_near_expiry`), in seconds; 0 alerts only once it has expired.
    #[arg(long, default_value_t = DEFAULT_CERT_WINDOW_SECS)]
    pub alert_cert_window_secs: u64,
}

/// The directory the binary serves under `/ui/` when `--ui-dir` is absent and this path is a
/// directory: where the coordinator's container image holds the operator web app
/// (`packaging/container/Dockerfile`).
pub const DEFAULT_UI_DIR: &str = "/usr/share/hostkeeper-coordinator/ui";

impl Serve {
    /// These flags with `--ui-dir` set to `default` when the flag is absent and `default` is a
    /// directory (a symbolic link to one included). A `--ui-dir` that was given is kept as
    /// given, a directory or not, so [`build`] still refuses one that is not. Anything else at
    /// `default` (nothing, a file, a path that cannot be read) leaves the flag absent, and no
    /// file is served.
    #[must_use]
    pub fn with_default_ui_dir(mut self, default: &Path) -> Self {
        if self.ui_dir.is_none() && default.is_dir() {
            self.ui_dir = Some(default.to_owned());
        }
        self
    }
}

/// What a running coordinator is made of.
pub struct Parts {
    /// The coordinator.
    pub coordinator: Arc<Coordinator>,
    /// The heartbeat listener's TLS (client certificates required).
    pub heartbeat_tls: TlsSource,
    /// The private listener's TLS.
    pub control_tls: TlsSource,
    /// Where to bind the heartbeat listener.
    pub heartbeat_listen: SocketAddr,
    /// Where to bind the private listener.
    pub control_listen: SocketAddr,
    /// Where to bind the LAN listener, when there is one. It serves the heartbeat listener's
    /// TLS and surface.
    pub lan_listen: Option<SocketAddr>,
    /// Where to bind the listener of the Macs (`--mdm-listen`), when there is one.
    pub mdm_listen: Option<SocketAddr>,
    /// Where to bind the health listener (`--health-listen`, a loopback address), when there
    /// is one.
    pub health_listen: Option<SocketAddr>,
    /// How often to look at the TLS files and the tokens file.
    pub reload: Duration,
    /// The lock of the state files, held for as long as the coordinator runs: dropping the
    /// parts lets it go.
    pub lock: LockGuard,
}

/// Reads every file the flags name and builds what runs, under the lock of the state files
/// ([`FileLock`]). Fetches nothing: the issuer's keys are fetched on the first token.
///
/// # Errors
/// [`StartError`]: a file is missing, unreadable or refused (the text names the flag or the
/// file; the ops file must be one this build reads), `--ui-dir` is missing or not a directory,
/// `--private-listen` (without `--private-listen-wildcard`) or `--lan-listen` is a wildcard
/// address, `--health-listen` is not a
/// loopback address, `--quiet-secs` is below two
/// heartbeat intervals,
/// another coordinator holds the lock of the state files, a halt was being written when the
/// coordinator stopped, both token flags or neither is given, an allow-list entry names no
/// services with no legacy service set or names a host group by a name that is no group name,
/// an entry of the Mac allowlist names services or a group the store does not hold,
/// `--rollout-file` is given (it is not read: hosts are placed by the `HostGroups` calls), the two URI
/// prefixes are refused, or the MDM gate's flags or device records are
/// ([`crate::mdm::MdmError`]).
pub fn build(flags: &Serve, roots: Roots) -> Result<Parts, StartError> {
    build_with(flags, roots, &FileLock)
}

/// [`build`], with the lock of the state files taken by `lock`.
///
/// # Errors
/// [`StartError`], as [`build`].
pub fn build_with(flags: &Serve, roots: Roots, lock: &dyn StateLock) -> Result<Parts, StartError> {
    if flags.rollout_file.is_some() {
        return Err(StartError::RolloutFileRetired);
    }
    listen::check_listen(flags)?;
    if flags.quiet_secs < QUIET_START.as_secs() {
        return Err(StartError::QuietBelowFloor {
            secs: flags.quiet_secs,
            floor: QUIET_START.as_secs(),
        });
    }

    let record_window = RecordWindow::new(flags.record_approve_for, flags.record_approve_max)?;

    let tokens = state::operator_tokens(flags)?;
    let oidc = AllowList::from_file(&flags.oidc_config)?;
    let prefixes = SanPrefixes::new(&flags.role_uri_prefix, &flags.host_role_uri_prefix)?;
    let legacy = flags.legacy_service.as_deref();
    for (n, entry) in oidc.entries().iter().enumerate() {
        let names = entry.host_groups.iter().flatten();
        if let Some(name) = names.into_iter().find(|g| g.parse::<GroupName>().is_err()) {
            return Err(StartError::OidcEntryHostGroup {
                entry: n + 1,
                name: name.clone(),
            });
        }
        if entry.services.is_none() && legacy.is_none() {
            return Err(StartError::OidcEntryUnnamed(n + 1));
        }
    }
    let keys = HttpsKeys::new(&oidc.entries()[0].issuer, roots.clone(), KEY_FETCH_TIMEOUT)
        .map_err(StartError::Keys)?;
    // The wall clock is read for the validity of CI's tokens and the expiry of their spent
    // ids, and for nothing the coordinator decides.
    let oidc_clock = hostkeeper_oidc::system_clock();
    let state::Opened {
        backend,
        loaded,
        marker,
        held,
        records,
        devices,
        setup: setup_store,
        mdm: mdm_store,
    } = state::open_store(flags, legacy, lock, &oidc_clock)?;
    let mdm_setup = Arc::new(state::mdm_setup(flags, setup_store)?);
    let wiring = mdm_listener::wire(flags, &mdm_store, &mdm_setup);
    let verifier = Verifier::new(
        oidc,
        Arc::new(keys),
        KEY_REFETCH,
        Arc::clone(&oidc_clock),
        Arc::new(Spent::new(Arc::clone(&backend), loaded.spent, oidc_clock)),
    );
    let desired = DesiredStore::over(Arc::clone(&backend), loaded.desired).by_groups_only();
    let ops = OpsStore::over(Arc::clone(&backend), loaded.ops)
        .with_marker(marker)
        .with_record_window(record_window);
    let accounts = AccountStore::over(Arc::clone(&backend), loaded.accounts);
    let verdicts = VerdictFile::over(backend, loaded.verdicts);
    let ui = flags.ui_dir.as_deref().map(UiDir::new).transpose()?;
    let interval = Duration::from_secs(flags.tls_reload_secs);
    let files = |client_ca| TlsFiles {
        cert: flags.tls_cert.clone(),
        key: flags.tls_key.clone(),
        client_ca,
    };
    let origin = Instant::now();
    let clock: Clock = Arc::new(move || origin.elapsed());
    let mut coordinator = Coordinator::new(desired, Arc::new(verifier), tokens, clock)
        .with_legacy_service(legacy.map(str::to_owned))
        .with_quiet(Duration::from_secs(flags.quiet_secs))
        .with_live_rollouts(flags.live_rollouts)
        .with_clock_skew(Duration::from_secs(flags.clock_skew_secs))
        .with_prefixes(prefixes)
        .with_ops(ops)
        .with_verdicts(verdicts)
        .with_accounts(accounts)
        .with_ui(ui)
        .with_alert_thresholds(alert_thresholds(flags))
        .with_repository_origin(crate::repository_origin::RepositoryOrigin::new(roots)?);
    coordinator = state::with_initial_admin(coordinator, flags)?;
    coordinator = state::with_login(coordinator, flags)?;
    coordinator = coordinator
        .with_shared_mdm_setup(mdm_setup)
        .with_mdmcert(Arc::new(
            hostkeeper_mdm_push::mdmcert::HttpsTransport::production(),
        ));
    let store = match (&flags.enrollment_dir, records) {
        (Some(dir), _) => Some(StoreAt::Dir(dir)),
        (None, Some(records)) => Some(StoreAt::Records(records)),
        (None, None) if flags.ca_key_file.is_some() => return Err(StartError::NoEnrollmentStore),
        (None, None) => None,
    };
    let heartbeat_tls = match (store, &flags.ca_key_file, &flags.server_cert_file) {
        (Some(store), Some(ca_key), Some(server_cert)) => {
            let allowlist = flags
                .mac_serial_allowlist
                .as_deref()
                .map(MacAllowlist::load)
                .transpose()?
                .unwrap_or_default();
            check_allowlist(&allowlist, &coordinator)?;
            // The LAN listener's address, an IPv4 one written as IPv6 named as IPv4.
            let lan_ips: Vec<_> = flags
                .lan_listen
                .map(|lan| lan.ip().to_canonical())
                .into_iter()
                .collect();
            let max_pending = flags
                .max_pending_enrollments
                .map_or(DEFAULT_MAX_PENDING, |max| {
                    usize::try_from(max).unwrap_or(usize::MAX)
                });
            // The gate's device records are kept with the enrollment store: in its directory, or
            // in the database, opened with it when the gate's flags are given.
            let devices = match &store {
                StoreAt::Dir(dir) => Some(Devices::Files(dir)),
                StoreAt::Records(_) => devices.map(Devices::Store),
            };
            let mdm = match devices {
                Some(devices) => {
                    let queue = wiring.as_ref().map(mdm_listener::Wiring::queue);
                    Mdm::open_with(
                        &flags.mdm,
                        devices,
                        Arc::new(SystemClock),
                        os_random(),
                        queue,
                    )?
                }
                None => None,
            };
            Mdm::check_baseline_groups(mdm.as_ref(), &allowlist)?;
            let enrollment = setup(
                Arc::new(RealFs),
                &Setup {
                    store,
                    ca_key,
                    ca_init: flags.ca_init,
                    server_cert,
                    server_names: &flags.server_names,
                    server_ips: &lan_ips,
                },
                Arc::new(SystemClock),
                os_random(),
            )?
            .with_max_pending(max_pending)
            .with_request_ttl(flags.enrollment_request_ttl)
            .with_host_validity_secs(flags.host_cert_validity_secs)
            .with_host_role_uri_prefix(coordinator.prefixes().host_role())
            // The gate, when the coordinator runs it, says whether a Mac is enrolled and
            // supervised; without it no device is known.
            .with_mac(MacPolicy {
                device_state: Mdm::device_state(mdm.as_ref()),
                allowlist,
            });
            let enrollment = Mdm::wire(mdm.as_ref(), enrollment);
            let client = ClientAuth {
                optional: true,
                extra_roots: vec![enrollment.ca_bundle().certificate_der().to_vec()],
            };
            coordinator = coordinator.with_enrollment(enrollment).with_mdm(mdm);
            let served = TlsFiles {
                cert: server_cert.clone(),
                key: server_cert.clone(),
                client_ca: Some(flags.client_ca.clone()),
            };
            TlsSource::files_with("heartbeat", served, interval, Alpn::H2AndHttp1, client)?
        }
        // clap requires --ca-key-file, --server-cert-file and --server-name together.
        _ => TlsSource::files("heartbeat", files(Some(flags.client_ca.clone())), interval)?,
    };
    let (coordinator, mdm_listen) = mdm_listener::attach(wiring, coordinator);
    Ok(Parts {
        coordinator: Arc::new(coordinator),
        heartbeat_tls,
        control_tls: TlsSource::files("control", files(None), interval)?,
        heartbeat_listen: flags.heartbeat_listen,
        control_listen: flags.private_listen,
        lan_listen: flags.lan_listen,
        mdm_listen,
        health_listen: flags.health_listen,
        reload: interval,
        lock: held,
    })
}

/// The thresholds of the alerts `flags` name: `--alert-silent-host-secs` and
/// `--alert-cert-window-secs`.
pub(crate) fn alert_thresholds(flags: &Serve) -> Thresholds {
    Thresholds {
        silent_after: Duration::from_secs(flags.alert_silent_host_secs),
        cert_window_secs: flags.alert_cert_window_secs,
    }
}

/// The lock of `path`, taken by `lock`.
fn take_lock(lock: &dyn StateLock, path: &Path) -> Result<LockGuard, StartError> {
    lock.acquire(path).map_err(|why| match why {
        LockError::Held => StartError::Locked {
            path: path.to_owned(),
        },
        LockError::Io(source) => StartError::Lock {
            path: path.to_owned(),
            source,
        },
    })
}

/// Binds the listeners with `bind` (which answers the address bound and the function that
/// accepts connections): the health listener first when there is one, which from then on
/// answers `starting` until the rest are bound; then the heartbeat listener, the private one,
/// the LAN listener when there is one, and the MDM gate's webhook listener when the coordinator
/// runs the gate; calls `ready` with their addresses, and serves until the future is dropped.
/// The LAN listener serves the heartbeat listener's TLS and surface. It never returns `Ok`.
///
/// # Errors
/// An address cannot be bound; the store is deposed ([`StartError::Deposed`]).
pub async fn run<Bind, Binding, Accept, Next, Stream>(
    parts: Parts,
    bind: Bind,
    ready: impl FnOnce(Bound),
) -> Result<Infallible, StartError>
where
    Bind: Fn(SocketAddr) -> Binding,
    Binding: Future<Output = io::Result<(SocketAddr, Accept)>>,
    Accept: FnMut() -> Next + Send,
    Next: Future<Output = io::Result<Stream>> + Send,
    Stream: AsyncRead + AsyncWrite + Remote + Unpin + Send + 'static,
{
    run_as(parts, bind, ready, Serving::default()).await
}

/// [`run`], its health listener saying what `serving` holds ([`start`] drains it).
async fn run_as<Bind, Binding, Accept, Next, Stream>(
    parts: Parts,
    bind: Bind,
    ready: impl FnOnce(Bound),
    serving: Serving,
) -> Result<Infallible, StartError>
where
    Bind: Fn(SocketAddr) -> Binding,
    Binding: Future<Output = io::Result<(SocketAddr, Accept)>>,
    Accept: FnMut() -> Next + Send,
    Next: Future<Output = io::Result<Stream>> + Send,
    Stream: AsyncRead + AsyncWrite + Remote + Unpin + Send + 'static,
{
    let (health, probing) = match parts.health_listen {
        Some(addr) => {
            let (addr, accept) = bind(addr).await?;
            tracing::info!(%addr, "serving the health listener");
            let probing = health::serve(
                accept,
                Arc::clone(&parts.coordinator),
                serving.clone(),
                CLIENT_PATIENCE,
                MAX_CONNECTIONS,
            );
            (Some(addr), Either::Left(probing))
        }
        None => (None, Either::Right(std::future::pending())),
    };
    let rest = run_listeners(parts, bind, |bound| {
        ready(Bound { health, ..bound });
        serving.set();
    });
    match select(pin!(probing), pin!(rest)).await {
        Either::Left((never, _)) => match never {},
        Either::Right((ended, _)) => ended,
    }
}

/// [`run`] past the health listener: binds the other listeners, calls `ready` with their
/// addresses (`health` none), and serves them until the future is dropped or the store is
/// deposed.
async fn run_listeners<Bind, Binding, Accept, Next, Stream>(
    parts: Parts,
    bind: Bind,
    ready: impl FnOnce(Bound),
) -> Result<Infallible, StartError>
where
    Bind: Fn(SocketAddr) -> Binding,
    Binding: Future<Output = io::Result<(SocketAddr, Accept)>>,
    Accept: FnMut() -> Next + Send,
    Next: Future<Output = io::Result<Stream>> + Send,
    Stream: AsyncRead + AsyncWrite + Remote + Unpin + Send + 'static,
{
    // The lock of the state files is held for as long as this serves.
    let _held = parts.lock;
    let (heartbeat_addr, heartbeat_accept) = bind(parts.heartbeat_listen).await?;
    let (control_addr, control_accept) = bind(parts.control_listen).await?;
    let lan = match parts.lan_listen {
        Some(addr) => Some(bind(addr).await?),
        None => None,
    };
    let webhook = match parts.coordinator.mdm() {
        Some(mdm) => match mdm.webhook_listen {
            Some(listen) => {
                let (addr, accept) = bind(listen).await?;
                tracing::info!(%addr, "serving the MDM webhook");
                Either::Left(Mdm::serve(Arc::clone(&mdm.gate), accept))
            }
            None => Either::Right(Either::Left(Mdm::looks(Arc::clone(&mdm.gate)))),
        },
        None => Either::Right(Either::Right(std::future::pending())),
    };
    let macs = match parts.mdm_listen {
        Some(addr) => Some(bind(addr).await?),
        None => None,
    };
    ready(Bound {
        heartbeat: heartbeat_addr,
        control: control_addr,
        lan: lan.as_ref().map(|(addr, _)| *addr),
        mdm: macs.as_ref().map(|(addr, _)| *addr),
        health: None,
    });
    let lan = lan.map(|(_, accept)| Listener {
        accept,
        tls: parts.heartbeat_tls.clone(),
        surface: Surface::Heartbeat,
    });
    let heartbeat = Listener {
        accept: heartbeat_accept,
        tls: parts.heartbeat_tls.clone(),
        surface: Surface::Heartbeat,
    };
    let control = Listener {
        accept: control_accept,
        tls: parts.control_tls.clone(),
        surface: Surface::Control,
    };
    let tokens = parts.coordinator.tokens().clone();
    let enrollment = parts.coordinator.enrollment().cloned();
    let coordinator = parts.coordinator;
    let watching = Arc::clone(&coordinator);
    let observing = observe_every(Arc::clone(&coordinator), OBSERVE_EVERY);
    let adopting = watch_every(Arc::clone(&coordinator), parts.reload);
    // Without a LAN listener, a future that never finishes stands in for it.
    let serving_lan = match lan {
        Some(lan) => Either::Left(serve(
            lan,
            Arc::clone(&coordinator),
            CLIENT_PATIENCE,
            MAX_CONNECTIONS,
        )),
        None => Either::Right(std::future::pending()),
    };
    let serving_macs = match macs {
        Some((_, accept)) => Either::Left(mdm_listener::serve(accept, Arc::clone(&coordinator))),
        None => Either::Right(std::future::pending()),
    };
    let serving = together(
        serve(
            heartbeat,
            Arc::clone(&coordinator),
            CLIENT_PATIENCE,
            MAX_CONNECTIONS,
        ),
        serve(
            control,
            Arc::clone(&coordinator),
            CLIENT_PATIENCE,
            MAX_CONNECTIONS,
        ),
    );
    let reloading = reload_every(
        enrollment,
        vec![parts.heartbeat_tls, parts.control_tls],
        tokens,
        parts.reload,
    );
    let all = together(
        together(together(serving, serving_lan), serving_macs),
        together(reloading, together(observing, together(webhook, adopting))),
    );
    // A store another coordinator led while this one's link was lost stops the serve: its memory
    // is stale (`docs/design/coordinator-storage.md`, section 5).
    // On the heap: the futures together are too large for a stack of a test's thread.
    match select(Box::pin(all), pin!(watching.deposed())).await {
        Either::Left((never, _)) => match never {},
        Either::Right(((epoch, found), _)) => Err(StartError::Deposed { epoch, found }),
    }
}

/// Resolves on SIGTERM or SIGINT.
async fn signalled() -> io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    select(pin!(term.recv()), pin!(int.recv())).await;
    Ok(())
}

/// Runs the coordinator, its sockets bound by `bind` and the issuer's keys fetched trusting
/// `roots`, until `stop` resolves, which drains the health listener ([`health::drain_on`]).
///
/// # Errors
/// [`StartError`]: the start is refused ([`build`]), an address cannot be bound, or `stop`
/// failed.
pub async fn start<Bind, Binding, Accept, Next, Stream>(
    flags: Serve,
    roots: Roots,
    bind: Bind,
    stop: impl Future<Output = io::Result<()>>,
) -> Result<(), StartError>
where
    Bind: Fn(SocketAddr) -> Binding,
    Binding: Future<Output = io::Result<(SocketAddr, Accept)>>,
    Accept: FnMut() -> Next + Send,
    Next: Future<Output = io::Result<Stream>> + Send,
    Stream: AsyncRead + AsyncWrite + Remote + Unpin + Send + 'static,
{
    let parts = build(&flags, roots)?;
    let health = Serving::default();
    let stop = health::drain_on(stop, health.clone());
    let serving = run_as(
        parts,
        bind,
        |bound| println!("{}", start_line(&bound)),
        health,
    );
    match select(pin!(stop), pin!(serving)).await {
        Either::Left((stopped, _)) => stopped.map_err(StartError::from),
        Either::Right((served, _)) => {
            let Err(why) = served;
            Err(why)
        }
    }
}

/// Builds the runtime and runs the coordinator on it until a signal, serving
/// [`DEFAULT_UI_DIR`] when `--ui-dir` is absent and that path is a directory.
fn runtime(flags: Serve) -> Result<(), StartError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let flags = flags.with_default_ui_dir(Path::new(DEFAULT_UI_DIR));
    runtime.block_on(start(flags, Roots::System, bind_tcp, signalled()))
}

/// The binary: flags in, exit code out. Bad flags are exit 2; a failed start, backup, restore or
/// export is exit 1; SIGTERM or SIGINT, a backup made, and a restore or export done are exit 0.
/// `serve` without
/// `--ui-dir` serves [`DEFAULT_UI_DIR`] when it is a directory ([`Serve::with_default_ui_dir`]).
#[must_use]
pub fn main(args: Vec<OsString>) -> ExitCode {
    let args = match config_file::expand(args, config_file::read_file) {
        Ok(args) => args,
        Err(why) => {
            eprintln!("hostkeeper-coordinator: {why}");
            return ExitCode::from(2);
        }
    };
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(usage) => {
            let _ = usage.print();
            return ExitCode::from(if usage.use_stderr() { 2 } else { 0 });
        }
    };
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .try_init();
    match cli.command {
        Command::Serve(flags) => match runtime(flags) {
            Ok(()) => ExitCode::SUCCESS,
            Err(why) => {
                eprintln!("hostkeeper-coordinator: {why}");
                ExitCode::from(1)
            }
        },
        Command::Backup(flags) => finish(backup(&flags, &FileLock)),
        Command::Restore(flags) => finish(restore(&flags, &FileLock)),
        Command::Export(flags) => finish(export(&flags, &FileLock, &wall_clock())),
    }
}

/// The exit of a backup, a restore or an export: its report on standard output and
/// exit 0, or why it was refused on standard error and exit 1.
fn finish(done: Result<Report, impl std::fmt::Display>) -> ExitCode {
    match done {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("hostkeeper-coordinator: {why}");
            ExitCode::from(1)
        }
    }
}
