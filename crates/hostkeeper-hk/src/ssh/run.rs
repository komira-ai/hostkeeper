//! The ssh verbs: what each asks of the coordinator and of the machine, in order.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use hostkeeper_proto::hostkeeper::operator::v1::RevokeRequest;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _};

use crate::ssh::cli::{ProxyArgs, RevokeArgs, SshArgs, StatusArgs, parse};
use crate::ssh::client::{self, Client, from_status, unary};
use crate::ssh::error::Error;
use crate::ssh::programs::Programs;
use crate::ssh::state::{CertInfo, State, check_host_id};
use crate::ssh::{Coordinator, Verb, access, tunnel};
use access::Identity;

/// What a run needs from outside: the OpenSSH programs, the time, the path of this program
/// (which ssh runs again as its `ProxyCommand` and `KnownHostsCommand`), where the state is
/// kept, how often a tunnel is probed, and the name that starts a diagnostic.
pub struct Env<'a> {
    /// The OpenSSH programs.
    pub programs: &'a dyn Programs,
    /// Unix seconds.
    pub now: u64,
    /// The path of the running program.
    pub exe: PathBuf,
    /// Where the key, the certificates and the pins are kept (mode 0700).
    pub state_dir: PathBuf,
    /// The HTTP/2 PING interval of a tunnel, which keeps a quiet session open through Envoy and
    /// NAT.
    pub keepalive_secs: u64,
    /// The program's name, the prefix of every diagnostic (`<name>: <error>`).
    pub name: &'a str,
}

/// Whether `ssh-proxy` turns this process's SIGHUP and SIGTERM into a close of its tunnel: what
/// [`System`](crate::ssh::System) runs, and never a test's own process, which [`run`] runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stops {
    /// No signal reaches the proxy.
    Ignored,
    /// SIGHUP and SIGTERM do ([`crate::ssh::tunnel::stop_signals`]).
    Process,
}

/// Why a verb stopped: its tail did not parse (clap's own text, with its code), or it failed.
enum Stop {
    Parse(clap::Error),
    Fail(Error),
}

impl From<Error> for Stop {
    fn from(error: Error) -> Stop {
        Stop::Fail(error)
    }
}

impl From<clap::Error> for Stop {
    fn from(error: clap::Error) -> Stop {
        Stop::Parse(error)
    }
}

/// `text` as one word of a shell command line, which is how ssh runs a `ProxyCommand`.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The command line ssh runs as its `ProxyCommand` for `host`: this program, the coordinator's
/// flags, then `ssh-proxy`.
fn proxy_command(
    coordinator: &Coordinator,
    env: &Env<'_>,
    host: &str,
    identity: Option<&Path>,
) -> String {
    let show = |path: &Path| path.to_string_lossy().into_owned();
    let mut words = vec![quote(&show(&env.exe))];
    for (flag, value) in [
        ("--endpoint", coordinator.endpoint.clone()),
        ("--ca-file", show(&coordinator.ca_file)),
        ("--token-file", show(&coordinator.token_file)),
        ("--timeout-secs", coordinator.timeout_secs.to_string()),
    ] {
        words.push(flag.to_owned());
        words.push(quote(&value));
    }
    words.push("ssh-proxy".to_owned());
    words.push(quote(host));
    if let Some(identity) = identity {
        words.push("--identity".to_owned());
        words.push(quote(&show(identity)));
    }
    words.join(" ")
}

/// The command line ssh runs as its `KnownHostsCommand`: this program and the pins file.
fn known_hosts_command(env: &Env<'_>, pins: &Path) -> String {
    format!(
        "{} ssh-known-host {} %H %t %K %f",
        quote(&env.exe.to_string_lossy()),
        quote(&pins.to_string_lossy())
    )
}

/// A client, the state directory, the key and a valid certificate for `host`.
async fn prepare(
    coordinator: &Coordinator,
    env: &Env<'_>,
    host: &str,
    requested: Option<&Path>,
    reason: &str,
) -> Result<(Client, State, Identity, CertInfo), Error> {
    check_host_id(host)?;
    let state = State::new(env.state_dir.clone());
    let mut client = client::connect(coordinator, env.keepalive_secs)?;
    let identity = access::identity(&state, env.programs, requested)?;
    let cert = access::certificate(
        &mut client,
        coordinator,
        &state,
        &identity,
        host,
        reason,
        env.now,
    )
    .await?;
    Ok((client, state, identity, cert))
}

