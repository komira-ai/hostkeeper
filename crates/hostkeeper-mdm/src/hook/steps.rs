//! The six steps over the record, the host and its units.

use std::io::Write;

use hostkeeper_hook::{Params, Report, Step};
use hostkeeper_mdm_core::acme::Acme;
use hostkeeper_mdm_core::commands::scep_ca_init_args;
use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_core::package::{
    self, CERT_RENEW, CERT_SERVICE, CERT_TIMER, DEPSYNCER, ETC, HOOK_ENABLED, PROXY, RUN, STATE,
    USER,
};
use hostkeeper_mdm_core::render::{self, CADDYFILE_NAME};
use hostkeeper_mdm_step::{
    Account, Cmd, Host, Kind, Mode, Planner, SYSTEMCTL, Trustee, execute, ids,
};

use super::Failure;
use super::record::{Before, CUTOVER, KEYS, PREPARED, Record, VERIFIED};
use crate::cli::{Layout, env, settings_text};
use crate::install::{SECRETS, SS_PATH, STATE_DIRS, UNITS_DIR, preflight, secret_file};
use crate::verify::{
    Outcome, active, certificate, listeners, listens_only, mode_is, profile_file, profile_path,
    state_dir,
};
use crate::{Grant, MdmError, System, tls};

/// `systemctl stop` for a unit that is not loaded (the daemon unlinked it): nothing to stop.
const EXIT_NOT_LOADED: i32 = 5;

/// The staged release's programs the hook runs itself.
const STAGED_PROGRAMS: [&str; 2] = ["scepserver", "lego"];

/// Where systemd keeps the links that enable a unit wanted by `target`.
fn wants(target: &str, unit: &str) -> String {
    format!("{UNITS_DIR}/{target}.wants/{unit}")
}

/// The links `systemctl enable` makes for the units the hook (and `mdm dep define-profile`)
/// enable: their `[Install] WantedBy=`.
fn enabled_links() -> [String; 3] {
    [
        wants("multi-user.target", PROXY),
        wants("timers.target", CERT_TIMER),
        wants("multi-user.target", DEPSYNCER),
    ]
}

/// Where the DNS provider's credentials are: written by the operator, read by the certificate
/// job.
fn dns_env() -> String {
    format!("{ETC}/dns.env")
}

/// Where lego keeps its certificates.
fn certificates() -> String {
    format!("{STATE}/lego/certificates")
}

/// A file the hook renders from the settings: where, the directory it makes for it (a
/// drop-in's), the text, its mode and its owner.
struct Rendered {
    path: String,
    dir: Option<String>,
    text: String,
    mode: u32,
    owner: Account,
}

/// The Caddyfile and the drop-ins of the settings, in the order they are written.
fn rendered(c: &MdmConfig, acme: &Acme) -> Result<Vec<Rendered>, MdmError> {
    let caddyfile = render::render(c)?.caddyfile;
    let mut files = vec![Rendered {
        path: format!("{ETC}/{CADDYFILE_NAME}"),
        dir: None,
        text: caddyfile,
        mode: 0o640,
        owner: Account::new("root", USER),
    }];
    files.extend(package::drop_ins(c, acme).into_iter().map(|unit| {
        Rendered {
            dir: unit
                .name
                .split_once('/')
                .map(|(dir, _)| format!("{UNITS_DIR}/{dir}")),
            path: format!("{UNITS_DIR}/{}", unit.name),
            text: unit.text,
            mode: 0o644,
            owner: Account::root(),
        }
    }));
    Ok(files)
}

/// The paths of the files [`rendered`] writes, which do not depend on the settings.
fn rendered_paths() -> Vec<String> {
    let mut paths = vec![format!("{ETC}/{CADDYFILE_NAME}")];
    paths.extend(
        [package::SLICE, PROXY, DEPSYNCER, CERT_SERVICE, CERT_RENEW].map(|unit| {
            format!(
                "{UNITS_DIR}/{}/{}",
                package::drop_in_dir(unit),
                package::DROP_IN
            )
        }),
    );
    paths
}

/// The mode and owner a rendered file at `path` is put back with.
fn rendered_mode(path: &str) -> (u32, Account) {
    match path.ends_with(CADDYFILE_NAME) {
        true => (0o640, Account::new("root", USER)),
        false => (0o644, Account::root()),
    }
}

