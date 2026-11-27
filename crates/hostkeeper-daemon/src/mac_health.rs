//! A Mac's hardware sources (docs/design/host-health-alerts.md sections 3.2, 4.4 and 4.5), each a
//! [`HardwareSource`] the daemon runs on a worker of its own ([`crate::hardware_sources`]), so a
//! slow program holds no heartbeat: its kernel panic reports, with its unexpected restarts, and its
//! capacity. Each reads through ports, so all of it runs against fakes; [`for_host`] gives a
//! Mac's own.
//!
//! **Panic reports** ([`PanicSource`], source [`PANIC_SOURCE`]): the `.panic` and `.ips` files of
//! [`DIAGNOSTIC_REPORTS`] itself, regular files only (so `Retired/` and links are not read), the
//! newest [`MAX_MARKED`] of them by their time (a directory of more passes over the older ones),
//! each read once, to at most [`MAX_REPORT_BYTES`], and parsed by [`crate::mac_panic`]. The
//! source's mark is the names of the reports read that are still among them, so a report is not
//! read again while it stays and the mark does not grow past its bound; a read that changes
//! neither the mark nor the records keeps the mark as it is, and the state file is not written. A
//! directory that does not exist has no reports. One that cannot be listed is the source
//! `unreadable` with its reason. A report that cannot be read makes the source `unreadable`
//! (naming the first such report) while the others are still read, and is read again next time;
//! an entry that leaves the directory while it is listed has no time (0) and is such a report.
//! Read at the start, again after [`JUDGE_AFTER_SECS`], then every [`PANIC_PERIOD_SECS`].
//!
//! **An unexpected restart** ([`judge_shutdown`], source [`SHUTDOWN_SOURCE`], at the panic
//! source's second read, [`JUDGE_AFTER_SECS`] after the start, by when macOS has written the panic
//! report of the boot before): in a boot whose boot before ended with no clean-shutdown marker
//! ([`HealthKeeper::unclean_previous_boot`], which a restart of the daemon in that boot still
//! knows), one `host` record, a warning, `unexpected_reboot`, whose id is [`SHUTDOWN_SOURCE`] and
//! the boot id; the source's mark is the boot id judged, so the boot gives one record however
//! often the daemon starts in it. It is merged with the newest panic report of that boot (made
//! after it began, and before this one did) read since the start, whose class its message names
//! and whose panic line is its `raw`. On a Mac the marker is advisory (launchd's SIGTERM of a
//! shutdown and of a `bootout` look alike), so the record is evidence for the coordinator, which
//! judges it with the heartbeats' gap.
//!
//! **Capacity** ([`CapacitySource`], source [`CAPACITY_SOURCE`]): `sysctl` and `system_profiler`,
//! each a fixed argument list through the [`Launchctl`] port (an empty environment, capped output,
//! killed at [`crate::mac::TOOL_DEADLINE`]), judged by [`crate::mac_capacity`] and kept as the
//! heartbeat's `baseline` ([`HealthKeeper::set_capacity`]). Either failing makes the source
//! `unreadable`, with the first failure's reason; the capacity is still what the other gave. Read
//! at the start, again after [`CAPACITY_AGAIN_SECS`] (early values can settle), then every
//! [`CAPACITY_PERIOD_SECS`].

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use hostkeeper_launchd::{Launchctl, LaunchctlProcess, PlatformKind};
use hostkeeper_proto::health::{MAX_EVENT_ID, MAX_MESSAGE, MAX_RAW};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use crate::hardware_sources::{HardwareSource, SourceRead};
use crate::health_state::cut;
use crate::host_health::{HealthKeeper, UncleanBoot};
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

/// The most reports read and kept in the panic source's mark: the newest.
pub const MAX_MARKED: usize = 512;

/// The source token of an unexpected restart's record.
pub const SHUTDOWN_SOURCE: &str = "shutdown";

