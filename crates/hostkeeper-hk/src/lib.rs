//! `hk`: the operator's command line for hostkeeper's coordinator.
//!
//! One program, generated from the API: every command of the table `hostkeeper_proto::ctl::COMMANDS`
//! (one per RPC the coordinator's JSON gateway serves) is `hk <service> <method> [flags]`, one
//! POST of the request the flags make, with the exit code of the gateway's answer. The SSH tunnel
//! is built in: [`ssh`] is `hk ssh`, `hk ssh-proxy`, `hk ssh-revoke`, `hk ssh-status` and the
//! `hk ssh-known-host` that ssh runs. [`xcode_push`] is `hk xcode-push`, which brings an Xcode
//! release into the coordinator's Xcode store from the operator's own Mac, and [`xcode_verify`] is
//! `hk xcode verify`, a hand-written method of the generated service `xcode` ([`hand`]) that
//! checks an archive there and keeps the record of what it found ([`xcode_record`]). [`tailnet_wait`] is
//! `hk tailnet-wait`, which waits for the coordinator after a CI runner joined the tailnet and
//! rejoins it once when the coordinator does not answer. [`settings`] is the file
//! `$HOME/.hostkeeper/hk.toml`, which gives `--endpoint`, `--ca-file` and `--token-file` their
//! defaults, and `hk settings init`, which writes it; [`home`] makes and checks `~/.hostkeeper`.
//!
//! - [`run`]: a command line against a [`World`], which holds everything it reads and writes: the
//!   command table, the streams, the home directory and the user's id, the runner's token request, the program
//!   that runs the ssh verbs, the programs of the Mac that `hk xcode-push` and `hk xcode verify` run and the
//!   `tailscale` and the clock of `hk tailnet-wait`. The tests of this crate run it with fakes for
//!   all of them.
//! - [`main`]: the program's `main`, which builds the real [`World`] from the process.
//! - [`markdown`]: `docs/cli.md`, from a command table.
//! - [`stamp`]: the version `hk --version` prints, with the commit of a release build.
//!
//! What `hk` takes from the table: every command of a package other than the host API's
//! (`hostkeeper.agent.v<n>`), which the gateway never serves and whose caller is the daemon
//! ([`is_served`]); the flags the table gives each field, and the request file for the rest.
//! What it adds: the global flags, and the hold of a request field named `wait_secs`, which
//! lengthens the call's deadline by its value.

mod args;
mod body;
mod call;
mod dispatch;
mod doc;
mod encode;
mod hand;
mod help;
mod home;
mod send;
mod settings;
pub mod ssh;
pub mod stamp;
mod table;
pub mod tailnet_wait;
#[cfg(test)]
mod tests;
pub mod xcode_push;
mod xcode_record;
mod xcode_verify;

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::PathBuf;
use std::process::ExitCode;

use hostkeeper_proto::ctl::{COMMANDS, Command};

pub use doc::markdown;
pub use table::is_served;

/// The program's name: the word `--help` and `docs/cli.md` use, and the diagnostic prefix, and
/// what `--version` starts with.
pub const PROGRAM: &str = "hk";

/// Everything a command line reads and writes besides its arguments.
pub struct World<'a> {
    /// The command table: one entry per method.
    pub table: &'a [Command],
    /// The version, as `--version` prints it after the name: `X.Y.Z+<12 hex>` for a release; its
    /// part before the `+` is what a refused call asks the coordinator if it is older than.
    pub stamp: &'a str,
    /// Standard input.
    pub stdin: &'a mut dyn Read,
    /// Standard output.
    pub stdout: &'a mut dyn Write,
    /// Standard error.
    pub stderr: &'a mut dyn Write,
    /// The operator's home directory, where the ssh commands keep their key and certificates.
    pub home: Option<PathBuf>,
    /// The user id `hk` runs as: the owner `~/.hostkeeper` and the settings file in it must have.
    pub uid: u32,
    /// The URL of the GitHub Actions runner's token request, which the job's runner sets.
    pub oidc_url: Option<Vec<u8>>,
    /// The bearer token of that request, which the job's runner sets.
    pub oidc_token: Option<Vec<u8>>,
    /// The roots (DER) that the runner's token request is trusted by; asked for only then.
    pub web_roots: &'a dyn Fn() -> Vec<Vec<u8>>,
    /// What runs the ssh commands.
    pub ssh: &'a dyn ssh::Launch,
    /// The system and the programs `hk xcode-push` and `hk xcode verify` run.
    pub mac: &'a xcode_push::Mac,
    /// The `tailscale` program and the wait of `hk tailnet-wait`.
    pub tailnet: &'a tailnet_wait::Tailnet<'a>,
}

