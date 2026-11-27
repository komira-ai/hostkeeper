//! The Linux daemon's reads of the kernel's evidence of hardware faults
//! (docs/design/host-health-alerts.md sections 3.1, 4.2 and 4.5): the kernel's log and its own
//! files, two [`HardwareSource`]s ([`KernelLog`], [`KernelFiles`]) that
//! [`crate::hardware_sources`] reads every [`POLL_SECS`], each on a worker of its own. Each read
//! is judged by the pure functions of [`crate::kernel_log`] and [`crate::kernel_files`] and
//! answers its records, its new mark and its state ([`SourceRead`]), which the keeper holds until
//! the coordinator writes them. Both know the boot the daemon read at its start ([`KernelBoot`]).
//!
//! **The kernel's log** (source `journal`). This boot's lines are followed on `/dev/kmsg`
//! ([`Kmsg`], [`DevKmsg`]): opened once, without blocking, each read taking the records logged
//! since the last, at most [`KMSG_MAX_RECORDS`] a read. The mark keeps the boot and the sequence
//! number of the last record read, so a restarted daemon, which reads the kernel's buffer from its
//! start again, passes over what it read before. Once in each boot the boot before is read from
//! the persistent journal ([`Journal`], [`Journalctl`]: `journalctl --dmesg --boot=-1`, its newest
//! [`PREVIOUS_BOOT_LINES`] lines): the lines logged after the last one the daemon read on
//! `/dev/kmsg` in that boot, which are the lines of its last minute, before a crash or a power
//! loss. A host with no `/var/log/journal` has no persistent journal: the source is
//! `unsupported`, and `/dev/kmsg` is still followed. A read that fails makes the source
//! `unreadable` with its reason; the journal is then read again at the next poll.
//!
//! **The kernel's files** (source `sysfs`), read under the host's root: `/proc/sys/kernel/tainted`,
//! the `HardwareCorrupted` of `/proc/meminfo`, the `errors_count` of each filesystem of
//! `/sys/fs/ext4`, the mount table `/proc/self/mounts`, and the dumps of `/sys/fs/pstore` and of
//! `/var/lib/systemd/pstore`, where `systemd-pstore` archives them. The mark keeps the boot, the
//! taint flags and the `HardwareCorrupted` of that boot already recorded, each filesystem's error
//! count, the read-only remounts asserted and the mounts seen writable, and the newest dumps
//! read. A file that cannot be read makes the source `unreadable`; a directory that is not there
//! (no ext4 filesystem, no pstore) is nothing to read. Nothing is ever written, removed or
//! cleared: a pstore dump stays where it is.
//!
//! Only `journalctl` is a program, bounded by [`JOURNAL_DEADLINE`] and its output cap; the other reads
//! are of the kernel's own files (`/dev/kmsg` without blocking). A pstore dump is listed at every
//! read and read only when it is new.
//!
//! A mark that does not parse is read as none: the source's next read is a first read.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use hostkeeper_launchd::{Launchctl, LaunchctlProcess};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::hardware_sources::{HardwareSource, SourceRead};
use crate::host_health::HealthKeeper;
use crate::kernel_files::{
    self, Dump, Ext4Errors, RemountMark, corrupted_event, dump_id, ext4_device_read, ext4_event,
    hardware_corrupted_kib, parse_mounts, parse_tainted, pstore_event, remount_events,
    remounts_ended, taint_bits, taint_events,
};
use crate::kernel_log::{self, KernelLine, KmsgRecord, boot_key, classify, same_boot};

/// The seconds between two reads.
pub const POLL_SECS: u64 = 60;

/// The kernel's log device.
pub const KMSG: &str = "/dev/kmsg";

/// The kernel's log device, under the host's root.
pub const KMSG_UNDER_ROOT: &str = "dev/kmsg";

/// The most records one read of `/dev/kmsg` takes; the rest wait for the next.
pub const KMSG_MAX_RECORDS: usize = 4096;

/// The program the journal of the boot before is read with.
pub const JOURNALCTL: &str = "/usr/bin/journalctl";

/// How long `journalctl` may take.
pub const JOURNAL_DEADLINE: Duration = Duration::from_secs(30);

/// The newest kernel lines of the boot before that are read.
pub const PREVIOUS_BOOT_LINES: usize = 100;

/// The persistent journal's directory, under the host's root: without it, journald keeps no
/// boot but this one.
pub const PERSISTENT_JOURNAL: &str = "var/log/journal";

