//! The daemon's readers of its host's hardware (docs/design/host-health-alerts.md sections 3.1,
//! 4.1, 4.2 and 4.5): each a [`HardwareSource`], run on a worker of its own so that a read that
//! hangs stops that source only, and read again every [`HardwareSource::every_secs`].
//!
//! **One read** ([`poll`]): the source is given the high-water mark it kept last
//! ([`HealthKeeper::mark`]: its own words, none before its first read on this host) and the
//! second it reads at, and answers a [`SourceRead`]: whether it could read (`ok`, `unsupported`,
//! `unreadable` with a reason), its records, and its new mark. The keeper holds the records and the
//! mark, written to the state file together, and reports the state in every heartbeat's
//! `HostHealth.sources` ([`HealthKeeper::read`]). A source that cannot read at all keeps its mark.
//!
//! **The worker** ([`run`]): one read, then a wait of the source's period on the daemon's clock,
//! until the sleeper says stop. [`start`] gives each source its worker; [`at_host`] starts the
//! sources of a host of a kind, and [`start_host`] does so for the daemon, when it keeps a health
//! state, each worker waiting on the daemon's clock until its stop flag is set: on Linux (systemd and OpenRC) SMART through `smartctl`
//! ([`crate::smart`]), the hwmon temperatures ([`crate::hwmon`]), the kernel's log and files
//! ([`crate::kernel_sources`]) and the BMC's event log through the IPMI device
//! ([`crate::sel_source`]); none on a Mac yet.

use std::path::Path;
use std::sync::Arc;

use hostkeeper_launchd::{LaunchctlProcess, PlatformKind};
use hostkeeper_link::{ClockSleeper, Dispatch, Sleeper, StopFlag, Tick, UnixClock};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_steps::Clock;

use crate::host_health::{HealthKeeper, PROC_STAT};
use crate::hwmon::HwmonSource;
use crate::ipmi::{DevIpmi, IPMI_DEVICE};
use crate::kernel_sources::{self, KernelBoot};
use crate::sel_source::SelReader;
use crate::smart::{SMARTCTL, SMARTCTL_DEADLINE, SmartSource};

/// What one read of a source came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRead {
    /// Whether it could read.
    pub status: pb::SourceStatus,
    /// Why it is unsupported or unreadable, in plain words; empty when it read.
    pub reason: String,
    /// Its new high-water mark; none keeps the mark it had.
    pub mark: Option<String>,
    /// The records it made.
    pub events: Vec<pb::HardwareEvent>,
}

impl SourceRead {
    /// A read that worked, with its new `mark` and its `events`.
    #[must_use]
    pub fn ok(mark: String, events: Vec<pb::HardwareEvent>) -> SourceRead {
        SourceRead {
            status: pb::SourceStatus::Ok,
            reason: String::new(),
            mark: Some(mark),
            events,
        }
    }

    /// The host does not have the source (`reason`): no records, and the mark kept.
    #[must_use]
    pub fn unsupported(reason: String) -> SourceRead {
        SourceRead {
            status: pb::SourceStatus::Unsupported,
            reason,
            mark: None,
            events: Vec::new(),
        }
    }

    /// The host has the source and reading it failed (`reason`): no records, and the mark kept.
    #[must_use]
    pub fn unreadable(reason: String) -> SourceRead {
        SourceRead {
            status: pb::SourceStatus::Unreadable,
            reason,
            mark: None,
            events: Vec::new(),
        }
    }
}

/// One reader of the host's hardware.
pub trait HardwareSource: Send {
    /// Its token, as its records' `source` and its `SourceState` name it.
    fn name(&self) -> &'static str;

    /// The seconds between two reads.
    fn every_secs(&self) -> u64;

    /// Reads the source at `now_unix`, after the read that kept `mark` (none before its first
    /// read on this host). It never fails: what goes wrong is its status and reason.
    fn read(&mut self, mark: Option<&str>, now_unix: u64) -> SourceRead;
}

/// One read of `source` at the second `clock` gives, handed to `keeper`; how many records it
/// holds now that it did not before.
pub fn poll(
    source: &mut dyn HardwareSource,
    keeper: &HealthKeeper,
    clock: &dyn UnixClock,
) -> usize {
    let now = clock.unix_secs();
    let mark = keeper.mark(source.name());
    let read = source.read(mark.as_deref(), now);
    keeper.read(source.name(), read, now)
}