/// Why a command line stopped before its answer: a usage error (exit 2), a failure (exit 1), or
/// an exit whose output is already written.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Usage(Vec<u8>),
    Fail(Vec<u8>),
    Exit(u8),
}

impl Stop {
    /// A usage error saying `message`.
    pub(crate) fn usage(message: impl Into<Vec<u8>>) -> Stop {
        Stop::Usage(message.into())
    }

    /// A failure saying `message`.
    pub(crate) fn fail(message: impl Into<Vec<u8>>) -> Stop {
        Stop::Fail(message.into())
    }
}

/// What a step of a command line yields, or why the command line ends.
pub(crate) type Flow<T> = Result<T, Stop>;

/// Runs the command line `args` (what follows the program's name) in `world`; the exit code.
pub fn run(world: &mut World<'_>, args: &[OsString]) -> u8 {
    let argv: Vec<Vec<u8>> = args.iter().map(|a| a.as_bytes().to_vec()).collect();
    let (code, message) = match dispatch::dispatch(world, &argv) {
        Ok(()) => (0, None),
        Err(Stop::Exit(code)) => (code, None),
        Err(Stop::Usage(message)) => (2, Some(message)),
        Err(Stop::Fail(message)) => (1, Some(message)),
    };
    if let Some(message) = message {
        let line = [PROGRAM.as_bytes(), b": ", &message, b"\n"].concat();
        // A stderr that cannot be written to leaves nothing to say it with.
        let _ = world.stderr.write_all(&line);
    }
    let _ = world.stdout.flush();
    let _ = world.stderr.flush();
    code
}

/// The value of the variable `name`, if it is set.
fn env_of(name: &str) -> Option<OsString> {
    #[expect(
        clippy::disallowed_methods,
        reason = "HOME is the operator's home, and the two ACTIONS_ID_TOKEN_REQUEST variables are \
                  set by the GitHub Actions runner for the job: secret material and a \
                  platform-set value, which AGENTS.md S2 allows"
    )]
    std::env::var_os(name)
}

/// The system's roots, as DER, for the runner's token request.
fn system_roots() -> Vec<Vec<u8>> {
    rustls_native_certs::load_native_certs()
        .certs
        .into_iter()
        .map(|cert| cert.as_ref().to_vec())
        .collect()
}

/// The program: `hk` on the process's arguments, standard streams and environment.
pub fn main() -> ExitCode {
    // The program's own name, whatever file it is run as, is `hk`: the first argument is skipped.
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    // Not locked: the ssh commands read and write the same streams through the runtime's own
    // handles, which would wait for a lock held here.
    let (mut stdin, mut stdout, mut stderr) =
        (std::io::stdin(), std::io::stdout(), std::io::stderr());
    let mut world = World {
        table: COMMANDS,
        stamp: stamp::version(),
        stdin: &mut stdin,
        stdout: &mut stdout,
        stderr: &mut stderr,
        home: env_of("HOME").map(PathBuf::from),
        uid: rustix::process::geteuid().as_raw(),
        oidc_url: env_of("ACTIONS_ID_TOKEN_REQUEST_URL").map(OsString::into_vec),
        oidc_token: env_of("ACTIONS_ID_TOKEN_REQUEST_TOKEN").map(OsString::into_vec),
        web_roots: &system_roots,
        ssh: &ssh::System,
        mac: &xcode_push::Mac::system(),
        tailnet: &tailnet_wait::Tailnet::system(),
    };
    ExitCode::from(run(&mut world, &args))
}