/// The taint flags, under the host's root.
pub const TAINTED: &str = "proc/sys/kernel/tainted";

/// The memory counters, under the host's root.
pub const MEMINFO: &str = "proc/meminfo";

/// The mount table, under the host's root.
pub const MOUNTS: &str = "proc/self/mounts";

/// The ext4 filesystems, under the host's root.
pub const EXT4_DIR: &str = "sys/fs/ext4";

/// pstore's own filesystem, under the host's root.
pub const PSTORE_DIR: &str = "sys/fs/pstore";

/// Where `systemd-pstore` archives the dumps it moves out of pstore, one directory each, under
/// the host's root.
pub const SYSTEMD_PSTORE_DIR: &str = "var/lib/systemd/pstore";

/// The most bytes read of one file of a dump: its end, where the panic is.
pub const PSTORE_FILE_MAX: u64 = 256 * 1024;

/// `/dev/kmsg`: the records the kernel logged.
pub trait Kmsg: Send {
    /// The records logged since the last call (at the first, every record the kernel still
    /// holds), oldest first; it stops reading once it holds `max` or more.
    ///
    /// # Errors
    /// The device could not be opened or read.
    fn read(&mut self, max: usize) -> io::Result<Vec<KmsgRecord>>;
}

/// [`Kmsg`] over a file: `/dev/kmsg` on the host, opened without blocking at the first read and
/// kept open, so each read goes on from the last.
pub struct DevKmsg {
    path: PathBuf,
    file: Option<File>,
    partial: Vec<u8>,
}

impl DevKmsg {
    /// The device at `path`, not yet opened.
    #[must_use]
    pub fn at(path: &Path) -> DevKmsg {
        DevKmsg {
            path: path.to_owned(),
            file: None,
            partial: Vec::new(),
        }
    }
}

impl Kmsg for DevKmsg {
    fn read(&mut self, max: usize) -> io::Result<Vec<KmsgRecord>> {
        let file = match self.file.take() {
            Some(file) => file,
            None => OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&self.path)?,
        };
        let file = self.file.insert(file);
        read_records(file, &mut self.partial, max)
    }
}

/// The records `reader` gives, with `partial`, the bytes of a record an earlier read cut, before
/// them: read until it holds `max` records or more, it has nothing more (`WouldBlock`, or the end
/// of a file), or `max` reads. `/dev/kmsg` gives one record a read and `BrokenPipe` when the
/// kernel overwrote the record it was at, after which it goes on from the oldest it holds: that
/// read is passed over, as is an `Interrupted` one. A record cut at the end of what was read is
/// kept in `partial` for the next call.
///
/// # Errors
/// Any other error of a read.
pub fn read_records(
    reader: &mut dyn Read,
    partial: &mut Vec<u8>,
    max: usize,
) -> io::Result<Vec<KmsgRecord>> {
    let mut records = Vec::new();
    let mut buffer = vec![0u8; 8192];
    for _ in 0..max {
        if records.len() >= max {
            break;
        }
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                partial.extend_from_slice(&buffer[..n]);
                let whole = partial
                    .iter()
                    .rposition(|b| *b == b'\n')
                    .map_or(0, |end| end + 1);
                let whole: Vec<u8> = partial.drain(..whole).collect();
                records.extend(kernel_log::parse_kmsg(&whole));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e)
                if e.kind() == io::ErrorKind::BrokenPipe
                    || e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(records)
}

/// What the journal holds of the boot before this one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviousBoot {
    /// The host keeps no persistent journal.
    NoJournal,
    /// The kernel's newest lines of the boot before, oldest first; none when the journal holds no
    /// boot before this one.
    Lines(Vec<KernelLine>),
}

/// Why the journal of the boot before could not be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum JournalError {
    /// The program could not be run, or did not end by its deadline: the port's own words.
    #[error("{0}")]
    Run(String),
    /// The program ended with a failure: how (`status 2`, `a signal`) and the first line of its
    /// standard error.
    #[error("journalctl ended with {how}: {first_line}")]
    Ended { how: String, first_line: String },
}

/// The persistent journal.
pub trait Journal: Send + Sync {
    /// The kernel's lines of the boot before this one.
    ///
    /// # Errors
    /// [`JournalError`]: why they could not be read.
    fn previous_boot(&self) -> Result<PreviousBoot, JournalError>;
}

/// [`Journal`] through `journalctl` ([`journal_args`]), a program run with a fixed argument list,
/// no shell and an empty environment, its output capped and killed at its deadline; the
/// persistent journal is looked for under `root`.
pub struct Journalctl {
    root: PathBuf,
    program: Box<dyn Launchctl>,
}