/// The worker of `source`: a read ([`poll`]), then a wait of its period on `sleeper`, until the
/// sleeper says stop.
pub fn run(
    source: &mut dyn HardwareSource,
    keeper: &HealthKeeper,
    clock: &dyn UnixClock,
    sleeper: &dyn Sleeper,
) {
    loop {
        poll(source, keeper, clock);
        if sleeper.sleep(source.every_secs()) == Tick::Stop {
            return;
        }
    }
}

/// Starts a worker ([`run`]) for each of `sources` on `dispatch`, each waiting on a sleeper of
/// its own from `sleeper`.
pub fn start(
    sources: Vec<Box<dyn HardwareSource>>,
    keeper: &Arc<HealthKeeper>,
    clock: &Arc<dyn UnixClock>,
    sleeper: &dyn Fn() -> Box<dyn Sleeper + Send>,
    dispatch: &dyn Dispatch,
) {
    for mut source in sources {
        let keeper = Arc::clone(keeper);
        let clock = Arc::clone(clock);
        let sleeper = sleeper();
        dispatch.start(Box::new(move || {
            run(source.as_mut(), &keeper, clock.as_ref(), sleeper.as_ref());
        }));
    }
}

/// The sources of a host of `kind` whose root is `root`, in the boot `boot`: on Linux SMART,
/// through the `smartctl` at `smartctl` (absent: `unsupported`), the hwmon temperatures, the
/// kernel's log and files ([`crate::kernel_sources::at_root`]), and the BMC's event log through
/// the IPMI device under `root` (absent: `unsupported`), in the boot whose start `root`'s
/// [`PROC_STAT`] gives at its first read; none on a Mac.
#[must_use]
pub fn for_host(
    kind: PlatformKind,
    root: &Path,
    smartctl: &Path,
    boot: &KernelBoot,
) -> Vec<Box<dyn HardwareSource>> {
    match kind {
        PlatformKind::Linux | PlatformKind::Openrc => {
            let mut sources: Vec<Box<dyn HardwareSource>> = vec![
                Box::new(SmartSource::new(
                    root,
                    smartctl,
                    Box::new(LaunchctlProcess::new(smartctl, SMARTCTL_DEADLINE)),
                )),
                Box::new(HwmonSource::new(root)),
            ];
            sources.extend(kernel_sources::at_root(root, boot));
            sources.push(Box::new(SelReader::new(
                Arc::new(DevIpmi::new(
                    &root.join(IPMI_DEVICE.trim_start_matches('/')),
                )),
                root.join(PROC_STAT),
            )));
            sources
        }
        PlatformKind::Macos => Vec::new(),
    }
}

/// Starts the sources of this host, a host of `kind` ([`for_host`] over `/` and [`SMARTCTL`]),
/// each on a thread of its own, reading at `clock`'s seconds and waiting on `sleeper`'s.
pub fn at_host(
    kind: PlatformKind,
    keeper: &Arc<HealthKeeper>,
    clock: &Arc<dyn UnixClock>,
    sleeper: &dyn Fn() -> Box<dyn Sleeper + Send>,
    dispatch: &dyn Dispatch,
) {
    let boot = KernelBoot::of(keeper);
    let sources = for_host(kind, Path::new("/"), Path::new(SMARTCTL), &boot);
    start(sources, keeper, clock, sleeper, dispatch);
}

/// Starts the sources of this host, a host of `kind`, for the daemon ([`at_host`]) when it keeps a
/// health state (`health`): each worker reads at `clock`'s seconds and waits on `waits`, stopping
/// at the end of a wait once `stop` is set. With no health state it starts nothing.
pub fn start_host(
    kind: PlatformKind,
    health: Option<&Arc<HealthKeeper>>,
    clock: &Arc<dyn UnixClock>,
    waits: &Arc<dyn Clock>,
    stop: &StopFlag,
    dispatch: &dyn Dispatch,
) {
    let Some(keeper) = health else {
        return;
    };
    let (waits, stop) = (Arc::clone(waits), stop.clone());
    let sleeper = move || -> Box<dyn Sleeper + Send> {
        Box::new(ClockSleeper::new(Arc::clone(&waits), stop.clone()))
    };
    at_host(kind, keeper, clock, &sleeper, dispatch);
}
