//! What the host last found of its maintenance (`maintenance-v1`, docs/design/maintenance-updates.md
//! section 1): the [`MaintenanceSource`] of the daemon's heartbeat loop. It keeps the
//! `MaintenanceReport` of the last look and gives it to every heartbeat that asks.
//!
//! **A look** runs on a worker of the sink's [`Dispatch`]: it reads the packages hostkeeper
//! itself installs ([`OwnPackages`]), then asks each [`Collector`] in order what it finds. The
//! sections join into the report ([`Section`]): the host texts, the Xcodes and the Mac's facts as
//! the collectors give them, what is due ordered reboots first, then security updates, then the
//! rest, each group in the order found, at most [`MAX_DUE`] of them (the count of the rest is
//! `due_left_out`), every text kept to its bound, and the report checked against the wire's
//! bounds ([`check_report`]) before it is kept. Its `looked_unix` is the second the look started.
//!
//! **A look that fails** (a collector's reason, naming it; the packages of hostkeeper that cannot
//! be read; a report that breaks a bound) stops at the first failure and leaves the rest of the
//! report as the last look that succeeded left it, with the reason in `look_failed`. The next
//! look clears it.
//!
//! **When.** A report ([`MaintenanceSource::report`]) starts a look when none is running and one is due:
//! before the first, [`LOOK_SECS`] after the last one started (however it ended), and at once
//! after [`MaintenanceFacts::look_again`]. It never waits for the look: a report made while it
//! runs is the report as it stands, which before the first look ends is the default one
//! (`looked_unix` 0). Nothing is locked while a collector runs. A look whose worker never ran, or
//! died, ends as well and is tried again by the next report. Every look is a line of the daemon's
//! log.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hostkeeper_launchd::PlatformKind;
use hostkeeper_link::{Dispatch, Log, MaintenanceSource, ThreadDispatch, UnixClock};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_proto::maintenance::{
    MAX_DUE, MAX_DUE_LABEL, MAX_DUE_VERSION, MAX_KERNEL_TEXT, MAX_LOOK_FAILED, MAX_OS_TEXT,
    check_report,
};

use crate::maintenance_collect::{Collector, LinuxScope, Look, Section, clip};
use crate::maintenance_collectors;
use crate::maintenance_own::{OwnPackages, PlacedPackages};
use crate::unix_clock::SystemUnixClock;

/// How long after a look started the next one is due, in seconds.
pub const LOOK_SECS: u64 = 60 * 60;

/// What the sink keeps between two calls.
#[derive(Default)]
struct State {
    /// The report of the last look that succeeded, with the reason of the last look when it
    /// failed.
    report: pb::MaintenanceReport,
    /// Whether a look was started and has not ended.
    running: bool,
    /// The second from which the next look is due: [`LOOK_SECS`] after the last one started, and
    /// 0 (due at once) before the first and after [`MaintenanceFacts::look_again`].
    next_look_unix: u64,
}

struct Inner {
    collectors: Vec<Box<dyn Collector>>,
    own: Box<dyn OwnPackages>,
    scope: LinuxScope,
    clock: Arc<dyn UnixClock>,
    dispatch: Arc<dyn Dispatch>,
    log: Arc<dyn Log>,
    state: Mutex<State>,
}

/// The daemon's maintenance facts.
pub struct MaintenanceFacts {
    inner: Arc<Inner>,
}

/// A look that was started; the sink's `running` ends with it, however it ends, and also when its
/// dispatcher drops the job without running it. A look that did not [`finish`](Running::finish)
/// (its job was dropped, or its worker panicked) is due again at the next report.
struct Running {
    inner: Arc<Inner>,
    finished: bool,
}

impl Running {
    /// The look ended with an outcome, a report or a reason.
    fn finish(mut self) {
        self.finished = true;
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let mut state = self.inner.state();
        state.running = false;
        if !self.finished {
            state.next_look_unix = 0;
        }
    }
}