impl Journalctl {
    /// `program` in the place of `journalctl`, the journal looked for under `root`.
    #[must_use]
    pub fn new(root: &Path, program: Box<dyn Launchctl>) -> Journalctl {
        Journalctl {
            root: root.to_owned(),
            program,
        }
    }
}

/// The arguments `journalctl` is run with: the kernel's lines of the boot before, newest first,
/// as JSON, with the fields the records need.
#[must_use]
pub fn journal_args() -> Vec<String> {
    vec![
        "--dmesg".to_owned(),
        "--boot=-1".to_owned(),
        "--output=json".to_owned(),
        "--output-fields=MESSAGE,_SOURCE_MONOTONIC_TIMESTAMP".to_owned(),
        "--reverse".to_owned(),
        format!("--lines={PREVIOUS_BOOT_LINES}"),
        "--no-pager".to_owned(),
        "--quiet".to_owned(),
    ]
}

impl Journal for Journalctl {
    fn previous_boot(&self) -> Result<PreviousBoot, JournalError> {
        if !self.root.join(PERSISTENT_JOURNAL).is_dir() {
            return Ok(PreviousBoot::NoJournal);
        }
        let output = self
            .program
            .run(&journal_args())
            .map_err(|e| JournalError::Run(e.to_string()))?;
        let stderr = output.stderr_text();
        match output.code {
            Some(0) => Ok(PreviousBoot::Lines(kernel_log::parse_journal(
                &output.stdout,
            ))),
            Some(1) if stderr.contains("boot") => Ok(PreviousBoot::Lines(Vec::new())),
            code => Err(JournalError::Ended {
                how: code.map_or_else(|| "a signal".to_owned(), |c| format!("status {c}")),
                first_line: stderr.lines().next().unwrap_or_default().to_owned(),
            }),
        }
    }
}

/// What the kernel's log keeps between reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogMark {
    /// The boot `/dev/kmsg` was last read in.
    pub boot: String,
    /// The sequence number the next record of that boot is read from.
    pub next_seq: u64,
    /// The stamp of the last record read in that boot.
    pub usec: u64,
    /// The boot before, and the stamp of the last record read in it.
    pub previous: String,
    pub previous_usec: u64,
    /// The boot in which the journal of the boot before was read.
    pub journal_read_in: String,
}

/// What the kernel's files keep between reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesMark {
    /// The boot they were last read in.
    pub boot: String,
    /// The taint flags of that boot already recorded.
    pub taint: u64,
    /// The `HardwareCorrupted` of that boot already recorded, kB.
    pub corrupted_kib: u64,
    /// Each ext4 filesystem's error count at the last read.
    pub ext4: BTreeMap<String, u64>,
    /// The read-only remounts.
    pub remounts: RemountMark,
    /// The time of the newest pstore dumps read, and their ids.
    pub pstore_mtime: u64,
    pub pstore_ids: Vec<String>,
}

/// The boot the kernel's sources read in, as the daemon read it at its start
/// ([`HealthKeeper::boot_id`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KernelBoot {
    /// The boot id; for a boot whose id could not be read, when it began as 16 hex digits, so
    /// that its reads still follow one another.
    pub id: String,
    /// When the boot began, Unix seconds; 0 when not read.
    pub unix: u64,
}

impl KernelBoot {
    /// The boot `keeper` read at the daemon's start.
    #[must_use]
    pub fn of(keeper: &HealthKeeper) -> KernelBoot {
        let unix = keeper.boot_unix();
        KernelBoot {
            id: keeper.boot_id().unwrap_or_else(|| format!("{unix:016x}")),
            unix,
        }
    }
}

/// The kernel's log, the source `journal`: `/dev/kmsg` and the journal of the boot before.
pub struct KernelLog {
    kmsg: Box<dyn Kmsg>,
    journal: Box<dyn Journal>,
    boot: KernelBoot,
}

impl KernelLog {
    /// The log of the boot `boot`, read through `kmsg` and `journal`.
    #[must_use]
    pub fn new(kmsg: Box<dyn Kmsg>, journal: Box<dyn Journal>, boot: KernelBoot) -> KernelLog {
        KernelLog {
            kmsg,
            journal,
            boot,
        }
    }
}

