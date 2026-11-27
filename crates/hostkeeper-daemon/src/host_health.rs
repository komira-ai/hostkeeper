//! The host's hardware health as the daemon keeps it (docs/design/host-health-alerts.md sections
//! 4.1, 4.4 and 4.5): [`HealthKeeper`], over the state file of [`crate::health_state`], which the
//! heartbeat loop asks for the health of each heartbeat ([`HostHealthSource`]) and tells the
//! records the coordinator wrote.
//!
//! **At start** ([`HealthKeeper::open`]) the daemon reads its state file and the boot
//! ([`BootFacts`]: the boot id of the [`Clock`], the second the boot began, the machine id), and
//! writes the file again with the marker `running`. In a boot the file was not last written in,
//! the boot before ended cleanly when the file's marker is `clean`; in the same boot (the daemon
//! restarted, or crashed) what the first start of the boot found is kept. With no file, a file
//! moved aside or a boot id that cannot be read, the boot before is not known to have ended
//! cleanly. So a kill of the daemon, a crash or a power loss leaves `running` behind, which the
//! next boot reads as unclean.
//!
//! **At SIGTERM** ([`HealthKeeper::stopping`]) it writes `clean` only when its
//! [`ShutdownProbe`] says the system is going down; a SIGTERM of a daemon restart leaves
//! `running`. The daemon's exit for its own update sends no SIGTERM and writes nothing.
//!
//! **Records.** A source of the host's hardware gives what it read with
//! [`HealthKeeper::record`]: the records, held until the coordinator writes them, and its new
//! high-water mark, both written to the state file before it returns, so they survive a restart
//! of the daemon. The first read of a source on a host (no mark yet) is its history: a record of
//! it older than [`HISTORY_SECS`] is kept as `info`, its message starting [`HISTORY_PREFIX`], and
//! raises nothing. Each heartbeat carries at most six held records, the most severe first, then
//! the oldest; each is held with its texts cut to their bounds, so six of them and the boot keep
//! the health within its bound of 4096 bytes encoded (about 3.7 KiB at most). The ones the
//! coordinator names written ([`HostHealthSource::written`]) are dropped and the file written.
//!
//! **Source states.** Each read of a source ([`HealthKeeper::read`], from the workers of
//! [`crate::hardware_sources`]) also says whether the source could be read; the last word of each
//! source is kept in memory, by its token, and every heartbeat lists them in `sources` (at most
//! twelve, in the order of their tokens), so a source that reports nothing is never taken for one
//! that found nothing wrong. When the records and the states together would encode to more than
//! the bound, the heartbeat carries fewer records, the least severe and newest left for the next.
//!
//! A state file that cannot be written is logged and tried again at the next change; what the
//! daemon holds in memory is still sent.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hostkeeper_launchd::{Launchctl, LaunchctlProcess, PlatformKind};
use hostkeeper_link::{HostHealthSource, Log};
use hostkeeper_openrc::manager::OPENRC_RUN_DIR;
use hostkeeper_proto::health::{
    MAX_BOOT_ID, MAX_HEALTH_BYTES, MAX_MESSAGE, MAX_SOURCE, MAX_SOURCE_REASON, MAX_SOURCES,
};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_steps::Clock;
use prost::Message;

use crate::hardware_sources::SourceRead;
use crate::health_state::{self, HEALTH_FILE_ASIDE, HealthFile, Marker, Read, SourceMark, cut};
use crate::mac::TOOL_DEADLINE;
use crate::maintenance_boot::{BOOTTIME_ARGS, SYSCTL, parse_kern_boottime};
use crate::maintenance_host::parse_boot_time;
use crate::shutdown_probe::{self, GoingDown, ShutdownProbe};

/// How old a record of a source's first read must be to be history: seven days.
pub const HISTORY_SECS: u64 = 7 * 24 * 60 * 60;

/// The start of a history record's message.
pub const HISTORY_PREFIX: &str = "history: ";

/// Linux's machine id, under the host's root.
pub const MACHINE_ID: &str = "etc/machine-id";

/// Linux's boot time (`btime`), under the host's root.
pub const PROC_STAT: &str = "proc/stat";

/// The boot the daemon runs in, as it read it at its start.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BootFacts {
    /// The boot id (Linux's `boot_id`, a Mac's `kern.bootsessionuuid`), cut to its bound; none
    /// when it cannot be read.
    pub boot_id: Option<String>,
    /// When the boot began, Unix seconds; 0 when not read.
    pub boot_unix: u64,
    /// The machine id (`/etc/machine-id`); empty on a Mac, and where the file is absent or empty.
    pub machine_id: String,
}