impl MaintenanceFacts {
    /// The sink over `collectors`, read in their order, and the packages `own` names, under
    /// `scope`, over `clock` and `log`, whose looks run on `dispatch`.
    #[must_use]
    pub fn new(
        collectors: Vec<Box<dyn Collector>>,
        own: Box<dyn OwnPackages>,
        scope: LinuxScope,
        clock: Arc<dyn UnixClock>,
        dispatch: Arc<dyn Dispatch>,
        log: Arc<dyn Log>,
    ) -> MaintenanceFacts {
        MaintenanceFacts {
            inner: Arc::new(Inner {
                collectors,
                own,
                scope,
                clock,
                dispatch,
                log,
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// The sink of a host of `kind` under `scope`, over the collectors of its platform
    /// ([`maintenance_collectors::at_host`]) and the packages recorded in the state directory
    /// `placed_dir`; none when the platform has no collector (a Mac today), so that it announces
    /// and sends nothing. Its clock is the host's, and each look runs on a thread of its own.
    #[must_use]
    pub fn at_host(
        kind: PlatformKind,
        scope: LinuxScope,
        log: Arc<dyn Log>,
        placed_dir: &Path,
    ) -> Option<MaintenanceFacts> {
        let collectors = maintenance_collectors::at_host(kind);
        if collectors.is_empty() {
            return None;
        }
        Some(MaintenanceFacts::new(
            collectors,
            Box::new(PlacedPackages::new(placed_dir)),
            scope,
            Arc::new(SystemUnixClock),
            Arc::new(ThreadDispatch),
            log,
        ))
    }

    /// A maintenance step ended, so what the host has installed changed: the next [`report`]
    /// starts a look at once, also while one is running (the look after it starts at the first
    /// report after it ends).
    ///
    /// [`report`]: MaintenanceSource::report
    pub fn look_again(&self) {
        self.inner.state().next_look_unix = 0;
    }

    /// Starts a look when one is due and none is running.
    fn start_if_due(&self) {
        let inner = &self.inner;
        let now = inner.clock.unix_secs();
        {
            let mut state = inner.state();
            if state.running || now < state.next_look_unix {
                return;
            }
            state.running = true;
            state.next_look_unix = now.saturating_add(LOOK_SECS);
        }
        let running = Running {
            inner: Arc::clone(inner),
            finished: false,
        };
        let worker = Arc::clone(inner);
        inner.dispatch.start(Box::new(move || {
            worker.look(now);
            running.finish();
        }));
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Looks at the host and keeps what it found, or why it could not, and logs it.
    fn look(&self, started_unix: u64) {
        match self.collect(started_unix) {
            Ok(report) => {
                let line = format!(
                    "maintenance: looked: {} due, {} unparsed lines",
                    report.due.len() + report.due_left_out as usize,
                    report.unparsed_lines
                );
                self.state().report = report;
                self.log.line(&line);
            }
            Err(reason) => {
                let reason = clip(&reason, MAX_LOOK_FAILED);
                self.state().report.look_failed.clone_from(&reason);
                self.log
                    .line(&format!("error: maintenance: the look failed: {reason}"));
            }
        }
    }

    /// The report of a look that started at `started_unix`, or the reason it failed. No lock is
    /// held while a collector runs.
    fn collect(&self, started_unix: u64) -> Result<pb::MaintenanceReport, String> {
        let own = self
            .own
            .names()
            .map_err(|why| format!("own packages: {why}"))?;
        let look = Look {
            scope: self.scope,
            own_packages: &own,
        };
        let sections = self
            .collectors
            .iter()
            .map(|collector| {
                collector
                    .collect(&look)
                    .map_err(|why| format!("{}: {why}", collector.name()))
            })
            .collect::<Result<Vec<Section>, String>>()?;
        let report = join(sections, started_unix);
        check_report(&report).map_err(|error| format!("report: {error}"))?;
        Ok(report)
    }
}

/// The report the `sections` make, in the order given, of a look that started at `looked_unix`,
/// as the module says.
fn join(sections: Vec<Section>, looked_unix: u64) -> pb::MaintenanceReport {
    let mut host = pb::MaintenanceHost::default();
    let mut report = pb::MaintenanceReport {
        looked_unix,
        ..pb::MaintenanceReport::default()
    };
    let mut due = Vec::new();
    for section in sections {
        fill_host(&mut host, &section.host);
        report.xcodes.extend(section.xcodes);
        report.mac = section.mac.or(report.mac);
        due.extend(section.due);
        report.unparsed_lines = report.unparsed_lines.saturating_add(section.unparsed);
    }
    host.os_name = clip(&host.os_name, MAX_OS_TEXT);
    host.os_version = clip(&host.os_version, MAX_OS_TEXT);
    host.os_build = clip(&host.os_build, MAX_OS_TEXT);
    host.hardware_serial = clip(&host.hardware_serial, MAX_OS_TEXT);
    host.running_kernel = clip(&host.running_kernel, MAX_KERNEL_TEXT);
    host.newest_kernel = clip(&host.newest_kernel, MAX_KERNEL_TEXT);
    host.boot_id = clip(&host.boot_id, MAX_KERNEL_TEXT);
    report.host = Some(host);
    let (kept, left_out) = order_due(due);
    report.due = kept;
    report.due_left_out = left_out;
    report
}

/// Takes from `from` each host text `into` holds none of, and its boot time when `into` has none.
fn fill_host(into: &mut pb::MaintenanceHost, from: &pb::MaintenanceHost) {
    let texts = [
        (&mut into.os_name, &from.os_name),
        (&mut into.os_version, &from.os_version),
        (&mut into.os_build, &from.os_build),
        (&mut into.hardware_serial, &from.hardware_serial),
        (&mut into.running_kernel, &from.running_kernel),
        (&mut into.newest_kernel, &from.newest_kernel),
        (&mut into.boot_id, &from.boot_id),
    ];
    for (held, found) in texts {
        if held.is_empty() {
            held.clone_from(found);
        }
    }
    if into.boot_unix == 0 {
        into.boot_unix = from.boot_unix;
    }
}

/// What is due, as the report keeps it: a reboot named twice kept once, the reboots first, then
/// the security updates, then the rest, each group in the order found; the first [`MAX_DUE`], each
/// text kept to its bound, and how many were left out.
fn order_due(mut due: Vec<pb::DueItem>) -> (Vec<pb::DueItem>, u32) {
    let reboot = i32::from(pb::MaintenanceKind::Reboot);
    let mut seen = BTreeSet::new();
    due.retain(|item| item.kind != reboot || seen.insert(item.label.clone()));
    due.sort_by_key(|item| {
        if item.kind == reboot {
            0_u8
        } else if item.security {
            1
        } else {
            2
        }
    });
    let left_out = u32::try_from(due.len().saturating_sub(MAX_DUE)).unwrap_or(u32::MAX);
    due.truncate(MAX_DUE);
    for item in &mut due {
        item.from = clip(&item.from, MAX_DUE_VERSION);
        item.to = clip(&item.to, MAX_DUE_VERSION);
        item.label = clip(&item.label, MAX_DUE_LABEL);
    }
    (due, left_out)
}

impl MaintenanceSource for MaintenanceFacts {
    fn report(&self) -> pb::MaintenanceReport {
        self.start_if_due();
        self.inner.state().report.clone()
    }
}

#[cfg(test)]
#[path = "maintenance_facts_tests.rs"]
mod tests;