/// The directories `prepare` refuses as a link: the state, the settings and the pkgs. Not the
/// binaries': they are the package's, under the daemon's link `current` to the version that
/// runs, and the walk to them refuses that link (the staged release's programs are checked by
/// their own path).
pub(crate) fn package_dirs(c: &MdmConfig) -> Vec<String> {
    [c.state(), c.etc(), c.pkg_dir()]
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// The settings a step that reads them has.
struct Settings {
    config: MdmConfig,
    acme: Acme,
}

/// Everything a step works with.
pub(crate) struct Steps<'a> {
    pub(crate) system: &'a System<'a>,
    pub(crate) record: &'a Record,
    pub(crate) params: &'a Params,
    /// The settings file the params name.
    pub(crate) settings: &'a str,
    /// Where the plan and the commands are logged: standard error.
    pub(crate) log: &'a mut dyn Write,
}

impl Steps<'_> {
    pub(crate) fn run(&mut self, step: Step, staged: Option<String>) -> Result<Report, Failure> {
        match step {
            Step::Prepare => self.prepare(staged.unwrap_or_default()),
            Step::Cutover => self.cutover(),
            Step::Verify => self.verify(),
            Step::Rollback => self.rollback(),
            Step::Cleanup => self.cleanup(),
            Step::Uninstall => self.uninstall(),
        }
    }

    /// The settings, read through a host that trusts nobody but root and the effective user.
    fn read_settings(&self) -> Result<Settings, Failure> {
        let base = self.system.machine.host(&[]);
        let text = settings_text(base.as_ref(), self.settings, Layout::Package)?;
        Ok(Settings {
            config: MdmConfig::parse(&text).map_err(MdmError::from)?,
            acme: Acme::parse(&text).map_err(MdmError::from)?,
        })
    }

    /// The host, trusting the service user below the state directory as the kit does.
    fn host<'s>(system: &'s System<'s>) -> Box<dyn Host + 's> {
        system.machine.host(&[Grant {
            dir: STATE.to_owned(),
            who: Trustee::User(USER.to_owned()),
        }])
    }

    fn root(&self, host: &dyn Host) -> Result<(), Failure> {
        Ok(crate::install::root_only(host, "the mdm hook")?)
    }

    /// Checks the host, the staged release and the settings, keeps what the files it renders
    /// held, then makes the user, the directories, the secrets, the CA and the rendered files.
    fn prepare(&mut self, staged: String) -> Result<Report, Failure> {
        let host = Self::host(self.system);
        let host = host.as_ref();
        self.root(host)?;
        for program in STAGED_PROGRAMS {
            let path = format!("{staged}/bin/{program}");
            if host.stat(&path).map_err(MdmError::from)?.map(|m| m.kind) != Some(Kind::File) {
                return Err(Failure::release(
                    "bad_artifact",
                    format!("{path} is not a file: the staged release has no {program}"),
                ));
            }
        }
        let Settings { config: c, acme } = self.read_settings()?;
        preflight(host, &c, &package_dirs(&c), None, self.log).map_err(Failure::from)?;
        let lego = format!("{staged}/bin/lego");
        tls::check_lego(host, &lego, "run", &tls::LEGO_FLAGS)?;
        tls::check_lego(host, &lego, "renew", &tls::LEGO_RENEW_FLAGS)?;
        let dns = dns_env();
        match host.stat(&dns).map_err(MdmError::from)? {
            Some(_) => {
                for var in acme.provider.vars() {
                    crate::secret::read(host, &dns, var)?;
                }
                log(
                    self.log,
                    &format!("{dns}: holds {}", acme.provider.vars().join(" ")),
                );
            }
            None => log(
                self.log,
                &format!("{dns} is not there: the certificate job waits for it"),
            ),
        }

        let root = Account::root();
        let mine = Account::new(USER, USER);
        let etc_owner = Account::new("root", USER);
        let mut p = Planner::new(host);
        p.user(USER, STATE).map_err(MdmError::from)?;
        p.dir(STATE, 0o700, &mine).map_err(MdmError::from)?;
        for d in STATE_DIRS {
            p.dir(&format!("{STATE}/{d}"), 0o700, &mine)
                .map_err(MdmError::from)?;
        }
        p.dir(ETC, 0o750, &etc_owner).map_err(MdmError::from)?;
        p.dir(c.pkg_dir().as_str(), 0o755, &root)
            .map_err(MdmError::from)?;
        let env = env(self.system, host);
        for (file, key) in SECRETS {
            secret_file(&env, &mut p, &format!("{ETC}/{file}"), key, self.log)?;
        }
        let ca = format!("{STATE}/scep/depot/ca.pem");
        if host.stat(&ca).map_err(MdmError::from)?.is_some() {
            log(self.log, &format!("SCEP CA: kept ({ca})"));
        } else {
            let init = Cmd::new(&format!("{staged}/bin/scepserver")).args(&scep_ca_init_args(&c));
            p.run(init.as_user(USER));
        }
        // A record of another version's update (one whose cleanup did not finish) keeps what
        // the host held before that update, not before this one.
        let version = self.params.target_version().unwrap_or_default();
        if self.record.get(PREPARED)?.is_some_and(|v| v != version) {
            self.record.forget_saved()?;
        }
        let mut saves = Vec::new();
        for file in rendered(&c, &acme)? {
            if let Some(dir) = &file.dir {
                p.dir(dir, 0o755, &root).map_err(MdmError::from)?;
            }
            let before = host.read(&file.path).map_err(MdmError::from)?;
            if p.file(&file.path, file.mode, &file.owner, file.text.as_bytes())
                .map_err(MdmError::from)?
            {
                saves.push((file.path, before.map_or(Before::Absent, Before::Held)));
            }
        }
        // Kept before the first change, so a rollback after any of them finds it.
        for (path, before) in &saves {
            self.record.save(path, before)?;
        }
        execute(&p.finish(), Mode::Apply, host, self.log).map_err(MdmError::from)?;
        self.record.set(PREPARED, version)?;
        self.record.remove(CUTOVER)?;
        self.record.remove(VERIFIED)?;
        Ok(Report::default().with_reversible(true))
    }

    /// Starts the new version: reload, enable, restart.
    fn cutover(&mut self) -> Result<Report, Failure> {
        if !self.record.has(PREPARED)? {
            return Err(Failure::release(
                "not_prepared",
                "prepare has not run".into(),
            ));
        }
        let host = Self::host(self.system);
        let host = host.as_ref();
        self.root(host)?;
        self.record.set(CUTOVER, "")?;
        self.systemctl(host, &["daemon-reload"], false)?;
        self.systemctl(host, &with_units(&["enable", "--"], &HOOK_ENABLED), false)?;
        let mut restart: Vec<&str> = RUN.to_vec();
        restart.push(CERT_TIMER);
        self.systemctl(host, &with_units(&["restart", "--"], &restart), false)?;
        self.start_the_rest(host)?;
        Ok(Report::default().with_reversible(true))
    }

    /// The proxy restarted when it has its certificate, depsyncer restarted if it runs, and
    /// the first certificate job started when there is no certificate yet and the DNS
    /// credentials are there.
    fn start_the_rest(&mut self, host: &dyn Host) -> Result<(), Failure> {
        let has_certificate = has_certificate(host)?;
        if has_certificate {
            self.systemctl(host, &["restart", "--", PROXY], false)?;
        } else {
            log(
                self.log,
                &format!("{PROXY}: waits for a TLS certificate in {}", certificates()),
            );
        }
        self.systemctl(host, &["try-restart", "--", DEPSYNCER], false)?;
        let has_dns = host.stat(&dns_env()).map_err(MdmError::from)?.is_some();
        if !has_certificate && has_dns {
            self.systemctl(host, &["start", "--no-block", "--", CERT_SERVICE], false)?;
        }
        Ok(())
    }

    /// The checks an update can break.
    fn verify(&mut self) -> Result<Report, Failure> {
        if !self.record.has(CUTOVER)? {
            return Err(Failure::release(
                "not_cut_over",
                "cutover has not run".into(),
            ));
        }
        let Settings { config: c, acme } = self.read_settings()?;
        let host = Self::host(self.system);
        let host = host.as_ref();
        self.root(host)?;
        let has_certificate = has_certificate(host)?;
        let rows = verify_rows(host, &c, &acme, has_certificate)?;
        let mut not_ready = Vec::new();
        let mut failed = Vec::new();
        for row in &rows {
            let line = match &row.outcome {
                Ok(_) => format!("ok    {}", row.name),
                Err(why) => format!("FAIL  {}: {why}", row.name),
            };
            log(self.log, &line);
            if let Err(why) = &row.outcome {
                let failure = format!("{}: {why}", row.name);
                match row.judged {
                    Judged::Liveness => not_ready.push(failure),
                    Judged::Hard => failed.push(failure),
                }
            }
        }
        for (name, outcome) in notes(host, &c, has_certificate) {
            let line = match outcome {
                Ok(_) => format!("note  {name}: ok"),
                Err(why) => format!("note  {name}: {why}"),
            };
            log(self.log, &line);
        }
        if !failed.is_empty() {
            return Err(Failure::release("check_failed", failed.join("; ")));
        }
        if !not_ready.is_empty() {
            return Err(Failure::deferred("not_ready", not_ready.join("; ")));
        }
        self.record.set(VERIFIED, "")?;
        Ok(Report::default().with_reversible(true))
    }

    /// Puts back the files `prepare` changed and, after a `cutover`, the units of the version
    /// the daemon linked back (or none, on a first install).
    fn rollback(&mut self) -> Result<Report, Failure> {
        let host = Self::host(self.system);
        let host = host.as_ref();
        self.root(host)?;
        let cut_over = self.record.has(CUTOVER)?;
        let mut p = Planner::new(host);
        for path in rendered_paths() {
            match self.record.saved(&path)? {
                Some(Before::Held(bytes)) => {
                    let (mode, owner) = rendered_mode(&path);
                    p.file(&path, mode, &owner, &bytes)
                        .map_err(MdmError::from)?;
                }
                Some(Before::Absent) => {
                    p.absent(&path).map_err(MdmError::from)?;
                }
                None => {}
            }
        }
        let first_install = self.params.previous_release().is_none();
        if cut_over && first_install {
            for link in enabled_links() {
                p.absent(&link).map_err(MdmError::from)?;
            }
        }
        execute(&p.finish(), Mode::Apply, host, self.log).map_err(MdmError::from)?;
        if cut_over {
            if first_install {
                self.stop_everything(host)?;
            } else {
                self.systemctl(host, &["daemon-reload"], false)?;
                let mut restart: Vec<&str> = RUN.to_vec();
                restart.push(CERT_TIMER);
                self.systemctl(host, &with_units(&["restart", "--"], &restart), false)?;
                self.start_the_rest(host)?;
            }
        }
        self.record.forget_saved()?;
        for key in KEYS {
            self.record.remove(key)?;
        }
        Ok(Report::default().with_reversible(true))
    }

    /// Forgets the kept files, once `verify` has succeeded. `verified` stays, so that a second
    /// run is a success that changes nothing.
    fn cleanup(&mut self) -> Result<Report, Failure> {
        if !self.record.has(VERIFIED)? {
            return Err(
                Failure::release("not_verified", "verify has not succeeded".into()).reversible(),
            );
        }
        self.record.forget_saved()?;
        self.record.remove(PREPARED)?;
        self.record.remove(CUTOVER)?;
        Ok(Report::default())
    }

    /// Stops the package's units, removes the links the hook made and moves the rendered files
    /// aside; the state, the secrets and the user stay.
    fn uninstall(&mut self) -> Result<Report, Failure> {
        let host = Self::host(self.system);
        let host = host.as_ref();
        self.root(host)?;
        let mut p = Planner::new(host);
        for link in enabled_links() {
            p.absent(&link).map_err(MdmError::from)?;
        }
        let there: Vec<(String, Vec<u8>)> = rendered_paths()
            .into_iter()
            .filter_map(|path| match host.read(&path) {
                Ok(Some(bytes)) => Some(Ok((path, bytes))),
                Ok(None) => None,
                Err(e) => Some(Err(e)),
            })
            .collect::<Result<_, _>>()
            .map_err(MdmError::from)?;
        if !there.is_empty() {
            let to = snapshot_dir(host, self.system.now)?;
            let mine = Account::new(USER, USER);
            p.dir(&format!("{STATE}/removed"), 0o700, &mine)
                .map_err(MdmError::from)?;
            p.dir(&to, 0o700, &mine).map_err(MdmError::from)?;
            for (path, bytes) in &there {
                let name = path
                    .strip_prefix(&format!("{UNITS_DIR}/"))
                    .or_else(|| path.strip_prefix(&format!("{ETC}/")))
                    .unwrap_or(path)
                    .replace('/', "_");
                p.file(&format!("{to}/{name}"), 0o600, &mine, bytes)
                    .map_err(MdmError::from)?;
                p.absent(path).map_err(MdmError::from)?;
            }
        }
        self.stop_everything(host)?;
        execute(&p.finish(), Mode::Apply, host, self.log).map_err(MdmError::from)?;
        self.systemctl(host, &["daemon-reload"], false)?;
        self.record.forget_saved()?;
        for key in KEYS {
            self.record.remove(key)?;
        }
        log(
            self.log,
            &format!(
                "kept: {STATE}, {ETC} (secrets and settings), the pkg directory, the user {USER}"
            ),
        );
        Ok(Report::default())
    }

    /// Every unit of the package stopped; a unit that is not loaded has nothing to stop.
    fn stop_everything(&mut self, host: &dyn Host) -> Result<(), Failure> {
        let all: Vec<&str> = package::UNITS.iter().map(|u| u.name).collect();
        self.systemctl(host, &with_units(&["stop", "--"], &all), true)
    }

    /// Runs `systemctl <words>`, logged; a non-zero exit fails the step as the release's (the
    /// new version's units did not take the command), unless `not_loaded_is_nothing` and it is
    /// systemctl's answer for a unit that is not loaded.
    fn systemctl(
        &mut self,
        host: &dyn Host,
        words: &[&str],
        not_loaded_is_nothing: bool,
    ) -> Result<(), Failure> {
        let cmd = Cmd::new(SYSTEMCTL).args(words);
        log(self.log, &format!("+ {}", cmd.line()));
        let out = host.run(&cmd).map_err(MdmError::from)?;
        match out.code {
            0 => Ok(()),
            EXIT_NOT_LOADED if not_loaded_is_nothing => {
                log(
                    self.log,
                    &format!("{}: not loaded, nothing to stop", cmd.line()),
                );
                Ok(())
            }
            code => Err(Failure::release(
                "unit_failed",
                format!("{} exited with status {code}", cmd.line()),
            )),
        }
    }
}