impl BootFacts {
    /// The boot of a host of `kind` whose root is `root`: the boot id from `clock`; on Linux the
    /// `btime` of [`PROC_STAT`] and the [`MACHINE_ID`]; on a Mac the `sec` of
    /// `sysctl -n kern.boottime` run through `sysctl` (exit 0 only), and no machine id.
    #[must_use]
    pub fn read(
        kind: PlatformKind,
        root: &Path,
        clock: &dyn Clock,
        sysctl: &dyn Launchctl,
    ) -> BootFacts {
        let boot_id = clock.boot_id().ok().map(|id| cut(id.as_str(), MAX_BOOT_ID));
        match kind {
            PlatformKind::Linux | PlatformKind::Openrc => BootFacts {
                boot_id,
                boot_unix: fs::read_to_string(root.join(PROC_STAT))
                    .map_or(0, |text| parse_boot_time(&text)),
                machine_id: fs::read_to_string(root.join(MACHINE_ID))
                    .map(|text| text.trim().to_owned())
                    .unwrap_or_default(),
            },
            PlatformKind::Macos => {
                let args: Vec<String> = BOOTTIME_ARGS.iter().map(|a| (*a).to_owned()).collect();
                BootFacts {
                    boot_id,
                    boot_unix: sysctl
                        .run(&args)
                        .ok()
                        .filter(|output| output.code == Some(0))
                        .map_or(0, |output| {
                            parse_kern_boottime(&String::from_utf8_lossy(&output.stdout))
                        }),
                    machine_id: String::new(),
                }
            }
        }
    }
}

/// The daemon's hardware health: its state file in memory, written after every change.
pub struct HealthKeeper {
    state_dir: PathBuf,
    probe: Box<dyn ShutdownProbe>,
    log: Arc<dyn Log>,
    inner: Mutex<Inner>,
}

struct Inner {
    file: HealthFile,
    /// Whether the boot id was read at the start; the file keeps the last one read when not.
    boot_known: bool,
    /// The last state of each source, by its token.
    states: BTreeMap<String, pb::SourceState>,
}

