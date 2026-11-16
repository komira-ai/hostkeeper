//! The commands: what each asks of the coordinator and of the machine, in order.

use std::path::{Path, PathBuf};

use hostkeeper_proto::hostkeeper::operator::v1::RevokeRequest;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _};

use crate::access::{self, Identity};
use crate::cli::{Cli, Command, Global};
use crate::client::{self, Client, from_status, unary};
use crate::error::Error;
use crate::programs::Programs;
use crate::state::{CertInfo, State, check_host_id};

/// What a run needs from outside: the OpenSSH programs, the time, and the path of this program
/// (which ssh runs again as its `ProxyCommand` and `KnownHostsCommand`).
pub struct Env<'a> {
    pub programs: &'a dyn Programs,
    /// Unix seconds.
    pub now: u64,
    pub exe: PathBuf,
}

/// `text` as one word of a shell command line, which is how ssh runs a `ProxyCommand`.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn state_of(global: &Global) -> Result<State, Error> {
    global
        .state_dir
        .clone()
        .map(State::new)
        .ok_or_else(|| Error::Usage("this command needs --state-dir".into()))
}

/// The flags of `global` that are set, as arguments of a later run of this program.
fn forwarded(global: &Global, identity: Option<&Path>) -> Vec<String> {
    let show = |path: &Path| path.to_string_lossy().into_owned();
    let flags = [
        ("--endpoint", global.endpoint.clone()),
        ("--ca-file", global.ca_file.as_deref().map(show)),
        ("--token-file", global.token_file.as_deref().map(show)),
        ("--state-dir", global.state_dir.as_deref().map(show)),
        ("--timeout-secs", Some(global.timeout_secs.to_string())),
        ("--keepalive-secs", Some(global.keepalive_secs.to_string())),
        (
            "--ssh-keygen-program",
            Some(show(&global.ssh_keygen_program)),
        ),
        ("--ssh-add-program", Some(show(&global.ssh_add_program))),
        (
            "--ssh-agent-socket",
            global.ssh_agent_socket.as_deref().map(show),
        ),
        ("--identity", identity.map(show)),
    ];
    flags
        .into_iter()
        .flat_map(|(flag, value)| value.map(|value| [flag.to_owned(), value]))
        .flatten()
        .collect()
}

/// A client, the state directory, the key and a valid certificate for `host`.
async fn prepare(
    global: &Global,
    env: &Env<'_>,
    host: &str,
    requested: Option<&Path>,
    reason: &str,
) -> Result<(Client, State, Identity, CertInfo), Error> {
    check_host_id(host)?;
    let state = state_of(global)?;
    let mut client = client::connect(global)?;
    let identity = access::identity(&state, env.programs, requested)?;
    let cert = access::certificate(
        &mut client,
        global,
        &state,
        &identity,
        host,
        reason,
        env.now,
    )
    .await?;
    Ok((client, state, identity, cert))
}

/// `hkssh ssh`: the pins, then the certificate, then ssh with this program as its proxy and as
/// the judge of the host key.
async fn ssh(
    global: &Global,
    env: &Env<'_>,
    host: &str,
    requested: Option<&Path>,
    reason: &str,
    ssh_args: &[String],
    diag: &mut dyn std::io::Write,
) -> Result<i32, Error> {
    check_host_id(host)?;
    let state = state_of(global)?;
    let mut client = client::connect(global)?;
    // The pins first: a host with none is refused before a certificate is made for it.
    let pins = access::pins(&mut client, global, host).await?;
    access::keep_pins(&state, host, &pins)?;
    if pins.state == "changed" {
        let _ = writeln!(
            diag,
            "hkssh: the host keys of {host} differ from the pinned ones; connecting with the pinned set"
        );
    }
    let (_, _, identity, cert) = prepare(global, env, host, requested, reason).await?;
    let exe = env.exe.to_string_lossy();
    let proxy = std::iter::once(quote(&exe))
        .chain(["proxy".to_owned(), quote(host)])
        .chain(forwarded(global, requested).iter().map(|a| quote(a)))
        .collect::<Vec<_>>()
        .join(" ");
    let known = format!(
        "{} known-host {} %H %t %K %f",
        quote(&exe),
        quote(&state.pins_file(host).to_string_lossy())
    );
    let mut args: Vec<String> = Vec::new();
    for option in [
        format!("ProxyCommand={proxy}"),
        format!("CertificateFile={}", state.cert_file(host).display()),
        "IdentitiesOnly=yes".into(),
        "PasswordAuthentication=no".into(),
        "KbdInteractiveAuthentication=no".into(),
        "StrictHostKeyChecking=yes".into(),
        "UserKnownHostsFile=/dev/null".into(),
        "GlobalKnownHostsFile=/dev/null".into(),
        format!("KnownHostsCommand={known}"),
    ] {
        args.push("-o".into());
        args.push(option);
    }
    args.push("-i".into());
    args.push(identity.ssh_identity.to_string_lossy().into_owned());
    args.push(format!("{}@{host}", cert.login_user));
    args.extend(ssh_args.iter().cloned());
    env.programs.run_ssh(&args)
}

