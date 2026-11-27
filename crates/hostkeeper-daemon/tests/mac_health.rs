//! A Mac's hardware sources (`hostkeeper_daemon::mac_health`), each driven as the daemon drives it
//! (`hardware_sources::poll` and `run`) into a keeper over a scratch state directory: the panic
//! reports of a fake report directory (read once each, the mark following the directory, history on
//! the first read, a listing refused or absent, a report that cannot be read), the unexpected
//! restart judged at the second read and merged with the panic of the boot before, the capacity
//! from fake `sysctl` and `system_profiler` and each of their failures, the sources' periods over a
//! scripted sleeper, and the real directory port over a scratch directory. Nothing reads the
//! host's own files or runs its programs, but the one test that runs only on a Mac.

mod support;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hostkeeper_daemon::hardware_sources::{HardwareSource, poll, run};
use hostkeeper_daemon::host_health::{BootFacts, HISTORY_PREFIX, HISTORY_SECS, HealthKeeper};
use hostkeeper_daemon::mac_capacity::{CAPACITY_SYSCTL_ARGS, PROFILER_ARGS};
use hostkeeper_daemon::mac_health::{
    CAPACITY_AGAIN_SECS, CAPACITY_PERIOD_SECS, CAPACITY_SOURCE, CapacitySource, HOST_SUBJECT,
    HostReportDir, JUDGE_AFTER_SECS, MAX_MARKED, MAX_REPORT_BYTES, PANIC_PERIOD_SECS, PANIC_SOURCE,
    PanicSource, ReportDir, ReportFile, SHUTDOWN_SOURCE, UNEXPECTED_REBOOT, for_host,
    judge_shutdown,
};
use hostkeeper_daemon::mac_panic::{PanicReport, parse};
use hostkeeper_daemon::shutdown_probe::{GoingDown, ShutdownProbe};
use hostkeeper_launchd::{FakeLaunchctl, LaunchctlOutput, PlatformKind};
use hostkeeper_link::testing::{FakeClock, RecordingLog, ScriptedSleeper};
use hostkeeper_link::{HostHealthSource, Sleeper, Tick};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use support::Scratch;

/// The second the tests read at: after every fixture's panic.
const NOW: u64 = 1_795_662_000;

/// When the boot before began: before every fixture's panic.
const BOOT_BEFORE: u64 = 1_795_000_000;

/// When the boot the daemon runs in began: after every fixture's panic.
const BOOT: u64 = 1_795_660_000;

/// When the fixture reports' files were last modified.
const MODIFIED: u64 = 1_795_100_000;

fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/mac_health/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct Dir {
    files: BTreeMap<String, (Vec<u8>, u64)>,
    list_error: Option<io::ErrorKind>,
    read_errors: BTreeSet<String>,
    reads: Vec<String>,
}

/// A report directory in memory; clones share it.
#[derive(Clone, Default)]
struct FakeReports(Arc<Mutex<Dir>>);

impl FakeReports {
    fn with(names: &[&str]) -> FakeReports {
        let reports = FakeReports::default();
        for name in names {
            reports.add(name, &fixture(name), MODIFIED);
        }
        reports
    }

    fn add(&self, name: &str, bytes: &[u8], modified: u64) {
        lock(&self.0)
            .files
            .insert(name.to_owned(), (bytes.to_vec(), modified));
    }

    fn remove(&self, name: &str) {
        lock(&self.0).files.remove(name);
    }

    fn refuse_list(&self, kind: Option<io::ErrorKind>) {
        lock(&self.0).list_error = kind;
    }

    fn refuse_read(&self, name: &str, refused: bool) {
        let mut dir = lock(&self.0);
        if refused {
            dir.read_errors.insert(name.to_owned());
        } else {
            dir.read_errors.remove(name);
        }
    }

    fn reads(&self) -> Vec<String> {
        lock(&self.0).reads.clone()
    }
}

impl ReportDir for FakeReports {
    fn list(&self) -> io::Result<Vec<ReportFile>> {
        let dir = lock(&self.0);
        if let Some(kind) = dir.list_error {
            return Err(io::Error::new(kind, "the listing is refused"));
        }
        Ok(dir
            .files
            .iter()
            .map(|(name, (_, modified))| ReportFile {
                name: name.clone(),
                modified_unix: *modified,
            })
            .collect())
    }

