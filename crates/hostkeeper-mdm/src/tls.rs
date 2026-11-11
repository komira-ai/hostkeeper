//! `hostkeeper-mdm tls`: the MDM's TLS certificate from an ACME CA over DNS-01, renewed by a timer, as
//! root. lego, pinned and installed by `hostkeeper-mdm install`, does the ACME work.
//!
//! It checks first, and refuses before anything changes: an ACME setting is unset or
//! malformed (`hostkeeper_mdm_core::acme::Acme`); `MDM_ETC/dns.env` is not a 0600 file of root's holding
//! every variable the provider reads; `MDM_BIN/lego` is missing, its `lego run --help` exits
//! non-zero, or it lists no flag the unit passes; the certificate path holds something that is
//! not a regular file, or a file `openssl x509` cannot read. The DNS credentials are never read into this program's
//! output or any command line: the certificate unit loads `dns.env` itself
//! (`EnvironmentFile=`), so lego gets them in its environment.
//!
//! The plan has each of these only when the host differs:
//! 1. the certificate unit, its twice-a-day timer, and the path unit that restarts the proxy
//!    when the certificate changes, under `/etc/systemd/system` (root's, 0644), then
//!    `daemon-reload`;
//! 2. the timer and the path unit enabled, and restarted when their unit changed, else started
//!    when not active;
//! 3. the certificate unit started once, after step 2, when there is no certificate yet (the
//!    first issuance), when the key beside it (the `.crt` path with `.key`) is missing, is not a
//!    regular file or is not the certificate's (a line says which; no key material is printed),
//!    or when the certificate ends within [`VALID_FOR_DAYS`] days. `lego run` writes both files;
//!    the reload path unit, already watching, restarts the proxy on the new certificate;
//! 4. in a second plan, once the certificate is there (after step 3 when it runs; a dry run
//!    that would issue shows no proxy step), the proxy enabled and started when not active.
//!
//! After an apply the certificate must be there, else the run fails (exit 1) before the proxy;
//! its subject, issuer and expiry are printed (public data, read by `openssl`).
//!
//! A refusal changes nothing only for the checks above, which run before the first plan. The
//! plan runs in two parts, because the proxy can be planned only once the certificate is
//! there, so a run can end halfway: when the certificate is missing after the issuing unit
//! ran, the units, the timer and the first issuance attempt stay in place and the proxy is not
//! started. Fix the cause (`journalctl -u` the certificate unit) and run `tls --apply` again;
//! the second run finds the rest to do.

use std::collections::BTreeSet;
use std::io::Write;

use hostkeeper_mdm_core::acme::Acme;
use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_core::render::{UnitNames, render_tls};
use hostkeeper_mdm_step::{Account, Cmd, Host, Kind, Mode, Planner, UnitVerb, execute};

use crate::install::UNITS_DIR;
use crate::verify::VALID_FOR_DAYS;
use crate::{Env, MdmError, secret};

const OPENSSL: &str = "/usr/bin/openssl";
/// The flags of `lego renew` the `mdm` package's renewal unit passes.
pub const LEGO_RENEW_FLAGS: [&str; 9] = [
    "--email",
    "--server",
    "--path",
    "--key-type",
    "--dns",
    "--domains",
    "--dns.resolvers",
    "--dns.propagation.wait",
    "--days",
];
/// The flags of `lego run` the certificate unit passes.
pub const LEGO_FLAGS: [&str; 9] = [
    "--accept-tos",
    "--email",
    "--server",
    "--path",
    "--key-type",
    "--dns",
    "--domains",
    "--dns.resolvers",
    "--dns.propagation.wait",
];

/// Check, then print or carry out the units, the first issuance and the proxy's start.
pub fn run(
    env: &Env<'_>,
    c: &MdmConfig,
    acme: &Acme,
    mode: Mode,
    out: &mut dyn Write,
) -> Result<(), MdmError> {
    let host = env.host;
    let euid = host.euid();
    if euid != 0 {
        return Err(MdmError::NeedsRoot { verb: "tls", euid });
    }
    let dns_env = format!("{}/dns.env", c.etc());
    let vars = acme.provider.vars();
    for var in vars {
        secret::read(host, &dns_env, var)?;
    }
    writeln!(out, "dns.env: holds {} (values not shown)", vars.join(" "))?;
    check_lego(host, &format!("{}/lego", c.bin()), "run", &LEGO_FLAGS)?;

    let crt = format!("{}/lego/certificates/{}.crt", c.state(), c.dns_name());
    let key = format!("{}.key", crt.strip_suffix(".crt").unwrap_or(crt.as_str()));
    let (issue, renew) = look_at_certificate(host, &crt, &key, out)?;

    let n = UnitNames::new(c);
    let root = Account::root();
    let mut p = Planner::new(host);
    let mut changed = BTreeSet::new();
    for unit in render_tls(c, acme)? {
        let path = format!("{UNITS_DIR}/{}", unit.name);
        if p.file(&path, 0o644, &root, unit.text.as_bytes())? {
            changed.insert(unit.name);
        }
    }
    if !changed.is_empty() {
        p.unit(UnitVerb::DaemonReload, &[]);
    }
    for unit in [&n.reload_path, &n.cert_timer] {
        p.service(unit, changed.contains(unit))?;
    }
    // The proxy is planned only with its certificate there: started without it, Caddy fails
    // and restarts every 5 s. An issuance runs in this plan, after the reload path unit is
    // started above, so the proxy has a plan of its own after it, planned once the
    // certificate is there.
    if issue || renew {
        p.unit(UnitVerb::Start, &[n.cert_service.as_str()]);
    }
    execute(&p.finish(), mode, host, out)?;
    if issue && mode == Mode::DryRun {
        writeln!(out, "proxy: started once {crt} is there")?;
        return Ok(());
    }
    if host.stat(&crt)?.is_none() {
        return Err(MdmError::NoCertificate(crt));
    }
    let mut p = Planner::new(host);
    p.service(&n.proxy, false)?;
    execute(&p.finish(), mode, host, out)?;
    if mode == Mode::DryRun {
        return Ok(());
    }
    let shown = host.run(&Cmd::new(OPENSSL).args(&[
        "x509", "-noout", "-subject", "-issuer", "-enddate", "-in", &crt,
    ]))?;
    if shown.code != 0 {
        return Err(MdmError::NotCertificate(crt));
    }
    for line in shown.stdout.lines() {
        writeln!(out, "certificate: {line}")?;
    }
    Ok(())
}

