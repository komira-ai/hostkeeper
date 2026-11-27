//! A Mac's hardware sources, read into the daemon's [`HealthKeeper`]
//! (docs/design/host-health-alerts.md sections 3.2, 4.4 and 4.5): its kernel panic reports, its
//! unexpected restarts, and its capacity. [`MacHealth`] reads them through ports, so all of it
//! runs against fakes; [`MacHealth::run`] is the schedule, on its own thread ([`start`]), so a
//! slow program holds no heartbeat.
//!
//! **Panic reports** ([`MacHealth::read_panics`], source [`PANIC_SOURCE`]): the `.panic` and `.ips`
//! files of [`DIAGNOSTIC_REPORTS`] itself, regular files only (so `Retired/` and links are not
//! read), the newest [`MAX_MARKED`] of them by their time (a directory of more passes over the
//! older ones), each read once, to at most [`MAX_REPORT_BYTES`], and parsed by
//! [`crate::mac_panic`]. The source's mark is the names of the reports read that are still among
//! them, so a report is not read again while it stays and the mark does not grow past its bound.
//! An entry that leaves the directory while it is listed has no time (0), and its read fails. A directory that does not exist has no reports. One that cannot be
//! listed (a privacy grant missing, say) is the source `unreadable` with its reason; a report that
//! cannot be read is logged and tried again at the next read. Every file the daemon reads there is
//! one a user of the Mac can read too: nothing here needs root to read.
//!
//! **An unexpected restart** ([`MacHealth::judge_shutdown`], source [`SHUTDOWN_SOURCE`]): at the
//! first start of a boot whose boot before ended with no clean-shutdown marker
//! ([`HealthKeeper::unclean_previous_boot`], which a restart of the daemon in that boot still
//! knows), one `host` record, a warning, `unexpected_reboot`, whose id is [`SHUTDOWN_SOURCE`] and
//! the boot id; the source's mark is the boot id judged, so the boot gives one record however
//! often the daemon starts in it. It is judged at the second pass ([`CAPACITY_AGAIN_SECS`] after the start), by when
//! macOS has written the panic report of the boot before: merged with the newest panic report of
//! that boot (made after it began, and before this one did), whose class its message names and
//! whose panic line is its `raw`. On a Mac the marker is advisory (launchd's SIGTERM of a
//! shutdown and of a `bootout` look alike), so the record is evidence for the coordinator, which
//! judges it with the heartbeats' gap.
//!
//! **Capacity** ([`MacHealth::read_capacity`], source [`CAPACITY_SOURCE`]): `sysctl` and
//! `system_profiler`, each a fixed argument list through the [`Launchctl`] port (an empty
//! environment, capped output, killed at [`crate::mac::TOOL_DEADLINE`]), judged by
//! [`crate::mac_capacity`]. Either failing makes the source `unreadable`, with the first failure's
//! reason; the capacity is still what the other gave.
//!
//! **The schedule** ([`MacHealth::run`]): at the start, the capacity and the panic reports; after
//! [`CAPACITY_AGAIN_SECS`], both again (early values can settle) and the restart judged; then the
//! panic reports every [`PANIC_PERIOD_SECS`], until the sleeper says stop.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess, PlatformKind};
use hostkeeper_link::{Log, Sleeper, Tick, UnixClock};
use hostkeeper_proto::health::{MAX_EVENT_ID, MAX_MESSAGE, MAX_RAW};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use crate::health_state::cut;
use crate::host_health::{HealthKeeper, PreviousBoot};
use crate::mac::TOOL_DEADLINE;
use crate::mac_capacity::{
    CAPACITY_SYSCTL_ARGS, PROFILER_ARGS, SYSTEM_PROFILER, capacity, parse_profile, parse_sysctls,
};
use crate::mac_panic::{PanicReport, is_candidate, parse};
use crate::maintenance_boot::SYSCTL;

pub use crate::mac_panic::PANIC_SOURCE;

/// Where macOS writes its diagnostic reports, panic reports among them.
pub const DIAGNOSTIC_REPORTS: &str = "/Library/Logs/DiagnosticReports";

