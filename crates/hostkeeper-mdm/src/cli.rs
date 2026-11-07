//! The command line: parsing, dispatch and the exit code.

use std::io::Write;

use clap::{Parser, Subcommand};
use hostkeeper_mdm_core::acme::Acme;
use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_core::pins::{BUILT_IN, Pins, Tool};
use hostkeeper_mdm_step::{Account, Host, Mode, ids};

use crate::fetch::{self, Fetch};
use crate::install::{self, Install};
use crate::verify::{self, Verify};
use crate::{Env, Grant, MdmError, System, tls};

/// hostkeeper-mdm: MDM for enrolling Macs as compute hosts, on a Linux host. Every verb prints its
/// plan by default; --apply acts. Exit 0 done, 1 a step or check failed, 2 refused.
#[derive(Parser)]
#[command(name = "hostkeeper-mdm", version)]
struct Cli {
    #[command(subcommand)]
    verb: Verb,
}

#[derive(Subcommand)]
enum Verb {
    /// Download the pinned upstream archives (as yourself, not root).
    Fetch {
        /// Where the archives go: absolute, outside every git work tree.
        #[arg(long)]
        dir: String,
        /// Only this tool (repeatable); every pinned tool by default.
        #[arg(long = "only", value_parser = Tool::named)]
        only: Vec<Tool>,
        /// Download; without it the plan is printed.
        #[arg(long)]
        apply: bool,
    },
    /// Install the services (root), or with --uninstall take their units away.
    Install {
        /// The settings file.
        #[arg(long)]
        settings: String,
        /// Where `fetch` put the archives.
        #[arg(
            long,
            required_unless_present = "uninstall",
            conflicts_with = "uninstall"
        )]
        bin_dir: Option<String>,
        /// Stop and disable the units and move their files aside; state and secrets stay.
        #[arg(long)]
        uninstall: bool,
        /// Act; without it the plan is printed.
        #[arg(long)]
        apply: bool,
    },
    /// The TLS certificate over ACME DNS-01 and its renewal (root). Only the checks before the
    /// plan refuse without changing anything: the plan runs in two parts, and when the
    /// certificate is not there after the issuing unit ran, the run fails (exit 1) with the
    /// units already written; run it again once the cause is fixed.
    Tls {
        /// The settings file.
        #[arg(long)]
        settings: String,
        /// Act; without it the plan is printed.
        #[arg(long)]
        apply: bool,
    },
    /// Read-only health checks, and installed files compared with the render.
    Verify {
        /// The settings file.
        #[arg(long)]
        settings: String,
        /// Leave out of both texts of every file the lines equal to this one, whole
        /// (repeatable; every line left out is printed).
        #[arg(long)]
        ignore: Vec<String>,
    },
}

fn mode(apply: bool) -> Mode {
    match apply {
        true => Mode::Apply,
        false => Mode::DryRun,
    }
}

/// The settings file's text.
fn read_settings(host: &dyn Host, path: &str) -> Result<String, MdmError> {
    let bytes = host
        .read(path)?
        .ok_or_else(|| MdmError::NoFile(path.to_owned()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The grant of the state directory to the service user, when the user exists: the state is
/// that user's, 0700.
fn state_grant(host: &dyn Host, c: &MdmConfig) -> Result<Vec<Grant>, MdmError> {
    let user = Account::new(c.user(), c.user());
    Ok(ids(host, &user)?
        .map(|(uid, _)| Grant {
            dir: c.state().to_string(),
            uid,
        })
        .into_iter()
        .collect())
}

/// Runs the verb `args` (the program's name first) names, with the built-in pins. The exit
/// code.
pub fn run(args: &[String], system: &System<'_>, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    run_with_pin_text(args, system, BUILT_IN, out, err)
}

/// [`run_with`] the pins `text` holds; a pin table that does not parse fails the run (exit 1,
/// `FAILED: ...` on `err`) before the arguments are read.
fn run_with_pin_text(
    args: &[String],
    system: &System<'_>,
    text: &str,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    match Pins::parse(text) {
        Ok(pins) => run_with(args, system, &pins, out, err),
        Err(e) => {
            let _ = writeln!(err, "FAILED: the built-in pins: {e}");
            1
        }
    }
}

/// [`run`] with other pins.
pub fn run_with(
    args: &[String],
    system: &System<'_>,
    pins: &Pins,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            let sink: &mut dyn Write = if e.use_stderr() { err } else { out };
            let _ = write!(sink, "{}", e.render());
            return u8::try_from(e.exit_code()).unwrap_or(2);
        }
    };
    match dispatch(cli.verb, system, pins, out) {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            let word = if e.exit_code() == 2 {
                "REFUSED"
            } else {
                "FAILED"
            };
            let _ = writeln!(err, "{word}: {e}");
            e.exit_code()
        }
    }
}

