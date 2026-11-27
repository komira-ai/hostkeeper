//! The tests of `kernel_sources`: the reads of `/dev/kmsg` (`read_records`, `DevKmsg`), the
//! journal of the boot before through `Journalctl` over a scripted `journalctl`, and `poll` of
//! both sources (the kernel's log and its files) over a scratch host root and a scratch state
//! directory, with a real `HealthKeeper`, its held records read back from the state file; the
//! marks, the sources' states, `run` and `start`.

use std::collections::VecDeque;
use std::io::Write as _;

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_link::HostHealthSource;
use hostkeeper_link::testing::{FakeClock, ManualDispatch, RecordingLog, ScriptedSleeper};
use hostkeeper_testkit::Scratch;

use super::*;
use crate::health_state::{self, Held};
use crate::host_health::BootFacts;
use crate::shutdown_probe::{GoingDown, ShutdownProbe};

const BOOT_A: &str = "6d3a1c0e-9b2f-4e57-a1c4-2f8e7d6b5a49";
const KEY_A: &str = "6d3a1c0e9b2f4e57";
const BOOT_B: &str = "b2c4d6e8-1a3b-4c5d-8e7f-0a1b2c3d4e5f";
const KEY_B: &str = "b2c4d6e81a3b4c5d";
const BOOT_UNIX: u64 = 1_795_600_000;
const NOW: u64 = 1_795_662_000;
/// Before the boot began, and after.
const T_PRE: u64 = 1_795_590_000;
const T_POST: u64 = 1_795_610_000;

const JOURNAL: &str = include_str!("../tests/fixtures/kernel/journal-previous-boot.json");
const PANIC: &str = include_str!("../tests/fixtures/kernel/pstore-panic-mce.txt");
const OOPS: &str = include_str!("../tests/fixtures/kernel/pstore-oops.txt");

/// The sequence numbers of the `/dev/kmsg` records of the tests ([`r`]).
const QUIET: u64 = 10;
const UE: u64 = 11;
const IO: u64 = 12;
const AER: u64 = 13;

const WOULD_BLOCK: Result<&str, io::ErrorKind> = Err(io::ErrorKind::WouldBlock);

struct Quiet;

impl ShutdownProbe for Quiet {
    fn going_down(&self) -> GoingDown {
        GoingDown::No("a test".to_owned())
    }
}

/// A `/dev/kmsg` that hands out the answers a test scripted, one a read, then nothing.
struct ScriptedKmsg(VecDeque<io::Result<Vec<KmsgRecord>>>);

impl Kmsg for ScriptedKmsg {
    fn read(&mut self, _max: usize) -> io::Result<Vec<KmsgRecord>> {
        self.0.pop_front().unwrap_or_else(|| Ok(Vec::new()))
    }
}

/// A reader that gives the chunks a test scripted, one a read, then the end.
struct Chunks(VecDeque<io::Result<Vec<u8>>>);

impl Read for Chunks {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.pop_front() {
            None => Ok(0),
            Some(Err(e)) => Err(e),
            Some(Ok(bytes)) => {
                buf[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
        }
    }
}

fn chunks(items: Vec<Result<&str, io::ErrorKind>>) -> Chunks {
    let one = |item: Result<&str, io::ErrorKind>| match item {
        Ok(text) => Ok(text.as_bytes().to_vec()),
        Err(kind) => Err(io::Error::from(kind)),
    };
    Chunks(items.into_iter().map(one).collect())
}

/// The records of `reader` read with `max`, no bytes kept before, and how many chunks are left.
fn drained(reader: &mut Chunks, max: usize) -> (Vec<KmsgRecord>, usize) {
    let records = read_records(reader, &mut Vec::new(), max).expect("the records");
    (records, reader.0.len())
}

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn put(root: &Scratch, rel: &str, text: &str) {
    let path = root.path().join(rel);
    fs::create_dir_all(path.parent().expect("a parent")).expect("the directories");
    fs::write(&path, text).expect("the file");
}

/// A file under the root, last written at `secs`.
fn put_at(root: &Scratch, rel: &str, text: &str, secs: u64) {
    put(root, rel, text);
    let file = OpenOptions::new()
        .write(true)
        .open(root.path().join(rel))
        .expect("the file");
    file.set_modified(UNIX_EPOCH + Duration::from_secs(secs))
        .expect("its time");
}

fn meminfo(kib: u64) -> String {
    format!("MemTotal:  16384 kB\nHardwareCorrupted: {kib:>8} kB\n")
}

fn mounts(options: &str) -> String {
    format!("/dev/sda1 / ext4 {options} 0 0\nproc /proc proc rw 0 0\n")
}

fn rec(seq: u64, usec: u64, text: &str) -> KmsgRecord {
    KmsgRecord {
        seq,
        usec,
        text: text.to_owned(),
    }
}

/// The `/dev/kmsg` record `seq` of the tests: [`QUIET`] says nothing, [`UE`] is an uncorrectable
/// memory error, [`IO`] an I/O error of `sdb`, [`AER`] a fatal PCIe error.
fn r(seq: u64) -> KmsgRecord {
    let text = match seq {
        QUIET => "systemd[1]: Started Session 4 of User root.",
        UE => "EDAC MC1: 1 UE memory read error on CPU_SrcID#1_MC#1_Chan#2_DIMM#0 (x)",
        IO => "I/O error, dev sdb, sector 123456 op 0x0:(READ) flags 0x0 phys_seg 1",
        _ => "pcieport 0000:00:01.0: AER: Uncorrected (Fatal) error received: 0000:af:00.0",
    };
    rec(seq, (seq - 9) * 1_000_000, text)
}

fn kmsg_line(boot: &str, seq: u64) -> KernelLine {
    let record = r(seq);
    let time_unix = BOOT_UNIX + record.usec / 1_000_000;
    KernelLine {
        boot_id: boot.to_owned(),
        usec: record.usec,
        time_unix,
        text: record.text,
    }
}

/// The id of the record of the `/dev/kmsg` record `seq` of the boot `boot`.
fn id(boot: &str, seq: u64) -> String {
    kernel_log::line_id(&kmsg_line(boot, seq))
}

fn ids(events: &[pb::HardwareEvent]) -> Vec<String> {
    events.iter().map(|e| e.event_id.clone()).collect()
}

/// What each record says: its code, its subject, and whether it is of the boot before.
fn brief(events: &[pb::HardwareEvent]) -> Vec<String> {
    let one = |e: &pb::HardwareEvent| format!("{} {} {}", e.code, e.subject, e.of_previous_boot);
    events.iter().map(one).collect()
}

fn ok() -> (pb::SourceStatus, String) {
    (pb::SourceStatus::Ok, String::new())
}

fn state_of(keeper: &HealthKeeper, source: &str) -> (pb::SourceStatus, String) {
    let health = keeper.health();
    let state = health
        .sources
        .iter()
        .find(|s| s.source == source)
        .expect("the source's state");
    (
        pb::SourceStatus::try_from(state.status).expect("a known status"),
        state.reason.clone(),
    )
}

fn log_mark(keeper: &HealthKeeper) -> LogMark {
    serde_json::from_str(&keeper.mark("journal").expect("a mark")).expect("a log mark")
}

fn files_mark(keeper: &HealthKeeper) -> FilesMark {
    serde_json::from_str(&keeper.mark("sysfs").expect("a mark")).expect("a files mark")
}

/// The answer of `journalctl` for a journal with no boot before this one.
fn no_boot() -> LaunchctlOutput {
    LaunchctlOutput::exit(
        1,
        "No journal boot entry found from the specified boot offset.",
    )
}

/// A scratch host: its root, its state directory, a scripted `journalctl`.
struct Host {
    state: Scratch,
    root: Scratch,
    launchctl: FakeLaunchctl,
    log: Arc<RecordingLog>,
}

impl Host {
    fn new(name: &str) -> Host {
        Host {
            state: scratch(&format!("{name}-s")),
            root: scratch(&format!("{name}-r")),
            launchctl: FakeLaunchctl::new(),
            log: Arc::new(RecordingLog::default()),
        }
    }