/// `words` then `units`.
fn with_units<'a>(words: &[&'a str], units: &[&'a str]) -> Vec<&'a str> {
    words.iter().chain(units).copied().collect()
}

fn log(out: &mut dyn Write, line: &str) {
    let _ = writeln!(out, "{line}");
}

/// Whether lego has a certificate: a `.crt` file in its certificate directory, the condition
/// the proxy's unit starts on.
fn has_certificate(host: &dyn Host) -> Result<bool, Failure> {
    let names = host.list(&certificates()).map_err(MdmError::from)?;
    Ok(names
        .unwrap_or_default()
        .iter()
        .any(|name| name.ends_with(".crt")))
}

/// `/var/lib/mdm/removed/<now>`, or the first of `<now>-1`, `<now>-2`, ... when that name is
/// taken: an earlier snapshot is never written over.
fn snapshot_dir(host: &dyn Host, now: u64) -> Result<String, Failure> {
    let base = format!("{STATE}/removed/{now}");
    let mut name = base.clone();
    let mut n = 0u32;
    while host.stat(&name).map_err(MdmError::from)?.is_some() {
        n += 1;
        name = format!("{base}-{n}");
    }
    Ok(name)
}

/// What a failed check means for `verify`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Judged {
    /// A service is not up yet: the step is deferred.
    Liveness,
    /// Anything else: the step fails.
    Hard,
}

