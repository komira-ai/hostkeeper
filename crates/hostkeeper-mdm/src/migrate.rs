//! `hostkeeper-mdm migrate --from <old settings file>`: the live stack of the old kit moved to
//! this kit's names while no Mac is enrolled (docs/design/hostkeeper-mdm.md section 8.3).
//!
//! It reads the old kit's settings and state, as root (a dry run too), and refuses before any
//! change when: the effective uid is not 0; the new settings are refused by the core (`--profile-id`
//! or an ACME key is not accepted); the old and the new state, etc or bin directory are the same
//! or one is inside the other, or the two unit prefixes are the same; `--settings` is inside the
//! old state, etc or bin directory; the file at `--settings` exists with
//! other bytes than the new settings; a symbolic link or other kind is in a tree or at a
//! path that is copied; the old `dns.env` is not a 0600 file of root's; none of the old kit's
//! unit files is installed and none of this kit's is either (a wrong `--from`); or `install`'s
//! own checks fail (they apply to the new kit, see `install.rs`; a port held by an active unit of
//! the old kit is accepted there, since this plan stops that unit first).
//!
//! Nothing under the old state or the old etc directory is ever a step: they are read, and
//! never written, removed, chmodded or chowned. The old SCEP CA, the old secrets other than
//! `dns.env`, the lego and Caddy state and `www` are not read.
//!
//! The one plan has these steps, in this order, each only when the host differs:
//! 1. `install`'s plan up to its first systemctl step: the restart markers, the user, the
//!    directories, the binaries, the secrets, a fresh SCEP CA under `MDM_SCEP_CA_CN` of the new
//!    settings (the old CA is not copied: no device holds it), the unit files and the Caddyfile;
//! 2. the new settings file, root's, 0644 (it holds no secret). The same bytes already there are
//!    kept;
//! 3. the copies from the old state, each only when its target is absent (one that is there is
//!    kept and printed as kept: the new services change these files after the move), owned by
//!    the new user, directories 0700 and files with their mode cut to at most 0600:
//!    * the tree `nanohub/mdm/push_cert` (NanoHUB's push certificate and key, in NanoHUB's own
//!      layout, whatever it is), file by file;
//!    * `push-topic` and `push-cert.sha256`, root's, 0644, so that `push-cert --profile-only`
//!      finds the topic and knows NanoHUB has the certificate;
//!    * every regular file of `dep/dep_names` whose name ends with `.` and the old
//!      `MDM_DEP_NAME` (NanoDEP's ABM token, its key pair and the server name; the new settings
//!      keep `MDM_DEP_NAME`); the other names there are not copied and are named in the output;
//!    * `dns.env` of the old etc directory, root's, 0600, for `tls`.
//!
//!    A source that is not there is not a refusal: a line says what the operator does instead
//!    (`push-cert --cert --key`; `dep get-cert` and `dep upload-token`, with a token the owner
//!    of the ABM account downloads again; writing `dns.env`);
//! 4. the old units, those of the old prefix and slice whose file is in `/etc/systemd/system`,
//!    that are enabled (`systemctl is-enabled` says `enabled` or `enabled-runtime`; a `static`
//!    unit is not) or active: stopped and disabled in one step. Their unit files stay;
//! 5. the rest of `install`'s plan: `daemon-reload`, and the new services enabled and started.
//!
//! A run that fails before step 4 leaves the old stack running. After the plan (a dry run
//! included) the back-out is printed (stop the new units, start the old ones) with the next
//! steps: `tls`, `push-cert --profile-only`, `verify` and `dep define-profile`, each with
//! `--settings` of the new file.

use std::io::Write;

use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_core::migrate::{Old, Target, new_settings};
use hostkeeper_mdm_core::pins::Pins;
use hostkeeper_mdm_core::render::UnitNames;
use hostkeeper_mdm_step::{
    Account, Cmd, Host, Kind, Meta, Mode, Plan, Planner, SYSTEMCTL, Step, UnitVerb, execute,
};

use crate::install::{self, Install, UNITS_DIR};
use crate::{Env, MdmError, secret};