impl HardwareSource for KernelLog {
    fn name(&self) -> &'static str {
        kernel_log::SOURCE
    }

    fn every_secs(&self) -> u64 {
        POLL_SECS
    }

    fn read(&mut self, mark: Option<&str>, _now_unix: u64) -> SourceRead {
        let boot = self.boot.clone();
        self.read_log(mark, &boot.id, boot.unix)
    }
}

/// The kernel's files, the source `sysfs`, under the host's root.
pub struct KernelFiles {
    root: PathBuf,
    boot: KernelBoot,
}

impl KernelFiles {
    /// The files under `root` in the boot `boot`.
    #[must_use]
    pub fn new(root: &Path, boot: KernelBoot) -> KernelFiles {
        KernelFiles {
            root: root.to_owned(),
            boot,
        }
    }
}

impl HardwareSource for KernelFiles {
    fn name(&self) -> &'static str {
        kernel_files::SOURCE
    }

    fn every_secs(&self) -> u64 {
        POLL_SECS
    }

    fn read(&mut self, mark: Option<&str>, now_unix: u64) -> SourceRead {
        self.read_files(mark, &self.boot.id, self.boot.unix, now_unix)
    }
}

impl KernelLog {
    fn read_log(&mut self, mark: Option<&str>, boot: &str, boot_unix: u64) -> SourceRead {
        let mut mark: LogMark = mark_of(mark);
        if !same_boot(&mark.boot, boot) {
            if !mark.boot.is_empty() {
                mark.previous = std::mem::take(&mut mark.boot);
                mark.previous_usec = mark.usec;
            }
            boot.clone_into(&mut mark.boot);
            mark.next_seq = 0;
            mark.usec = 0;
        }
        let mut events = Vec::new();
        let mut unreadable = None;
        let mut unsupported = None;
        if !same_boot(&mark.journal_read_in, boot) {
            match self.journal.previous_boot() {
                Ok(PreviousBoot::NoJournal) => {
                    unsupported = Some(format!(
                        "no persistent journal: /{PERSISTENT_JOURNAL} is absent"
                    ));
                }
                Ok(PreviousBoot::Lines(lines)) => {
                    let after: Vec<KernelLine> = lines
                        .into_iter()
                        .filter(|line| !same_boot(&line.boot_id, boot))
                        .filter(|line| {
                            !same_boot(&line.boot_id, &mark.previous)
                                || line.usec > mark.previous_usec
                        })
                        .collect();
                    events.extend(classify(&after, true));
                    boot.clone_into(&mut mark.journal_read_in);
                }
                Err(why) => unreadable = Some(why.to_string()),
            }
        }
        match self.kmsg.read(KMSG_MAX_RECORDS) {
            Ok(records) => {
                let fresh: Vec<KmsgRecord> = records
                    .into_iter()
                    .filter(|record| record.seq >= mark.next_seq)
                    .collect();
                if let Some(last) = fresh.last() {
                    mark.next_seq = last.seq.saturating_add(1);
                    mark.usec = last.usec;
                }
                let lines: Vec<KernelLine> = fresh
                    .into_iter()
                    .map(|record| KernelLine {
                        boot_id: boot.to_owned(),
                        usec: record.usec,
                        time_unix: if boot_unix == 0 {
                            0
                        } else {
                            boot_unix + record.usec / 1_000_000
                        },
                        text: record.text,
                    })
                    .collect();
                events.extend(classify(&lines, false));
            }
            Err(e) => unreadable = Some(format!("{KMSG} cannot be read: {e}")),
        }
        source_read(mark_text(&mark), events, unreadable, unsupported)
    }
}