/// `hk ssh`: the pins, then the certificate, then ssh with this program as its proxy and as
/// the judge of the host key.
async fn ssh(
    coordinator: &Coordinator,
    env: &Env<'_>,
    args: SshArgs,
    diag: &mut dyn std::io::Write,
) -> Result<i32, Error> {
    let SshArgs {
        host_id: host,
        identity: requested,
        reason,
        ssh_args,
    } = args;
    check_host_id(&host)?;
    let state = State::new(env.state_dir.clone());
    let mut client = client::connect(coordinator, env.keepalive_secs)?;
    // The pins first: a host with none is refused before a certificate is made for it.
    let pins = access::pins(&mut client, coordinator, &host).await?;
    access::keep_pins(&state, &host, &pins)?;
    if pins.state == "changed" {
        let _ = writeln!(
            diag,
            "{}: the host keys of {host} differ from the pinned ones; connecting with the pinned set",
            env.name
        );
    }
    let (_, _, identity, cert) =
        prepare(coordinator, env, &host, requested.as_deref(), &reason).await?;
    let proxy = proxy_command(coordinator, env, &host, requested.as_deref());
    let known = known_hosts_command(env, &state.pins_file(&host));
    let mut argv: Vec<OsString> = Vec::new();
    for option in [
        format!("ProxyCommand={proxy}"),
        format!("CertificateFile={}", state.cert_file(&host).display()),
        "IdentitiesOnly=yes".into(),
        "PasswordAuthentication=no".into(),
        "KbdInteractiveAuthentication=no".into(),
        "StrictHostKeyChecking=yes".into(),
        "UserKnownHostsFile=/dev/null".into(),
        "GlobalKnownHostsFile=/dev/null".into(),
        format!("KnownHostsCommand={known}"),
    ] {
        argv.push("-o".into());
        argv.push(option.into());
    }
    argv.push("-i".into());
    argv.push(identity.ssh_identity.into_os_string());
    argv.push(format!("{}@{host}", cert.login_user).into());
    argv.extend(ssh_args);
    env.programs.run_ssh(&argv)
}

/// `hk ssh-revoke`.
async fn revoke(
    coordinator: &Coordinator,
    env: &Env<'_>,
    args: RevokeArgs,
    output: &mut (impl AsyncWrite + Unpin),
) -> Result<i32, Error> {
    let request = match (args.operator, args.serial, args.host) {
        (Some(operator), _, _) => RevokeRequest {
            operator,
            serial: 0,
        },
        (None, Some(serial), _) => RevokeRequest {
            operator: String::new(),
            serial,
        },
        (None, None, host) => {
            let host = host.unwrap_or_default();
            check_host_id(&host)?;
            let held = State::new(env.state_dir.clone())
                .cert_info(&host)
                .ok_or_else(|| Error::Unusable(format!("no certificate is held for {host}")))?;
            RevokeRequest {
                operator: String::new(),
                serial: held.serial,
            }
        }
    };
    let mut client = client::connect(coordinator, env.keepalive_secs)?;
    let reply = client
        .revoke(unary(request, coordinator))
        .await
        .map_err(|status| from_status(&status))?
        .into_inner();
    say(output, &format!("revoked: {}\n", reply.revoked)).await?;
    Ok(0)
}

/// `hk ssh-status`.
async fn status(
    coordinator: &Coordinator,
    env: &Env<'_>,
    host: &str,
    output: &mut (impl AsyncWrite + Unpin),
) -> Result<i32, Error> {
    check_host_id(host)?;
    let state = State::new(env.state_dir.clone());
    let mut client = client::connect(coordinator, env.keepalive_secs)?;
    let pins = access::pins(&mut client, coordinator, host).await?;
    let mut text = format!("host: {host}\nhost keys: {}\n", pins.state);
    for fingerprint in &pins.fingerprints {
        text.push_str(&format!("fingerprint: {fingerprint}\n"));
    }
    match state.cert_info(host) {
        Some(held) => {
            let validity = if held.expires >= env.now {
                "valid"
            } else {
                "expired"
            };
            text.push_str(&format!(
                "certificate: serial {} for {}, expires {} ({validity})\n",
                held.serial, held.login_user, held.expires
            ));
        }
        None => text.push_str("certificate: none held\n"),
    }
    say(output, &text).await?;
    Ok(0)
}