/// The most bytes of a report read: a panic report's panic line is near its start, and a report
/// cut here is still read ([`crate::mac_panic`]).
pub const MAX_REPORT_BYTES: u64 = 256 * 1024;

/// The most report names the panic source's mark keeps.
pub const MAX_MARKED: usize = 512;

/// The source token of an unexpected restart's record.
pub const SHUTDOWN_SOURCE: &str = "shutdown";

/// The `code` of an unexpected restart's record.
pub const UNEXPECTED_REBOOT: &str = "unexpected_reboot";

/// The subject of an unexpected restart's record.
pub const HOST_SUBJECT: &str = "host";

/// The source token of the capacity's state.
pub const CAPACITY_SOURCE: &str = "system_profiler";

/// The seconds after the start of the second pass.
pub const CAPACITY_AGAIN_SECS: u64 = 120;

/// The seconds between two reads of the panic reports after the second pass.
pub const PANIC_PERIOD_SECS: u64 = 300;

/// One file of the report directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportFile {
    /// Its name.
    pub name: String,
    /// When it was last modified, Unix seconds; 0 when not known.
    pub modified_unix: u64,
}

/// The directory the panic reports are read from.
pub trait ReportDir: Send + Sync {
    /// Its regular files, a link not followed and a directory left out.
    ///
    /// # Errors
    /// The listing's own; `NotFound` when there is no directory.
    fn list(&self) -> io::Result<Vec<ReportFile>>;

    /// At most `max` bytes of the file `name`, from its start.
    ///
    /// # Errors
    /// The read's own.
    fn read(&self, name: &str, max: u64) -> io::Result<Vec<u8>>;
}

/// A directory of the host.
pub struct HostReportDir {
    dir: PathBuf,
}

impl HostReportDir {
    /// The directory `dir`.
    #[must_use]
    pub fn at(dir: &Path) -> HostReportDir {
        HostReportDir {
            dir: dir.to_path_buf(),
        }
    }
}

impl ReportDir for HostReportDir {
    fn list(&self) -> io::Result<Vec<ReportFile>> {
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let modified_unix = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            files.push(ReportFile {
                name: entry.file_name().to_string_lossy().into_owned(),
                modified_unix,
            });
        }
        Ok(files)
    }

    fn read(&self, name: &str, max: u64) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        fs::File::open(self.dir.join(name))?
            .take(max)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

/// A Mac's hardware sources, read into its keeper.
pub struct MacHealth {
    keeper: Arc<HealthKeeper>,
    reports: Box<dyn ReportDir>,
    sysctl: Box<dyn Launchctl>,
    profiler: Box<dyn Launchctl>,
    clock: Arc<dyn UnixClock>,
    log: Arc<dyn Log>,
}

impl MacHealth {
    /// The sources of `keeper`: the panic reports of `reports`, the capacity from `sysctl` and
    /// `profiler` (the programs [`CAPACITY_SYSCTL_ARGS`] and [`PROFILER_ARGS`] are given to), the
    /// second of each read from `clock`, lines to `log`.
    #[must_use]
    pub fn new(
        keeper: Arc<HealthKeeper>,
        reports: Box<dyn ReportDir>,
        sysctl: Box<dyn Launchctl>,
        profiler: Box<dyn Launchctl>,
        clock: Arc<dyn UnixClock>,
        log: Arc<dyn Log>,
    ) -> MacHealth {
        MacHealth {
            keeper,
            reports,
            sysctl,
            profiler,
            clock,
            log,
        }
    }

    /// The sources of a Mac's own files and programs.
    #[must_use]
    pub fn at_host(
        keeper: Arc<HealthKeeper>,
        clock: Arc<dyn UnixClock>,
        log: Arc<dyn Log>,
    ) -> MacHealth {
        MacHealth::new(
            keeper,
            Box::new(HostReportDir::at(Path::new(DIAGNOSTIC_REPORTS))),
            Box::new(LaunchctlProcess::new(Path::new(SYSCTL), TOOL_DEADLINE)),
            Box::new(LaunchctlProcess::new(
                Path::new(SYSTEM_PROFILER),
                TOOL_DEADLINE,
            )),
            clock,
            log,
        )
    }

