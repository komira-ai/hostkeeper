//! `hostkeeper-coordinator serve`'s flags and start-up: read every file, bind both listeners,
//! print the start line, then serve until SIGTERM or SIGINT.
//!
//! Everything is a flag; the coordinator reads no environment variable. Without
//! `--enrollment-dir` it holds no CA key: its certificate and key are files the farm CA issued,
//! and the CA certificate it checks updaters against is a file too. With it, it is also the
//! hosts' CA ([`crate::enrollment`]): `--ca-key-file` is the CA's key (made with `--ca-init`),
//! the heartbeat listener serves the server certificate the start keeps in `--server-cert-file`
//! for the names of `--server-name`, lets a client without a certificate through the handshake,
//! and admits a client certificate of its own CA as well as one of `--client-ca`. The handlers
//! of `Enroll`, `Poll` and `Renew` are served as gRPC on the heartbeat listener
//! ([`crate::grpc`]). The services it manages are the ones `--rollout-file` defines;
//! `--legacy-service` names the one whose heartbeat is the top level of a heartbeat;
//! the operator's tokens are those of `--tokens-file`, or the one token of
//! `--operator-token-file`. The private listener (`--private-listen`) serves CI's calls and the
//! operator's; it takes the address of one interface. With `--ui-dir` it also serves the static
//! files of that directory under `/ui/` (`crate::ui`).
//! How the sockets are bound is a function the caller gives ([`bind_tcp`] in the binary), so
//! the whole start can be driven over in-memory connections.
//!
//! Before it opens any state file, the start takes the lock of `<state-file>.lock`
//! ([`crate::lock`]) and holds it for as long as the coordinator runs, so a second `serve` on
//! the same files is refused. It then refuses to start over a halt that was being written when
//! the coordinator stopped (`<ops-file>.halts.unfinished`, [`crate::ops::Write::Halt`]), and
//! reads the spent ids of CI's tokens (`<state-file>.jti`, [`crate::jti_file`]).

