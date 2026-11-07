//! The command line: parsing, dispatch and the exit code.
//!
//! `verify` reads the ACME settings only when the settings file sets at least one `MDM_ACME_`
//! key; it then needs all five to parse, and refuses (exit 2) when they do not.

use std::io::Write;

use clap::{Parser, Subcommand};
use hostkeeper_mdm_core::acme::Acme;
use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_core::migrate::{NEW_STATE, NEW_USER, Old, Target};
use hostkeeper_mdm_core::pins::{BUILT_IN, Pins, Tool};
use hostkeeper_mdm_core::profile::Topic;
use hostkeeper_mdm_step::{Account, Host, Mode, ids};

use crate::dep::{self, Dep, Serial};
use crate::fetch::{self, Fetch};
use crate::install::{self, Install};
use crate::migrate::{self, Migrate};
use crate::push_cert::{self, PushCert, With};
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
    /// Give NanoHUB the APNs push certificate and its key, and write the enrollment profile the
    /// proxy serves at /enroll (root). With --profile-only, the profile alone.
    PushCert {
        /// The settings file.
        #[arg(long)]
        settings: String,
        /// The push certificate (PEM), from the Apple Push Certificates Portal.
        #[arg(long, requires = "key", conflicts_with = "profile_only")]
        cert: Option<String>,
        /// Its private key (PEM, unencrypted, mode 0600).
        #[arg(long, requires = "cert")]
        key: Option<String>,
        /// Write the enrollment profile only, for the recorded topic or --topic.
        #[arg(long, required_unless_present = "cert")]
        profile_only: bool,
        /// The push topic (com.apple.mgmt.External.<uuid>), in place of the recorded one.
        #[arg(long, requires = "profile_only", value_parser = topic)]
        topic: Option<Topic>,
        /// Act; without it the plan is printed.
        #[arg(long)]
        apply: bool,
    },
    /// Link NanoDEP to Apple Business Manager and define the ADE profile (root).
    Dep {
        #[command(subcommand)]
        verb: DepVerb,
    },
    /// Move the old kit's live stack to this kit's names, while no Mac is enrolled (root). The
    /// old state directory is never changed.
    Migrate {
        /// The old kit's settings file.
        #[arg(long)]
        from: String,
        /// Where the new settings file goes.
        #[arg(long)]
        settings: String,
        /// Where `fetch` put the archives.
        #[arg(long)]
        bin_dir: String,
        /// The final MDM profile identifier (MDM_PROFILE_ID), as com.example.hostkeeper.mdm.
        #[arg(long)]
        profile_id: String,
        /// The new SCEP CA's common name (MDM_SCEP_CA_CN); "<MDM_ORG> MDM SCEP CA" without it.
        #[arg(long)]
        scep_ca_cn: Option<String>,
        /// MDM_ACME_RESOLVERS of the new settings, in place of the old file's.
        #[arg(long)]
        acme_resolvers: Option<String>,
        /// MDM_ACME_PROPAGATION_WAIT of the new settings, in place of the old file's.
        #[arg(long)]
        acme_propagation_wait: Option<String>,
        /// Act; without it the plan is printed.
        #[arg(long)]
        apply: bool,
    },
}

/// The `dep` verbs, one per ABM step, in the order an operator takes them.
#[derive(Subcommand)]
enum DepVerb {
    /// NanoDEP's token certificate, written to --out, to upload in ABM.
    GetCert {
        /// The settings file.
        #[arg(long)]
        settings: String,
        /// Where the certificate goes, outside every git work tree.
        #[arg(long)]
        out: String,
        /// Act; without it the plan is printed.
        #[arg(long)]
        apply: bool,
    },
    /// The server token ABM gives for that certificate (.p7m), given to NanoDEP.
    UploadToken {
        /// The settings file.
        #[arg(long)]
        settings: String,
        /// The token file.
        #[arg(long)]
        token: String,
        /// Act; without it the plan is printed.
        #[arg(long)]
        apply: bool,
    },
    /// The ADE profile defined at Apple and given to new devices; depsyncer started.
    DefineProfile {
        /// The settings file.
        #[arg(long)]
        settings: String,
        /// Act; without it the plan is printed.
        #[arg(long)]
        apply: bool,
    },
    /// The recorded ADE profile assigned to devices already in ABM. Exit 1 unless Apple
    /// assigns every one.
    Assign {
        /// The settings file.
        #[arg(long)]
        settings: String,
        /// A device's serial number (repeatable).
        #[arg(long = "serial", required = true, value_parser = Serial::new)]
        serials: Vec<Serial>,
        /// Act; without it the plan is printed.
        #[arg(long)]
        apply: bool,
    },
}

impl DepVerb {
    /// The settings file, the verb and whether to act.
    fn split(self) -> (String, Dep, bool) {
        match self {
            DepVerb::GetCert {
                settings,
                out,
                apply,
            } => (settings, Dep::GetCert { out }, apply),
            DepVerb::UploadToken {
                settings,
                token,
                apply,
            } => (settings, Dep::UploadToken { token }, apply),
            DepVerb::DefineProfile { settings, apply } => (settings, Dep::DefineProfile, apply),
            DepVerb::Assign {
                settings,
                serials,
                apply,
            } => (settings, Dep::Assign { serials }, apply),
        }
    }
}