    /// A host whose kernel files are all quiet.
    fn quiet(name: &str) -> Host {
        let host = Host::new(name);
        put(&host.root, TAINTED, "0\n");
        put(&host.root, MEMINFO, &meminfo(0));
        put(&host.root, MOUNTS, &mounts("rw,relatime"));
        host
    }

    /// A quiet host with a persistent journal, which `journalctl` answers `answer` to.
    fn with_journal(name: &str, answer: LaunchctlOutput) -> Host {
        let host = Host::quiet(name);
        host.make_journal();
        host.launchctl.always(&journal_args(), Ok(answer));
        host
    }

    fn make_journal(&self) {
        fs::create_dir_all(self.root.path().join(PERSISTENT_JOURNAL)).expect("the journal");
    }

    fn keeper_in(&self, boot: &str, boot_unix: u64) -> HealthKeeper {
        let boot_id = Some(boot.to_owned());
        let facts = BootFacts {
            boot_id,
            boot_unix,
            machine_id: "m-1".to_owned(),
        };
        let log = Arc::new(RecordingLog::default());
        HealthKeeper::open(self.state.path(), facts, Box::new(Quiet), log)
    }

    fn keeper(&self, boot: &str) -> HealthKeeper {
        self.keeper_in(boot, BOOT_UNIX)
    }

    fn sources_over(&self, kmsg: Box<dyn Kmsg>) -> KernelSources {
        let journal = Journalctl::new(self.root.path(), Box::new(self.launchctl.clone()));
        KernelSources::new(self.root.path(), kmsg, Box::new(journal), self.log.clone())
    }

    /// Sources whose `/dev/kmsg` gives, a read each, the records `answers` name.
    fn sources(&self, answers: Vec<io::Result<Vec<u64>>>) -> KernelSources {
        let records = |seqs: Vec<u64>| -> Vec<KmsgRecord> { seqs.into_iter().map(r).collect() };
        let answers = answers.into_iter().map(|answer| answer.map(records));
        self.sources_over(Box::new(ScriptedKmsg(answers.collect())))
    }

    /// One poll of sources whose `/dev/kmsg` has nothing.
    fn poll(&self, keeper: &HealthKeeper, now: u64) {
        self.sources(Vec::new()).poll(keeper, now);
    }

    /// One poll of sources whose `/dev/kmsg` gives the records `seqs`.
    fn poll_seqs(&self, keeper: &HealthKeeper, now: u64, seqs: &[u64]) {
        self.sources(vec![Ok(seqs.to_vec())]).poll(keeper, now);
    }

    /// The records held of `source`, in the order they were held, read from the state file.
    fn held(&self, source: &str) -> Vec<pb::HardwareEvent> {
        let file = match health_state::read(self.state.path(), "") {
            health_state::Read::Read(file) => file,
            other => panic!("the state file is not read: {other:?}"),
        };
        file.held
            .records
            .iter()
            .filter(|h| h.source == source)
            .map(Held::event)
            .collect()
    }

