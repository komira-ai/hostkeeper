//! `hostkeeper-mdm install`: NanoHUB, the SCEP server, NanoDEP and the HTTPS proxy as capped system
//! services, as root.
//!
//! It checks the host first, read only, and refuses before anything changes: the effective uid
//! is not 0 (the plan reads root-only files, so a dry run needs root too); `uname -m` is not
//! `x86_64` (the pins are linux-amd64 builds); `MDM_STATE`,
//! `MDM_ETC`, `MDM_BIN` or `MDM_PKG_DIR` is a symbolic link; no interface carries `MDM_BIND`; a port it uses
//! is held by another process (one held by the kit's own active unit is a rerun, and, when
//! `migrate` sets `Install::replacing`, one held by the replaced kit's active unit of the same
//! role is accepted, since `migrate` stops that unit before the new one starts); the SCEP depot
//! holds only one of `ca.pem` and `ca.key`, or both but not as two regular files that
//! `openssl` reads (the key with the empty password `scepserver ca -init` encrypts it with)
//! and that hold one key pair; an archive in `--bin-dir` is missing or its
//! sha256 is not its pin; a secret file is not mode 0600 or not root's.
//!
//! The plan has each of these only when the host differs:
//! 1. the user `MDM_USER` (a system user and group of its name, no login);
//! 2. `MDM_STATE` and its subdirectories, `MDM_USER`'s, 0700; `MDM_ETC`, root and `MDM_USER`'s
//!    group, 0750; `MDM_BIN`, root's, 0755; `MDM_PKG_DIR`, the pkgs the proxy serves at
//!    `/pkg/<sha256>.pkg`, root's, 0755 (a release pkg is public, and only root adds one);
//! 3. each binary unpacked from its checked archive into `MDM_BIN`, root's, 0755, the one it
//!    replaces kept as `<name>.previous`;
//! 4. the secrets, each made once from 32 random bytes as hex and never printed:
//!    `NANOHUB_API_KEY` (`nanohub.env`), `NANODEP_API` (`nanodep.env`) and
//!    `SCEP_CHALLENGE_PASSWORD` (`scep.env`), in 0600 root files under `MDM_ETC`;
//! 5. the SCEP CA (`scepserver ca -init`, as `MDM_USER`), unless the depot holds one (both
//!    files, checked above): an existing CA is kept, since replacing it re-enrolls every Mac;
//! 6. the slice and the service units under `/etc/systemd/system` (root's, 0644) and the
//!    Caddyfile (root and `MDM_USER`'s group, 0640), exactly as `hostkeeper-mdm-core` renders them;
//! 7. `daemon-reload` when a unit changed; the SCEP server, NanoHUB and depserver enabled, and
//!    restarted when their unit, the slice, their binary or their secret changed, else started
//!    when not active; the proxy likewise, but only once the TLS certificate is there;
//!    depsyncer is left alone (it is enabled once an Apple token exists).
//!
//! A service whose inputs change in the plan is restarted by it. So that a run which stops
//! before the restart does not lose it, the plan begins with an empty 0600 root file
//! `/run/<MDM_UNIT_PREFIX>/restart/<unit>` for each such service that has none yet (the
//! directory is made 0700 when a file is), and removes the file of each service right after
//! that service's restart. A service whose file is there when the plan is made is
//! restarted whether or not anything changed in this run (the proxy only once the TLS
//! certificate is there). `/run` is a tmpfs: a reboot starts every service with the files in
//! place, so the files go with it.
//!
//! `--uninstall` stops and disables every unit of the kit that is installed (the timer, the
//! path unit and the services in one command), then stops the certificate job and the reload
//! job where installed (one that is running would go on after its file is moved), removes the
//! pending-restart files of the services, and moves
//! the unit files to `MDM_STATE/removed/<unix seconds>/` (a 0700 directory of `MDM_USER`'s;
//! when that name exists the first free `<unix seconds>-1`, `-2`, ... is used), then reloads
//! systemd. The state (database, SCEP CA, DEP token, TLS key and certificate), the secrets,
//! the binaries, the pkg directory and the user stay.