impl KernelFiles {
    fn read_files(
        &self,
        mark: Option<&str>,
        boot: &str,
        boot_unix: u64,
        now_unix: u64,
    ) -> SourceRead {
        let mut mark: FilesMark = mark_of(mark);
        let key = boot_key(boot);
        let mut events = Vec::new();
        let mut problems: Vec<String> = Vec::new();
        if !same_boot(&mark.boot, boot) {
            events.extend(remounts_ended(&mut mark.remounts, now_unix));
            boot.clone_into(&mut mark.boot);
            mark.taint = 0;
            mark.corrupted_kib = 0;
        }
        match self.text(TAINTED).map(|text| parse_tainted(&text)) {
            Ok(Some(tainted)) => {
                events.extend(taint_events(tainted, mark.taint, &key, now_unix));
                mark.taint |= taint_bits(tainted);
            }
            Ok(None) => problems.push(format!("/{TAINTED} is not a number")),
            Err(why) => problems.push(why),
        }
        match self.text(MEMINFO) {
            Ok(text) => {
                if let Some(kib) = hardware_corrupted_kib(&text).filter(|k| *k > mark.corrupted_kib)
                {
                    events.push(corrupted_event(kib, &key, now_unix));
                    mark.corrupted_kib = kib;
                }
            }
            Err(why) => problems.push(why),
        }
        match ext4_errors(&self.root.join(EXT4_DIR)) {
            Ok(filesystems) => {
                for errors in filesystems {
                    let before = mark.ext4.get(&errors.device).copied().unwrap_or(0);
                    events.extend(ext4_event(&errors, before, now_unix));
                    mark.ext4.insert(errors.device, errors.count);
                }
            }
            Err(e) => problems.push(format!("/{EXT4_DIR} cannot be read: {e}")),
        }
        match self.text(MOUNTS) {
            Ok(text) => events.extend(remount_events(
                &parse_mounts(&text),
                &mut mark.remounts,
                &key,
                now_unix,
            )),
            Err(why) => problems.push(why),
        }
        match dumps(&self.root) {
            Ok(found) => events.extend(new_dumps(found, &mut mark, boot_unix)),
            Err(e) => problems.push(format!("pstore cannot be read: {e}")),
        }
        source_read(mark_text(&mark), events, problems.into_iter().next(), None)
    }

    /// The text of `path` under the root; why not, in plain words.
    fn text(&self, path: &str) -> Result<String, String> {
        fs::read_to_string(self.root.join(path)).map_err(|e| format!("/{path} cannot be read: {e}"))
    }
}

/// The mark `mark`, read; the default when there is none or it does not parse.
fn mark_of<T: Default + DeserializeOwned>(mark: Option<&str>) -> T {
    mark.and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_default()
}

fn mark_text<T: Serialize>(mark: &T) -> String {
    serde_json::to_string(mark).unwrap_or_default()
}

/// What a read came to: its new `mark` and its `events`, and unreadable with the reason when
/// there is one, else unsupported with its reason, else read. The mark and the records stand
/// either way: a source that reads in part (the journal failed, `/dev/kmsg` read) keeps what it
/// read.
fn source_read(
    mark: String,
    events: Vec<pb::HardwareEvent>,
    unreadable: Option<String>,
    unsupported: Option<String>,
) -> SourceRead {
    let (status, reason) = match (unreadable, unsupported) {
        (Some(why), _) => (pb::SourceStatus::Unreadable, why),
        (None, Some(why)) => (pb::SourceStatus::Unsupported, why),
        (None, None) => (pb::SourceStatus::Ok, String::new()),
    };
    SourceRead {
        status,
        reason,
        mark: Some(mark),
        events,
    }
}

/// The error counters of the ext4 filesystems of `dir` that are read
/// ([`kernel_files::ext4_device_read`]), sorted by name; none when `dir` is not there. A
/// filesystem whose `errors_count` cannot be read or parsed is passed over.
///
/// # Errors
/// `dir` could not be listed.
pub fn ext4_errors(dir: &Path) -> io::Result<Vec<Ext4Errors>> {
    let number = |path: PathBuf| {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| text.trim().parse::<u64>().ok())
    };
    Ok(listed(dir)?
        .into_iter()
        .filter_map(|path| {
            let device = path.file_name()?.to_str()?.to_owned();
            Some((device, path))
        })
        .filter(|(device, _)| ext4_device_read(device))
        .filter_map(|(device, path)| {
            Some(Ext4Errors {
                count: number(path.join("errors_count"))?,
                last_error_unix: number(path.join("last_error_time")).unwrap_or(0),
                device,
            })
        })
        .collect())
}

/// One pstore dump as listed, not yet read: its name, when it was written and its files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// The first of its files, or the directory `systemd-pstore` archived it in.
    pub name: String,
    /// When it was written, Unix seconds; 0 when not known.
    pub mtime: u64,
    /// Its files, sorted.
    pub paths: Vec<PathBuf>,
}

impl Found {
    /// The id of its record, as [`kernel_files::dump_id`] gives it once read.
    #[must_use]
    pub fn id(&self) -> String {
        dump_id(&Dump {
            name: self.name.clone(),
            mtime: self.mtime,
            text: String::new(),
        })
    }

    /// The dump, its files read: the last [`PSTORE_FILE_MAX`] bytes of each, joined by a newline;
    /// a file that cannot be read is left out.
    #[must_use]
    pub fn read(&self) -> Dump {
        let text: Vec<String> = self.paths.iter().filter_map(|p| tail(p).ok()).collect();
        Dump {
            name: self.name.clone(),
            mtime: self.mtime,
            text: text.join("\n"),
        }
    }
}