/// `--topic`, checked.
fn topic(text: &str) -> Result<Topic, String> {
    Topic::new(text).map_err(|e| e.to_string())
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

/// The ACME settings in `text`, a settings file `MdmConfig::parse` accepted: none when it sets
/// no `MDM_ACME_` key, otherwise all of them, and a key that is missing, empty or malformed is
/// an error. (Every line of an accepted file is a comment, blank, or `KEY=VALUE` of a known key,
/// so a line that starts with the prefix sets an ACME key.)
fn acme_settings(text: &str) -> Result<Option<Acme>, MdmError> {
    match text.lines().any(|l| l.starts_with("MDM_ACME_")) {
        true => Ok(Some(Acme::parse(text)?)),
        false => Ok(None),
    }
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

/// The grant of the directory that holds the operator's file `path` to that directory's owner,
/// when it is there: a key, a token or a certificate the operator keeps in a directory of their
/// own.
fn file_grant(system: &System<'_>, path: &str) -> Result<Vec<Grant>, MdmError> {
    let dir = match path.rsplit_once('/') {
        Some(("", _)) | None => return Ok(Vec::new()),
        Some((dir, _)) => dir,
    };
    Ok(system
        .machine
        .owner(dir)?
        .map(|uid| Grant {
            dir: dir.to_owned(),
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
        api: system.api,
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
            let f = Fetch {
                dir,
                only,
                max_archive: fetch::MAX_ARCHIVE,
            };
            fetch::run(&env, &f, pins, mode(apply), out)?;
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
            let acme = acme_settings(&text)?;
            let host = system.machine.host(&state_grant(base.as_ref(), &config)?);
            let v = Verify {
                config: &config,
                acme: acme.as_ref(),
                ignore: &ignore,
            };
            return verify::run(&env(system, host.as_ref()), &v, out);
        }
        Verb::PushCert {
            settings,
            cert,
            key,
            profile_only: _,
            topic,
            apply,
        } => {
            let config = MdmConfig::parse(&read_settings(base.as_ref(), &settings)?)?;
            let mut grants = state_grant(base.as_ref(), &config)?;
            let with = match (cert, key) {
                (Some(cert), Some(key)) => {
                    grants.extend(file_grant(system, &cert)?);
                    grants.extend(file_grant(system, &key)?);
                    With::Pair { cert, key }
                }
                _ => With::ProfileOnly(topic),
            };
            let host = system.machine.host(&grants);
            let a = PushCert { config, with };
            push_cert::run(&env(system, host.as_ref()), &a, mode(apply), out)?;
        }
        Verb::Dep { verb } => {
            let (settings, d, apply) = verb.split();
            let config = MdmConfig::parse(&read_settings(base.as_ref(), &settings)?)?;
            let mut grants = state_grant(base.as_ref(), &config)?;
            match &d {
                Dep::GetCert { out: file } => grants.extend(file_grant(system, file)?),
                Dep::UploadToken { token } => grants.extend(file_grant(system, token)?),
                Dep::DefineProfile | Dep::Assign { .. } => {}
            }
            let host = system.machine.host(&grants);
            return dep::run(&env(system, host.as_ref()), &config, &d, mode(apply), out);
        }
        Verb::Migrate {
            from,
            settings,
            bin_dir,
            profile_id,
            scep_ca_cn,
            acme_resolvers,
            acme_propagation_wait,
            apply,
        } => {
            let old = Old::parse(&read_settings(base.as_ref(), &from)?)?;
            let mut grants = Vec::new();
            // The old state is the old service user's, the new one the new user's once it
            // exists, and the archives the operator's.
            for (dir, user) in [(old.state().as_str(), old.user()), (NEW_STATE, NEW_USER)] {
                if let Some((uid, _)) = ids(base.as_ref(), &Account::new(user, user))? {
                    grants.push(Grant {
                        dir: dir.to_owned(),
                        uid,
                    });
                }
            }
            if let Some(uid) = system.machine.owner(&bin_dir)? {
                grants.push(Grant {
                    dir: bin_dir.clone(),
                    uid,
                });
            }
            let host = system.machine.host(&grants);
            let a = Migrate {
                from,
                old,
                settings,
                bin_dir,
                target: Target {
                    profile_id,
                    scep_ca_cn,
                    acme_resolvers,
                    acme_propagation_wait,
                },
                max_archive: fetch::MAX_ARCHIVE,
            };
            migrate::run(&env(system, host.as_ref()), &a, pins, mode(apply), out)?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::run_with_pin_text;
    use crate::{HttpLoopback, Net, RealMachine, System, system_random};

    /// Catches a pin table that does not parse being run with anyway, or failing silently: the
    /// run fails with exit 1 and says why, before it reads the arguments or the host.
    #[test]
    fn a_pin_table_that_does_not_parse_fails_the_run() {
        let net = Net::new(true);
        let api = HttpLoopback::new();
        let env = System {
            machine: &RealMachine,
            net: &net,
            api: &api,
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
