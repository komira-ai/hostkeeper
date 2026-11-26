//! The `probe` kind (`MAINTENANCE_KIND_PROBE`, docs/operations/maintenance.md, "Probe: drain,
//! read, put back"): on a drained Mac, the maintenance hook's `cutover` runs a fixed list of
//! read-only programs ([`STEPS`]) the way it runs its own, and keeps what each gave in the probe
//! file beside the run record ([`PROBE_FILE`]), which the daemon's report carries
//! (`MaintenanceReport.probe`) until the next probe. It installs nothing and changes nothing on the
//! Mac but that file, unless the order names an Xcode archive to check (below).
//!
//! **Where the programs run.** In the hook's own launchd job, which the daemon starts in the
//! System domain as root for each step of a run (`hostkeeper-launchd`'s launcher), each as a child
//! of the hook by its absolute path, with an empty environment, no standard input and a deadline
//! ([`super::macos::HostPrograms`]): exactly the context of the Mac's update steps. So the probe
//! answers, with nobody's shell in between, who is at the console then, which launchd domain the
//! hook runs in, what environment its programs are given and what `fdesetup` answers there
//! (docs/operations/mac-proof.md, MP-E8 and MP-A6).
//!
//! **What fails.** A program that exits non-zero, writes on standard error, does not start or is
//! killed at its deadline is a result, kept with its exit code (none when it did not exit by
//! itself) and its output; the probe goes on to the next. Only a probe file that cannot be written
//! fails the `cutover` (`probe_unwritten`, which halts the host as any failed apply does), and
//! `verify` fails `probe_missing` when the file does not hold this run's probe.
//!
//! **Output.** Each step keeps the non-empty lines of its standard output, then of its standard
//! error, each trimmed and joined by `; `, cut at a character's boundary to
//! [`MAX_PROBE_OUTPUT`] bytes (`cut` says so). No program is given a password, and none of the
//! programs reads a secret.
//!
//! **An archive to check.** When the order's `Maintain.xcodes` names an Xcode archive (the probe of
//! `hk xcode add` and `hk xcode verify`) and the kind has an [`ArchiveCheck`], `prepare` fetches the
//! archive from the coordinator's store into `/Applications/.hostkeeper-check`, and `cutover` runs
//! the archive check's steps in place of [`STEPS`] and removes that directory: the Mac's programs
//! are given the archive's path, nothing is installed, and nothing is left in `/Applications`
//! ([`super::xcode::check`]). With no archive in the order, the probe is as above.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hostkeeper_launchd::LaunchctlError;
use hostkeeper_link::UnixClock;
use hostkeeper_proto::hostkeeper::agent::v1::{MaintenanceProbe, MaintenanceProbeStep};
use hostkeeper_proto::maintenance::{MAX_PROBE_OUTPUT, MAX_PROBE_STEPS};
use prost::Message as _;

use super::kind::{Cutover, KindError, MaintenanceKind};
use super::macos::{HostPrograms, Programs};
use super::params::MaintainParams;
use super::xcode::check::ArchiveCheck;
use crate::unix_clock::SystemUnixClock;

/// The file of the last probe, beside the run record: a `MaintenanceProbe`, encoded.
pub const PROBE_FILE: &str = "maintenance-probe.pb";
/// The most bytes the probe file may hold: a probe at every bound is far less.
pub const MAX_PROBE_FILE_BYTES: u64 = 8 * 1024;

/// The reason of a probe whose file could not be written.
pub const REASON_PROBE_UNWRITTEN: &str = "probe_unwritten";
/// The reason of a `verify` that finds no probe of the run in the probe file.
pub const REASON_PROBE_MISSING: &str = "probe_missing";

/// How long each program of the probe may run.
pub const STEP_DEADLINE: Duration = Duration::from_secs(60);

/// One program of the probe: its name in the report, its absolute path and its arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub name: &'static str,
    pub program: &'static str,
    pub args: &'static [&'static str],
}