/// What `migrate` was asked for.
pub struct Migrate {
    /// The old kit's settings file.
    pub from: String,
    /// The old kit's settings, read from it.
    pub old: Old,
    /// Where the new settings file goes.
    pub settings: String,
    /// Where `fetch` put the archives.
    pub bin_dir: String,
    /// What the new settings take that the old file does not settle.
    pub target: Target,
    /// The largest archive read, in bytes.
    pub max_archive: u64,
}

/// Why `migrate` refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum MoveError {
    /// The old and the new kit share a directory or a unit prefix.
    #[error(
        "the old kit's {old} and the new kit's {new} are the same or one is inside the other: the old kit's state stays untouched"
    )]
    Overlap {
        /// The old kit's setting and value.
        old: String,
        /// The new kit's.
        new: String,
    },
    /// The new settings file is there with other bytes.
    #[error(
        "{path} is there with other settings than the ones this run writes: move it aside, or read it and run again with the arguments that make it"
    )]
    SettingsDiffer {
        /// The file.
        path: String,
    },
    /// Something to copy is of a kind that is not copied.
    #[error(
        "{path} is not a {want} (a symbolic link or other kind): it is not followed, and nothing is copied until it is dealt with"
    )]
    WrongKind {
        /// The path.
        path: String,
        /// What it must be.
        want: &'static str,
    },
    /// No unit of either kit is installed.
    #[error(
        "no unit file of the old kit (prefix {prefix}) and none of this kit's is in {UNITS_DIR}: wrong --from?"
    )]
    NoKit {
        /// The old kit's unit prefix.
        prefix: String,
    },
}

impl MoveError {
    /// The exit code: 2, since every case is a refused input or host.
    pub fn exit_code(&self) -> u8 {
        2
    }
}

/// Check the old and the new state, then print or carry out the move.
///
/// # Errors
/// [`MdmError`].
pub fn run(
    env: &Env<'_>,
    a: &Migrate,
    pins: &Pins,
    mode: Mode,
    out: &mut dyn Write,
) -> Result<(), MdmError> {
    let host = env.host;
    install::root_only(host, "migrate")?;
    let new = new_settings(&a.old, &a.target)?;
    let c = &new.config;
    apart(a, c)?;

    let prefix = a.old.unit_prefix();
    let old_units = UnitNames::with_prefix(prefix, &format!("{prefix}.slice"));
    let there = installed(host, &old_units)?;
    if there.is_empty() && installed(host, &UnitNames::new(c))?.is_empty() {
        return Err(MoveError::NoKit {
            prefix: prefix.to_owned(),
        }
        .into());
    }

    let install = Install {
        config: c.clone(),
        bin_dir: a.bin_dir.clone(),
        max_archive: a.max_archive,
        replacing: Some(old_units),
    };
    let base = install::plan(env, &install, pins, out)?.steps;
    let cut = base
        .iter()
        .position(|s| matches!(s, Step::Unit { .. }))
        .unwrap_or(base.len());

    let mut m = Mover::new(host);
    m.settings(&a.settings, &new.text, out)?;
    m.copy_state(a, c, out)?;
    let stopped = m.stop_old(&there, prefix, out)?;

    let mut steps = base[..cut].to_vec();
    steps.extend(m.p.finish().steps);
    steps.extend_from_slice(&base[cut..]);
    execute(&Plan { steps }, mode, host, out)?;
    finish(a, c, &stopped, out)
}