/// The pstore dumps under `root`, listed and not read: in [`PSTORE_DIR`], the `dmesg-*` files
/// written in one second are one dump; in [`SYSTEMD_PSTORE_DIR`], each directory is one, of its
/// `dmesg.txt` or else its `dmesg-*` files. A directory that is not there holds none.
///
/// # Errors
/// A directory that is there could not be listed.
pub fn dumps(root: &Path) -> io::Result<Vec<Found>> {
    let mut dumps: Vec<Found> = Vec::new();
    let mut by_second: BTreeMap<u64, Vec<PathBuf>> = BTreeMap::new();
    for path in dmesg_files(&root.join(PSTORE_DIR))? {
        by_second.entry(mtime(&path)).or_default().push(path);
    }
    for (second, paths) in by_second {
        dumps.push(found(paths, second, None));
    }
    for dir in listed(&root.join(SYSTEMD_PSTORE_DIR))? {
        if !dir.is_dir() {
            continue;
        }
        let assembled = dir.join("dmesg.txt");
        let paths = if assembled.is_file() {
            vec![assembled]
        } else {
            dmesg_files(&dir)?
        };
        if let Some(first) = paths.first() {
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned());
            let second = mtime(first);
            dumps.push(found(paths, second, name));
        }
    }
    Ok(dumps)
}

/// The entries of `dir`, sorted; none when it is not there.
fn listed(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    Ok(paths)
}

/// The `dmesg-*` files of `dir`, sorted; none when it is not there.
fn dmesg_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    Ok(listed(dir)?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("dmesg-"))
        })
        .filter(|path| path.is_file())
        .collect())
}

/// The modification time of `path`, Unix seconds; 0 when it cannot be read.
fn mtime(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

/// The dump of `paths`, written at `second`, named `name` or else by its first file.
fn found(paths: Vec<PathBuf>, second: u64, name: Option<String>) -> Found {
    Found {
        name: name
            .or_else(|| {
                paths
                    .first()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_default(),
        mtime: second,
        paths,
    }
}

/// The last [`PSTORE_FILE_MAX`] bytes of `path`, as text.
fn tail(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len > PSTORE_FILE_MAX {
        file.seek(SeekFrom::Start(len - PSTORE_FILE_MAX))?;
    }
    let mut bytes = Vec::new();
    file.take(PSTORE_FILE_MAX).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The records of the dumps of `dumps` not read before by `mark`, which it brings up to date:
/// those written after the newest dumps it read, and those written in the same second that it
/// did not read. Only those are read.
fn new_dumps(dumps: Vec<Found>, mark: &mut FilesMark, boot_unix: u64) -> Vec<pb::HardwareEvent> {
    let newest = dumps.iter().map(|dump| dump.mtime).max().unwrap_or(0);
    let fresh: Vec<Found> = dumps
        .into_iter()
        .filter(|dump| {
            dump.mtime > mark.pstore_mtime
                || (dump.mtime == mark.pstore_mtime && !mark.pstore_ids.contains(&dump.id()))
        })
        .collect();
    if newest > mark.pstore_mtime {
        mark.pstore_mtime = newest;
        mark.pstore_ids.clear();
    }
    mark.pstore_ids.extend(
        fresh
            .iter()
            .filter(|dump| dump.mtime == mark.pstore_mtime)
            .map(Found::id),
    );
    fresh
        .iter()
        .map(|dump| pstore_event(&dump.read(), boot_unix))
        .collect()
}

/// The kernel's two sources of a host whose root is `root`, in the boot `boot`: its log
/// (`<root>/dev/kmsg` and [`JOURNALCTL`]) and its files.
#[must_use]
pub fn at_root(root: &Path, boot: &KernelBoot) -> Vec<Box<dyn HardwareSource>> {
    vec![
        Box::new(KernelLog::new(
            Box::new(DevKmsg::at(&root.join(KMSG_UNDER_ROOT))),
            Box::new(Journalctl::new(
                root,
                Box::new(LaunchctlProcess::new(
                    Path::new(JOURNALCTL),
                    JOURNAL_DEADLINE,
                )),
            )),
            boot.clone(),
        )),
        Box::new(KernelFiles::new(root, boot.clone())),
    ]
}

#[cfg(test)]
#[path = "kernel_sources_tests.rs"]
mod tests;