/// The probe's programs, in the order run: who owns the console and who is logged in, who the
/// hook runs as and in which launchd domain, the environment its programs get, the macOS it runs,
/// and FileVault's two answers the update path reads (MP-E8 and MP-A6).
pub const STEPS: [Step; 8] = [
    Step {
        name: "console",
        program: "/usr/bin/stat",
        args: &["-f", "%Su", "/dev/console"],
    },
    Step {
        name: "who",
        program: "/usr/bin/who",
        args: &[],
    },
    Step {
        name: "id",
        program: "/usr/bin/id",
        args: &[],
    },
    Step {
        name: "managername",
        program: "/bin/launchctl",
        args: &["managername"],
    },
    Step {
        name: "env",
        program: "/usr/bin/env",
        args: &[],
    },
    Step {
        name: "sw_vers",
        program: "/usr/bin/sw_vers",
        args: &[],
    },
    Step {
        name: "fdesetup_status",
        program: "/usr/bin/fdesetup",
        args: &["status"],
    },
    Step {
        name: "fdesetup_supportsauthrestart",
        program: "/usr/bin/fdesetup",
        args: &["supportsauthrestart"],
    },
];

const _: () = assert!(STEPS.len() <= MAX_PROBE_STEPS);

/// The `probe` kind of a Mac: its programs, its clock and the directory of its file.
pub struct ProbeKind {
    programs: Box<dyn Programs>,
    clock: Box<dyn UnixClock>,
    /// The run's directory, where the probe file goes; `None` for parameters on standard input,
    /// where no `cutover` runs.
    dir: Option<PathBuf>,
    /// The check of the Xcode archive an order names; `None` for a probe that checks none.
    archive: Option<ArchiveCheck>,
}

impl ProbeKind {
    /// The probe of `programs`, timed by `clock`, keeping its file in `dir`.
    pub fn new(
        programs: Box<dyn Programs>,
        clock: Box<dyn UnixClock>,
        dir: Option<&Path>,
    ) -> ProbeKind {
        ProbeKind {
            programs,
            clock,
            dir: dir.map(Path::to_path_buf),
            archive: None,
        }
    }

    /// This probe, checking the Xcode archive an order names with `check`.
    #[must_use]
    pub fn with_archive_check(mut self, check: ArchiveCheck) -> ProbeKind {
        self.archive = Some(check);
        self
    }

    /// The probe of this Mac, in the run's directory `dir`, checking an archive an order names
    /// with the Mac's own ports ([`ArchiveCheck::at_host`]).
    pub fn at_host(dir: Option<&Path>) -> ProbeKind {
        ProbeKind::new(Box::new(HostPrograms), Box::new(SystemUnixClock), dir)
            .with_archive_check(ArchiveCheck::at_host())
    }

    /// Every step of [`STEPS`], in order.
    fn steps(&self) -> Vec<MaintenanceProbeStep> {
        STEPS.iter().map(|step| self.step(step)).collect()
    }

    /// What `step` gave.
    fn step(&self, step: &Step) -> MaintenanceProbeStep {
        let tool = self.programs.tool(Path::new(step.program), STEP_DEADLINE);
        let args: Vec<String> = step.args.iter().map(|arg| (*arg).to_owned()).collect();
        let (exit_code, text) = match tool.run(&args) {
            Ok(out) => (
                out.code,
                joined(&[
                    &String::from_utf8_lossy(&out.stdout),
                    &String::from_utf8_lossy(&out.stderr),
                ]),
            ),
            Err(LaunchctlError::Spawn { why, .. }) => {
                (None, format!("{} did not start: {why}", step.program))
            }
            Err(LaunchctlError::TimedOut { .. }) => (
                None,
                format!(
                    "{} was killed at its deadline of {}s",
                    step.program,
                    STEP_DEADLINE.as_secs()
                ),
            ),
        };
        let (output, cut) = bounded(text);
        MaintenanceProbeStep {
            name: step.name.to_owned(),
            exit_code,
            output,
            cut,
        }
    }

    /// The probe file's path, or the failure that this hook keeps none.
    fn file(&self) -> Result<PathBuf, io::Error> {
        self.dir
            .as_deref()
            .map(|dir| dir.join(PROBE_FILE))
            .ok_or_else(|| io::Error::other("the hook keeps no run directory"))
    }
}