/// `lego` is a file, `lego <command> --help` exits 0 and lists every one of `flags`, the flags
/// a certificate unit passes to that command.
pub(crate) fn check_lego(
    host: &dyn Host,
    lego: &str,
    command: &str,
    flags: &[&'static str],
) -> Result<(), MdmError> {
    let lego = lego.to_owned();
    if host.stat(&lego)?.map(|m| m.kind) != Some(Kind::File) {
        return Err(MdmError::NoLego(lego));
    }
    let help = host.run(&Cmd::new(&lego).arg(command).arg("--help"))?;
    if help.code != 0 {
        return Err(MdmError::LegoHelp {
            lego,
            code: help.code,
        });
    }
    let help = help.stdout + &help.stderr;
    // Whole words: `--dns` is not there because `--dns.resolvers` is.
    let words: BTreeSet<&str> = help
        .split(|ch: char| ch.is_whitespace() || ch == ',' || ch == '=')
        .collect();
    if let Some(&flag) = flags.iter().find(|f| !words.contains(*f)) {
        return Err(MdmError::LegoFlag { lego, flag });
    }
    Ok(())
}

/// Looks at the certificate and its key before any plan: `(issue, renew)`, whether a new
/// certificate is needed because there is none or the key cannot serve it, and whether the
/// certificate ends within [`VALID_FOR_DAYS`] days.
fn look_at_certificate(
    host: &dyn Host,
    crt: &str,
    key: &str,
    out: &mut dyn Write,
) -> Result<(bool, bool), MdmError> {
    match host.stat(crt)?.map(|m| m.kind) {
        None => return Ok((true, false)),
        Some(Kind::File) => {}
        Some(_) => return Err(MdmError::CertNotFile(crt.to_owned())),
    }
    let read = host.run(&Cmd::new(OPENSSL).args(&["x509", "-noout", "-in", crt]))?;
    if read.code != 0 {
        return Err(MdmError::CertUnreadable(crt.to_owned()));
    }
    let why = match host.stat(key)?.map(|m| m.kind) {
        None => Some("is not there"),
        Some(Kind::File) => match key_matches(host, crt, key)? {
            Some(true) => None,
            Some(false) => Some("is not the key of the certificate"),
            None => Some("cannot be compared with the certificate"),
        },
        Some(_) => Some("is not a regular file"),
    };
    if let Some(why) = why {
        writeln!(out, "key: {key} {why}, a new certificate is issued")?;
        return Ok((true, false));
    }
    let secs = VALID_FOR_DAYS * 24 * 3600;
    let ends = host.run(&Cmd::new(OPENSSL).args(&[
        "x509",
        "-noout",
        "-checkend",
        &secs.to_string(),
        "-in",
        crt,
    ]))?;
    let renew = ends.code != 0;
    if renew {
        let days = VALID_FOR_DAYS;
        writeln!(
            out,
            "certificate: ends within {days} days, a new one is issued"
        )?;
    }
    Ok((false, renew))
}

/// Whether the public key of the certificate is the key's; `None` when openssl cannot say.
/// Only the two public keys are compared, and neither is printed.
fn key_matches(host: &dyn Host, crt: &str, key: &str) -> Result<Option<bool>, MdmError> {
    let from_crt = host.run(&Cmd::new(OPENSSL).args(&["x509", "-noout", "-pubkey", "-in", crt]))?;
    let from_key = host.run(&Cmd::new(OPENSSL).args(&["pkey", "-pubout", "-in", key]))?;
    Ok(match from_crt.code == 0 && from_key.code == 0 {
        true => Some(from_crt.stdout == from_key.stdout),
        false => None,
    })
}