    fn held_ids(&self, source: &str) -> Vec<String> {
        ids(&self.held(source))
    }
}

// ---- read_records

/// Proves a record cut across two reads is completed by the second, in one call.
#[test]
fn a_record_cut_across_two_reads_is_completed_by_the_second() {
    let mut reader = chunks(vec![Ok("6,1,10,-;one\n6,2,20,-;tw"), Ok("o\n")]);
    let mut partial = Vec::new();
    let records = read_records(&mut reader, &mut partial, 10).expect("the records");
    assert_eq!(records, [rec(1, 10, "one"), rec(2, 20, "two")]);
    assert!(partial.is_empty());
}

/// Proves the cut end of a call stays in `partial` and is completed by the next call.
#[test]
fn a_record_cut_at_the_end_of_a_call_is_kept_for_the_next_call() {
    let mut partial = Vec::new();
    let mut first = chunks(vec![Ok("6,1,10,-;one\n6,2,20,-;tw"), WOULD_BLOCK]);
    let records = read_records(&mut first, &mut partial, 10).expect("the first call");
    assert_eq!(records, [rec(1, 10, "one")]);
    assert_eq!(partial, b"6,2,20,-;tw");
    let mut second = chunks(vec![Ok("o\n")]);
    let records = read_records(&mut second, &mut partial, 10).expect("the second call");
    assert_eq!(records, [rec(2, 20, "two")]);
}

/// Proves `WouldBlock`, the end of a file and the max bound each end a call, the rest left over.
#[test]
fn would_block_the_end_of_a_file_and_the_max_bound_end_the_call() {
    let blocked = vec![Ok("6,1,1,-;a\n"), WOULD_BLOCK, Ok("6,2,2,-;b\n")];
    assert_eq!(drained(&mut chunks(blocked), 10), (vec![rec(1, 1, "a")], 1));
    let eof = vec![Ok("6,1,1,-;a\n"), Ok(""), Ok("6,2,2,-;b\n")];
    assert_eq!(drained(&mut chunks(eof), 10), (vec![rec(1, 1, "a")], 1));
    assert_eq!(drained(&mut chunks(vec![Ok("6,1,1,-;a\n"); 5]), 2).1, 3);
    let burst = vec![Ok("6,1,1,-;a\n6,2,2,-;b\n6,3,3,-;c\n"), Ok("6,4,4,-;d\n")];
    let (records, left) = drained(&mut chunks(burst), 2);
    assert_eq!((records.len(), left), (3, 1));
}

/// Proves `BrokenPipe` and `Interrupted` reads are passed over and another error is returned.
#[test]
fn broken_pipe_and_interrupted_reads_are_passed_over_and_another_error_is_returned() {
    let (broken, interrupted) = (io::ErrorKind::BrokenPipe, io::ErrorKind::Interrupted);
    let items = vec![
        Err(broken),
        Ok("6,1,1,-;a\n"),
        Err(interrupted),
        Ok("6,2,2,-;b\n"),
    ];
    assert_eq!(
        drained(&mut chunks(items), 10).0,
        [rec(1, 1, "a"), rec(2, 2, "b")]
    );
    let items = vec![Ok("6,1,1,-;a\n"), Err(io::ErrorKind::PermissionDenied)];
    let error = read_records(&mut chunks(items), &mut Vec::new(), 10).expect_err("the error");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

// ---- DevKmsg

/// Proves a read goes on from the last: all records, then none, then only the appended ones.
#[test]
fn a_dev_kmsg_read_goes_on_from_the_last() {
    let dir = scratch("devkmsg");
    let path = dir.path().join("kmsg");
    fs::write(&path, "6,1,10,-;one\n6,2,20,-;two\n").expect("the file");
    let mut kmsg = DevKmsg::at(&path);
    assert_eq!(
        kmsg.read(100).expect("the first read"),
        [rec(1, 10, "one"), rec(2, 20, "two")]
    );
    assert_eq!(kmsg.read(100).expect("the second read"), Vec::new());
    let mut file = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("the file");
    file.write_all(b"6,3,30,-;three\n").expect("the append");
    assert_eq!(
        kmsg.read(100).expect("the third read"),
        [rec(3, 30, "three")]
    );
}

/// Proves a device that is not there is an error, and is opened again at the next read.
#[test]
fn a_dev_kmsg_that_is_missing_is_an_error_until_it_is_there() {
    let dir = scratch("devkmsg-missing");
    let path = dir.path().join("kmsg");
    let mut kmsg = DevKmsg::at(&path);
    assert_eq!(
        kmsg.read(10).expect_err("a missing device").kind(),
        io::ErrorKind::NotFound
    );
    fs::write(&path, "6,1,10,-;one\n").expect("the file");
    assert_eq!(kmsg.read(10).expect("the read"), [rec(1, 10, "one")]);
}

/// Proves a poll over a `/dev/kmsg` that cannot be opened says the journal source is unreadable.
#[test]
fn a_poll_with_no_dev_kmsg_says_the_journal_source_is_unreadable() {
    let host = Host::quiet("nokmsg");
    let keeper = host.keeper(BOOT_A);
    let mut sources = host.sources_over(Box::new(DevKmsg::at(&host.root.path().join("kmsg"))));
    sources.poll(&keeper, NOW);
    let (status, reason) = state_of(&keeper, "journal");
    assert_eq!(status, pb::SourceStatus::Unreadable);
    assert!(reason.starts_with("/dev/kmsg cannot be read: "), "{reason}");
}

/// Proves the arguments `journalctl` is run with.
#[test]
fn the_journalctl_arguments_are_pinned() {
    let expected = "--dmesg --boot=-1 --output=json \
        --output-fields=MESSAGE,_SOURCE_MONOTONIC_TIMESTAMP --reverse --lines=100 --no-pager --quiet";
    assert_eq!(journal_args().join(" "), expected);
}

// ---- the kernel's log: poll

/// Proves a first poll holds the journal's boot before, then `/dev/kmsg`'s lines, and the mark.
#[test]
fn a_first_poll_holds_this_boots_log_and_the_boot_befores() {
    let host = Host::with_journal("first", LaunchctlOutput::success(JOURNAL));
    let keeper = host.keeper(BOOT_A);
    host.poll_seqs(&keeper, NOW, &[QUIET, UE, IO]);
    let held = host.held("journal");
    let expected = [
        "pcie_corrected 0000:3b:00.0 true",
        "memory_corrected DIMM_D1 true",
        "io_error nvme1n1 true",
        "cpu_machine_check CPU 0 true",
        "memory_uncorrectable DIMM_D1 true",
        "memory_uncorrectable CPU_SrcID#1_MC#1_Chan#2_DIMM#0 false",
        "io_error sdb false",
    ];
    assert_eq!(brief(&held), expected);
    let lines = kernel_log::parse_journal(JOURNAL.as_bytes());
    let mce = pb::HardwareEvent {
        event_id: format!(
            "journal:0f1e2d3c4b5a6978:86398000000:{}",
            kernel_log::digest8(&lines[4].text)
        ),
        time_unix: 1_795_661_998,
        of_previous_boot: true,
        component: pb::HardwareComponent::Cpu.into(),
        subject: "CPU 0".to_owned(),
        severity: pb::HardwareSeverity::Critical.into(),
        state: pb::HardwareEventState::Event.into(),
        source: "journal".to_owned(),
        code: "cpu_machine_check".to_owned(),
        message: "Uncorrected machine check on CPU 0, bank 13".to_owned(),
        raw: lines[4].text.clone(),
        about: String::new(),
    };
    assert_eq!(held[3], mce);
    let mark = LogMark {
        boot: BOOT_A.to_owned(),
        next_seq: 13,
        usec: 3_000_000,
        journal_read_in: BOOT_A.to_owned(),
        ..LogMark::default()
    };
    assert_eq!(log_mark(&keeper), mark);
    let state = |source: &str| pb::SourceState {
        source: source.to_owned(),
        status: pb::SourceStatus::Ok.into(),
        reason: String::new(),
    };
    assert_eq!(keeper.health().sources, [state("journal"), state("sysfs")]);
    assert_eq!(host.launchctl.calls(), [journal_args()]);
}

/// Proves a second poll in a boot runs no `journalctl` and holds only records after the mark.
#[test]
fn a_second_poll_in_the_boot_reads_no_journal_and_only_new_records() {
    let host = Host::with_journal("second", LaunchctlOutput::success(JOURNAL));
    let keeper = host.keeper(BOOT_A);
    let mut sources = host.sources(vec![Ok(vec![QUIET, UE, IO]), Ok(vec![IO, AER])]);
    sources.poll(&keeper, NOW);
    keeper.written(&host.held_ids("journal"));
    sources.poll(&keeper, NOW + 60);
    assert_eq!(host.held_ids("journal"), [id(BOOT_A, AER)]);
    assert_eq!(host.launchctl.calls().len(), 1);
    assert_eq!(log_mark(&keeper).next_seq, 14);
}

/// Proves a restarted daemon whose `/dev/kmsg` gives the old records and a new one holds the new.
#[test]
fn a_restarted_daemon_holds_only_records_after_its_mark() {
    let host = Host::quiet("restart");
    let keeper = host.keeper(BOOT_A);
    host.poll_seqs(&keeper, NOW, &[QUIET, UE, IO]);
    keeper.written(&host.held_ids("journal"));
    drop(keeper);
    let restarted = host.keeper(BOOT_A);
    host.poll_seqs(&restarted, NOW + 60, &[QUIET, UE, IO, AER]);
    assert_eq!(host.held_ids("journal"), [id(BOOT_A, AER)]);
    assert_eq!(log_mark(&restarted).next_seq, 14);
}

/// `journalctl --output=json` lines of `(boot, kernel stamp, message)`.
fn journal_json(lines: &[(&str, u64, &str)]) -> String {
    let one = |(boot, usec, text): &(&str, u64, &str)| {
        let real = BOOT_UNIX * 1_000_000 + usec;
        let boot = boot.replace('-', "");
        format!(
            "{{\"_BOOT_ID\":\"{boot}\",\"_SOURCE_MONOTONIC_TIMESTAMP\":\"{usec}\",\"__REALTIME_TIMESTAMP\":\"{real}\",\"MESSAGE\":\"{text}\"}}\n"
        )
    };
    lines.iter().map(one).collect()
}

/// Proves a new boot passes over journal lines up to the last stamp read, holds those after.
#[test]
fn a_new_boot_reads_the_journal_lines_after_the_last_one_read() {
    let host = Host::with_journal("newboot", no_boot());
    let keeper = host.keeper(BOOT_A);
    host.poll_seqs(&keeper, NOW, &[QUIET, UE, IO]);
    drop(keeper);
    let l = |usec, text| (BOOT_A, usec, text);
    let json = journal_json(&[
        l(1_000_000, "I/O error, dev sdz, sector 9"),
        l(3_000_000, "I/O error, dev sdy, sector 8"),
        l(3_500_000, "I/O error, dev sdc, sector 5"),
        l(
            4_000_000,
            "AER: Uncorrected (Non-Fatal) error received: 0000:af:00.1",
        ),
        (BOOT_B, 9_000_000, "I/O error, dev sdw, sector 7"),
    ]);
    host.launchctl
        .then(&journal_args(), Ok(LaunchctlOutput::success(&json)));
    let restarted = host.keeper(BOOT_B);
    host.sources(Vec::new()).poll(&restarted, NOW + 3600);
    let held = host.held("journal");
    assert_eq!(
        brief(&held[2..]),
        ["io_error sdc true", "pcie_uncorrected 0000:af:00.1 true"]
    );
    assert_eq!(held[2].time_unix, BOOT_UNIX + 3);
    let mark = log_mark(&restarted);
    assert_eq!(
        (mark.previous, mark.previous_usec),
        (BOOT_A.to_owned(), 3_000_000)
    );
    assert_eq!(
        (mark.journal_read_in, host.launchctl.calls().len()),
        (BOOT_B.to_owned(), 2)
    );
}

/// Proves no persistent journal is unsupported, runs no program, and a journal made later is read.
#[test]
fn a_host_with_no_persistent_journal_is_unsupported_and_reads_one_made_later() {
    let host = Host::quiet("nojournal");
    let keeper = host.keeper(BOOT_A);
    let mut sources = host.sources(vec![Ok(vec![UE]), Ok(Vec::new())]);
    sources.poll(&keeper, NOW);
    let reason = "no persistent journal: /var/log/journal is absent";
    assert_eq!(
        state_of(&keeper, "journal"),
        (pb::SourceStatus::Unsupported, reason.to_owned())
    );
    assert_eq!(
        host.log.lines()[0],
        format!("error: health source journal: {reason}")
    );
    assert!(host.launchctl.calls().is_empty());
    assert_eq!(host.held_ids("journal"), [id(BOOT_A, UE)]);
    assert_eq!(log_mark(&keeper).journal_read_in, "");
    host.make_journal();
    host.launchctl.always(&journal_args(), Ok(no_boot()));
    sources.poll(&keeper, NOW + 60);
    assert_eq!(state_of(&keeper, "journal"), ok());
    assert_eq!(host.launchctl.calls(), [journal_args()]);
    assert_eq!(log_mark(&keeper).journal_read_in, BOOT_A);
}

/// The journal source's reason, and the runs of `journalctl`, over two polls it fails with `answer`.
fn failing_journal(answer: Option<Result<LaunchctlOutput, LaunchctlError>>) -> (String, usize) {
    let host = Host::quiet("jfail");
    host.make_journal();
    if let Some(answer) = answer {
        host.launchctl.always(&journal_args(), answer);
    }
    let keeper = host.keeper(BOOT_A);
    let mut sources = host.sources(Vec::new());
    sources.poll(&keeper, NOW);
    sources.poll(&keeper, NOW + 60);
    let (status, reason) = state_of(&keeper, "journal");
    assert_eq!(status, pb::SourceStatus::Unreadable);
    assert_eq!(log_mark(&keeper).journal_read_in, "");
    (reason, host.launchctl.calls().len())
}

/// Proves another exit status, or a signal, is unreadable with the message's first line, rerun.
#[test]
fn a_journalctl_that_fails_makes_the_source_unreadable_and_is_run_again() {
    let failed = LaunchctlOutput::exit(2, "Failed to open journal files\nsecond line");
    let reason = "journalctl ended with status 2: Failed to open journal files";
    assert_eq!(failing_journal(Some(Ok(failed))), (reason.to_owned(), 2));
    let other = LaunchctlOutput::exit(1, "Something else went wrong");
    let reason = "journalctl ended with status 1: Something else went wrong";
    assert_eq!(failing_journal(Some(Ok(other))), (reason.to_owned(), 2));
    let killed = LaunchctlOutput {
        code: None,
        stdout: Vec::new(),
        stderr: b"killed".to_vec(),
    };
    let reason = "journalctl ended with a signal: killed";
    assert_eq!(failing_journal(Some(Ok(killed))), (reason.to_owned(), 2));
}

/// Proves a `journalctl` that cannot start or timed out is unreadable with the error, and rerun.
#[test]
fn a_journalctl_that_cannot_start_or_times_out_is_unreadable_and_run_again() {
    let args = journal_args().join(" ");
    let reason = format!("cannot run launchctl {args}: the fake has no answer");
    assert_eq!(failing_journal(None), (reason, 2));
    let timed_out = LaunchctlError::TimedOut { args: args.clone() };
    let reason = format!("launchctl {args} did not finish within its deadline and was killed");
    assert_eq!(failing_journal(Some(Err(timed_out))), (reason, 2));
}

/// Proves a boot whose start is not known gives its `/dev/kmsg` records no time.
#[test]
fn kmsg_records_of_a_boot_with_no_known_start_have_no_time() {
    let host = Host::quiet("nostart");
    let keeper = host.keeper_in(BOOT_A, 0);
    host.poll_seqs(&keeper, NOW, &[UE]);
    let held = host.held("journal");
    assert_eq!((ids(&held), held[0].time_unix), (vec![id(BOOT_A, UE)], 0));
}

/// Proves a mark that does not parse is read as none, for both sources, and replaced.
#[test]
fn a_mark_that_does_not_parse_is_read_as_none() {
    let host = Host::quiet("badmark");
    put(&host.root, TAINTED, "16\n");
    let keeper = host.keeper(BOOT_A);
    keeper.record("journal", "not json", Vec::new(), NOW);
    keeper.record("sysfs", "{\"taint\": \"many\"}", Vec::new(), NOW);
    host.poll_seqs(&keeper, NOW, &[UE]);
    assert_eq!(host.held_ids("journal"), [id(BOOT_A, UE)]);
    assert_eq!(host.held_ids("sysfs"), [format!("sysfs:taint:{KEY_A}:M")]);
    assert_eq!(
        (log_mark(&keeper).next_seq, files_mark(&keeper).taint),
        (12, 16)
    );
}

/// Proves a state change is logged once, and `journalctl` ending 1 about the boot is read once.
#[test]
fn a_change_of_a_sources_state_is_logged_once() {
    let host = Host::with_journal("statelog", no_boot());
    let keeper = host.keeper(BOOT_A);
    let boom = || io::Error::new(io::ErrorKind::PermissionDenied, "kmsg is closed");
    let answers = vec![
        Ok(Vec::new()),
        Ok(Vec::new()),
        Err(boom()),
        Err(boom()),
        Ok(Vec::new()),
    ];
    let mut sources = host.sources(answers);
    for step in 0..5 {
        sources.poll(&keeper, NOW + 60 * step);
    }
    let lines = [
        "health source journal: read",
        "health source sysfs: read",
        "error: health source journal: /dev/kmsg cannot be read: kmsg is closed",
        "health source journal: read",
    ];
    assert_eq!(host.log.lines(), lines);
    assert!(host.held("journal").is_empty());
    assert_eq!(host.launchctl.calls().len(), 1);
    assert_eq!(log_mark(&keeper).journal_read_in, BOOT_A);
}

// ---- the kernel's files: poll

/// Proves a taint flag is held once a boot, a later flag when set, and all again in a new boot.
#[test]
fn a_taint_flag_is_held_once_a_boot_and_again_in_a_new_boot() {
    let host = Host::quiet("taint");
    put(&host.root, TAINTED, "16\n");
    let keeper = host.keeper(BOOT_A);
    host.poll(&keeper, NOW);
    host.poll(&keeper, NOW + 60);
    let held = host.held("sysfs");
    let taint = |key: &str, flag: char| format!("sysfs:taint:{key}:{flag}");
    assert_eq!(ids(&held), [taint(KEY_A, 'M')]);
    assert_eq!(held[0].raw, "/proc/sys/kernel/tainted: 16");
    assert_eq!(state_of(&keeper, "sysfs"), ok());
    put(&host.root, TAINTED, "48\n");
    host.poll(&keeper, NOW + 120);
    assert_eq!(host.held_ids("sysfs")[1], taint(KEY_A, 'B'));
    drop(keeper);
    let restarted = host.keeper(BOOT_B);
    host.poll(&restarted, NOW + 3600);
    let held = host.held_ids("sysfs");
    assert_eq!(held[2..], [taint(KEY_B, 'M'), taint(KEY_B, 'B')]);
    let mark = files_mark(&restarted);
    assert_eq!((mark.boot.as_str(), mark.taint), (BOOT_B, 48));
}

/// Proves `HardwareCorrupted` is held when it grows only.
#[test]
fn hardware_corrupted_is_held_when_it_grows_only() {
    let host = Host::quiet("corrupted");
    let keeper = host.keeper(BOOT_A);
    for (step, kib) in [2048, 2048, 1024, 4096].into_iter().enumerate() {
        put(&host.root, MEMINFO, &meminfo(kib));
        host.poll(&keeper, NOW + 60 * step as u64);
    }
    let held = host.held("sysfs");
    let id = |kib: u64| format!("sysfs:hwcorrupted:{KEY_A}:{kib}");
    assert_eq!(ids(&held), [id(2048), id(4096)]);
    assert_eq!(held[1].raw, "HardwareCorrupted: 4096 kB");
    assert_eq!(held[1].time_unix, NOW + 180);
    assert_eq!(files_mark(&keeper).corrupted_kib, 4096);
}

/// Proves ext4 counts are held on growth only, odd devices passed over, the last error time used.
#[test]
fn ext4_error_counts_are_held_on_growth_only() {
    let host = Host::quiet("ext4");
    let ext4 = |rel: &str| format!("{EXT4_DIR}/{rel}");
    put(&host.root, &ext4("sda1/errors_count"), "3\n");
    put(&host.root, &ext4("sda1/last_error_time"), "1795661000\n");
    put(&host.root, &ext4("dm-0/errors_count"), "0\n");
    put(&host.root, &ext4("loop0/errors_count"), "9\n");
    put(&host.root, &ext4("features/meta_bg"), "supported\n");
    put(&host.root, &ext4("nvme0n1p2/errors_count"), "many\n");
    let keeper = host.keeper(BOOT_A);
    host.poll(&keeper, NOW);
    host.poll(&keeper, NOW + 60);
    let held = host.held("sysfs");
    assert_eq!(ids(&held), ["sysfs:ext4:sda1:3"]);
    assert_eq!(held[0].time_unix, 1_795_661_000);
    assert_eq!(held[0].message, "ext4 on sda1: 3 errors recorded, 3 new");
    assert_eq!(held[0].raw, "/sys/fs/ext4/sda1/errors_count: 3");
    put(&host.root, &ext4("sda1/errors_count"), "5\n");
    fs::remove_file(host.root.path().join(ext4("sda1/last_error_time"))).expect("the file");
    host.poll(&keeper, NOW + 120);
    let held = host.held("sysfs");
    assert_eq!(ids(&held), ["sysfs:ext4:sda1:3", "sysfs:ext4:sda1:5"]);
    assert_eq!(held[1].time_unix, NOW + 120);
}

fn ro_id(now: u64) -> String {
    format!("sysfs:ro:{KEY_A}:{}:{now}", kernel_log::digest8("/"))
}

/// A quiet host and its keeper of `BOOT_A`, `/` seen writable at `NOW` and read-only at `NOW + 60`.
fn remounted(name: &str) -> (Host, HealthKeeper) {
    let host = Host::quiet(name);
    let keeper = host.keeper(BOOT_A);
    host.poll(&keeper, NOW);
    put(&host.root, MOUNTS, &mounts("ro,relatime"));
    host.poll(&keeper, NOW + 60);
    (host, keeper)
}

/// Proves a read-only remount is asserted once, and deasserted when the mount is writable again.
#[test]
fn a_read_only_remount_is_asserted_then_deasserted() {
    let (host, keeper) = remounted("remount");
    host.poll(&keeper, NOW + 120);
    let held = host.held("sysfs");
    assert_eq!(ids(&held), [ro_id(NOW + 60)]);
    assert_eq!(held[0].state, i32::from(pb::HardwareEventState::Asserted));
    assert_eq!(held[0].message, "/ (sda1) was remounted read-only");
    assert_eq!(held[0].raw, "/dev/sda1 / ext4 ro,relatime 0 0");
    assert!(files_mark(&keeper).remounts.read_only.contains_key("/"));
    put(&host.root, MOUNTS, &mounts("rw,relatime"));
    host.poll(&keeper, NOW + 180);
    let held = host.held("sysfs");
    assert_eq!(held[1].event_id, format!("{}:end", ro_id(NOW + 60)));
    assert_eq!(held[1].about, ro_id(NOW + 60));
    assert_eq!(held[1].state, i32::from(pb::HardwareEventState::Deasserted));
    assert_eq!(held[1].message, "/ (sda1) is writable again");
    assert_eq!(held[1].raw, "/dev/sda1 / ext4 rw,relatime 0 0");
    assert!(files_mark(&keeper).remounts.read_only.is_empty());
}

/// Proves a remount whose mount is gone, and one whose boot ended, are deasserted.
#[test]
fn a_remount_whose_mount_is_gone_or_whose_boot_ended_is_deasserted() {
    let (host, keeper) = remounted("rogone");
    put(&host.root, MOUNTS, "proc /proc proc rw 0 0\n");
    host.poll(&keeper, NOW + 120);
    let held = host.held("sysfs");
    assert_eq!(held.len(), 2);
    assert_eq!(
        (held[1].message.as_str(), held[1].raw.as_str()),
        ("/ is no longer mounted", "")
    );
    let (host, keeper) = remounted("roboot");
    drop(keeper);
    let restarted = host.keeper(BOOT_B);
    host.poll(&restarted, NOW + 3600);
    let held = host.held("sysfs");
    assert_eq!(held.len(), 2);
    assert_eq!(held[1].event_id, format!("{}:end", ro_id(NOW + 60)));
    assert_eq!(held[1].message, "/ (sda1): its boot ended");
    assert!(files_mark(&restarted).remounts.read_only.is_empty());
}

/// Proves both pstore directories' dumps are held once, and a new dump of the newest second too.
#[test]
fn pstore_dumps_are_held_once_and_a_new_dump_of_the_same_second_too() {
    let host = Host::quiet("pstore");
    let (efi, archived) = ("dmesg-efi-171234567801001", "171234567801002");
    put_at(&host.root, &format!("{PSTORE_DIR}/{efi}"), PANIC, T_PRE);
    put_at(
        &host.root,
        &format!("{SYSTEMD_PSTORE_DIR}/{archived}/dmesg.txt"),
        OOPS,
        T_POST,
    );
    let keeper = host.keeper(BOOT_A);
    host.poll(&keeper, NOW);
    host.poll(&keeper, NOW + 60);
    let id = |name: &str, mtime: u64| format!("sysfs:pstore:{mtime}:{}", kernel_log::digest8(name));
    let held = host.held("sysfs");
    assert_eq!(ids(&held), [id(efi, T_PRE), id(archived, T_POST)]);
    assert_eq!(
        brief(&held),
        ["panic_hardware board true", "kernel_oops board false"]
    );
    assert_eq!(
        held[1].raw,
        "<4>[  512.000100] Oops: 0000 [#1] PREEMPT SMP NOPTI"
    );
    assert_eq!(files_mark(&keeper).pstore_mtime, T_POST);
    let late = "dmesg-efi-171234567801003";
    put_at(
        &host.root,
        &format!("{PSTORE_DIR}/{late}"),
        "plain log\n",
        T_POST,
    );
    host.poll(&keeper, NOW + 120);
    host.poll(&keeper, NOW + 180);
    let held = host.held("sysfs");
    assert_eq!(held.len(), 3);
    assert_eq!(brief(&held[2..]), ["kernel_dump board false"]);
    assert_eq!(
        (held[2].event_id.as_str(), held[2].raw.as_str()),
        (id(late, T_POST).as_str(), "plain log")
    );
    assert_eq!(files_mark(&keeper).pstore_ids.len(), 2);
}

/// Proves `dumps` reads both directories and gives none for empty ones, files, or a bare root.
#[test]
fn dumps_reads_both_pstore_directories() {
    let host = Host::new("dumps");
    assert_eq!(dumps(host.root.path()).expect("no directory"), Vec::new());
    let pstore = |name: &str| format!("{PSTORE_DIR}/{name}");
    let archive = |name: &str| format!("{SYSTEMD_PSTORE_DIR}/{name}");
    put_at(&host.root, &pstore("dmesg-efi-100"), "a\n", 1000);
    put_at(&host.root, &pstore("dmesg-efi-101"), "b\n", 1000);
    put_at(&host.root, &pstore("dmesg-efi-102"), "c\n", 2000);
    put_at(&host.root, &archive("200/dmesg.txt"), "d\n", 3000);
    put_at(&host.root, &archive("201/dmesg-efi-1"), "e\n", 4000);
    put_at(&host.root, &archive("201/dmesg-efi-2"), "f\n", 4001);
    fs::create_dir_all(host.root.path().join(archive("202"))).expect("an empty directory");
    put(&host.root, &archive("203"), "a file");
    let read = dumps(host.root.path()).expect("the dumps");
    let brief: Vec<(&str, u64, &str)> = read
        .iter()
        .map(|d| (d.name.as_str(), d.mtime, d.text.as_str()))
        .collect();
    let expected = [
        ("dmesg-efi-100", 1000, "a\n\nb\n"),
        ("dmesg-efi-102", 2000, "c\n"),
        ("200", 3000, "d\n"),
        ("201", 4000, "e\n\nf\n"),
    ];
    assert_eq!(brief, expected);
}

/// Proves only the last `PSTORE_FILE_MAX` bytes of a large pstore file are read.
#[test]
fn a_large_pstore_file_is_read_from_its_end() {
    let host = Host::new("bigdump");
    let text = format!("{}{}", "A".repeat(10), "B".repeat(262_144));
    put_at(&host.root, &format!("{PSTORE_DIR}/dmesg-efi-1"), &text, 5);
    let read = dumps(host.root.path()).expect("the dumps");
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].text.len(), 262_144);
    assert!(read[0].text.bytes().all(|b| b == b'B'));
}