    /// The schedule (the module's doc), each wait through `sleeper`; returns when it says stop.
    pub fn run(&self, sleeper: &dyn Sleeper) {
        self.read_capacity();
        let mut read = self.read_panics();
        if sleeper.sleep(CAPACITY_AGAIN_SECS) == Tick::Stop {
            return;
        }
        self.read_capacity();
        read.extend(self.read_panics());
        self.judge_shutdown(&read);
        while sleeper.sleep(PANIC_PERIOD_SECS) == Tick::Beat {
            self.read_panics();
        }
    }

    /// Reads the panic reports not read before, holds their records, and keeps the source's
    /// state and mark (the module's doc); the reports it read.
    pub fn read_panics(&self) -> Vec<PanicReport> {
        let now = self.clock.unix_secs();
        let files = match self.reports.list() {
            Ok(files) => files,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                self.log.line(&format!(
                    "error: the panic reports are not read: {DIAGNOSTIC_REPORTS} cannot be \
                     listed: {e}"
                ));
                self.state(
                    PANIC_SOURCE,
                    pb::SourceStatus::Unreadable,
                    &format!("not listed: {e}"),
                );
                return Vec::new();
            }
        };
        let before = self.keeper.mark(PANIC_SOURCE);
        let marked: BTreeSet<String> = before
            .as_deref()
            .map(|mark| mark.lines().map(str::to_owned).collect())
            .unwrap_or_default();
        let mut files: Vec<ReportFile> = files
            .into_iter()
            .filter(|f| is_candidate(&f.name))
            .collect();
        files.sort_by(|a, b| (b.modified_unix, &b.name).cmp(&(a.modified_unix, &a.name)));
        files.truncate(MAX_MARKED);
        let mut reports = Vec::new();
        let mut kept = Vec::new();
        for file in files {
            if marked.contains(&file.name) {
                kept.push(file.name);
                continue;
            }
            match self.reports.read(&file.name, MAX_REPORT_BYTES) {
                Ok(bytes) => {
                    if let Some(report) = parse(&file.name, &bytes, file.modified_unix) {
                        reports.push(report);
                    }
                    kept.push(file.name);
                }
                Err(e) => self
                    .log
                    .line(&format!("error: the report {} is not read: {e}", file.name)),
            }
        }
        kept.sort();
        let mark = kept.join("\n");
        self.state(PANIC_SOURCE, pb::SourceStatus::Ok, "");
        if before.as_deref() != Some(mark.as_str()) || !reports.is_empty() {
            let boot_unix = self.keeper.boot_unix();
            let events = reports.iter().map(|r| r.event(boot_unix)).collect();
            self.keeper.record(PANIC_SOURCE, &mark, events, now);
        }
        for report in &reports {
            self.log.line(&format!(
                "a kernel panic report was read: {} ({})",
                report.event_id,
                report.class.word()
            ));
        }
        reports
    }

    /// Holds the record of an unexpected restart when this start is the first of a boot whose
    /// boot before ended unclean, merged with the newest of `reports` of that boot (the module's
    /// doc); whether it held one.
    pub fn judge_shutdown(&self, reports: &[PanicReport]) -> bool {
        let (Some(previous), Some(boot_id)) =
            (self.keeper.unclean_previous_boot(), self.keeper.boot_id())
        else {
            return false;
        };
        if self.keeper.mark(SHUTDOWN_SOURCE).as_deref() == Some(boot_id.as_str()) {
            return false;
        }
        let boot_unix = self.keeper.boot_unix();
        let panic = reports
            .iter()
            .filter(|r| of_boot(r, &previous, boot_unix))
            .max_by_key(|r| (r.time_unix, r.event_id.clone()));
        let (message, raw) = match panic {
            Some(report) => (
                format!(
                    "Unexpected restart after a kernel panic ({})",
                    report.class.word()
                ),
                report.panic_line.clone(),
            ),
            None => (
                "Unexpected restart: the boot before ended with no clean shutdown".to_owned(),
                format!(
                    "boot {} ended with no clean-shutdown marker",
                    previous.boot_id
                ),
            ),
        };
        let event = pb::HardwareEvent {
            event_id: cut(&format!("{SHUTDOWN_SOURCE}:{boot_id}"), MAX_EVENT_ID),
            time_unix: boot_unix,
            of_previous_boot: true,
            component: pb::HardwareComponent::Host.into(),
            subject: HOST_SUBJECT.to_owned(),
            severity: pb::HardwareSeverity::Warning.into(),
            state: pb::HardwareEventState::Event.into(),
            source: SHUTDOWN_SOURCE.to_owned(),
            code: UNEXPECTED_REBOOT.to_owned(),
            message: cut(&message, MAX_MESSAGE),
            raw: cut(&raw, MAX_RAW),
            about: String::new(),
        };
        let now = self.clock.unix_secs();
        self.keeper
            .record(SHUTDOWN_SOURCE, &boot_id, vec![event], now)
            > 0
    }

    /// Reads the capacity and keeps it and the source's state (the module's doc).
    pub fn read_capacity(&self) {
        let mut failures = Vec::new();
        let sysctls = run_text(self.sysctl.as_ref(), &CAPACITY_SYSCTL_ARGS, "sysctl")
            .and_then(|text| {
                parse_sysctls(&text).ok_or_else(|| "sysctl printed no three numbers".to_owned())
            })
            .map_err(|why| failures.push(why))
            .ok();
        let profile = run_text(self.profiler.as_ref(), &PROFILER_ARGS, "system_profiler")
            .and_then(|text| {
                parse_profile(&text).ok_or_else(|| "system_profiler printed no JSON".to_owned())
            })
            .map_err(|why| failures.push(why))
            .ok();
        self.keeper
            .set_capacity(capacity(sysctls, profile.as_ref()));
        match failures.first() {
            None => self.state(CAPACITY_SOURCE, pb::SourceStatus::Ok, ""),
            Some(why) => {
                self.log
                    .line(&format!("error: the capacity is not wholly read: {why}"));
                self.state(CAPACITY_SOURCE, pb::SourceStatus::Unreadable, why);
            }
        }
    }

    fn state(&self, source: &str, status: pb::SourceStatus, reason: &str) {
        self.keeper.set_source(pb::SourceState {
            source: source.to_owned(),
            status: status.into(),
            reason: reason.to_owned(),
        });
    }
}