use std::convert::Infallible;
use std::ffi::OsString;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use futures::future::{Either, select};
use hostkeeper_enrollment::enroll::DEFAULT_MAX_PENDING;
use hostkeeper_enrollment::fs::RealFs;
use hostkeeper_enrollment::validity::SystemClock;
use hostkeeper_oidc::{AllowList, ConfigError, HttpsKeys, KeySourceError, Roots, Verifier};
use hostkeeper_tls::{Alpn, ClientAuth, TlsError, TlsFiles, TlsSource};
use hostkeeper_wire::identity::{
    DEFAULT_HOST_ROLE_URI_PREFIX, DEFAULT_ROLE_URI_PREFIX, PrefixError, SanPrefixes,
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::desired::{DesiredStore, StoreError};
use crate::enrollment::{
    AllowlistError, MacAllowlist, MacPolicy, NoDeviceState, Setup, SetupError, os_random, setup,
};
use crate::fleet::QUIET_START;
use crate::http::Surface;
use crate::jti_file::{JtiError, JtiFile, jti_path};
use crate::listen::{CLIENT_PATIENCE, Listener, MAX_CONNECTIONS, accept_tcp, reload_every, serve};
use crate::lock::{FileLock, LockError, LockGuard, StateLock, lock_path};
use crate::ops::{OpsError, OpsStore};
use crate::service::{Clock, Coordinator};
use crate::services::{Services, ServicesError};
use crate::tokens::{OperatorTokens, TokensError};
use crate::ui::{UiDir, UiDirError};
use crate::verdict_file::{VerdictFile, VerdictsError, verdicts_path};

/// Why the coordinator did not start, for the operator.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The operator token file is refused.
    #[error("--operator-token-file: {0}")]
    OperatorToken(#[source] hostkeeper_wire::secret::SecretError),
    /// The tokens file is refused.
    #[error(transparent)]
    Tokens(#[from] TokensError),
    /// `--tokens-file` and `--operator-token-file` are both given.
    #[error(
        "--tokens-file and --operator-token-file are both given: name the operator's tokens one way"
    )]
    TokenFlags,
    /// Neither is given.
    #[error("one of --tokens-file and --operator-token-file is required")]
    NoTokenFlag,
    /// `--private-listen` is a wildcard address.
    #[error(
        "--private-listen: {0} is a wildcard address; the private listener takes CI's calls and the operator's, so name the address of one interface"
    )]
    PrivateListen(SocketAddr),
    /// The allow-list of CI's OIDC token is refused.
    #[error(transparent)]
    Oidc(#[from] ConfigError),
    /// The rollout file is refused.
    #[error("--rollout-file: {0}")]
    Services(#[from] ServicesError),
    /// The two certificate URI prefixes are refused.
    #[error("--role-uri-prefix and --host-role-uri-prefix: {0}")]
    Prefixes(#[from] PrefixError),
    /// `--legacy-service` names a service the rollout file does not define.
    #[error("--legacy-service names {0}, which --rollout-file does not define")]
    LegacyService(String),
    /// An allow-list entry names no services and there is no legacy service for it to stand for.
    #[error("--oidc-config: entry {0} names no services and --legacy-service is not set")]
    OidcEntryUnnamed(usize),
    /// An allow-list entry names a service the rollout file does not define.
    #[error(
        "--oidc-config: entry {entry} names the service {service}, which is not defined by --rollout-file"
    )]
    OidcEntryService {
        /// The entry, counting from 1.
        entry: usize,
        /// The service it names.
        service: String,
    },
    /// The OIDC key fetcher could not be made.
    #[error("the issuer's keys: {0}")]
    Keys(#[source] KeySourceError),
    /// The desired-state file is refused.
    #[error(transparent)]
    Desired(#[from] StoreError),
    /// The ops file is refused.
    #[error("--ops-file: {0}")]
    Ops(#[from] OpsError),
    /// A TLS file is refused.
    #[error(transparent)]
    Tls(#[from] TlsError),
    /// Enrollment did not start: its CA, its store or the server certificate.
    #[error(transparent)]
    Enrollment(#[from] SetupError),
    /// The Mac serial allowlist is refused.
    #[error(transparent)]
    MacAllowlist(#[from] AllowlistError),
    /// The Mac serial allowlist names a service the rollout file does not define.
    #[error("--mac-serial-allowlist names service {0}, which --rollout-file does not define")]
    MacService(String),
    /// `--ui-dir` does not name a directory.
    #[error("--ui-dir: {0}")]
    UiDir(#[from] UiDirError),
    /// A socket, the runtime or the signal handlers failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Another coordinator holds the lock of the state files.
    #[error(
        "{path}: another coordinator holds the lock of these state files; one coordinator runs on them at a time"
    )]
    Locked {
        /// The lock file.
        path: PathBuf,
    },
    /// The lock of the state files could not be taken.
    #[error("{path}: the lock of the state files could not be taken: {source}")]
    Lock {
        /// The lock file.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// A halt was being written to the ops file when the coordinator stopped, and may not have
    /// landed.
    #[error(
        "{path}: a halt was being written to the ops file when the coordinator stopped and may be lost; read the halts file, settle it by hand, and remove this file to start"
    )]
    HaltWriteUnfinished {
        /// The marker file.
        path: PathBuf,
    },
    /// `--quiet-secs` is below two heartbeat intervals.
    #[error(
        "--quiet-secs: {secs} is below {floor}, two heartbeat intervals: a restarted coordinator would order before it has heard the fleet"
    )]
    QuietBelowFloor {
        /// What the flag said.
        secs: u64,
        /// The floor.
        floor: u64,
    },
    /// The file of spent token ids is refused.
    #[error(transparent)]
    Jti(#[from] JtiError),
    /// The file of verdicts is refused.
    #[error(transparent)]
    Verdicts(#[from] VerdictsError),
}

/// How long the verifier waits before it fetches the issuer's keys again for an unknown `kid`.
const KEY_REFETCH: Duration = Duration::from_secs(30);

/// How long fetching the issuer's discovery document or keys may take.
const KEY_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// `hostkeeper-coordinator`: tells each host's updater which version to run, one live update at
/// a time.
#[derive(Clone, Debug, Parser)]
#[command(
    name = "hostkeeper-coordinator",
    version = concat!(env!("CARGO_PKG_VERSION"), "+", env!("HOSTKEEPER_BUILD_COMMIT"))
)]
pub struct Cli {
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The verbs of the binary.
#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Serve the heartbeat and private listeners until SIGTERM or SIGINT.
    Serve(Serve),
}

/// The flags of `serve`.
#[derive(Clone, Debug, Args)]
pub struct Serve {
    /// Where updaters dial (mutual TLS).
    #[arg(long)]
    pub heartbeat_listen: SocketAddr,
    /// The private listener (TLS): where CI sets the desired state and reads the convergence
    /// with its OIDC token, and where an operator reads the status and acts with a token. It
    /// takes the address of one interface (loopback, a tunnel or a management network): a
    /// wildcard address is refused at start.
    // The name this flag had, `--control-listen`, is a hidden alias; giving both is refused. It
    // is not in the doc comment above because that is the flag's help, which does not show it.
    #[arg(long, alias = "control-listen")]
    pub private_listen: SocketAddr,
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
    /// The desired-state file: written by the coordinator, read back at start. Its
    /// directory must be writable by the coordinator.
    #[arg(long)]
    pub state_file: PathBuf,
    /// The allow-list of CI's OIDC token: a TOML file with a `[remote_operator]` table.
    #[arg(long)]
    pub oidc_config: PathBuf,
    /// The operator's tokens: a TOML file with a `[[token]]` table for each, with its `name` (1
    /// to 32 bytes of a-z, 0-9 and -) and the `sha256` of the token as lowercase hex, so the
    /// file holds no secret; mode 0600, owned by the coordinator's user. It is read again
    /// every `--tls-reload-secs`, so a token is revoked or replaced without a restart. Exactly
    /// one of this and `--operator-token-file`.
    #[arg(long)]
    pub tokens_file: Option<PathBuf>,
    /// A file holding one operator token, mode 0600, owned by the coordinator's user: the one
    /// token, named `operator`. Read once, at start. Exactly one of this and `--tokens-file`.
    #[arg(long)]
    pub operator_token_file: Option<PathBuf>,
    /// The ops file: what operators changed (exclusions, labels) and the audit ring, written
    /// by the coordinator and read back at start; created by the first change. Default: the
    /// state file's name with `.ops` added. Its directory must be writable by the
    /// coordinator. The audit file `<ops-file>.audit` is beside it.
    #[arg(long)]
    pub ops_file: Option<PathBuf>,
    /// The rollout file: TOML with a `[[service]]` table for each service the coordinator
    /// manages, the roles it runs on and how its rollout goes (`docs/design/host-updates.md`).
    /// Required; an empty file defines none. Which of them a CI token may set is said by the
    /// allow-list entry that admits it (`--oidc-config`).
    // The name this flag had, `--services-file`, is a hidden alias; giving both is refused. It
    // is not in the doc comment above because that is the flag's help, which does not show it.
    #[arg(long, alias = "services-file")]
    pub rollout_file: PathBuf,
    /// The service whose heartbeat is the top level of a heartbeat, which a desired-state body
    /// without `service` sets and an allow-list entry without `services` admits. It must be
    /// defined by `--rollout-file`. Without it only the `services` entries of a heartbeat
    /// count.
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
    /// The enrollment store: a directory of the coordinator's own holding the pending
    /// requests, the issued certificates, the approved hosts and the deny rows. With it the
    /// coordinator is the hosts' CA: the heartbeat listener serves `--server-cert-file`, lets a
    /// client without a certificate through the handshake (each route then decides; only
    /// `Enroll` and `Poll` will answer one), and admits a host certificate of its CA.
    /// Requires --ca-key-file, --server-cert-file and --server-name.
    #[arg(long, requires_all = ["ca_key_file", "server_cert_file", "server_names"])]
    pub enrollment_dir: Option<PathBuf>,
    /// The CA's key: PKCS#8, mode 0600, owned by the coordinator's user (a file open to
    /// group or others, or a symbolic link, is refused). Its certificate is beside it, the same
    /// path with `.cert` added. Back both up once, sealed and offline.
    #[arg(long, requires = "enrollment_dir")]
    pub ca_key_file: Option<PathBuf>,
    /// Make the CA when --ca-key-file does not exist; without it a missing CA stops the start.
    /// The CA is made once: every daemon config pins its certificate's SHA-256.
    #[arg(long, requires = "ca_key_file")]
    pub ca_init: bool,
    /// The coordinator's server certificate, the CA certificate and its key, one PEM file
    /// (mode 0600) the coordinator writes: made at start when missing or within the last third
    /// of its validity.
    #[arg(long, requires = "enrollment_dir")]
    pub server_cert_file: Option<PathBuf>,
    /// A DNS name of the coordinator, as daemons dial it: the server certificate names each
    /// one given.
    #[arg(long = "server-name", requires = "enrollment_dir")]
    pub server_names: Vec<String>,
    /// How many enrollment requests may wait for an operator at once (default 256). A new
    /// request over it is refused `pending_full`; no waiting request is dropped for it, and a
    /// host posting its waiting request again is still answered.
    #[arg(long, requires = "enrollment_dir", value_parser = clap::value_parser!(u64).range(1..))]
    pub max_pending_enrollments: Option<u64>,
    /// The Mac serial numbers a Mac may be approved for by its MDM nonce, with the host id
    /// prefix, role and services of each: a TOML file of `[[mac]]` entries (`serial`,
    /// `host_id_prefix`, `role`, `services`). Without it no Mac is approved by its nonce.
    #[arg(long, requires = "enrollment_dir")]
    pub mac_serial_allowlist: Option<PathBuf>,
    /// A directory of static files (the operator web app) that the private listener serves
    /// under `/ui/`, to `GET` and `HEAD` only: `/ui/` is its `index.html`, and only files ending
    /// in .html, .css, .js, .json, .svg, .png or .ico of at most 4 MiB inside it are served.
    /// The files need no token; the operator API they call does. It must be a directory at
    /// start. Without it no file is served.
    #[arg(long)]
    pub ui_dir: Option<PathBuf>,
    /// How long after its start the coordinator orders nothing, in seconds, so that it hears
    /// the fleet first and knows which host holds the update slot. At least two heartbeat
    /// intervals (30); a lower value is refused at start.
    #[arg(long, default_value_t = QUIET_START.as_secs())]
    pub quiet_secs: u64,
}

/// Where the listeners are bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bound {
    /// The heartbeat listener.
    pub heartbeat: SocketAddr,
    /// The private listener.
    pub control: SocketAddr,
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
/// `--private-listen` is a wildcard address, `--quiet-secs` is below two heartbeat intervals,
/// another coordinator holds the lock of the state files, a halt was being written when the
/// coordinator stopped, both token flags or neither is given, the legacy service or an
/// allow-list entry names a service `--rollout-file` does not define, an entry names no
/// services with no legacy service set, or the two URI prefixes are refused.
pub fn build(flags: &Serve, roots: Roots) -> Result<Parts, StartError> {
    build_with(flags, roots, &FileLock)
}

/// [`build`], with the lock of the state files taken by `lock`.
///
/// # Errors
/// [`StartError`], as [`build`].
pub fn build_with(flags: &Serve, roots: Roots, lock: &dyn StateLock) -> Result<Parts, StartError> {
    if flags.private_listen.ip().to_canonical().is_unspecified() {
        return Err(StartError::PrivateListen(flags.private_listen));
    }
    if flags.quiet_secs < QUIET_START.as_secs() {
        return Err(StartError::QuietBelowFloor {
            secs: flags.quiet_secs,
            floor: QUIET_START.as_secs(),
        });
    }

    let tokens = match (&flags.tokens_file, &flags.operator_token_file) {
        (Some(path), None) => OperatorTokens::from_file(path)?,
        (None, Some(path)) => OperatorTokens::single(
            &hostkeeper_wire::secret::read_token(path).map_err(StartError::OperatorToken)?,
        ),
        (Some(_), Some(_)) => return Err(StartError::TokenFlags),
        (None, None) => return Err(StartError::NoTokenFlag),
    };
    let oidc = AllowList::from_file(&flags.oidc_config)?;
    let services = Services::from_file(&flags.rollout_file)?;
    let prefixes = SanPrefixes::new(&flags.role_uri_prefix, &flags.host_role_uri_prefix)?;
    let legacy = flags.legacy_service.as_deref();
    if let Some(name) = legacy.filter(|name| services.get(name).is_none()) {
        return Err(StartError::LegacyService(name.to_owned()));
    }
    for (n, entry) in oidc.entries().iter().enumerate() {
        match &entry.services {
            None if legacy.is_none() => return Err(StartError::OidcEntryUnnamed(n + 1)),
            None => {}
            Some(names) => {
                if let Some(name) = names.iter().find(|s| services.get(s).is_none()) {
                    return Err(StartError::OidcEntryService {
                        entry: n + 1,
                        service: name.clone(),
                    });
                }
            }
        }
    }
    let keys = HttpsKeys::new(&oidc.entries()[0].issuer, roots, KEY_FETCH_TIMEOUT)
        .map_err(StartError::Keys)?;
    // The lock before any state file: none is opened (an open settles a staged halts file,
    // which is a write) before this process is the one coordinator on them.
    let held = take_lock(lock, &lock_path(&flags.state_file))?;
    let ops_path = flags.ops_file.clone().unwrap_or_else(|| {
        let mut name = flags.state_file.clone().into_os_string();
        name.push(".ops");
        PathBuf::from(name)
    });
    refuse_unfinished_halt(&ops_path)?;
    // The wall clock is read for the validity of CI's tokens and the expiry of their spent
    // ids, and for nothing the coordinator decides.
    let oidc_clock = hostkeeper_oidc::system_clock();
    let spent = JtiFile::open(&jti_path(&flags.state_file), Arc::clone(&oidc_clock))?;
    let verifier = Verifier::new(
        oidc,
        Arc::new(keys),
        KEY_REFETCH,
        oidc_clock,
        Arc::new(spent),
    );
    let desired = DesiredStore::open(&flags.state_file, legacy)?;
    let ops = OpsStore::open(&ops_path)?;
    let verdicts = VerdictFile::open(&verdicts_path(&flags.state_file))?;
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
        .with_services(services)
        .with_legacy_service(legacy.map(str::to_owned))
        .with_quiet(Duration::from_secs(flags.quiet_secs))
        .with_prefixes(prefixes)
        .with_ops(ops)
        .with_verdicts(verdicts)
        .with_ui(ui);
    let heartbeat_tls = match (
        &flags.enrollment_dir,
        &flags.ca_key_file,
        &flags.server_cert_file,
    ) {
        (Some(dir), Some(ca_key), Some(server_cert)) => {
            let allowlist = flags
                .mac_serial_allowlist
                .as_deref()
                .map(MacAllowlist::load)
                .transpose()?
                .unwrap_or_default();
            if let Some(unknown) = allowlist
                .services()
                .find(|service| coordinator.services.get(service.as_str()).is_none())
            {
                return Err(StartError::MacService(unknown.as_str().to_owned()));
            }
            let max_pending = flags
                .max_pending_enrollments
                .map_or(DEFAULT_MAX_PENDING, |max| {
                    usize::try_from(max).unwrap_or(usize::MAX)
                });
            let enrollment = setup(
                Arc::new(RealFs),
                &Setup {
                    dir,
                    ca_key,
                    ca_init: flags.ca_init,
                    server_cert,
                    server_names: &flags.server_names,
                },
                Arc::new(SystemClock),
                os_random(),
            )?
            .with_max_pending(max_pending)
            // The MDM gate is not built into this coordinator: no device is known to it.
            .with_mac(MacPolicy {
                device_state: Arc::new(NoDeviceState),
                allowlist,
            });
            let client = ClientAuth {
                optional: true,
                extra_roots: vec![enrollment.ca_bundle().certificate_der().to_vec()],
            };
            coordinator = coordinator.with_enrollment(Arc::new(enrollment));
            let served = TlsFiles {
                cert: server_cert.clone(),
                key: server_cert.clone(),
                client_ca: Some(flags.client_ca.clone()),
            };
            TlsSource::files_with("heartbeat", served, interval, Alpn::H2AndHttp1, client)?
        }
        // clap requires the three together: one alone is no enrollment.
        _ => TlsSource::files("heartbeat", files(Some(flags.client_ca.clone())), interval)?,
    };
    Ok(Parts {
        coordinator: Arc::new(coordinator),
        heartbeat_tls,
        control_tls: TlsSource::files("control", files(None), interval)?,
        heartbeat_listen: flags.heartbeat_listen,
        control_listen: flags.private_listen,
        reload: interval,
        lock: held,
    })
}

/// Refuses to start over the marker of a halt that was being written to the ops file at `ops`
/// when the coordinator stopped ([`crate::ops::Write::Halt`]): the halt may be lost, and the
/// operator settles the halts file by hand and removes the marker.
///
/// # Errors
/// [`StartError::HaltWriteUnfinished`]: the marker is there.
pub(crate) fn refuse_unfinished_halt(ops: &Path) -> Result<(), StartError> {
    let marker = crate::ops_file::unfinished_path(ops);
    if marker.exists() {
        return Err(StartError::HaltWriteUnfinished { path: marker });
    }
    Ok(())
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

/// Binds a TCP socket at `addr`: the address it got, and the function that accepts its
/// connections.
///
/// # Errors
/// The address cannot be bound.
pub async fn bind_tcp(
    addr: SocketAddr,
) -> io::Result<(
    SocketAddr,
    impl FnMut() -> futures::future::BoxFuture<'static, io::Result<tokio::net::TcpStream>> + Send,
)> {
    let tcp = tokio::net::TcpListener::bind(addr).await?;
    Ok((tcp.local_addr()?, accept_tcp(tcp)))
}

/// Two futures that never finish, polled together: it never returns.
async fn together(
    first: impl Future<Output = Infallible>,
    second: impl Future<Output = Infallible>,
) -> Infallible {
    let (Either::Left((never, _)) | Either::Right((never, _))) =
        select(pin!(first), pin!(second)).await;
    match never {}
}

/// Binds both listeners with `bind` (which answers the address bound and the function that
/// accepts connections), calls `ready` with their addresses, and serves until the future is
/// dropped. It never returns `Ok`.
///
/// # Errors
/// An address cannot be bound.
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
    Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // The lock of the state files is held for as long as this serves.
    let _held = parts.lock;
    let (heartbeat_addr, heartbeat_accept) = bind(parts.heartbeat_listen).await?;
    let (control_addr, control_accept) = bind(parts.control_listen).await?;
    ready(Bound {
        heartbeat: heartbeat_addr,
        control: control_addr,
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
    let coordinator = parts.coordinator;
    let serving = together(
        serve(
            heartbeat,
            Arc::clone(&coordinator),
            CLIENT_PATIENCE,
            MAX_CONNECTIONS,
        ),
        serve(control, coordinator, CLIENT_PATIENCE, MAX_CONNECTIONS),
    );
    match together(
        serving,
        reload_every(
            vec![parts.heartbeat_tls, parts.control_tls],
            tokens,
            parts.reload,
        ),
    )
    .await {}
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
/// `roots`, until `stop` resolves.
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
    Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let parts = build(&flags, roots)?;
    let serving = run(parts, bind, |bound| {
        println!(
            "hostkeeper-coordinator {} heartbeat={} control={}",
            env!("CARGO_PKG_VERSION"),
            bound.heartbeat,
            bound.control
        );
    });
    match select(pin!(stop), pin!(serving)).await {
        Either::Left((stopped, _)) => stopped.map_err(StartError::from),
        Either::Right((served, _)) => {
            let Err(why) = served;
            Err(why)
        }
    }
}

/// Builds the runtime and runs the coordinator on it until a signal.
fn runtime(flags: Serve) -> Result<(), StartError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(start(flags, Roots::System, bind_tcp, signalled()))
}

/// The binary: flags in, exit code out. Bad flags are exit 2; a failed start is exit 1;
/// SIGTERM or SIGINT is exit 0.
#[must_use]
pub fn main(args: Vec<OsString>) -> ExitCode {
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
    let Command::Serve(flags) = cli.command;
    match runtime(flags) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("hostkeeper-coordinator: {why}");
            ExitCode::from(1)
        }
    }
}