/// One check of `verify`.
struct Row {
    name: String,
    outcome: Outcome,
    judged: Judged,
}

/// The checks of `verify`, in the order they print.
fn verify_rows(
    host: &dyn Host,
    c: &MdmConfig,
    acme: &Acme,
    has_certificate: bool,
) -> Result<Vec<Row>, Failure> {
    let mut rows = Vec::new();
    let mut row = |name: String, outcome: Outcome, judged: Judged| {
        rows.push(Row {
            name,
            outcome,
            judged,
        });
    };
    let mut live: Vec<&str> = RUN.to_vec();
    if has_certificate {
        live.extend([PROXY, CERT_TIMER]);
    }
    for unit in live {
        row(
            format!("{unit} is active"),
            active(host, unit),
            Judged::Liveness,
        );
    }
    let listening = host
        .run(&Cmd::new(SS_PATH).arg("-Hltn"))
        .map_err(MdmError::from)?;
    if listening.code != 0 {
        return Err(Failure::environment(
            "step_failed",
            format!("{SS_PATH} -Hltn exited with status {}", listening.code),
        ));
    }
    let ss = listening.stdout;
    let bind = c.bind().to_string();
    let mut ports = vec![
        ("NanoHUB", c.nanohub_port(), "127.0.0.1"),
        ("SCEP", c.scep_port(), "127.0.0.1"),
        ("depserver", c.dep_port(), "127.0.0.1"),
    ];
    if has_certificate {
        ports.push(("the proxy", 443, bind.as_str()));
    }
    for (what, port, addr) in ports {
        row(
            format!("{what} listens on {addr}:{port} only"),
            listens_only(&ss, port, addr),
            Judged::Liveness,
        );
    }
    let none = match listeners(&ss, 80).as_slice() {
        [] => Ok(String::new()),
        some => Err(format!("listening on {}", some.join(", "))),
    };
    row("nothing listens on port 80".to_owned(), none, Judged::Hard);
    row(
        format!("{STATE} is a 0700 directory of {USER}"),
        state_dir(host, c),
        Judged::Hard,
    );
    row(
        format!("{ETC} is a 0750 directory of root"),
        mode_is(host, ETC, Kind::Dir, 0o750, 0),
        Judged::Hard,
    );
    for (file, _) in SECRETS {
        let path = format!("{ETC}/{file}");
        row(
            format!("{path} is a 0600 file of root"),
            mode_is(host, &path, Kind::File, 0o600, 0),
            Judged::Hard,
        );
    }
    row(
        format!("{} is a 0755 directory of root", c.pkg_dir()),
        mode_is(host, c.pkg_dir().as_str(), Kind::Dir, 0o755, 0),
        Judged::Hard,
    );
    for file in rendered(c, acme)? {
        row(
            format!("{} is as the settings render it", file.path),
            same_as(host, &file),
            Judged::Hard,
        );
    }
    Ok(rows)
}