    fn read(&self, name: &str, max: u64) -> io::Result<Vec<u8>> {
        let mut dir = lock(&self.0);
        assert_eq!(max, MAX_REPORT_BYTES);
        if dir.read_errors.contains(name) {
            return Err(io::Error::other("the read is refused"));
        }
        dir.reads.push(name.to_owned());
        Ok(dir
            .files
            .get(name)
            .map(|(b, _)| b.clone())
            .unwrap_or_default())
    }
}

/// A probe that never says the system goes down: every start reads the boot before unclean.
struct Never;

impl ShutdownProbe for Never {
    fn going_down(&self) -> GoingDown {
        GoingDown::No("a test".to_owned())
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

/// What a test drives: the keeper, its directory, its programs, its clock and its log.

/// What a test drives: the keeper, its directory, its programs and its clock.
struct Mac {
    keeper: Arc<HealthKeeper>,
    reports: FakeReports,
    sysctl: FakeLaunchctl,
    profiler: FakeLaunchctl,
    clock: Arc<FakeClock>,
}

impl Mac {
    fn panics(&self) -> PanicSource {
        PanicSource::new(Arc::clone(&self.keeper), Box::new(self.reports.clone()))
    }

    fn capacity(&self) -> CapacitySource {
        CapacitySource::new(
            Arc::clone(&self.keeper),
            Box::new(self.sysctl.clone()),
            Box::new(self.profiler.clone()),
        )
    }

    /// One read of `source`, as the daemon's worker makes it.
    fn poll(&self, source: &mut dyn HardwareSource) -> usize {
        poll(source, &self.keeper, self.clock.as_ref())
    }

    fn health(&self) -> pb::HostHealth {
        self.keeper.health()
    }
}

/// The keeper of the scratch `dir` at a start in the boot `boot_id` that began at `boot_unix`,
/// over `reports`, with programs that answer the fixtures.
fn mac(dir: &Scratch, boot_id: Option<&str>, boot_unix: u64, reports: FakeReports) -> Mac {
    let keeper = Arc::new(HealthKeeper::open(
        dir.path(),
        BootFacts {
            boot_id: boot_id.map(str::to_owned),
            boot_unix,
            machine_id: String::new(),
        },
        Box::new(Never),
        Arc::new(RecordingLog::default()),
    ));
    let sysctl = FakeLaunchctl::new();
    sysctl.always(
        &args(&CAPACITY_SYSCTL_ARGS),
        Ok(LaunchctlOutput::success(&String::from_utf8_lossy(
            &fixture("sysctl-capacity.txt"),
        ))),
    );
    let profiler = FakeLaunchctl::new();
    profiler.always(
        &args(&PROFILER_ARGS),
        Ok(LaunchctlOutput::success(&String::from_utf8_lossy(
            &fixture("system-profiler.json"),
        ))),
    );
    Mac {
        keeper,
        reports,
        sysctl,
        profiler,
        clock: Arc::new(FakeClock::new(NOW, 0)),
    }
}

/// A Mac whose boot before (`boot-1`) ended unclean, now in `boot-2`.
fn after_unclean_boot(dir: &Scratch, reports: FakeReports) -> Mac {
    drop(mac(
        dir,
        Some("boot-1"),
        BOOT_BEFORE,
        FakeReports::default(),
    ));
    mac(dir, Some("boot-2"), BOOT, reports)
}

fn ids(health: &pb::HostHealth) -> Vec<String> {
    health.events.iter().map(|e| e.event_id.clone()).collect()
}

fn source(health: &pb::HostHealth, name: &str) -> Option<pb::SourceState> {
    health.sources.iter().find(|s| s.source == name).cloned()
}

fn state(name: &str, status: pb::SourceStatus, reason: &str) -> pb::SourceState {
    pb::SourceState {
        source: name.to_owned(),
        status: status.into(),
        reason: reason.to_owned(),
    }
}

fn ok(name: &str) -> pb::SourceState {
    state(name, pb::SourceStatus::Ok, "")
}

/// Proves a first read holds each panic of the directory once (the `.panic` and `.ips` reports of
/// one panic as one record, an app's crash and a file of another kind as none), the most severe
/// first, a report older than the history window as `info` history, and says the source is read.
#[test]
fn a_first_read_holds_each_panic_once_and_older_ones_as_history() {
    let dir = Scratch::new("mac-panics-first");
    let reports = FakeReports::with(&[
        "serror.panic",
        "serror-copy.ips",
        "amcc.ips",
        "app-crash.ips",
    ]);
    reports.add(
        "notes.txt",
        b"panic(cpu 0 caller 0x1): not a report",
        MODIFIED,
    );
    reports.add("old.panic", &fixture("text.panic"), NOW - HISTORY_SECS - 1);
    let m = mac(&dir, Some("boot-2"), BOOT, reports.clone());
    assert_eq!(m.poll(&mut m.panics()), 3, "three records held");
    let health = m.health();
    assert_eq!(
        ids(&health),
        [
            "panic:2b6f9c1e-7d1a-4c3e-9f20-6a1d0e5b8c44",
            "panic:0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0",
            "panic:file:388c34265b1d2abb",
        ]
    );
    let old = &health.events[2];
    assert_eq!(old.severity, i32::from(pb::HardwareSeverity::Info));
    assert_eq!(
        old.message,
        format!("{HISTORY_PREFIX}Kernel panic (software)")
    );
    assert_eq!(source(&health, PANIC_SOURCE), Some(ok(PANIC_SOURCE)));
    assert_eq!(
        reports.reads(),
        [
            "serror.panic",
            "serror-copy.ips",
            "app-crash.ips",
            "amcc.ips",
            "old.panic"
        ],
        "the newest first; a file of another kind is not read"
    );
    assert_eq!(
        m.keeper.mark(PANIC_SOURCE).as_deref(),
        Some("amcc.ips\napp-crash.ips\nold.panic\nserror-copy.ips\nserror.panic")
    );
}

/// Proves a report read before is not read again while it stays, a new one is read alone, the
/// mark drops a report that left the directory, and records written by the coordinator are not
/// held again by a later read.
#[test]
fn reports_read_before_are_not_read_again_and_the_mark_follows_the_directory() {
    let dir = Scratch::new("mac-panics-again");
    let reports = FakeReports::with(&["serror.panic", "app-crash.ips"]);
    let m = mac(&dir, Some("boot-2"), BOOT, reports.clone());
    let mut panics = m.panics();
    assert_eq!(m.poll(&mut panics), 1);
    m.keeper.written(&ids(&m.health()));
    assert_eq!(m.poll(&mut panics), 0);
    assert_eq!(reports.reads().len(), 2, "nothing read again");
    assert!(m.health().events.is_empty(), "nothing held again");
    reports.add("kext.panic", &fixture("kext.panic"), MODIFIED);
    assert_eq!(m.poll(&mut panics), 1);
    assert_eq!(
        ids(&m.health()),
        ["panic:12345678-90ab-cdef-1234-567890abcdef"]
    );
    assert_eq!(reports.reads().len(), 3);
    reports.remove("serror.panic");
    assert_eq!(m.poll(&mut panics), 0);
    assert_eq!(
        m.keeper.mark(PANIC_SOURCE).as_deref(),
        Some("app-crash.ips\nkext.panic")
    );
}

/// Proves a report that cannot be read makes the source unreadable, naming it, while the others
/// are read and held; it is left out of the mark, and read at the next read once it can be.
#[test]
fn a_report_that_cannot_be_read_is_unreadable_and_tried_again() {
    let dir = Scratch::new("mac-panics-retry");
    let reports = FakeReports::with(&["amcc.ips", "serror.panic"]);
    reports.refuse_read("amcc.ips", true);
    let m = mac(&dir, Some("boot-2"), BOOT, reports.clone());
    let mut panics = m.panics();
    assert_eq!(m.poll(&mut panics), 1, "the other report is held");
    assert_eq!(
        source(&m.health(), PANIC_SOURCE),
        Some(state(
            PANIC_SOURCE,
            pb::SourceStatus::Unreadable,
            "amcc.ips not read: the read is refused"
        ))
    );
    assert_eq!(m.keeper.mark(PANIC_SOURCE).as_deref(), Some("serror.panic"));
    reports.refuse_read("amcc.ips", false);
    assert_eq!(m.poll(&mut panics), 1);
    assert_eq!(source(&m.health(), PANIC_SOURCE), Some(ok(PANIC_SOURCE)));
    assert_eq!(
        ids(&m.health()),
        [
            "panic:2b6f9c1e-7d1a-4c3e-9f20-6a1d0e5b8c44",
            "panic:0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0"
        ]
    );
}

/// Proves a directory that cannot be listed is the source `unreadable` with its reason and keeps
/// no mark, one that is not there is read with no reports, and a listing that works again says
/// the source is read.
#[test]
fn a_refused_listing_is_unreadable_and_an_absent_directory_is_empty() {
    let dir = Scratch::new("mac-panics-refused");
    let reports = FakeReports::with(&["serror.panic"]);
    reports.refuse_list(Some(io::ErrorKind::PermissionDenied));
    let m = mac(&dir, Some("boot-2"), BOOT, reports.clone());
    let mut panics = m.panics();
    assert_eq!(m.poll(&mut panics), 0);
    assert_eq!(
        source(&m.health(), PANIC_SOURCE),
        Some(state(
            PANIC_SOURCE,
            pb::SourceStatus::Unreadable,
            "not listed: the listing is refused"
        ))
    );
    assert_eq!(m.keeper.mark(PANIC_SOURCE), None);
    reports.refuse_list(Some(io::ErrorKind::NotFound));
    assert_eq!(m.poll(&mut panics), 0);
    assert_eq!(source(&m.health(), PANIC_SOURCE), Some(ok(PANIC_SOURCE)));
    assert_eq!(m.keeper.mark(PANIC_SOURCE).as_deref(), Some(""));
    reports.refuse_list(None);
    assert_eq!(m.poll(&mut panics), 1);
}

/// Proves only the newest `MAX_MARKED` reports are read and marked, so the mark never grows past
/// its bound and the older reports past it are not read at every read.
#[test]
fn the_mark_keeps_the_newest_reports_only() {
    let dir = Scratch::new("mac-panics-bound");
    let reports = FakeReports::default();
    let crash = fixture("app-crash.ips");
    for n in 0..MAX_MARKED + 3 {
        let bytes = if n < 3 {
            fixture("amcc.ips")
        } else {
            crash.clone()
        };
        reports.add(&format!("crash-{n:04}.ips"), &bytes, MODIFIED + n as u64);
    }
    let m = mac(&dir, Some("boot-2"), BOOT, reports.clone());
    let mut panics = m.panics();
    assert_eq!(m.poll(&mut panics), 0);
    assert_eq!(m.poll(&mut panics), 0, "nor at the next read");
    assert_eq!(reports.reads().len(), MAX_MARKED);
    assert!(!reports.reads().contains(&"crash-0000.ips".to_owned()));
    let mark = m.keeper.mark(PANIC_SOURCE).expect("a mark");
    let names: Vec<&str> = mark.lines().collect();
    assert_eq!(names.len(), MAX_MARKED);
    assert_eq!(names.first().copied(), Some("crash-0003.ips"));
    assert_eq!(
        names.last().copied(),
        Some(format!("crash-{:04}.ips", MAX_MARKED + 2).as_str())
    );
}

fn serror() -> PanicReport {
    parse("serror.panic", &fixture("serror.panic"), MODIFIED).expect("a panic report")
}

/// Proves an unclean boot before gives one `host` warning, `unexpected_reboot`, merged with that
/// boot's panic (its class in the message, its panic line as `raw`), whole; judged again, it holds
/// nothing more.
#[test]
fn an_unclean_boot_before_is_one_unexpected_restart_merged_with_its_panic() {
    let dir = Scratch::new("mac-shutdown-panic");
    let m = after_unclean_boot(&dir, FakeReports::default());
    assert!(judge_shutdown(&m.keeper, &[serror()], NOW));
    assert_eq!(
        m.health().events,
        [pb::HardwareEvent {
            event_id: "shutdown:boot-2".to_owned(),
            time_unix: BOOT,
            of_previous_boot: true,
            component: pb::HardwareComponent::Host.into(),
            subject: HOST_SUBJECT.to_owned(),
            severity: pb::HardwareSeverity::Warning.into(),
            state: pb::HardwareEventState::Event.into(),
            source: SHUTDOWN_SOURCE.to_owned(),
            code: UNEXPECTED_REBOOT.to_owned(),
            message: "Unexpected restart after a kernel panic (SError)".to_owned(),
            raw: serror().panic_line,
            about: String::new(),
        }]
    );
    assert!(
        !judge_shutdown(&m.keeper, &[serror()], NOW),
        "one record a boot"
    );
    assert_eq!(m.keeper.mark(SHUTDOWN_SOURCE).as_deref(), Some("boot-2"));
}

/// Proves an unclean boot before with no panic of its own is still one record, saying so, and a
/// panic before that boot began, or after this one began, is not merged; of two panics of that
/// boot the newest is; with this boot's start not read, any panic since the boot before began is.
#[test]
fn only_a_panic_of_the_boot_before_is_merged() {
    let dir = Scratch::new("mac-shutdown-window");
    let m = after_unclean_boot(&dir, FakeReports::default());
    let early = PanicReport {
        time_unix: BOOT_BEFORE - 1,
        ..serror()
    };
    let late = PanicReport {
        time_unix: BOOT,
        ..serror()
    };
    assert!(judge_shutdown(&m.keeper, &[early, late], NOW));
    let event = &m.health().events[0];
    assert_eq!(
        (event.message.as_str(), event.raw.as_str()),
        (
            "Unexpected restart: the boot before ended with no clean shutdown",
            "boot boot-1 ended with no clean-shutdown marker"
        )
    );

    let dir = Scratch::new("mac-shutdown-newest");
    let m = after_unclean_boot(&dir, FakeReports::default());
    let watchdog =
        parse("w.panic", &fixture("soc-watchdog.panic"), MODIFIED).expect("a panic report");
    assert!(judge_shutdown(&m.keeper, &[serror(), watchdog], NOW));
    assert_eq!(
        m.health().events[0].message,
        "Unexpected restart after a kernel panic (SoC watchdog)"
    );

    let dir = Scratch::new("mac-shutdown-no-start");
    drop(mac(
        &dir,
        Some("boot-1"),
        BOOT_BEFORE,
        FakeReports::default(),
    ));
    let m = mac(&dir, Some("boot-2"), 0, FakeReports::default());
    let late = PanicReport {
        time_unix: NOW,
        ..serror()
    };
    assert!(judge_shutdown(&m.keeper, &[late], NOW));
    let event = &m.health().events[0];
    assert_eq!(
        (event.message.as_str(), event.time_unix),
        ("Unexpected restart after a kernel panic (SError)", 0)
    );
}

/// A probe that says the system goes down.
struct Says;

impl ShutdownProbe for Says {
    fn going_down(&self) -> GoingDown {
        GoingDown::Yes("a test".to_owned())
    }
}

/// Proves no unexpected restart is judged on the first start on a host, after a clean shutdown,
/// at a second start in a boot already judged, or when the boot id cannot be read; a restart of
/// the daemon in the boot before it was judged still judges it.
#[test]
fn no_unexpected_restart_without_an_unclean_boot_before() {
    let dir = Scratch::new("mac-shutdown-first");
    let m = mac(&dir, Some("boot-1"), BOOT, FakeReports::default());
    assert!(
        !judge_shutdown(&m.keeper, &[serror()], NOW),
        "the first start"
    );

    let dir = Scratch::new("mac-shutdown-clean");
    drop(mac(
        &dir,
        Some("boot-1"),
        BOOT_BEFORE,
        FakeReports::default(),
    ));
    let clean = HealthKeeper::open(
        dir.path(),
        BootFacts {
            boot_id: Some("boot-1".to_owned()),
            boot_unix: BOOT_BEFORE,
            machine_id: String::new(),
        },
        Box::new(Says),
        Arc::new(RecordingLog::default()),
    );
    clean.stopping();
    drop(clean);
    let m = mac(&dir, Some("boot-2"), BOOT, FakeReports::default());
    assert!(
        !judge_shutdown(&m.keeper, &[serror()], NOW),
        "after a clean stop"
    );

    let dir = Scratch::new("mac-shutdown-same");
    drop(after_unclean_boot(&dir, FakeReports::default()));
    let m = mac(&dir, Some("boot-2"), BOOT, FakeReports::default());
    assert!(
        judge_shutdown(&m.keeper, &[], NOW),
        "a restart before the judgement still judges"
    );
    m.keeper.written(&ids(&m.health()));
    drop(m);
    let m = mac(&dir, Some("boot-2"), BOOT, FakeReports::default());
    assert!(
        !judge_shutdown(&m.keeper, &[serror()], NOW),
        "a boot judged is not judged again"
    );
    assert!(m.health().events.is_empty());

    let dir = Scratch::new("mac-shutdown-unknown");
    drop(mac(
        &dir,
        Some("boot-1"),
        BOOT_BEFORE,
        FakeReports::default(),
    ));
    let m = mac(&dir, None, BOOT, FakeReports::default());
    assert!(!judge_shutdown(&m.keeper, &[serror()], NOW), "no boot id");
    assert!(m.health().events.is_empty());
}

/// Proves both programs read give the capacity of the fixtures, sent as the heartbeat's
/// `baseline`, and say the source is read, with no mark and no record.
#[test]
fn the_capacity_is_read_from_both_programs() {
    let dir = Scratch::new("mac-capacity");
    let m = mac(&dir, Some("boot-2"), BOOT, FakeReports::default());
    assert_eq!(m.poll(&mut m.capacity()), 0);
    let health = m.health();
    let baseline = health.baseline.clone().expect("a capacity");
    assert_eq!(
        (
            baseline.mem_total_kib,
            baseline.cpus_present,
            baseline.cpus_online
        ),
        (33_554_432, 12, 12)
    );
    assert_eq!(
        baseline
            .disks
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>(),
        ["disk0"]
    );
    assert_eq!(source(&health, CAPACITY_SOURCE), Some(ok(CAPACITY_SOURCE)));
    assert_eq!(m.keeper.mark(CAPACITY_SOURCE), None);
    assert_eq!(m.sysctl.calls(), [args(&CAPACITY_SYSCTL_ARGS)]);
    assert_eq!(m.profiler.calls(), [args(&PROFILER_ARGS)]);
}

/// Proves each failure of a program (a non-zero exit, a signal, output it cannot read, a
/// program that does not run) makes the source `unreadable` with the first failure's reason,
/// while the other program's output still gives the capacity; both failing give none.
#[test]
fn each_failure_of_a_program_is_unreadable_and_the_other_still_counts() {
    let cases: [(Option<LaunchctlOutput>, Option<LaunchctlOutput>, &str, bool); 6] = [
        (
            Some(LaunchctlOutput::exit(3, "no")),
            None,
            "sysctl exited 3",
            true,
        ),
        (
            None,
            Some(LaunchctlOutput {
                code: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
            }),
            "system_profiler was ended by a signal",
            true,
        ),
        (
            Some(LaunchctlOutput::success("12\n")),
            None,
            "sysctl printed no three numbers",
            true,
        ),
        (
            None,
            Some(LaunchctlOutput::success("{\"SPHardwareDataType\":")),
            "system_profiler printed no JSON",
            true,
        ),
        (
            Some(LaunchctlOutput::exit(1, "")),
            Some(LaunchctlOutput::exit(2, "")),
            "sysctl exited 1",
            false,
        ),
        (
            Some(LaunchctlOutput::success("1\n2\n3\n")),
            Some(LaunchctlOutput::success("{}")),
            "",
            true,
        ),
    ];
    for (n, (sysctl, profiler, reason, some)) in cases.into_iter().enumerate() {
        let dir = Scratch::new(&format!("mac-capacity-fail-{n}"));
        let m = mac(&dir, Some("boot-2"), BOOT, FakeReports::default());
        if let Some(answer) = sysctl {
            m.sysctl.then(&args(&CAPACITY_SYSCTL_ARGS), Ok(answer));
        }
        if let Some(answer) = profiler {
            m.profiler.then(&args(&PROFILER_ARGS), Ok(answer));
        }
        m.poll(&mut m.capacity());
        let health = m.health();
        assert_eq!(health.baseline.is_some(), some, "{reason}");
        let expected = if reason.is_empty() {
            ok(CAPACITY_SOURCE)
        } else {
            state(CAPACITY_SOURCE, pb::SourceStatus::Unreadable, reason)
        };
        assert_eq!(source(&health, CAPACITY_SOURCE), Some(expected), "{reason}");
    }
    let dir = Scratch::new("mac-capacity-spawn");
    let m = mac(&dir, Some("boot-2"), BOOT, FakeReports::default());
    let mut silent = CapacitySource::new(
        Arc::clone(&m.keeper),
        Box::new(FakeLaunchctl::new()),
        Box::new(FakeLaunchctl::new()),
    );
    m.poll(&mut silent);
    let read = source(&m.health(), CAPACITY_SOURCE).expect("a state");
    assert_eq!(read.status, i32::from(pb::SourceStatus::Unreadable));
    assert!(
        read.reason.starts_with("sysctl did not run: "),
        "{}",
        read.reason
    );
    assert_eq!(m.health().baseline, None);
}

/// A sleeper that runs a step at each wait and says what it returns.
struct Steps(
    Mutex<VecDeque<Box<dyn FnOnce() -> Tick + Send>>>,
    Mutex<Vec<u64>>,
);

impl Sleeper for Steps {
    fn sleep(&self, secs: u64) -> Tick {
        lock(&self.1).push(secs);
        lock(&self.0).pop_front().map_or(Tick::Stop, |step| step())
    }
}

/// Proves the panic source's period: read at the start; again after `JUDGE_AFTER_SECS`, when the
/// restart is judged, merged with a panic report macOS wrote only after the start; then every
/// `PANIC_PERIOD_SECS` until stopped, each report read once.
#[test]
fn the_panic_source_judges_the_restart_at_its_second_read_then_reads_every_five_minutes() {
    let dir = Scratch::new("mac-schedule");
    let reports = FakeReports::default();
    let m = after_unclean_boot(&dir, reports.clone());
    let later = reports.clone();
    let steps = Steps(
        Mutex::new(VecDeque::from([
            Box::new(move || {
                later.add("serror.panic", &fixture("serror.panic"), MODIFIED);
                Tick::Beat
            }) as Box<dyn FnOnce() -> Tick + Send>,
            Box::new(|| Tick::Beat),
            Box::new(|| Tick::Beat),
        ])),
        Mutex::new(Vec::new()),
    );
    let mut panics = m.panics();
    run(&mut panics, &m.keeper, m.clock.as_ref(), &steps);
    assert_eq!(
        *lock(&steps.1),
        [
            JUDGE_AFTER_SECS,
            PANIC_PERIOD_SECS,
            PANIC_PERIOD_SECS,
            PANIC_PERIOD_SECS
        ]
    );
    let health = m.health();
    assert_eq!(
        ids(&health),
        [
            "panic:2b6f9c1e-7d1a-4c3e-9f20-6a1d0e5b8c44",
            "shutdown:boot-2"
        ]
    );
    assert_eq!(
        health.events[1].message,
        "Unexpected restart after a kernel panic (SError)"
    );
    assert_eq!(reports.reads(), ["serror.panic"], "read once in five reads");
}

/// Proves a stop during the first wait ends the panic source after its first read, with no
/// restart judged; a panic read at the first read is merged at the second.
#[test]
fn the_restart_is_judged_only_at_the_second_read() {
    let dir = Scratch::new("mac-schedule-stop");
    let m = after_unclean_boot(&dir, FakeReports::with(&["serror.panic"]));
    let sleeper = ScriptedSleeper::new(0);
    let mut panics = m.panics();
    run(&mut panics, &m.keeper, m.clock.as_ref(), &sleeper);
    assert_eq!(sleeper.waits(), [JUDGE_AFTER_SECS]);
    assert_eq!(
        ids(&m.health()),
        ["panic:2b6f9c1e-7d1a-4c3e-9f20-6a1d0e5b8c44"]
    );
    m.poll(&mut panics);
    assert_eq!(
        m.health().events[1].message,
        "Unexpected restart after a kernel panic (SError)"
    );
}

/// Proves the capacity's period: read at the start, again after `CAPACITY_AGAIN_SECS`, then every
/// `CAPACITY_PERIOD_SECS` until stopped.
#[test]
fn the_capacity_is_read_at_start_after_two_minutes_then_daily() {
    let dir = Scratch::new("mac-schedule-capacity");
    let m = mac(&dir, Some("boot-2"), BOOT, FakeReports::default());
    let sleeper = ScriptedSleeper::new(2);
    run(&mut m.capacity(), &m.keeper, m.clock.as_ref(), &sleeper);
    assert_eq!(
        sleeper.waits(),
        [
            CAPACITY_AGAIN_SECS,
            CAPACITY_PERIOD_SECS,
            CAPACITY_PERIOD_SECS
        ]
    );
    assert_eq!(m.sysctl.calls().len(), 3);
}

/// Proves a Mac's sources are its panic reports and its capacity, and no other host has any of
/// them. Making them reads nothing.
#[test]
fn a_mac_has_its_panic_and_capacity_sources_and_no_other_host_does() {
    let dir = Scratch::new("mac-for-host");
    let m = mac(&dir, Some("boot-2"), BOOT, FakeReports::default());
    let names = |kind| -> Vec<&'static str> {
        for_host(kind, &m.keeper).iter().map(|s| s.name()).collect()
    };
    assert_eq!(names(PlatformKind::Macos), [PANIC_SOURCE, CAPACITY_SOURCE]);
    assert!(names(PlatformKind::Linux).is_empty());
    assert!(names(PlatformKind::Openrc).is_empty());
    assert!(m.health().sources.is_empty(), "nothing read");
}

/// Proves the host's directory port lists regular files only (no directory such as `Retired`, no
/// link), with their times, and reads at most the bytes asked; a directory or file that is not
/// there is `NotFound`.
#[test]
fn the_host_directory_lists_regular_files_and_reads_to_its_cap() {
    let dir = Scratch::new("mac-report-dir");
    fs::write(dir.path().join("a.panic"), b"0123456789").expect("write");
    fs::create_dir(dir.path().join("Retired")).expect("mkdir");
    fs::write(dir.path().join("Retired/b.panic"), b"x").expect("write");
    std::os::unix::fs::symlink(dir.path().join("a.panic"), dir.path().join("link.panic"))
        .expect("symlink");
    let host = HostReportDir::at(dir.path());
    let files = host.list().expect("listed");
    assert_eq!(
        files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["a.panic"]
    );
    assert!(files[0].modified_unix > 0);
    assert_eq!(host.read("a.panic", 4).expect("read"), b"0123");
    assert_eq!(host.read("a.panic", 64).expect("read"), b"0123456789");
    assert_eq!(
        host.read("none.panic", 4).map_err(|e| e.kind()),
        Err(io::ErrorKind::NotFound)
    );
    let gone = HostReportDir::at(&dir.path().join("gone"));
    assert_eq!(
        gone.list().map_err(|e| e.kind()),
        Err(io::ErrorKind::NotFound)
    );
}

/// Proves, on a Mac (release.yml's `macos-test`, as the runner's user, no root), that the Mac's
/// own `sysctl` and `system_profiler` give a capacity the parsers read, with the memory and CPUs
/// of `hw.memsize` and `hw.physicalcpu`.
#[cfg(target_os = "macos")]
#[test]
fn the_macs_own_programs_give_its_capacity() {
    let dir = Scratch::new("mac-own");
    let m = mac(&dir, Some("boot-2"), BOOT, FakeReports::default());
    let mut capacity = for_host(PlatformKind::Macos, &m.keeper)
        .into_iter()
        .find(|s| s.name() == CAPACITY_SOURCE)
        .expect("the capacity source");
    m.poll(capacity.as_mut());
    let read = m.health();
    assert_eq!(source(&read, CAPACITY_SOURCE), Some(ok(CAPACITY_SOURCE)));
    let baseline = read.baseline.expect("a capacity");
    assert!(
        baseline.mem_total_kib > 0 && baseline.cpus_present > 0,
        "{baseline:?}"
    );
}