/// The old and the new kit share nothing: no directory is the same or inside the other, the
/// new settings file is not inside an old directory, and the unit prefixes differ.
fn apart(a: &Migrate, c: &MdmConfig) -> Result<(), MdmError> {
    let mut old = vec![
        ("MDM_STATE", a.old.state().as_str()),
        ("MDM_ETC", a.old.etc().as_str()),
    ];
    old.extend(a.old.get("MDM_BIN").map(|bin| ("MDM_BIN", bin)));
    let new = [
        ("MDM_STATE", c.state().as_str()),
        ("MDM_ETC", c.etc().as_str()),
        ("MDM_BIN", c.bin().as_str()),
    ];
    for (old_key, o) in &old {
        for (new_key, n) in &new {
            let inside = |x: &str, y: &str| x == y || x.starts_with(&format!("{y}/"));
            if inside(o, n) || inside(n, o) {
                return Err(MoveError::Overlap {
                    old: format!("{old_key} {o}"),
                    new: format!("{new_key} {n}"),
                }
                .into());
            }
        }
    }
    for (old_key, o) in &old {
        if a.settings.starts_with(&format!("{o}/")) {
            return Err(MoveError::Overlap {
                old: format!("{old_key} {o}"),
                new: format!("--settings {}", a.settings),
            }
            .into());
        }
    }
    if a.old.unit_prefix() == c.unit_prefix() {
        return Err(MoveError::Overlap {
            old: format!("MDM_UNIT_PREFIX {}", a.old.unit_prefix()),
            new: format!("MDM_UNIT_PREFIX {}", c.unit_prefix()),
        }
        .into());
    }
    Ok(())
}

/// Every name of `n` that has a unit file, in the order they are stopped: the timer and the
/// path unit, the services (the proxy first), the jobs, the slice.
fn installed(host: &dyn Host, n: &UnitNames) -> Result<Vec<String>, MdmError> {
    let all = [
        &n.reload_path,
        &n.cert_timer,
        &n.proxy,
        &n.depsyncer,
        &n.depserver,
        &n.nanohub,
        &n.scep,
        &n.cert_service,
        &n.reload_service,
        &n.slice,
    ];
    let mut there = Vec::new();
    for unit in all {
        if host.stat(&format!("{UNITS_DIR}/{unit}"))?.is_some() {
            there.push(unit.clone());
        }
    }
    Ok(there)
}

/// The steps between `install`'s first half and its second: the settings, the copies and the
/// old units' stop.
struct Mover<'h> {
    host: &'h dyn Host,
    p: Planner<'h>,
    user: Account,
    copied: usize,
    kept: usize,
}