use std::collections::BTreeSet;
use std::io::{self, Write};

use hostkeeper_mdm_core::commands::scep_ca_init_args;
use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_core::pins::Pins;
use hostkeeper_mdm_core::render::{self, UnitNames};
use hostkeeper_mdm_step::{
    Account, Cmd, Host, Kind, Plan, PlanError, Planner, Step, UnitVerb, execute,
};
use sha2::{Digest, Sha256};

use crate::{Env, MdmError, archive, secret};

/// Where systemd reads local units from.
pub const UNITS_DIR: &str = "/etc/systemd/system";
const IP: &str = "/usr/bin/ip";
/// The machine-hardware query.
pub const UNAME: &str = "/usr/bin/uname";
/// What `uname -m` prints on the hosts the pinned tools are built for.
const SUPPORTED_MACHINE: &str = "x86_64";
const OPENSSL: &str = "/usr/bin/openssl";
/// How openssl is given the password of the SCEP CA's key: the empty one. `scepserver ca -init`
/// encrypts `ca.key` (DES-EDE3-CBC PEM) with its `-key-password`, which the kit does not pass,
/// and the SCEP unit reads it with `-capass`, which it does not pass either. Given on the
/// command line, openssl never asks for it on the terminal (which a run without one fails).
const SCEP_KEY_PASSIN: &str = "pass:";
/// Where pending-restart markers live: a tmpfs, so a reboot (which restarts every service
/// with the files in place) clears them.
const RUN_DIR: &str = "/run";
/// The socket lister.
pub const SS_PATH: &str = "/usr/bin/ss";
/// The secret files and the key each holds.
pub const SECRETS: [(&str, &str); 3] = [
    ("nanohub.env", "NANOHUB_API_KEY"),
    ("nanodep.env", "NANODEP_API"),
    ("scep.env", "SCEP_CHALLENGE_PASSWORD"),
];
pub(crate) const STATE_DIRS: [&str; 7] = [
    "nanohub",
    "scep",
    "scep/depot",
    "dep",
    "lego",
    "caddy",
    "www",
];

/// What `install` was asked for.
pub struct Install {
    /// The settings.
    pub config: MdmConfig,
    /// Where `fetch` put the archives.
    pub bin_dir: String,
    /// The largest archive read, in bytes.
    pub max_archive: u64,
    /// The kit this one replaces (`migrate`): a port held by that kit's active unit of the same
    /// role is accepted, since the plan of `migrate` stops that unit first. `None` for `install`.
    pub replacing: Option<UnitNames>,
}

/// Run `cmd` and return its standard output, or why it failed.
pub fn checked(host: &dyn Host, cmd: &Cmd) -> Result<String, MdmError> {
    let out = host.run(cmd)?;
    match out.code {
        0 => Ok(out.stdout),
        code => Err(MdmError::Command {
            line: cmd.line(),
            code,
        }),
    }
}

/// [`plan`], then print or carry the plan out.
pub fn run(
    env: &Env<'_>,
    a: &Install,
    pins: &Pins,
    mode: hostkeeper_mdm_step::Mode,
    out: &mut dyn Write,
) -> Result<(), MdmError> {
    let p = plan(env, a, pins, out)?;
    Ok(execute(&p, mode, env.host, out)?)
}

/// [`plan_uninstall`], then print or carry the plan out.
pub fn run_uninstall(
    env: &Env<'_>,
    c: &MdmConfig,
    mode: hostkeeper_mdm_step::Mode,
    out: &mut dyn Write,
) -> Result<(), MdmError> {
    let p = plan_uninstall(env, c, out)?;
    Ok(execute(&p, mode, env.host, out)?)
}