/// `hk ssh-known-host <pins> [%H %t %K %f]`: the line ssh may trust for the key it presents,
/// else nothing, with exit code 0 either way. `args` is what follows the verb. Anything missing
/// (ssh asking only which key types to prefer, a pins file not there) or not text is nothing.
pub fn known_host(args: &[OsString], stdout: &mut dyn std::io::Write) -> u8 {
    if let [pins, host, kind, key, fingerprint] = args
        && let Ok(text) = std::fs::read_to_string(Path::new(pins))
        && let (Some(host), Some(kind), Some(key), Some(fingerprint)) = (
            host.to_str(),
            kind.to_str(),
            key.to_str(),
            fingerprint.to_str(),
        )
        && let Some(line) = access::known_host_line(&text, host, kind, key, fingerprint)
    {
        let _ = writeln!(stdout, "{line}").and_then(|()| stdout.flush());
    }
    0
}

async fn say(output: &mut (impl AsyncWrite + Unpin), text: &str) -> Result<(), Error> {
    let fail = |source| Error::Write {
        path: PathBuf::from("standard output"),
        source,
    };
    output.write_all(text.as_bytes()).await.map_err(fail)?;
    output.flush().await.map_err(fail)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the arguments of `run`, and whether the proxy takes the process's signals"
)]
async fn dispatch<R, W>(
    verb: Verb,
    coordinator: &Coordinator,
    args: &[OsString],
    env: &Env<'_>,
    input: R,
    output: &mut W,
    diag: &mut dyn std::io::Write,
    stops: Stops,
) -> Result<i32, Stop>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    Ok(match verb {
        Verb::Ssh => ssh(coordinator, env, parse(args)?, diag).await?,
        Verb::Proxy => {
            let ProxyArgs {
                host_id,
                identity,
                reason,
            } = parse(args)?;
            let (mut client, _, _, cert) =
                prepare(coordinator, env, &host_id, identity.as_deref(), &reason).await?;
            // Taken only now: a signal before the tunnel is asked for ends the process, which
            // leaves nothing in the coordinator's audit to close.
            let signals = match stops {
                Stops::Ignored => Box::pin(tokio_stream::pending()),
                Stops::Process => tunnel::stop_signals()?,
            };
            tunnel::proxy(
                &mut client,
                &host_id,
                cert.serial,
                Box::new(input),
                output,
                signals,
            )
            .await?;
            0
        }
        Verb::Revoke => revoke(coordinator, env, parse(args)?, output).await?,
        Verb::Status => {
            let StatusArgs { host_id } = parse(args)?;
            status(coordinator, env, &host_id, output).await?
        }
    })
}

/// Runs `verb` with the tail `args`; its exit code (2 for a tail that does not parse, 1 for any
/// other failure, ssh's own code for `ssh`). `input` and `output` are the byte stream of
/// `Proxy` and the text of the others; `diag` is where a failure (`<name>: <error>`) and a
/// warning go. No signal reaches `Proxy`; [`System`](crate::ssh::System) runs [`run_with`], whose
/// proxy closes its tunnel at SIGHUP and SIGTERM.
pub async fn run<R, W>(
    verb: Verb,
    coordinator: &Coordinator,
    args: &[OsString],
    env: &Env<'_>,
    input: R,
    output: W,
    diag: &mut dyn std::io::Write,
) -> u8
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    run_with(
        verb,
        coordinator,
        args,
        env,
        input,
        output,
        diag,
        Stops::Ignored,
    )
    .await
}

/// [`run`], with `stops` saying whether `Proxy` takes the process's SIGHUP and SIGTERM.
#[expect(
    clippy::too_many_arguments,
    reason = "the arguments of `run`, and whether the proxy takes the process's signals"
)]
pub(crate) async fn run_with<R, W>(
    verb: Verb,
    coordinator: &Coordinator,
    args: &[OsString],
    env: &Env<'_>,
    input: R,
    mut output: W,
    diag: &mut dyn std::io::Write,
    stops: Stops,
) -> u8
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    match dispatch(
        verb,
        coordinator,
        args,
        env,
        input,
        &mut output,
        diag,
        stops,
    )
    .await
    {
        Ok(code) => u8::try_from(code).unwrap_or(255),
        Err(Stop::Fail(error)) => {
            let _ = writeln!(diag, "{}: {error}", env.name);
            error.exit_code()
        }
        Err(Stop::Parse(error)) => {
            let text = error.render().to_string();
            if error.use_stderr() {
                let _ = diag.write_all(text.as_bytes());
                2
            } else {
                let _ = say(&mut output, &text).await;
                0
            }
        }
    }
}
