//! The names and files of one privileged test, so that two tests, or two runs, never share a unit
//! or a path, and every unit a test made is torn down when it ends.
//!
//! A [`Rig`] lives in a scratch directory the test owns (under the target directory, never a
//! fixed path). It gives each unit a name of its own, `hostkeeper-ci-<test>-<pid>-<n>`
//! ([`Rig::unit`]), and remembers it; it writes the fixture's params files ([`Rig::params`]) and
//! the unit file of the fixture's `serve` ([`Rig::unit_file`]); and it gives the `systemctl` lines
//! that stop, unlink and reset every unit it named ([`Rig::teardown`]), which the test's drop guard
//! runs, failed or not. It runs nothing itself: the system calls are the tests'.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use hostkeeper_hook::{Params, Scalar};

use crate::fixture::{MODE_KEY, Mode, PID_FILE_KEY, SERVE};

/// The start of every unit name a rig gives: `journalctl -u 'hostkeeper-ci-*'` finds them all.
pub const UNIT_PREFIX: &str = "hostkeeper-ci-";
/// The service a privileged test runs its steps for.
pub const SERVICE: &str = "hostkeeper-ci";
/// The longest test name a rig takes.
pub const MAX_TEST_NAME: usize = 64;

/// Why a rig could not be made or could not write a file.
#[derive(Debug, thiserror::Error)]
pub enum RigError {
    #[error("{0:?} is not an absolute path")]
    NotAbsolute(PathBuf),
    #[error("the test name {0:?} is not 1 to {MAX_TEST_NAME} lowercase letters, digits and `_`")]
    TestName(String),
    #[error(
        "{0:?} holds a character other than letters, digits and `/._+-`, which a unit file's \
         ExecStart= would read otherwise"
    )]
    NotPlainPath(PathBuf),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// One test's units and files.
#[derive(Debug)]
pub struct Rig {
    dir: PathBuf,
    fixture: PathBuf,
    test: String,
    pid: u32,
    next: AtomicU64,
    units: Mutex<Vec<String>>,
}

/// Whether every byte of `path` stays itself on an `ExecStart=` line.
fn plain_path(path: &Path) -> bool {
    path.as_os_str()
        .as_encoded_bytes()
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || b"/._+-".contains(b))
}

/// `<unit>.service`.
pub fn service_unit(unit: &str) -> String {
    format!("{unit}.service")
}

impl Rig {
    /// The rig of the test `test` (its name, lowercase letters, digits and `_`), run by the
    /// process `pid`, in the directory `dir` it owns, with the fixture program at `fixture`.
    pub fn new(dir: &Path, fixture: &Path, test: &str, pid: u32) -> Result<Rig, RigError> {
        for path in [dir, fixture] {
            if !path.is_absolute() {
                return Err(RigError::NotAbsolute(path.to_path_buf()));
            }
        }
        let named = (1..=MAX_TEST_NAME).contains(&test.len())
            && test
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        if !named {
            return Err(RigError::TestName(test.to_owned()));
        }
        Ok(Rig {
            dir: dir.to_path_buf(),
            fixture: fixture.to_path_buf(),
            test: test.to_owned(),
            pid,
            next: AtomicU64::new(0),
            units: Mutex::default(),
        })
    }

    /// The test's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The fixture program.
    pub fn fixture(&self) -> &Path {
        &self.fixture
    }

    /// A new unit name, `hostkeeper-ci-<test>-<pid>-<n>` (the test's `_` written `-`), without the
    /// `.service` systemd adds; remembered for [`Rig::teardown`].
    pub fn unit(&self) -> String {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let name = format!(
            "{UNIT_PREFIX}{}-{}-{n}",
            self.test.replace('_', "-"),
            self.pid
        );
        let mut units = self.units.lock().unwrap_or_else(PoisonError::into_inner);
        units.push(name.clone());
        name
    }

    /// Every unit name this rig gave, in order.
    pub fn units(&self) -> Vec<String> {
        self.units
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The path of the pid file `name` in the test's directory (written by the fixture).
    pub fn pid_file(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}-pids"))
    }

    /// Writes the params file `name` of a fixture step in `mode`, with `pid_file` if given, and
    /// gives its path.
    pub fn params(
        &self,
        name: &str,
        mode: &Mode,
        pid_file: Option<&Path>,
    ) -> Result<PathBuf, RigError> {
        let mut params = Params::default();
        params.insert(MODE_KEY, Scalar::Text(mode.to_string()));
        if let Some(file) = pid_file {
            params.insert(
                PID_FILE_KEY,
                Scalar::Text(file.to_string_lossy().into_owned()),
            );
        }
        let path = self.dir.join(format!("{name}-params"));
        fs::write(&path, params.to_toml())?;
        Ok(path)
    }

    /// The unit file of the fixture's `serve`: a `Type=exec` service whose main process is the
    /// fixture (`exec`: the start ends once the fixture runs, not at the fork), until it is
    /// stopped. A fixture path that `ExecStart=` would not read as itself
    /// (a space, a quote, `%`, `\`, `$`, `;`, ...) is refused.
    pub fn unit_file_text(&self) -> Result<String, RigError> {
        if !plain_path(&self.fixture) {
            return Err(RigError::NotPlainPath(self.fixture.clone()));
        }
        Ok(format!(
            "[Unit]\nDescription=hostkeeper privileged test: the fixture's {SERVE}\n\n\
             [Service]\nType=exec\nExecStart={} {SERVE}\n",
            self.fixture.display()
        ))
    }

    /// Writes [`Rig::unit_file_text`] to `<unit>.service` in the test's directory, for
    /// `systemctl link`, and gives its path.
    pub fn unit_file(&self, unit: &str) -> Result<PathBuf, RigError> {
        let path = self.dir.join(service_unit(unit));
        fs::write(&path, self.unit_file_text()?)?;
        Ok(path)
    }

    /// The arguments of `systemd-run` that start `unit` as a transient service of the fixture's
    /// `serve` with no property, and return once it has started.
    pub fn serve_command(&self, unit: &str) -> Vec<OsString> {
        let mut args: Vec<OsString> = ["--system", "--quiet", "--collect"]
            .into_iter()
            .map(OsString::from)
            .collect();
        args.push(format!("--unit={unit}").into());
        args.push("--".into());
        args.push(self.fixture.clone().into());
        args.push(SERVE.into());
        args
    }

    /// The `systemctl` lines that tear down every unit this rig named: stop it, remove the link
    /// of a linked unit file, and forget a failed state. Each may fail for a unit that has
    /// nothing to undo; the guard that runs them goes on.
    pub fn teardown(&self) -> Vec<Vec<OsString>> {
        self.units()
            .iter()
            .flat_map(|unit| {
                let name = service_unit(unit);
                ["stop", "disable", "reset-failed"].map(|verb| {
                    [verb, "--system", name.as_str()]
                        .map(OsString::from)
                        .to_vec()
                })
            })
            .collect()
    }
}

/// The arguments of `systemctl` that print `properties` of `unit`.
pub fn show_command(unit: &str, properties: &[&str]) -> Vec<OsString> {
    let properties = format!("--property={}", properties.join(","));
    let name = service_unit(unit);
    ["show", "--system", properties.as_str(), "--", name.as_str()]
        .map(OsString::from)
        .to_vec()
}

/// The `Key=Value` lines `systemctl show` printed, as a map; a line with no `=` is left out.
pub fn parse_show(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}