/// The `code` of an unexpected restart's record.
pub const UNEXPECTED_REBOOT: &str = "unexpected_reboot";

/// The subject of an unexpected restart's record.
pub const HOST_SUBJECT: &str = "host";

/// The source token of the capacity's state.
pub const CAPACITY_SOURCE: &str = "system_profiler";

/// The seconds from the panic source's first read to its second, at which the restart is judged.
pub const JUDGE_AFTER_SECS: u64 = 120;

/// The seconds between two reads of the panic reports after the second.
pub const PANIC_PERIOD_SECS: u64 = 300;

/// The seconds from the capacity's first read to its second.
pub const CAPACITY_AGAIN_SECS: u64 = 120;

/// The seconds between two reads of the capacity after the second: a day.
pub const CAPACITY_PERIOD_SECS: u64 = 24 * 60 * 60;

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

/// The panic reports, and the unexpected restart judged at the second read.
pub struct PanicSource {
    keeper: Arc<HealthKeeper>,
    reports: Box<dyn ReportDir>,
    /// The reports read since the start, until the restart is judged.
    read: Vec<PanicReport>,
    /// How many times the source was read.
    reads: u64,
}

impl PanicSource {
    /// The panic reports of `reports`, whose restart is judged into `keeper`.
    #[must_use]
    pub fn new(keeper: Arc<HealthKeeper>, reports: Box<dyn ReportDir>) -> PanicSource {
        PanicSource {
            keeper,
            reports,
            read: Vec::new(),
            reads: 0,
        }
    }

    fn read_reports(&self, mark: Option<&str>) -> (SourceRead, Vec<PanicReport>) {
        let files = match self.reports.list() {
            Ok(files) => files,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                return (
                    SourceRead::unreadable(format!("not listed: {e}")),
                    Vec::new(),
                );
            }
        };
        let marked: BTreeSet<&str> = mark.map(|m| m.lines().collect()).unwrap_or_default();
        let mut files: Vec<ReportFile> = files
            .into_iter()
            .filter(|f| is_candidate(&f.name))
            .collect();
        files.sort_by(|a, b| (b.modified_unix, &b.name).cmp(&(a.modified_unix, &a.name)));
        files.truncate(MAX_MARKED);
        let mut reports = Vec::new();
        let mut kept = Vec::new();
        let mut failed = None;
        for file in files {
            if marked.contains(file.name.as_str()) {
                kept.push(file.name);
                continue;
            }
            match self.reports.read(&file.name, MAX_REPORT_BYTES) {
                Ok(bytes) => {
                    reports.extend(parse(&file.name, &bytes, file.modified_unix));
                    kept.push(file.name);
                }
                Err(e) => {
                    failed = failed.or_else(|| Some(format!("{} not read: {e}", file.name)));
                }
            }
        }
        kept.sort();
        let new_mark = kept.join("\n");
        let boot_unix = self.keeper.boot_unix();
        let events: Vec<pb::HardwareEvent> = reports.iter().map(|r| r.event(boot_unix)).collect();
        let changed = mark != Some(new_mark.as_str()) || !events.is_empty();
        let (status, reason) = match failed {
            None => (pb::SourceStatus::Ok, String::new()),
            Some(why) => (pb::SourceStatus::Unreadable, why),
        };
        let read = SourceRead {
            status,
            reason,
            mark: changed.then_some(new_mark),
            events,
        };
        (read, reports)
    }
}

impl HardwareSource for PanicSource {
    fn name(&self) -> &'static str {
        PANIC_SOURCE
    }

    fn every_secs(&self) -> u64 {
        if self.reads < 2 {
            JUDGE_AFTER_SECS
        } else {
            PANIC_PERIOD_SECS
        }
    }

    fn read(&mut self, mark: Option<&str>, now_unix: u64) -> SourceRead {
        let (read, reports) = self.read_reports(mark);
        self.reads += 1;
        if self.reads <= 2 {
            self.read.extend(reports);
        }
        if self.reads == 2 {
            judge_shutdown(&self.keeper, &self.read, now_unix);
            self.read = Vec::new();
        }
        read
    }
}