pub(crate) fn root_only(host: &dyn Host, verb: &'static str) -> Result<(), MdmError> {
    match host.euid() {
        0 => Ok(()),
        euid => Err(MdmError::NeedsRoot { verb, euid }),
    }
}

/// Check the host, then plan the user, directories, binaries, secrets, CA, units and services.
pub fn plan(
    env: &Env<'_>,
    a: &Install,
    pins: &Pins,
    out: &mut dyn Write,
) -> Result<Plan, MdmError> {
    let (c, host) = (&a.config, env.host);
    preflight(host, c, a.replacing.as_ref(), out)?;
    let binaries = binaries(host, &a.bin_dir, pins, a.max_archive)?;
    writeln!(out, "archives: every one matches its pin")?;

    let (state, etc, bin) = (c.state().as_str(), c.etc().as_str(), c.bin().as_str());
    let (root, mine) = (Account::root(), Account::new(c.user(), c.user()));
    let etc_owner = Account::new("root", c.user());
    let mut p = Planner::new(host);
    p.user(c.user(), state)?;
    p.dir(state, 0o700, &mine)?;
    for d in STATE_DIRS {
        p.dir(&format!("{state}/{d}"), 0o700, &mine)?;
    }
    p.dir(etc, 0o750, &etc_owner)?;
    p.dir(bin, 0o755, &root)?;
    p.dir(c.pkg_dir().as_str(), 0o755, &root)?;

    let mut changed: BTreeSet<String> = BTreeSet::new();
    for (name, bytes) in binaries {
        let path = format!("{bin}/{name}");
        // The binary a new one replaces is kept beside it.
        if let Some(old) = host.read(&path)?.filter(|old| *old != bytes) {
            p.file(&format!("{path}.previous"), 0o755, &root, &old)?;
        }
        if p.file(&path, 0o755, &root, &bytes)? {
            changed.insert(name.to_owned());
        }
    }
    for (file, key) in SECRETS {
        if secret_file(env, &mut p, &format!("{etc}/{file}"), key, out)? {
            changed.insert(file.to_owned());
        }
    }
    // `preflight` has refused a depot that is part of a CA or damaged.
    let ca = format!("{state}/scep/depot/ca.pem");
    if host.stat(&ca)?.is_some() {
        writeln!(out, "SCEP CA: kept ({ca})")?;
    } else {
        let init = Cmd::new(&format!("{bin}/scepserver")).args(&scep_ca_init_args(c));
        p.run(init.as_user(c.user()));
    }

    let rendered = render::render(c)?;
    let mut units_changed = false;
    for unit in &rendered.units {
        let path = format!("{UNITS_DIR}/{}", unit.name);
        if p.file(&path, 0o644, &root, unit.text.as_bytes())? {
            units_changed = true;
            changed.insert(unit.name.clone());
        }
    }
    let caddyfile = format!("{etc}/{}", render::CADDYFILE_NAME);
    if p.file(&caddyfile, 0o640, &etc_owner, rendered.caddyfile.as_bytes())? {
        changed.insert(render::CADDYFILE_NAME.to_owned());
    }

    if units_changed {
        p.unit(UnitVerb::DaemonReload, &[]);
    }
    let n = UnitNames::new(c);
    let mut restarts = Restarts::new(host, c);
    let core = [
        (&n.scep, "scepserver", "scep.env"),
        (&n.nanohub, "nanohub", "nanohub.env"),
        (&n.depserver, "depserver", "nanodep.env"),
    ];
    for (unit, binary, env_file) in core {
        let inputs = [n.slice.as_str(), unit.as_str(), binary, env_file];
        let changed_now = inputs.iter().any(|k| changed.contains(*k));
        restarts.service(&mut p, unit, changed_now)?;
    }
    let cert = format!("{state}/lego/certificates/{}.crt", c.dns_name());
    if host.stat(&cert)?.is_some() {
        let inputs = [
            n.slice.as_str(),
            n.proxy.as_str(),
            "caddy",
            render::CADDYFILE_NAME,
        ];
        let changed_now = inputs.iter().any(|k| changed.contains(*k));
        restarts.service(&mut p, &n.proxy, changed_now)?;
    } else {
        let waits = format!("proxy: not started; it waits for the TLS certificate {cert}");
        writeln!(out, "{waits}")?;
    }
    Ok(restarts.wrap(p.finish())?)
}