/// Whether `report` happened in the boot `previous`, which the boot that began at `boot_unix`
/// followed: at or after `previous` began (when known), and before this one began (when known).
fn of_boot(report: &PanicReport, previous: &PreviousBoot, boot_unix: u64) -> bool {
    report.time_unix >= previous.boot_unix && (boot_unix == 0 || report.time_unix < boot_unix)
}

/// What `program` printed with `args`, when it ran and exited 0; why not otherwise, named `name`.
fn run_text(program: &dyn Launchctl, args: &[&str], name: &str) -> Result<String, String> {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    let output = program
        .run(&args)
        .map_err(|e| format!("{name} did not run: {e}"))?;
    match output.code {
        Some(0) => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        Some(code) => Err(format!("{name} exited {code}")),
        None => Err(format!("{name} was ended by a signal")),
    }
}

/// Starts `health`'s schedule on its own thread when the host is a Mac (`kind`), each wait through
/// `sleeper`; whether it started. The thread is not joined: it ends at the sleeper's stop, or
/// with the process. A thread that cannot be made starts nothing, and the host's heartbeats go on
/// without its sources.
#[must_use]
pub fn start(kind: PlatformKind, health: MacHealth, sleeper: Box<dyn Sleeper + Send>) -> bool {
    kind == PlatformKind::Macos
        && std::thread::Builder::new()
            .name("hostkeeper-health".to_owned())
            .spawn(move || health.run(sleeper.as_ref()))
            .is_ok()
}