/// `hkssh revoke`.
async fn revoke(
    global: &Global,
    operator: Option<String>,
    serial: Option<u64>,
    host: Option<String>,
    output: &mut (impl AsyncWrite + Unpin),
) -> Result<i32, Error> {
    let request = match (operator, serial, host) {
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
            let held = state_of(global)?
                .cert_info(&host)
                .ok_or_else(|| Error::Unusable(format!("no certificate is held for {host}")))?;
            RevokeRequest {
                operator: String::new(),
                serial: held.serial,
            }
        }
    };
    let mut client = client::connect(global)?;
    let reply = client
        .revoke(unary(request, global))
        .await
        .map_err(|status| from_status(&status))?
        .into_inner();
    say(output, &format!("revoked: {}\n", reply.revoked)).await?;
    Ok(0)
}

/// `hkssh status`.
async fn status(
    global: &Global,
    env: &Env<'_>,
    host: &str,
    output: &mut (impl AsyncWrite + Unpin),
) -> Result<i32, Error> {
    check_host_id(host)?;
    let state = state_of(global)?;
    let mut client = client::connect(global)?;
    let pins = access::pins(&mut client, global, host).await?;
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

/// `hkssh known-host`: the line ssh may trust for the key it presents, else nothing. Anything
/// missing (ssh asking only which key types to prefer, a pins file not there) is nothing.
async fn known_host(
    pins: &Path,
    rest: &[String],
    output: &mut (impl AsyncWrite + Unpin),
) -> Result<i32, Error> {
    if let ([host, kind, key, fingerprint], Ok(text)) = (rest, std::fs::read_to_string(pins))
        && let Some(line) = access::known_host_line(&text, host, kind, key, fingerprint)
    {
        say(output, &format!("{line}\n")).await?;
    }
    Ok(0)
}

async fn say(output: &mut (impl AsyncWrite + Unpin), text: &str) -> Result<(), Error> {
    let fail = |source| Error::Write {
        path: PathBuf::from("standard output"),
        source,
    };
    output.write_all(text.as_bytes()).await.map_err(fail)?;
    output.flush().await.map_err(fail)
}

/// Runs the command; its exit code. `input` and `output` are the byte stream of `proxy` and the
/// text of the others; `diag` is where a warning goes.
pub async fn run<R, W>(
    cli: Cli,
    env: &Env<'_>,
    input: R,
    mut output: W,
    diag: &mut dyn std::io::Write,
) -> Result<i32, Error>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let global = &cli.global;
    match cli.command {
        Command::Ssh {
            host_id,
            identity,
            reason,
            ssh_args,
        } => {
            ssh(
                global,
                env,
                &host_id,
                identity.as_deref(),
                &reason,
                &ssh_args,
                diag,
            )
            .await
        }
        Command::Proxy {
            host_id,
            identity,
            reason,
        } => {
            let (mut client, _, _, cert) =
                prepare(global, env, &host_id, identity.as_deref(), &reason).await?;
            crate::tunnel::proxy(&mut client, &host_id, cert.serial, input, output).await?;
            Ok(0)
        }
        Command::Revoke {
            operator,
            serial,
            host,
        } => revoke(global, operator, serial, host, &mut output).await,
        Command::Status { host_id } => status(global, env, &host_id, &mut output).await,
        Command::KnownHost { pins, rest } => known_host(&pins, &rest, &mut output).await,
    }
}