fn lock(mutex: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl HealthKeeper {
    /// The keeper of the state file of `state_dir`, at the start of the daemon in `boot`: the
    /// file read, the shutdown of the boot before judged, and the file written again with the
    /// marker `running` (the module's doc). `probe` is asked at SIGTERM; lines go to `log`.
    #[must_use]
    pub fn open(
        state_dir: &Path,
        boot: BootFacts,
        probe: Box<dyn ShutdownProbe>,
        log: Arc<dyn Log>,
    ) -> HealthKeeper {
        let found = match health_state::read(state_dir, &boot.machine_id) {
            Read::Absent => None,
            Read::Read(file) => Some(file),
            Read::Aside { why, moved } => {
                log.line(&match moved {
                    Ok(()) => format!(
                        "error: the health state file was moved aside to {HEALTH_FILE_ASIDE}: {why}"
                    ),
                    Err(e) => format!(
                        "error: the health state file is not read: {why}; it could not be moved \
                         aside: {e}"
                    ),
                });
                None
            }
        };
        let mut file = found.unwrap_or_default();
        file.previous_clean = match &boot.boot_id {
            None => false,
            Some(id) if *id == file.boot_id => file.previous_clean,
            Some(_) => file.marker == Marker::Clean && !file.boot_id.is_empty(),
        };
        if let Some(id) = &boot.boot_id {
            file.boot_id.clone_from(id);
        }
        if !boot.machine_id.is_empty() {
            file.machine_id = boot.machine_id;
        }
        file.boot_unix = boot.boot_unix;
        file.marker = Marker::Running;
        let keeper = HealthKeeper {
            state_dir: state_dir.to_path_buf(),
            probe,
            log,
            inner: Mutex::new(Inner {
                file,
                boot_known: boot.boot_id.is_some(),
                states: BTreeMap::new(),
            }),
        };
        keeper.save(&lock(&keeper.inner).file);
        keeper
    }

    /// At SIGTERM: asks the probe whether the system is going down and, when it is, writes the
    /// clean-shutdown marker; what the probe found, which is logged.
    pub fn stopping(&self) -> GoingDown {
        let found = self.probe.going_down();
        match &found {
            GoingDown::Yes(why) => {
                let mut inner = lock(&self.inner);
                inner.file.marker = Marker::Clean;
                self.save(&inner.file);
                self.log
                    .line(&format!("the clean-shutdown marker is written: {why}"));
            }
            GoingDown::No(what) => self
                .log
                .line(&format!("the clean-shutdown marker is not written: {what}")),
            GoingDown::Unknown(why) => self.log.line(&format!(
                "error: the clean-shutdown marker is not written: {why}"
            )),
        }
        found
    }

    /// Holds the records `events` that the source `source` read, `now_unix` being the second it
    /// read them, and keeps `mark` as its high-water mark, the state file written before it
    /// returns; how many records it holds now that it did not before. On the source's first
    /// read on this host, a record older than [`HISTORY_SECS`] (its time known) is history. A
    /// record refused ([`crate::health_state::Refused`]) is logged.
    pub fn record(
        &self,
        source: &str,
        mark: &str,
        events: Vec<pb::HardwareEvent>,
        now_unix: u64,
    ) -> usize {
        let mut inner = lock(&self.inner);
        let first = !inner.file.sources.contains_key(source);
        inner
            .file
            .sources
            .entry(source.to_owned())
            .or_insert_with(|| SourceMark {
                first_read_unix: now_unix,
                mark: String::new(),
            })
            .mark = mark.to_owned();
        let mut held = 0;
        for mut event in events {
            if first && is_history(&event, now_unix) {
                event.severity = pb::HardwareSeverity::Info.into();
                event.message = cut(&format!("{HISTORY_PREFIX}{}", event.message), MAX_MESSAGE);
            }
            match inner.file.held.hold(event) {
                Ok(true) => held += 1,
                Ok(false) => {}
                Err(e) => self
                    .log
                    .line(&format!("error: a record of {source} is not held: {e}")),
            }
        }
        self.save(&inner.file);
        held
    }

    /// Takes what one read of `source` at `now_unix` came to: its state, kept for the
    /// heartbeats, with its reason cut to its bound; and, when it carries a mark or records, its
    /// records and its mark (none keeps the mark it had), as [`HealthKeeper::record`] holds them.
    /// How many records it holds now that it did not before. A read with neither a mark nor a
    /// record leaves the state file as it was, so a source the host lacks has no first read yet.
    pub fn read(&self, source: &str, read: SourceRead, now_unix: u64) -> usize {
        let name = cut(source, MAX_SOURCE);
        lock(&self.inner).states.insert(
            name.clone(),
            pb::SourceState {
                source: name,
                status: read.status.into(),
                reason: cut(&read.reason, MAX_SOURCE_REASON),
            },
        );
        if read.mark.is_none() && read.events.is_empty() {
            return 0;
        }
        let mark = read.mark.or_else(|| self.mark(source)).unwrap_or_default();
        self.record(source, &mark, read.events, now_unix)
    }

    /// The mark `source` last kept; none before its first read on this host.
    #[must_use]
    pub fn mark(&self, source: &str) -> Option<String> {
        lock(&self.inner)
            .file
            .sources
            .get(source)
            .map(|mark| mark.mark.clone())
    }

    fn save(&self, file: &HealthFile) {
        if let Err(e) = health_state::write(&self.state_dir, file, &|_| false) {
            self.log
                .line(&format!("error: the health state file is not written: {e}"));
        }
    }
}

/// Whether `event` of a source's first read, read at `now_unix`, is history: its time is known
/// and more than [`HISTORY_SECS`] before.
fn is_history(event: &pb::HardwareEvent, now_unix: u64) -> bool {
    event.time_unix != 0 && event.time_unix.saturating_add(HISTORY_SECS) < now_unix
}

impl HostHealthSource for HealthKeeper {
    fn health(&self) -> pb::HostHealth {
        let inner = lock(&self.inner);
        let mut health = pb::HostHealth {
            events: inner.file.held.to_send(),
            baseline: None,
            boot_id: if inner.boot_known {
                inner.file.boot_id.clone()
            } else {
                String::new()
            },
            boot_unix: inner.file.boot_unix,
            clean_previous_shutdown: inner.file.previous_clean,
            sources: inner.states.values().take(MAX_SOURCES).cloned().collect(),
        };
        for keep in (0..=health.events.len()).rev() {
            health.events.truncate(keep);
            if health.encoded_len() <= MAX_HEALTH_BYTES {
                break;
            }
        }
        health
    }

    fn written(&self, event_ids: &[String]) {
        let mut inner = lock(&self.inner);
        if inner.file.held.written(event_ids) > 0 {
            self.save(&inner.file);
        }
    }
}

/// The keeper of a host of `kind` whose state directory is `state_dir`, its boot read from the
/// host's own files and programs ([`BootFacts::read`]) and its probe the platform's
/// ([`shutdown_probe::for_platform`]).
#[must_use]
pub fn at_host(
    kind: PlatformKind,
    state_dir: &Path,
    clock: &dyn Clock,
    log: Arc<dyn Log>,
) -> Arc<HealthKeeper> {
    let sysctl = LaunchctlProcess::new(Path::new(SYSCTL), TOOL_DEADLINE);
    let boot = BootFacts::read(kind, Path::new("/"), clock, &sysctl);
    let probe = shutdown_probe::for_platform(kind, Path::new(OPENRC_RUN_DIR));
    Arc::new(HealthKeeper::open(state_dir, boot, probe, log))
}
