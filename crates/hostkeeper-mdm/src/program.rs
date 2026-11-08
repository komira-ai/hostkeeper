//! `mdm`, the program of the `mdm` service package (`bin/mdm`, docs/design/two-programs.md
//! section 6.2): the package's update hook and the operator's Apple-facing verbs, run by root on
//! the MDM host as `/var/lib/hostkeeper/packages/mdm/current/bin/mdm`.
//!
//! - `mdm update-hook <step> [<staged release>] --params <file>`: the hook hostkeeper's daemon
//!   runs ([`crate::hook`]).
//! - `mdm push-cert --settings FILE (--cert C --key K | --profile-only [--topic T]) [--apply]`
//!   and `mdm dep get-cert|upload-token|define-profile|assign ...`: the verbs of
//!   `hostkeeper-mdm` of the same names ([`crate::push_cert`], [`crate::dep`]), with the same
//!   flags and the same dry run unless `--apply`. The settings file is read as the package's: a
//!   key the package fixes ([`hostkeeper_mdm_core::package::FIXED`]) may be left out, and is
//!   refused with another value.
//!
//! Exit codes as `hostkeeper-mdm`'s for the verbs (0 done, 1 failed, 2 refused), and the hook
//! contract's for `update-hook`.

use std::ffi::OsString;
use std::io::{self, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::cli::{self, DepVerb, Layout, PushCertArgs};
use crate::{System, hook};

/// mdm: the program of the mdm service package: its update hook, and the Apple-facing verbs an
/// operator runs as root on the MDM host. push-cert and dep print their plan by default and act
/// with --apply; update-hook carries out the step it is given at once.
#[derive(Parser)]
#[command(name = "mdm", version = crate::stamp::version())]
struct Cli {
    #[command(subcommand)]
    verb: Verb,
}

#[derive(Subcommand)]
enum Verb {
    /// The update hook hostkeeper runs as root: <step> [<staged release>] --params <file>.
    UpdateHook {
        /// The step and its arguments, as the hook contract gives them.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// Give NanoHUB the APNs push certificate and its key, and write the enrollment profile the
    /// proxy serves at /enroll (root). With --profile-only, the profile alone.
    PushCert(PushCertArgs),
    /// Link NanoDEP to Apple Business Manager and define the ADE profile (root).
    Dep {
        #[command(subcommand)]
        verb: DepVerb,
    },
}

/// Runs the command line `args` (the program's name first). The exit code.
pub fn run(args: &[OsString], system: &System<'_>, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            let sink: &mut dyn Write = if e.use_stderr() { err } else { out };
            let _ = write!(sink, "{}", e.render());
            return u8::try_from(e.exit_code()).unwrap_or(2);
        }
    };
    match cli.verb {
        Verb::UpdateHook { args } => hook::run(&args, system, out, err),
        Verb::PushCert(a) => cli::exit(cli::push_cert(system, a, Layout::Package, out), err),
        Verb::Dep { verb } => cli::exit(cli::dep(system, verb, Layout::Package, out), err),
    }
}

/// The program's entry: the arguments, the machine, exit as documented.
pub fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    crate::system::with_machine(|system| {
        ExitCode::from(run(
            &args,
            system,
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        ))
    })
}