/// Proves a missing tainted file makes sysfs unreadable while the rest is still read.
#[test]
fn a_missing_tainted_file_makes_sysfs_unreadable_but_the_rest_is_read() {
    let host = Host::new("notaint");
    put(&host.root, MEMINFO, &meminfo(100));
    put(&host.root, MOUNTS, &mounts("rw,relatime"));
    let keeper = host.keeper(BOOT_A);
    host.poll(&keeper, NOW);
    let (status, reason) = state_of(&keeper, "sysfs");
    assert_eq!(status, pb::SourceStatus::Unreadable);
    assert!(
        reason.starts_with("/proc/sys/kernel/tainted cannot be read: "),
        "{reason}"
    );
    assert_eq!(host.held("sysfs")[0].code, "memory_hardware_corrupted");
}

/// Proves a tainted file that is not a number, and files that cannot be read, say so.
#[test]
fn files_that_cannot_be_read_make_sysfs_unreadable() {
    let reason_of = |name: &str, break_it: fn(&Host)| {
        let host = Host::quiet(name);
        break_it(&host);
        let keeper = host.keeper(BOOT_A);
        host.poll(&keeper, NOW);
        let (status, reason) = state_of(&keeper, "sysfs");
        assert_eq!(status, pb::SourceStatus::Unreadable);
        reason
    };
    let number = reason_of("unread0", |host: &Host| {
        put(&host.root, TAINTED, "tainted\n")
    });
    assert_eq!(number, "/proc/sys/kernel/tainted is not a number");
    let ext4 = reason_of("unread1", |host: &Host| {
        put(&host.root, EXT4_DIR, "not a directory")
    });
    assert!(ext4.starts_with("/sys/fs/ext4 cannot be read: "), "{ext4}");
    let pstore = reason_of("unread3", |host: &Host| {
        put(&host.root, PSTORE_DIR, "not a directory")
    });
    assert!(pstore.starts_with("pstore cannot be read: "), "{pstore}");
}

