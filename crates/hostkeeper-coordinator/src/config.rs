//! `hostkeeper-coordinator serve`'s flags and start-up: read every file, bind both listeners,
//! print the start line, then serve until SIGTERM or SIGINT.
//!
//! Everything is a flag; the coordinator reads no environment variable. It holds no CA key:
//! its certificate and key are files the farm CA issued, and the CA certificate it checks
//! updaters against is a file too. The services it manages are the ones `--rollout-file`
//! defines; `--legacy-service` names the one whose heartbeat is the top level of a heartbeat;
//! the operator's tokens are those of `--tokens-file`, or the one token of
//! `--operator-token-file`. The private listener (`--private-listen`) serves CI's calls and the
//! operator's; it takes the address of one interface. With `--ui-dir` it also serves the static
//! files of that directory under `/ui/` (`crate::ui`).
//! How the sockets are bound is a function the caller gives ([`bind_tcp`] in the binary), so
//! the whole start can be driven over in-memory connections.

use std::convert::Infallible;
use std::ffi::OsString;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::pin;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand};
use futures::future::{Either, select};
use hostkeeper_oidc::{AllowList, ConfigError, HttpsKeys, KeySourceError, Roots, Verifier};
use hostkeeper_tls::{TlsError, TlsFiles, TlsSource};
use hostkeeper_wire::identity::{
    DEFAULT_HOST_ROLE_URI_PREFIX, DEFAULT_ROLE_URI_PREFIX, PrefixError, SanPrefixes,
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::desired::{DesiredStore, StoreError};
use crate::http::Surface;
use crate::listen::{CLIENT_PATIENCE, Listener, MAX_CONNECTIONS, accept_tcp, reload_every, serve};
use crate::ops::{OpsError, OpsStore};
use crate::service::{Clock, Coordinator};
use crate::services::{Services, ServicesError};
use crate::tokens::{OperatorTokens, TokensError};
use crate::ui::{UiDir, UiDirError};

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
    /// `--ui-dir` does not name a directory.
    #[error("--ui-dir: {0}")]
    UiDir(#[from] UiDirError),
    /// A socket, the runtime or the signal handlers failed.
    #[error(transparent)]
    Io(#[from] io::Error),
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
    /// A directory of static files (the operator web app) that the private listener serves
    /// under `/ui/`, to `GET` and `HEAD` only: `/ui/` is its `index.html`, and only files ending
    /// in .html, .css, .js, .json, .svg, .png or .ico of at most 4 MiB inside it are served.
    /// The files need no token; the operator API they call does. It must be a directory at
    /// start. Without it no file is served.
    #[arg(long)]
    pub ui_dir: Option<PathBuf>,
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
}

/// Reads every file the flags name and builds what runs. Fetches nothing: the issuer's keys
/// are fetched on the first token.
///
/// # Errors
/// [`StartError`]: a file is missing, unreadable or refused (the text names the flag or the
/// file; the ops file must be one this build reads), `--ui-dir` is missing or not a directory,
/// `--private-listen` is a wildcard address, both token flags or neither is given, the legacy
/// service or an allow-list entry names a service `--rollout-file` does not define, an entry
/// names no services with no legacy service set, or the two URI prefixes are refused.
pub fn build(flags: &Serve, roots: Roots) -> Result<Parts, StartError> {
    if flags.private_listen.ip().to_canonical().is_unspecified() {
        return Err(StartError::PrivateListen(flags.private_listen));
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
    let verifier = Verifier::new(
        oidc,
        Arc::new(keys),
        KEY_REFETCH,
        hostkeeper_oidc::system_clock(),
    );
    let desired = DesiredStore::open(&flags.state_file, legacy)?;
    let ops_path = flags.ops_file.clone().unwrap_or_else(|| {
        let mut name = flags.state_file.clone().into_os_string();
        name.push(".ops");
        PathBuf::from(name)
    });
    let ops = OpsStore::open(&ops_path)?;
    let ui = flags.ui_dir.as_deref().map(UiDir::new).transpose()?;
    let interval = Duration::from_secs(flags.tls_reload_secs);
    let files = |client_ca| TlsFiles {
        cert: flags.tls_cert.clone(),
        key: flags.tls_key.clone(),
        client_ca,
    };
    let origin = Instant::now();
    let clock: Clock = Arc::new(move || origin.elapsed());
    Ok(Parts {
        coordinator: Arc::new(
            Coordinator::new(desired, Arc::new(verifier), tokens, clock)
                .with_services(services)
                .with_legacy_service(legacy.map(str::to_owned))
                .with_prefixes(prefixes)
                .with_ops(ops)
                .with_ui(ui),
        ),
        heartbeat_tls: TlsSource::files(
            "heartbeat",
            files(Some(flags.client_ca.clone())),
            interval,
        )?,
        control_tls: TlsSource::files("control", files(None), interval)?,
        heartbeat_listen: flags.heartbeat_listen,
        control_listen: flags.private_listen,
        reload: interval,
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