impl<'h> Mover<'h> {
    fn new(host: &'h dyn Host) -> Mover<'h> {
        Mover {
            host,
            p: Planner::new(host),
            user: Account::new(
                hostkeeper_mdm_core::migrate::NEW_USER,
                hostkeeper_mdm_core::migrate::NEW_USER,
            ),
            copied: 0,
            kept: 0,
        }
    }

    /// The new settings file, unless it holds these bytes already.
    fn settings(&mut self, path: &str, text: &str, out: &mut dyn Write) -> Result<(), MdmError> {
        match self.host.read(path)? {
            Some(have) if have == text.as_bytes() => {
                writeln!(out, "new settings: kept ({path})")?;
            }
            Some(_) => {
                return Err(MoveError::SettingsDiffer {
                    path: path.to_owned(),
                }
                .into());
            }
            None => {
                self.p
                    .file(path, 0o644, &Account::root(), text.as_bytes())?;
            }
        }
        Ok(())
    }

    /// `path` as a regular file or a directory: `None` when nothing is there, an error for
    /// another kind.
    fn source(&self, path: &str, kind: Kind) -> Result<Option<Meta>, MdmError> {
        let want = match kind {
            Kind::Dir => "directory",
            _ => "regular file",
        };
        match self.host.stat(path)? {
            Some(meta) if meta.kind != kind => Err(MoveError::WrongKind {
                path: path.to_owned(),
                want,
            }
            .into()),
            found => Ok(found),
        }
    }

    /// The directory `dst` (0700, the new user's) unless it is there.
    fn make_dir(&mut self, dst: &str) -> Result<(), MdmError> {
        match self.host.stat(dst)? {
            None => {
                self.p.dir(dst, 0o700, &self.user)?;
                Ok(())
            }
            Some(meta) if meta.kind == Kind::Dir => Ok(()),
            Some(_) => Err(MoveError::WrongKind {
                path: dst.to_owned(),
                want: "directory",
            }
            .into()),
        }
    }

    /// The file `src` to `dst` with `mode` and `owner`, unless `dst` is there.
    fn file(
        &mut self,
        src: &str,
        dst: &str,
        (mode, owner): (u32, &Account),
        out: &mut dyn Write,
    ) -> Result<(), MdmError> {
        if self.host.stat(dst)?.is_some() {
            self.kept += 1;
            writeln!(out, "kept: {dst} (already there)")?;
            return Ok(());
        }
        let bytes = self
            .host
            .read(src)?
            .ok_or_else(|| MdmError::NoFile(src.to_owned()))?;
        self.p.file(dst, mode, owner, &bytes)?;
        self.copied += 1;
        Ok(())
    }

    /// The directory `src` and everything in it to `dst`, as the new user's, whatever the layout.
    fn tree(&mut self, src: &str, dst: &str, out: &mut dyn Write) -> Result<(), MdmError> {
        self.make_dir(dst)?;
        let names = self.host.list(src)?.unwrap_or_default();
        for name in names {
            let (from, to) = (format!("{src}/{name}"), format!("{dst}/{name}"));
            match self.host.stat(&from)? {
                Some(meta) if meta.kind == Kind::Dir => self.tree(&from, &to, out)?,
                Some(meta) if meta.kind == Kind::File => {
                    let user = self.user.clone();
                    self.file(&from, &to, (meta.mode & 0o600, &user), out)?;
                }
                _ => {
                    return Err(MoveError::WrongKind {
                        path: from,
                        want: "regular file or directory",
                    }
                    .into());
                }
            }
        }
        Ok(())
    }

    /// What the group `what` did, since the counts were `(copied, kept)`.
    fn summary(
        &self,
        what: &str,
        was: (usize, usize),
        out: &mut dyn Write,
    ) -> Result<(), MdmError> {
        let (copied, kept) = (self.copied - was.0, self.kept - was.1);
        writeln!(out, "{what}: {copied} file(s) copied, {kept} kept")?;
        Ok(())
    }

    /// The copies of the old state and of `dns.env`.
    fn copy_state(
        &mut self,
        a: &Migrate,
        c: &MdmConfig,
        out: &mut dyn Write,
    ) -> Result<(), MdmError> {
        let (old_state, new_state) = (a.old.state().as_str(), c.state().as_str());
        let root = Account::root();

        let was = (self.copied, self.kept);
        let src = format!("{old_state}/nanohub/mdm/push_cert");
        if self.source(&src, Kind::Dir)?.is_some() {
            self.make_dir(&format!("{new_state}/nanohub/mdm"))?;
            self.tree(&src, &format!("{new_state}/nanohub/mdm/push_cert"), out)?;
            self.summary("push certificate and key", was, out)?;
        } else {
            let line = format!(
                "no push certificate: {src} is not there; run push-cert --cert <pem> --key <pem> after the move"
            );
            writeln!(out, "{line}")?;
        }

        for name in ["push-topic", "push-cert.sha256"] {
            let src = format!("{old_state}/{name}");
            if self.source(&src, Kind::File)?.is_some() {
                self.file(&src, &format!("{new_state}/{name}"), (0o644, &root), out)?;
            } else {
                let line = format!(
                    "no {name}: {src} is not there; push-cert --cert <pem> --key <pem> writes it"
                );
                writeln!(out, "{line}")?;
            }
        }

        self.dep_names(a, new_state, out)?;

        let src = format!("{}/dns.env", a.old.etc());
        match self.source(&src, Kind::File)? {
            Some(meta) => {
                secret::check_mode(&src, meta.mode, meta.uid)?;
                let dst = format!("{}/dns.env", c.etc());
                self.file(&src, &dst, (0o600, &root), out)?;
            }
            None => {
                let line = format!(
                    "no dns.env: {src} is not there; write {}/dns.env (root, 0600) with the DNS provider's key before tls",
                    c.etc()
                );
                writeln!(out, "{line}")?;
            }
        }
        Ok(())
    }

    /// NanoDEP's files of the old DEP name.
    fn dep_names(
        &mut self,
        a: &Migrate,
        new_state: &str,
        out: &mut dyn Write,
    ) -> Result<(), MdmError> {
        let src = format!("{}/dep/dep_names", a.old.state());
        let suffix = format!(".{}", a.old.dep_name());
        let names = match self.source(&src, Kind::Dir)? {
            Some(_) => self.host.list(&src)?.unwrap_or_default(),
            None => Vec::new(),
        };
        let (ours, others): (Vec<String>, Vec<String>) =
            names.into_iter().partition(|n| n.ends_with(&suffix));
        if ours.is_empty() {
            let line = format!(
                "no DEP token: {src} holds no file named *{suffix}; run dep get-cert and dep upload-token after the move (the owner of the ABM account downloads the token again)"
            );
            writeln!(out, "{line}")?;
        } else {
            let was = (self.copied, self.kept);
            let dst = format!("{new_state}/dep/dep_names");
            self.make_dir(&dst)?;
            for name in &ours {
                let from = format!("{src}/{name}");
                let meta = self.source(&from, Kind::File)?;
                let mode = meta.map_or(0o600, |m| m.mode & 0o600);
                let user = self.user.clone();
                self.file(&from, &format!("{dst}/{name}"), (mode, &user), out)?;
            }
            self.summary("DEP token and keys", was, out)?;
        }
        if !others.is_empty() {
            let line = format!("not copied (not named *{suffix}): {}", others.join(", "));
            writeln!(out, "{line}")?;
        }
        Ok(())
    }

    /// The old units in `there` that are enabled or active stopped and disabled, in one step;
    /// their names.
    fn stop_old(
        &mut self,
        there: &[String],
        prefix: &str,
        out: &mut dyn Write,
    ) -> Result<Vec<String>, MdmError> {
        let mut running = Vec::new();
        for unit in there {
            if enabled(self.host, unit)? || self.p.unit_is("is-active", unit)? {
                running.push(unit.clone());
            }
        }
        if there.is_empty() {
            let line = format!("old kit: no unit file with prefix {prefix} (moved already)");
            writeln!(out, "{line}")?;
        } else if running.is_empty() {
            let line = "old kit: no unit is enabled or active; the unit files stay";
            writeln!(out, "{line}")?;
        } else {
            let refs: Vec<&str> = running.iter().map(String::as_str).collect();
            self.p.unit(UnitVerb::DisableNow, &refs);
            let line = format!(
                "old kit: stops and disables {}; the unit files stay",
                running.join(" ")
            );
            writeln!(out, "{line}")?;
        }
        Ok(running)
    }
}

/// Whether `unit` is enabled: `systemctl is-enabled` says `enabled` or `enabled-runtime`. A
/// `static` unit (a slice, a oneshot job a timer or a path unit starts) also answers 0, and is
/// not taken as enabled: it is stopped only when it is active.
fn enabled(host: &dyn Host, unit: &str) -> Result<bool, MdmError> {
    let out = host.run(&Cmd::new(SYSTEMCTL).arg("is-enabled").arg(unit))?;
    Ok(out.code == 0 && ["enabled", "enabled-runtime"].contains(&out.stdout.trim()))
}

/// The back-out and the next steps.
fn finish(
    a: &Migrate,
    c: &MdmConfig,
    stopped: &[String],
    out: &mut dyn Write,
) -> Result<(), MdmError> {
    let n = UnitNames::new(c);
    let untouched = format!(
        "the old kit's state {} and etc {} are untouched",
        a.old.state(),
        a.old.etc()
    );
    writeln!(out, "{untouched}")?;
    let new_off = format!(
        "back out: systemctl disable --now -- {} {} {} {}",
        n.proxy, n.depserver, n.nanohub, n.scep
    );
    writeln!(out, "{new_off}")?;
    if stopped.is_empty() {
        writeln!(out, "back out: no old unit was running, so none to start")?;
    } else {
        let old_on = format!("back out: systemctl enable --now -- {}", stopped.join(" "));
        writeln!(out, "{old_on}")?;
    }
    writeln!(out, "next, in this order:")?;
    let s = &a.settings;
    for line in [
        format!("hostkeeper-mdm tls --settings {s} --apply"),
        format!("hostkeeper-mdm push-cert --settings {s} --profile-only --apply"),
        format!("hostkeeper-mdm verify --settings {s}"),
        format!("hostkeeper-mdm dep define-profile --settings {s} --apply"),
    ] {
        writeln!(out, "  {line}")?;
    }
    Ok(())
}
