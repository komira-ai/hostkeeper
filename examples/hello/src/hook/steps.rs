//! The six steps over the record, the units and the server.

use std::io::Write;
use std::path::PathBuf;

use super::args::Step;
use super::settings::{Settings, Units};
use crate::ports::Host;
use crate::record::{ACTIVE, CUTOVER, KEYS, NEXT, PREPARED, Record, Slot, VERIFIED};
use crate::report::{Failure, Report};

/// `verify` asks to be run again after this many seconds when the server does not answer.
const RETRY_AFTER_SECS: u64 = 2;
/// The exit status of a step that is deferred.
const EXIT_DEFERRED: i32 = 75;

/// Everything a step works with.
pub struct Steps<'a> {
    pub host: &'a Host<'a>,
    pub settings: &'a Settings,
    pub record: &'a Record,
    pub err: &'a mut dyn Write,
}

impl Steps<'_> {
    pub fn run(&mut self, step: Step, artifact: Option<PathBuf>) -> Result<Report, Failure> {
        match step {
            Step::Prepare => self.prepare(artifact.unwrap_or_default()),
            Step::Cutover => self.cutover(),
            Step::Verify => self.verify(),
            Step::Rollback => self.rollback(),
            Step::Cleanup => self.cleanup(),
            Step::Uninstall => self.uninstall(),
        }
    }

    /// Chooses the idle slot and records it. Nothing a client sees changes.
    fn prepare(&mut self, artifact: PathBuf) -> Result<Report, Failure> {
        let program = artifact.join("bin").join("hello");
        if !program.is_file() {
            return Err(Failure::release(format!(
                "{} is not a file: the staged release has no program",
                program.display()
            ))
            .with_reason("bad_artifact"));
        }
        self.record.remove(VERIFIED)?;
        self.record.remove(CUTOVER)?;
        let next = self.record.slot(ACTIVE)?.map_or(Slot::A, Slot::other);
        self.record.set_slot(NEXT, next)?;
        self.record.set(PREPARED, "")?;
        Ok(Report::release().with_detail(format!("prepared slot {}", next.name())))
    }

    /// Stops the serving slot and starts the idle one. `cutover` is recorded first, so that a
    /// `rollback` after a start that failed (or a crash half way) restarts the slot this stopped.
    fn cutover(&mut self) -> Result<Report, Failure> {
        if !self.record.has(PREPARED)? {
            return Err(Failure::release("prepare has not run"));
        }
        let next = self.recorded_next()?;
        let active = self.record.slot(ACTIVE)?;
        self.record.set(CUTOVER, "")?;
        if let Some(active) = active.filter(|active| *active != next) {
            self.stop(active)?;
        }
        self.start(next)?;
        Ok(Report::release().with_detail(format!("started slot {}", next.name())))
    }

    /// Asks the server on the port whether it is ready and runs the target version. Changes
    /// no unit.
    fn verify(&mut self) -> Result<Report, Failure> {
        if !self.record.has(CUTOVER)? {
            return Err(Failure::release("cutover has not run"));
        }
        let target = self.settings.target_version.clone().ok_or_else(|| {
            Failure::environment(format!(
                "params key {:?} is missing",
                super::settings::TARGET_VERSION_KEY
            ))
        })?;
        let (status, _) = self.ask("/readyz")?;
        match status {
            200 => {}
            503 => return Err(Failure::release("GET /readyz answered 503").with_reason("not_ready")),
            other => return Err(bad_status("/readyz", other)),
        }
        let (status, body) = self.ask("/version")?;
        if status != 200 {
            return Err(bad_status("/version", status));
        }
        let version = body.trim();
        if version != target {
            return Err(Failure::release(format!(
                "the server runs {version:?}, the update is {target:?}"
            ))
            .with_reason("wrong_version"));
        }
        self.record.set(VERIFIED, "")?;
        let mut report = Report::release();
        report.promotion_pending = Some(false);
        Ok(report)
    }

    /// Undoes `cutover` (stops the started slot and starts the serving one again) and forgets
    /// the update.
    fn rollback(&mut self) -> Result<Report, Failure> {
        if self.record.has(CUTOVER)? {
            if let Some(next) = self.record.slot(NEXT)? {
                self.stop(next)?;
                if let Some(active) = self.record.slot(ACTIVE)?.filter(|active| *active != next) {
                    self.start(active)?;
                }
            }
        }
        for key in [NEXT, PREPARED, CUTOVER, VERIFIED] {
            self.record.remove(key)?;
        }
        let mut report = Report::release();
        report.reversible = Some(true);
        Ok(report)
    }

    /// Makes the started slot the serving one. Refused until `verify` has succeeded. `verified`
    /// stays, so that a second run is a success that changes nothing.
    fn cleanup(&mut self) -> Result<Report, Failure> {
        if !self.record.has(VERIFIED)? {
            let mut refused = Failure::release("verify has not succeeded");
            // Nothing has been committed: the updater may roll the update back.
            refused.report.reversible = Some(true);
            return Err(refused);
        }
        if let Some(next) = self.record.slot(NEXT)? {
            self.record.set_slot(ACTIVE, next)?;
            self.record.remove(NEXT)?;
        }
        self.record.remove(PREPARED)?;
        self.record.remove(CUTOVER)?;
        Ok(Report::release())
    }

    /// Stops both slots and removes the record.
    fn uninstall(&mut self) -> Result<Report, Failure> {
        for slot in Slot::ALL {
            self.stop(slot)?;
        }
        for key in KEYS {
            self.record.remove(key)?;
        }
        self.record.remove_slots()?;
        Ok(Report::release())
    }

    fn recorded_next(&self) -> Result<Slot, Failure> {
        self.record
            .slot(NEXT)?
            .ok_or_else(|| Failure::environment("the record has prepared but no next slot"))
    }

    /// `GET path` from the server; a server that does not answer defers the step.
    fn ask(&self, path: &str) -> Result<(u16, String), Failure> {
        self.host
            .probe
            .get(self.settings.port, path)
            .map_err(|why| {
                let mut failure = Failure::release(why).with_reason("not_listening");
                failure.code = EXIT_DEFERRED;
                failure.report.retry_after_secs = Some(RETRY_AFTER_SECS);
                failure
            })
    }

    /// Creates the marker of `slot` and starts its unit.
    fn start(&mut self, slot: Slot) -> Result<(), Failure> {
        self.record.mark(slot)?;
        let argv = match self.settings.units {
            Units::Systemd => vec![
                "/usr/bin/systemctl".to_owned(),
                "start".to_owned(),
                format!("hello@{}.service", slot.name()),
            ],
            Units::Launchd => vec![
                "/bin/launchctl".to_owned(),
                "kickstart".to_owned(),
                format!("system/hello.{}", slot.name()),
            ],
        };
        self.command(&argv, false)
    }

    /// Removes the marker of `slot` and stops its unit. A job launchd has no process for answers
    /// non-zero to the stop; that is logged and is no failure.
    fn stop(&mut self, slot: Slot) -> Result<(), Failure> {
        self.record.unmark(slot)?;
        let (argv, lenient) = match self.settings.units {
            Units::Systemd => (
                vec![
                    "/usr/bin/systemctl".to_owned(),
                    "stop".to_owned(),
                    format!("hello@{}.service", slot.name()),
                ],
                false,
            ),
            Units::Launchd => (
                vec![
                    "/bin/launchctl".to_owned(),
                    "kill".to_owned(),
                    "SIGTERM".to_owned(),
                    format!("system/hello.{}", slot.name()),
                ],
                true,
            ),
        };
        self.command(&argv, lenient)
    }

    fn command(&mut self, argv: &[String], lenient: bool) -> Result<(), Failure> {
        let words: Vec<&str> = argv.iter().map(String::as_str).collect();
        let shown = argv.join(" ");
        match self.host.runner.run(&words) {
            Ok(0) => Ok(()),
            Ok(code) if lenient => {
                let _ = writeln!(self.err, "hello update-hook: `{shown}` exited with {code}: ignored");
                Ok(())
            }
            Ok(code) => Err(Failure::release(format!("`{shown}` exited with {code}"))),
            Err(why) => Err(Failure::environment(format!("cannot run `{shown}`: {why}"))),
        }
    }
}

fn bad_status(path: &str, status: u16) -> Failure {
    Failure::release(format!("GET {path} answered {status}")).with_reason("bad_status")
}