impl MaintenanceKind for ProbeKind {
    /// Nothing to stage, unless the order names an archive and the kind checks one: the archive
    /// is fetched ([`ArchiveCheck::prepare`]).
    fn prepare(&self, params: &MaintainParams) -> Result<(), KindError> {
        match &self.archive {
            Some(check) => check.prepare(&params.maintain().xcodes),
            None => Ok(()),
        }
    }

    /// Runs every step (of [`STEPS`], or of the archive check when the order names an archive and
    /// the kind checks one) and writes the probe file; applied, its version the number of steps
    /// run. A hook that keeps no run directory runs no step.
    fn cutover(&self, params: &MaintainParams) -> Result<Cutover, KindError> {
        let unwritten = |error: io::Error| KindError::Failed {
            reason: REASON_PROBE_UNWRITTEN,
            detail: format!("the probe file could not be written: {error}"),
        };
        let path = self.file().map_err(unwritten)?;
        let xcodes = &params.maintain().xcodes;
        let steps = match &self.archive {
            Some(check) if !xcodes.is_empty() => check.steps(xcodes),
            _ => self.steps(),
        };
        let probe = MaintenanceProbe {
            run_id: params.run_id().to_owned(),
            steps,
            probed_unix: self.clock.unix_secs(),
        };
        write_probe(&path, &probe).map_err(unwritten)?;
        Ok(Cutover::Applied {
            version: format!("{} steps", probe.steps.len()),
        })
    }

    /// Never asked for: the probe needs no reboot.
    fn after_reboot(&self, _params: &MaintainParams) -> Result<String, KindError> {
        Err(KindError::unsupported(super::params::Kind::Probe))
    }

    /// The probe file holds this run's probe.
    fn verify(&self, params: &MaintainParams) -> Result<(), KindError> {
        let found = self.file().and_then(|path| read_probe(&path));
        match found {
            Ok(probe) if probe.run_id == params.run_id() => Ok(()),
            Ok(probe) => Err(missing(format!(
                "the probe file holds the probe of run {}",
                probe.run_id
            ))),
            Err(error) => Err(missing(format!(
                "the probe file could not be read: {error}"
            ))),
        }
    }
}

/// The failure of a `verify` that finds no probe of the run.
fn missing(detail: String) -> KindError {
    KindError::Failed {
        reason: REASON_PROBE_MISSING,
        detail,
    }
}

/// The non-empty lines of each of `texts`, trimmed, in order, joined by `; `.
pub fn joined(texts: &[&str]) -> String {
    texts
        .iter()
        .flat_map(|text| text.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// `text` cut to at most [`MAX_PROBE_OUTPUT`] bytes at a character's boundary, and whether it was.
pub fn bounded(mut text: String) -> (String, bool) {
    if text.len() <= MAX_PROBE_OUTPUT {
        return (text, false);
    }
    let end = (0..=MAX_PROBE_OUTPUT)
        .rev()
        .find(|at| text.is_char_boundary(*at))
        .unwrap_or(0);
    text.truncate(end);
    (text, true)
}

/// Writes `probe` to `path` whole: to a file beside it, then renamed over it.
pub fn write_probe(path: &Path, probe: &MaintenanceProbe) -> Result<(), io::Error> {
    let staged = path.with_extension("pb.tmp");
    fs::write(&staged, probe.encode_to_vec())?;
    fs::rename(&staged, path)
}

/// The probe `path` holds: at most [`MAX_PROBE_FILE_BYTES`], a `MaintenanceProbe`.
pub fn read_probe(path: &Path) -> Result<MaintenanceProbe, io::Error> {
    let bytes = fs::read(path)?;
    if bytes.len() as u64 > MAX_PROBE_FILE_BYTES {
        return Err(io::Error::other(format!(
            "{} bytes, more than {MAX_PROBE_FILE_BYTES}",
            bytes.len()
        )));
    }
    MaintenanceProbe::decode(bytes.as_slice()).map_err(io::Error::other)
}

#[cfg(test)]
#[path = "probe_tests.rs"]
mod tests;