// ---- run, start, at_host

/// Proves `run` polls at once and after each wait of `POLL_SECS` until the sleeper stops it.
#[test]
fn run_polls_every_wait_until_the_sleeper_stops_it() {
    let host = Host::quiet("run");
    let keeper = host.keeper(BOOT_A);
    let sources = host.sources(vec![Ok(vec![UE]), Ok(vec![IO]), Ok(vec![AER])]);
    let sleeper = ScriptedSleeper::new(2);
    run(sources, &keeper, &FakeClock::new(NOW, 10), &sleeper);
    assert_eq!(sleeper.waits(), [60, 60, 60]);
    let expected = [id(BOOT_A, UE), id(BOOT_A, IO), id(BOOT_A, AER)];
    assert_eq!(host.held_ids("journal"), expected);
}

/// Proves `start` runs one polling job on Linux and OpenRC with a keeper, none on a Mac or without.
#[test]
fn start_runs_one_job_on_linux_hosts_with_a_keeper_and_none_elsewhere() {
    let cases = [
        ("startmac", PlatformKind::Macos, true, 0),
        ("startnone", PlatformKind::Linux, false, 0),
        ("startlinux", PlatformKind::Linux, true, 1),
        ("startopenrc", PlatformKind::Openrc, true, 1),
    ];
    for (name, kind, with_keeper, jobs) in cases {
        let host = Host::quiet(name);
        let keeper = Arc::new(host.keeper(BOOT_A));
        let dispatch = ManualDispatch::default();
        let sources = host.sources(vec![Ok(vec![UE])]);
        let build = move || {
            assert_eq!(jobs, 1, "sources are built only for a job");
            sources
        };
        let clock = Arc::new(FakeClock::new(NOW, 10));
        let sleeper = Box::new(ScriptedSleeper::new(0));
        let keeper = with_keeper.then_some(keeper);
        start(kind, keeper, build, clock, sleeper, &dispatch);
        assert_eq!(dispatch.pending(), jobs, "{name}");
        assert_eq!(dispatch.run_next(), jobs == 1, "{name}");
        let held: Vec<String> = (0..jobs).map(|_| id(BOOT_A, UE)).collect();
        assert_eq!(host.held_ids("journal"), held, "{name}");
    }
}

/// Proves the host's own sources are built over `/` without reading anything.
#[test]
fn the_host_sources_are_built_without_reading_anything() {
    let log = Arc::new(RecordingLog::default());
    let sources = at_host(log.clone());
    assert_eq!(sources.root.as_path(), Path::new("/"));
    assert!(log.lines().is_empty());
}