/// Holds the record of an unexpected restart when the boot before this one ended unclean and this
/// boot was not judged yet, merged with the newest of `reports` of that boot (the module's doc);
/// whether it held one.
pub fn judge_shutdown(keeper: &HealthKeeper, reports: &[PanicReport], now_unix: u64) -> bool {
    let (Some(previous), Some(boot_id)) = (keeper.unclean_previous_boot(), keeper.boot_id()) else {
        return false;
    };
    if keeper.mark(SHUTDOWN_SOURCE).as_deref() == Some(boot_id.as_str()) {
        return false;
    }
    let boot_unix = keeper.boot_unix();
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
    keeper.record(SHUTDOWN_SOURCE, &boot_id, vec![event], now_unix) > 0
}

/// Whether `report` happened in the boot `previous`, which the boot that began at `boot_unix`
/// followed: at or after `previous` began (when known), and before this one began (when known).
fn of_boot(report: &PanicReport, previous: &UncleanBoot, boot_unix: u64) -> bool {
    report.time_unix >= previous.boot_unix && (boot_unix == 0 || report.time_unix < boot_unix)
}

/// The capacity, from `sysctl` and `system_profiler`.
pub struct CapacitySource {
    keeper: Arc<HealthKeeper>,
    sysctl: Box<dyn Launchctl>,
    profiler: Box<dyn Launchctl>,
    /// How many times the source was read.
    reads: u64,
}

impl CapacitySource {
    /// The capacity from `sysctl` and `profiler` (the programs [`CAPACITY_SYSCTL_ARGS`] and
    /// [`PROFILER_ARGS`] are given to), kept in `keeper`.
    #[must_use]
    pub fn new(
        keeper: Arc<HealthKeeper>,
        sysctl: Box<dyn Launchctl>,
        profiler: Box<dyn Launchctl>,
    ) -> CapacitySource {
        CapacitySource {
            keeper,
            sysctl,
            profiler,
            reads: 0,
        }
    }
}

impl HardwareSource for CapacitySource {
    fn name(&self) -> &'static str {
        CAPACITY_SOURCE
    }

    fn every_secs(&self) -> u64 {
        if self.reads < 2 {
            CAPACITY_AGAIN_SECS
        } else {
            CAPACITY_PERIOD_SECS
        }
    }

    fn read(&mut self, _mark: Option<&str>, _now_unix: u64) -> SourceRead {
        self.reads += 1;
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
        match failures.into_iter().next() {
            None => SourceRead {
                status: pb::SourceStatus::Ok,
                reason: String::new(),
                mark: None,
                events: Vec::new(),
            },
            Some(why) => SourceRead::unreadable(why),
        }
    }
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

/// The sources of a host of `kind` over `keeper`: on a Mac its own panic reports
/// ([`DIAGNOSTIC_REPORTS`]) and capacity (`sysctl`, `system_profiler`); none on any other host.
#[must_use]
pub fn for_host(kind: PlatformKind, keeper: &Arc<HealthKeeper>) -> Vec<Box<dyn HardwareSource>> {
    if kind != PlatformKind::Macos {
        return Vec::new();
    }
    vec![
        Box::new(PanicSource::new(
            Arc::clone(keeper),
            Box::new(HostReportDir::at(Path::new(DIAGNOSTIC_REPORTS))),
        )),
        Box::new(CapacitySource::new(
            Arc::clone(keeper),
            Box::new(LaunchctlProcess::new(Path::new(SYSCTL), TOOL_DEADLINE)),
            Box::new(LaunchctlProcess::new(
                Path::new(SYSTEM_PROFILER),
                TOOL_DEADLINE,
            )),
        )),
    ]
}