/// The pending-restart markers of one run: `<RUN_DIR>/<MDM_UNIT_PREFIX>/restart/<unit>`, an
/// empty 0600 root file made before the first change of the run for each unit whose inputs the
/// run changes, and removed right after that unit's restart. A run that stops in between leaves them, and
/// the next run restarts the units they name even when it changes nothing.
struct Restarts<'h> {
    host: &'h dyn Host,
    dir: String,
    /// The markers the plan makes.
    make: Vec<String>,
    /// The markers the plan removes, those of the units it restarts.
    clear: Vec<String>,
}

impl<'h> Restarts<'h> {
    fn new(host: &'h dyn Host, c: &MdmConfig) -> Restarts<'h> {
        Restarts {
            host,
            dir: format!("{RUN_DIR}/{}/restart", c.unit_prefix()),
            make: Vec::new(),
            clear: Vec::new(),
        }
    }

    /// Plan `unit` enabled, and restarted when `changed_now` (an input of it differs on the
    /// host) or its marker is there from a run that stopped; else started when not active.
    fn service(
        &mut self,
        p: &mut Planner<'_>,
        unit: &str,
        changed_now: bool,
    ) -> Result<(), MdmError> {
        let marker = format!("{}/{unit}", self.dir);
        let pending = self.host.stat(&marker)?.is_some();
        if changed_now && !pending {
            self.make.push(marker.clone());
        }
        let again = changed_now || pending;
        if again {
            self.clear.push(marker);
        }
        p.service(unit, again)?;
        Ok(())
    }

    /// `plan` with the markers made before its first step, each removed after its restart.
    fn wrap(self, plan: Plan) -> Result<Plan, PlanError> {
        let mut before = Planner::new(self.host);
        if !self.make.is_empty() {
            before.dir(&self.dir, 0o700, &Account::root())?;
        }
        let mut steps = before.finish().steps;
        steps.extend(self.make.into_iter().map(|path| Step::File {
            path,
            mode: 0o600,
            owner: Account::root(),
            bytes: Vec::new(),
        }));
        // Each marker goes right after its unit's restart, so a run that stops at a later
        // restart leaves only the markers of the units it did not restart.
        for step in plan.steps {
            let cleared: Vec<String> = match &step {
                Step::Unit {
                    verb: UnitVerb::Restart,
                    units,
                } => units
                    .iter()
                    .map(|u| format!("{}/{u}", self.dir))
                    .filter(|m| self.clear.contains(m))
                    .collect(),
                _ => Vec::new(),
            };
            steps.push(step);
            steps.extend(cleared.into_iter().map(|path| Step::Absent { path }));
        }
        Ok(Plan { steps })
    }
}

/// The host checks, read only.
pub(crate) fn preflight(
    host: &dyn Host,
    c: &MdmConfig,
    replacing: Option<&UnitNames>,
    out: &mut dyn Write,
) -> Result<(), MdmError> {
    root_only(host, "install")?;
    architecture(host)?;
    for path in [&c.state(), &c.etc(), &c.bin(), &c.pkg_dir()] {
        let link = host.stat(path.as_str())?.map(|m| m.kind) == Some(Kind::Other);
        if link {
            return Err(MdmError::IsLink(path.to_string()));
        }
    }
    // `ip -o -4 addr show`: `<n>: <if>    inet <addr>/<bits> brd ...`, one per line.
    let addrs = checked(host, &Cmd::new(IP).args(&["-o", "-4", "addr", "show"]))?;
    let bind = c.bind().to_string();
    let carried = addrs.lines().any(|l| {
        let mut words = l.split_whitespace();
        let after_inet = words.by_ref().find(|w| *w == "inet").and(words.next());
        after_inet.and_then(|a| a.split('/').next()) == Some(bind.as_str())
    });
    if !carried {
        return Err(MdmError::NotCarried(bind));
    }
    // `ss -Hltn`: `<state> <recv-q> <send-q> <local address:port> <peer>`.
    let listening = checked(host, &Cmd::new(SS_PATH).arg("-Hltn"))?;
    let n = UnitNames::new(c);
    for (port, unit, replaced) in [
        (c.nanohub_port(), &n.nanohub, replacing.map(|r| &r.nanohub)),
        (c.scep_port(), &n.scep, replacing.map(|r| &r.scep)),
        (c.dep_port(), &n.depserver, replacing.map(|r| &r.depserver)),
        (443, &n.proxy, replacing.map(|r| &r.proxy)),
    ] {
        let tail = format!(":{port}");
        let held = listening.lines().any(|l| {
            l.split_whitespace()
                .nth(3)
                .is_some_and(|local| local.ends_with(&tail))
        });
        if held {
            if is_active(host, unit)? {
                writeln!(out, "port {port}: held by {unit} (a rerun)")?;
                continue;
            }
            match replaced {
                Some(old) if is_active(host, old)? => {
                    let line = format!("port {port}: held by {old} (stopped first by this plan)");
                    writeln!(out, "{line}")?;
                }
                _ => return Err(MdmError::PortHeld(port)),
            }
        }
    }
    scep_depot(host, c)
}

/// Whether `systemctl is-active <unit>` succeeds.
pub(crate) fn is_active(host: &dyn Host, unit: &str) -> Result<bool, MdmError> {
    let cmd = Cmd::new(hostkeeper_mdm_step::SYSTEMCTL)
        .arg("is-active")
        .arg(unit);
    Ok(host.run(&cmd)?.code == 0)
}

/// The host must be x86-64: the pinned tools are linux-amd64 builds.
fn architecture(host: &dyn Host) -> Result<(), MdmError> {
    let cmd = Cmd::new(UNAME).arg("-m");
    let out = host.run(&cmd)?;
    match (out.code, out.stdout.trim()) {
        (0, SUPPORTED_MACHINE) => Ok(()),
        (0, machine) => Err(MdmError::Architecture {
            machine: machine.to_owned(),
        }),
        (code, _) => Err(MdmError::Command {
            line: cmd.line(),
            code,
        }),
    }
}

/// The SCEP depot holds no CA, or a whole one: `scepserver ca -init` writes `ca.key`, then
/// `ca.pem`, each only if it is not there, so a depot with one of them is a failed init or a
/// damaged backup, and one with both must be two regular files that openssl reads and that
/// hold one key pair. The key is read with the password the kit makes it with
/// ([`SCEP_KEY_PASSIN`]), so a key encrypted with another password is refused as unreadable.
/// Nothing printed by openssl is kept.
fn scep_depot(host: &dyn Host, c: &MdmConfig) -> Result<(), MdmError> {
    let depot = format!("{}/scep/depot", c.state());
    let (pem, key) = (format!("{depot}/ca.pem"), format!("{depot}/ca.key"));
    let (have_pem, have_key) = (host.stat(&pem)?, host.stat(&key)?);
    let damaged = |why: &str| MdmError::DepotDamaged {
        depot: depot.clone(),
        why: why.to_owned(),
    };
    match (have_pem, have_key) {
        (None, None) => Ok(()),
        (Some(_), None) => Err(MdmError::DepotPartial {
            depot,
            present: "ca.pem",
            missing: "ca.key",
        }),
        (None, Some(_)) => Err(MdmError::DepotPartial {
            depot,
            present: "ca.key",
            missing: "ca.pem",
        }),
        (Some(pem_meta), Some(key_meta)) => {
            for (name, meta) in [("ca.pem", pem_meta), ("ca.key", key_meta)] {
                if meta.kind != Kind::File {
                    return Err(damaged(&format!("{name} is not a regular file")));
                }
            }
            let cert_key = host.run(&Cmd::new(OPENSSL).args(&[
                "x509",
                "-noout",
                "-pubkey",
                "-in",
                pem.as_str(),
            ]))?;
            if cert_key.code != 0 {
                return Err(damaged("openssl cannot read ca.pem as a certificate"));
            }
            let own_key = host.run(&Cmd::new(OPENSSL).args(&[
                "pkey",
                "-passin",
                SCEP_KEY_PASSIN,
                "-pubout",
                "-in",
                key.as_str(),
            ]))?;
            if own_key.code != 0 {
                return Err(damaged("openssl cannot read ca.key as a private key"));
            }
            match cert_key.stdout == own_key.stdout {
                true => Ok(()),
                false => Err(damaged(
                    "ca.pem and ca.key are not a pair (the certificate's public key is not the key's)",
                )),
            }
        }
    }
}

/// Every pinned archive in `dir` checked against its pin, and the binaries unpacked from them:
/// `(installed name, bytes)`.
fn binaries(
    host: &dyn Host,
    dir: &str,
    pins: &Pins,
    max: u64,
) -> Result<Vec<(&'static str, Vec<u8>)>, MdmError> {
    let mut out = Vec::new();
    for pin in pins.all() {
        let path = format!("{dir}/{}", pin.archive());
        // One bounded read of one open file: a name swapped for a link cannot get past the cap.
        let read = host.read_limited(&path, max).map_err(|e| match e.kind() {
            io::ErrorKind::FileTooLarge => MdmError::ArchiveTooBig {
                path: path.clone(),
                max,
            },
            _ => MdmError::Io(e),
        });
        let bytes = read?.ok_or_else(|| MdmError::NoArchive(path.clone()))?;
        let got = hex::encode(Sha256::digest(&bytes));
        if got != pin.sha256_hex() {
            return Err(MdmError::ArchiveDiffers {
                path,
                got,
                want: pin.sha256_hex(),
            });
        }
        for (member, name) in pin.members() {
            out.push((
                name,
                archive::member(pin.archive(), pin.packing, &bytes, &member)?,
            ));
        }
    }
    Ok(out)
}

/// Plan the secret file `path` holding `key`, unless it holds a value already (then it must be
/// root's, 0600); whether one is made. The value is never printed.
pub(crate) fn secret_file(
    env: &Env<'_>,
    p: &mut Planner<'_>,
    path: &str,
    key: &str,
    out: &mut dyn Write,
) -> Result<bool, MdmError> {
    let host = env.host;
    let existing = host.read(path)?.unwrap_or_default();
    // A file systemd would not read with certainty is refused by its cause, never made again.
    secret::check_text(path, &existing)?;
    if secret::has_value(&existing, key) {
        let (mode, uid) = host.stat(path)?.map_or((0, 0), |m| (m.mode, m.uid));
        secret::check_mode(path, mode, uid)?;
        writeln!(out, "secret {key}: kept ({path})")?;
        return Ok(false);
    }
    // A file with the key empty is made again; one that holds anything else would lose it.
    if secret::holds_other_lines(&existing, key) {
        return Err(MdmError::SecretOther {
            path: path.to_owned(),
            key: key.to_owned(),
        });
    }
    let mut value = [0u8; 32];
    (env.random)(&mut value)?;
    let line = format!("{key}={}\n", hex::encode(value));
    p.file(path, 0o600, &Account::root(), line.as_bytes())?;
    writeln!(out, "secret {key}: makes {path} (0600 root; never shown)")?;
    Ok(true)
}

/// `MDM_STATE/removed/<now>`, or the first of `<now>-1`, `<now>-2`, ... when that name is
/// taken: an earlier snapshot is never written over.
fn snapshot_dir(host: &dyn Host, c: &MdmConfig, now: u64) -> Result<String, MdmError> {
    let base = format!("{}/removed/{now}", c.state());
    let mut name = base.clone();
    let mut n = 0u32;
    while host.stat(&name)?.is_some() {
        n += 1;
        name = format!("{base}-{n}");
    }
    Ok(name)
}

/// Plan the rollback: units stopped and disabled, the two jobs stopped, their files moved to
/// `MDM_STATE/removed/<now>/` (`<now>-1`, `-2`, ... when that exists).
pub fn plan_uninstall(env: &Env<'_>, c: &MdmConfig, out: &mut dyn Write) -> Result<Plan, MdmError> {
    let host = env.host;
    root_only(host, "install --uninstall")?;
    let n = UnitNames::new(c);
    let all = [
        &n.slice,
        &n.scep,
        &n.nanohub,
        &n.depserver,
        &n.depsyncer,
        &n.proxy,
        &n.cert_service,
        &n.cert_timer,
        &n.reload_path,
        &n.reload_service,
    ];
    let mut there = Vec::new();
    for unit in all {
        if host.stat(&format!("{UNITS_DIR}/{unit}"))?.is_some() {
            there.push(unit);
        }
    }
    // The slice stops with its services; the two jobs are started by the timer and the path
    // unit, which are stopped.
    let idle = [&n.slice, &n.cert_service, &n.reload_service];
    let stop: Vec<&str> = there
        .iter()
        .filter(|u| !idle.contains(u))
        .map(|u| u.as_str())
        .collect();
    // A job may be running now (the timer started it, or the path unit did): it is stopped
    // before its unit file is moved, so it cannot go on to restart the proxy. Stopping a unit
    // that is not running is not an error.
    let jobs: Vec<&str> = there
        .iter()
        .filter(|u| [&n.cert_service, &n.reload_service].contains(u))
        .map(|u| u.as_str())
        .collect();
    let mut p = Planner::new(host);
    if !stop.is_empty() {
        p.unit(UnitVerb::DisableNow, &stop);
    }
    if !jobs.is_empty() {
        p.run(
            Cmd::new(hostkeeper_mdm_step::SYSTEMCTL)
                .arg("stop")
                .arg("--")
                .args(&jobs),
        );
    }
    // A pending restart means nothing once the units are gone: an install after this one starts
    // them.
    let markers = format!("{RUN_DIR}/{}/restart", c.unit_prefix());
    for unit in [&n.scep, &n.nanohub, &n.depserver, &n.proxy] {
        p.absent(&format!("{markers}/{unit}"))?;
    }
    let to = snapshot_dir(host, c, env.now)?;
    // The copies are the service user's, in a 0700 directory of its own: every directory on
    // the way is then root's or the service user's, which the host layer insists on.
    let mine = Account::new(c.user(), c.user());
    if !there.is_empty() {
        p.dir(&to, 0o700, &mine)?;
    }
    for unit in &there {
        let path = format!("{UNITS_DIR}/{unit}");
        let bytes = host.read(&path)?.unwrap_or_default();
        p.file(&format!("{to}/{unit}"), 0o644, &mine, &bytes)?;
        p.absent(&path)?;
    }
    if there.is_empty() {
        writeln!(out, "no unit of the kit is installed")?;
    } else {
        p.unit(UnitVerb::DaemonReload, &[]);
    }
    let kept = format!(
        "kept: {} (database, SCEP CA, DEP token, TLS key and certificate), {} (secrets), {}, {} (pkgs), the user {}",
        c.state(),
        c.etc(),
        c.bin(),
        c.pkg_dir(),
        c.user()
    );
    writeln!(out, "{kept}")?;
    Ok(p.finish())
}