/// The file `want` names is a regular file with its mode and owner, holding exactly its text.
fn same_as(host: &dyn Host, want: &Rendered) -> Outcome {
    let meta = host
        .stat(&want.path)
        .map_err(|e| e.to_string())?
        .ok_or("is not there".to_owned())?;
    // An owner that cannot be looked up is no owner a file can have: the check below fails.
    let owner = ids(host, &want.owner)
        .ok()
        .flatten()
        .unwrap_or((u32::MAX, u32::MAX));
    let have = (meta.kind, meta.mode, (meta.uid, meta.gid));
    if have != (Kind::File, want.mode, owner) {
        return Err(format!(
            "is {:?} {:04o} owner {}:{}, not File {:04o} owner {}:{}",
            have.0, have.1, meta.uid, meta.gid, want.mode, want.owner.user, want.owner.group
        ));
    }
    match host.read(&want.path).map_err(|e| e.to_string())? {
        Some(bytes) if bytes == want.text.as_bytes() => Ok(String::new()),
        _ => Err("differs from the render".to_owned()),
    }
}

/// What `verify` prints and does not judge: no update changes these.
fn notes(host: &dyn Host, c: &MdmConfig, has_certificate: bool) -> Vec<(String, Outcome)> {
    let mut notes = Vec::new();
    let dns = dns_env();
    notes.push((
        format!("{dns} is a 0600 file of root"),
        mode_is(host, &dns, Kind::File, 0o600, 0),
    ));
    let crt = format!("{}/{}.crt", certificates(), c.dns_name());
    notes.push(match has_certificate {
        true => (
            format!(
                "the certificate is valid for {} more days",
                crate::verify::VALID_FOR_DAYS
            ),
            certificate(host, &crt),
        ),
        false => (
            "the TLS certificate".to_owned(),
            Err(format!("none yet in {}", certificates())),
        ),
    });
    notes.push((
        format!("{} is a 0640 file of root, group {USER}", profile_path(c)),
        profile_file(host, c),
    ));
    notes
}

#[cfg(test)]
#[path = "steps_tests.rs"]
mod tests;