/// The verb's [`Env`] over `host`.
fn env<'a>(system: &System<'a>, host: &'a dyn Host) -> Env<'a> {
    Env {
        host,
        net: system.net,
        random: system.random,
        now: system.now,
    }
}

/// Carries the verb out; false when a check failed.
fn dispatch(
    verb: Verb,
    system: &System<'_>,
    pins: &Pins,
    out: &mut dyn Write,
) -> Result<bool, MdmError> {
    let base = system.machine.host(&[]);
    match verb {
        Verb::Fetch { dir, only, apply } => {
            let env = env(system, base.as_ref());
            fetch::run(&env, &Fetch { dir, only }, pins, mode(apply), out)?;
        }
        Verb::Install {
            settings,
            bin_dir,
            uninstall,
            apply,
        } => {
            let config = MdmConfig::parse(&read_settings(base.as_ref(), &settings)?)?;
            let mut grants = state_grant(base.as_ref(), &config)?;
            match (uninstall, bin_dir) {
                (false, Some(bin_dir)) => {
                    // The archives are the operator's, fetched as their own user.
                    if let Some(uid) = system.machine.owner(&bin_dir)? {
                        grants.push(Grant {
                            dir: bin_dir.clone(),
                            uid,
                        });
                    }
                    let host = system.machine.host(&grants);
                    let a = Install {
                        config,
                        bin_dir,
                        max_archive: fetch::MAX_ARCHIVE,
                    };
                    install::run(&env(system, host.as_ref()), &a, pins, mode(apply), out)?;
                }
                _ => {
                    let host = system.machine.host(&grants);
                    install::run_uninstall(&env(system, host.as_ref()), &config, mode(apply), out)?;
                }
            }
        }
        Verb::Tls { settings, apply } => {
            let text = read_settings(base.as_ref(), &settings)?;
            let (config, acme) = (MdmConfig::parse(&text)?, Acme::parse(&text)?);
            let host = system.machine.host(&state_grant(base.as_ref(), &config)?);
            tls::run(
                &env(system, host.as_ref()),
                &config,
                &acme,
                mode(apply),
                out,
            )?;
        }
        Verb::Verify { settings, ignore } => {
            let text = read_settings(base.as_ref(), &settings)?;
            let config = MdmConfig::parse(&text)?;
            let acme = Acme::parse(&text).ok();
            let host = system.machine.host(&state_grant(base.as_ref(), &config)?);
            let v = Verify {
                config: &config,
                acme: acme.as_ref(),
                ignore: &ignore,
            };
            return verify::run(&env(system, host.as_ref()), &v, out);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::run_with_pin_text;
    use crate::{Net, RealMachine, System, system_random};

    /// Catches a pin table that does not parse being run with anyway, or failing silently: the
    /// run fails with exit 1 and says why, before it reads the arguments or the host.
    #[test]
    fn a_pin_table_that_does_not_parse_fails_the_run() {
        let net = Net::new(true);
        let env = System {
            machine: &RealMachine,
            net: &net,
            random: &system_random,
            now: 0,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let args = ["hostkeeper-mdm".to_owned(), "--help".to_owned()];
        let code = run_with_pin_text(&args, &env, "caddy\n", &mut out, &mut err);
        assert_eq!(code, 1);
        assert!(out.is_empty());
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "FAILED: the built-in pins: pins line 1: 1 fields, not 4 (tool version url sha256)\n"
        );
    }
}
