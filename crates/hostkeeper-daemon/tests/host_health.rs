//! The daemon's hardware health (`hostkeeper_daemon::host_health` over the state file of
//! `hostkeeper_daemon::health_state`): the clean-shutdown marker across an orderly stop, a kill,
//! a restart of the daemon and a reboot; the records held across restarts until written; the
//! first read's history; the file moved aside, cut short by a crash, or not writable; the boot
//! read from a scratch root and a fake `sysctl`. Each daemon start is a `HealthKeeper::open` over
//! the same scratch state directory, with no real `/proc`, `/etc` or program.

mod support;

use std::fs;
use std::sync::Arc;

use hostkeeper_daemon::health_state::{
    self, FOLDED_CODE, HEALTH_FILE, HEALTH_FILE_ASIDE, HealthFile, MAX_HELD, Marker, Queue, Read,
    Refused,
};
use hostkeeper_daemon::host_health::{
    BootFacts, HISTORY_PREFIX, HISTORY_SECS, HealthKeeper, MACHINE_ID, PROC_STAT,
};
use hostkeeper_daemon::shutdown_probe::{GoingDown, ShutdownProbe};
use hostkeeper_daemon::testing::FakeClock;
use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput, PlatformKind};
use hostkeeper_link::HostHealthSource;
use hostkeeper_link::testing::RecordingLog;
use hostkeeper_proto::health::{
    MAX_EVENTS, MAX_MESSAGE, MAX_RAW, MAX_SOURCE_REASON, MAX_SOURCES, MAX_SUBJECT, check_health,
};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use support::Scratch;

/// The second the tests read at.
const NOW: u64 = 1_795_662_000;

/// A probe that answers what the test says.
struct Says(GoingDown);

impl ShutdownProbe for Says {
    fn going_down(&self) -> GoingDown {
        self.0.clone()
    }
}

fn boot(id: &str) -> BootFacts {
    BootFacts {
        boot_id: Some(id.to_owned()),
        boot_unix: 1_795_000_000,
        machine_id: "m-1".to_owned(),
    }
}

/// A daemon's start in `boot`, over `dir`, its probe answering `probe`.
fn start(dir: &Scratch, boot: BootFacts, probe: GoingDown) -> (HealthKeeper, Arc<RecordingLog>) {
    let log = Arc::new(RecordingLog::default());
    let keeper = HealthKeeper::open(dir.path(), boot, Box::new(Says(probe)), log.clone());
    (keeper, log)
}

fn yes() -> GoingDown {
    GoingDown::Yes("the system is stopping".to_owned())
}

fn on_file(dir: &Scratch) -> HealthFile {
    match health_state::read(dir.path(), "") {
        Read::Read(file) => file,
        other => panic!("no state file: {other:?}"),
    }
}

fn event(
    id: &str,
    severity: pb::HardwareSeverity,
    time_unix: u64,
    source: &str,
) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id: id.to_owned(),
        time_unix,
        severity: severity.into(),
        state: pb::HardwareEventState::Event.into(),
        source: source.to_owned(),
        code: "memory_uncorrectable".to_owned(),
        message: format!("record {id}"),
        ..pb::HardwareEvent::default()
    }
}

fn ids(health: &pb::HostHealth) -> Vec<String> {
    health.events.iter().map(|e| e.event_id.clone()).collect()
}

/// Proves the first start on a host writes the marker `running` with the boot and machine, and
/// reports the boot with no clean shutdown before it and nothing held.
///
/// Catches: a first start reported as following a clean shutdown, or no file written at start.
#[test]
fn a_first_start_writes_running_and_knows_no_clean_shutdown_before() {
    let dir = Scratch::new("health-first");
    let (keeper, log) = start(&dir, boot("boot-1"), yes());
    let file = on_file(&dir);
    assert_eq!(
        (
            file.marker,
            file.boot_id.as_str(),
            file.machine_id.as_str(),
            file.boot_unix
        ),
        (Marker::Running, "boot-1", "m-1", 1_795_000_000)
    );
    let health = keeper.health();
    assert_eq!(
        health,
        pb::HostHealth {
            boot_id: "boot-1".to_owned(),
            boot_unix: 1_795_000_000,
            clean_previous_shutdown: false,
            ..pb::HostHealth::default()
        }
    );
    assert!(log.lines().is_empty());
}

/// Proves an orderly stop (SIGTERM while the system goes down) writes `clean`, and the next boot
/// reports its previous shutdown clean and writes `running` again.
///
/// Catches: the marker not written at an orderly stop, or not cleared at the next start, so a
/// crash after it would read as clean.
#[test]
fn an_orderly_stop_leaves_the_marker_and_the_next_boot_reads_it_clean() {
    let dir = Scratch::new("health-orderly");
    let (keeper, log) = start(&dir, boot("boot-1"), yes());
    assert_eq!(keeper.stopping(), yes());
    assert_eq!(on_file(&dir).marker, Marker::Clean);
    assert_eq!(
        log.lines(),
        ["the clean-shutdown marker is written: the system is stopping"]
    );
    drop(keeper);
    let (next, _) = start(&dir, boot("boot-2"), yes());
    assert!(next.health().clean_previous_shutdown);
    assert_eq!(next.health().boot_id, "boot-2");
    let file = on_file(&dir);
    assert_eq!((file.marker, file.previous_clean), (Marker::Running, true));
}

/// Proves a daemon killed with no SIGTERM (a kill -9, a crash, a power loss) leaves `running`,
/// and the next boot reports its previous shutdown unclean; so does a boot after a clean one,
/// once the daemon of that boot was killed.
///
/// Catches: a marker left `clean` from an earlier boot, so a crash reads as a clean reboot.
#[test]
fn a_killed_daemon_leaves_no_marker_and_the_next_boot_reads_it_unclean() {
    let dir = Scratch::new("health-killed");
    let (first, _) = start(&dir, boot("boot-1"), yes());
    first.stopping();
    drop(first);
    let (second, _) = start(&dir, boot("boot-2"), yes());
    assert!(second.health().clean_previous_shutdown);
    assert_eq!(on_file(&dir).marker, Marker::Running);
    drop(second);
    let (third, _) = start(&dir, boot("boot-3"), yes());
    assert!(!third.health().clean_previous_shutdown);
}

/// Proves a SIGTERM while the system is not going down (a restart of the daemon or of its unit),
/// or one the probe cannot judge, leaves `running`, logged; the next boot reads unclean.
///
/// Catches: every SIGTERM read as an orderly shutdown, so a later crash reads as clean.
#[test]
fn a_sigterm_of_a_restart_or_one_not_judged_leaves_running() {
    for (probe, line) in [
        (
            GoingDown::No("the system is running".to_owned()),
            "the clean-shutdown marker is not written: the system is running",
        ),
        (
            GoingDown::Unknown("systemctl did not run".to_owned()),
            "error: the clean-shutdown marker is not written: systemctl did not run",
        ),
    ] {
        let dir = Scratch::new("health-restart");
        let (keeper, log) = start(&dir, boot("boot-1"), probe.clone());
        assert_eq!(keeper.stopping(), probe);
        assert_eq!(log.lines(), [line]);
        assert_eq!(on_file(&dir).marker, Marker::Running);
        drop(keeper);
        let (next, _) = start(&dir, boot("boot-2"), yes());
        assert!(!next.health().clean_previous_shutdown);
    }
}

/// Proves a restart of the daemon in the same boot keeps what the boot's first start found, a
/// clean shutdown before it or not, whatever marker the restart left.
///
/// Catches: a restart in the boot read as a new boot, so a clean reboot turns unclean (or the
/// other way) once the daemon restarts.
#[test]
fn a_restart_in_the_same_boot_keeps_what_its_first_start_found() {
    let dir = Scratch::new("health-same-boot");
    let (first, _) = start(&dir, boot("boot-1"), yes());
    first.stopping();
    drop(first);
    let (second, _) = start(&dir, boot("boot-2"), GoingDown::No("running".to_owned()));
    second.stopping();
    drop(second);
    let (third, _) = start(&dir, boot("boot-2"), yes());
    assert!(third.health().clean_previous_shutdown);
    third.stopping();
    drop(third);
    let (fourth, _) = start(&dir, boot("boot-2"), yes());
    assert!(fourth.health().clean_previous_shutdown);
}

/// Proves a boot id that cannot be read reports no boot and no clean shutdown, keeps the boot
/// the file last named, and the start after it in that boot judges from the file as written.
///
/// Catches: an unreadable boot id written as the boot, or read as a new boot with a clean one
/// before it.
#[test]
fn an_unreadable_boot_id_reports_no_boot_and_keeps_the_last_one() {
    let dir = Scratch::new("health-no-boot-id");
    let (first, _) = start(&dir, boot("boot-1"), yes());
    first.stopping();
    drop(first);
    let unread = BootFacts {
        boot_id: None,
        ..boot("")
    };
    let (second, _) = start(&dir, unread, yes());
    let health = second.health();
    assert_eq!(
        (health.boot_id.as_str(), health.clean_previous_shutdown),
        ("", false)
    );
    assert_eq!(on_file(&dir).boot_id, "boot-1");
}

/// Proves records held survive restarts of the daemon, are sent the most severe first, then the
/// oldest, at most six a heartbeat, leave only when the coordinator names them written (the file
/// written then), and an id written that is not held changes nothing.
///
/// Catches: held records lost at a restart, dropped before they are written, or sent in the wrong
/// order.
#[test]
fn held_records_survive_restarts_and_leave_only_when_written() {
    let dir = Scratch::new("health-held");
    let (keeper, _) = start(&dir, boot("boot-1"), yes());
    let records = vec![
        event("i-old", pb::HardwareSeverity::Info, NOW - 30, "sel"),
        event("w-new", pb::HardwareSeverity::Warning, NOW - 10, "sel"),
        event("c-new", pb::HardwareSeverity::Critical, NOW - 5, "sel"),
        event("c-old", pb::HardwareSeverity::Critical, NOW - 20, "sel"),
        event("w-old", pb::HardwareSeverity::Warning, NOW - 40, "sel"),
        event("i-new", pb::HardwareSeverity::Info, NOW - 1, "sel"),
        event("i-mid", pb::HardwareSeverity::Info, NOW - 15, "sel"),
    ];
    assert_eq!(keeper.record("sel", "7@1", records, NOW), 7);
    drop(keeper);
    let (again, _) = start(&dir, boot("boot-1"), yes());
    assert_eq!(
        ids(&again.health()),
        ["c-old", "c-new", "w-old", "w-new", "i-old", "i-mid"]
    );
    assert_eq!(again.mark("sel").as_deref(), Some("7@1"));
    again.written(&["c-old".to_owned(), "w-old".to_owned(), "none".to_owned()]);
    again.written(&["none".to_owned()]);
    drop(again);
    let (third, _) = start(&dir, boot("boot-2"), yes());
    assert_eq!(
        ids(&third.health()),
        ["c-new", "w-new", "i-old", "i-mid", "i-new"]
    );
    assert_eq!(third.mark("smart"), None);
}

/// Proves the first read of a source marks its records older than seven days history (`info`,
/// the message prefixed), and nothing else: not a newer record, not one with no time, not an old
/// record of a later read.
///
/// Catches: an old SEL or old panic reports raising alerts at a daemon's first run, or a real
/// new fault demoted.
#[test]
fn the_first_read_of_a_source_marks_old_records_history() {
    let dir = Scratch::new("health-history");
    let (keeper, _) = start(&dir, boot("boot-1"), yes());
    let old = NOW - HISTORY_SECS - 1;
    let edge = NOW - HISTORY_SECS;
    keeper.record(
        "sel",
        "a",
        vec![
            event("old", pb::HardwareSeverity::Critical, old, "sel"),
            event("edge", pb::HardwareSeverity::Critical, edge, "sel"),
            event("timeless", pb::HardwareSeverity::Critical, 0, "sel"),
        ],
        NOW,
    );
    keeper.record(
        "sel",
        "b",
        vec![event(
            "later-old",
            pb::HardwareSeverity::Critical,
            old,
            "sel",
        )],
        NOW,
    );
    let sent = keeper.health().events;
    let by_id = |id: &str| {
        sent.iter()
            .find(|e| e.event_id == id)
            .cloned()
            .expect("the record")
    };
    let history = by_id("old");
    assert_eq!(history.severity, i32::from(pb::HardwareSeverity::Info));
    assert_eq!(history.message, format!("{HISTORY_PREFIX}record old"));
    for id in ["edge", "timeless", "later-old"] {
        let kept = by_id(id);
        assert_eq!(
            (kept.severity, kept.message),
            (
                i32::from(pb::HardwareSeverity::Critical),
                format!("record {id}")
            )
        );
    }
}

/// Proves a history record's message is cut to its bound at a character boundary.
///
/// Catches: a history prefix pushing a message past 120 bytes, which the coordinator refuses
/// with the whole health.
#[test]
fn a_history_message_is_cut_to_its_bound() {
    let dir = Scratch::new("health-history-cut");
    let (keeper, _) = start(&dir, boot("boot-1"), yes());
    let mut long = event("long", pb::HardwareSeverity::Warning, 1, "sel");
    long.message = "é".repeat(60);
    keeper.record("sel", "", vec![long], NOW);
    let message = &keeper.health().events[0].message;
    assert!(message.starts_with(HISTORY_PREFIX));
    assert_eq!(message.len(), MAX_MESSAGE - 1);
}

/// Proves a record held twice is held once, a record whose `event_id` is empty or past 64 bytes
/// is refused and logged, and each text is cut to its bound at a character boundary.
///
/// Catches: a record read twice sent twice, or a record past a bound held, which would make every
/// later heartbeat's health refused whole.
#[test]
fn records_are_held_once_refused_without_an_id_and_cut_to_their_bounds() {
    let dir = Scratch::new("health-bounds");
    let (keeper, log) = start(&dir, boot("boot-1"), yes());
    let mut big = event("big", pb::HardwareSeverity::Warning, NOW, "sel");
    big.subject = "ü".repeat(20);
    big.raw = "r".repeat(300);
    let held = keeper.record(
        "sel",
        "",
        vec![
            big.clone(),
            big,
            event("", pb::HardwareSeverity::Critical, NOW, "sel"),
            event(&"x".repeat(65), pb::HardwareSeverity::Critical, NOW, "sel"),
        ],
        NOW,
    );
    assert_eq!(held, 1);
    let sent = keeper.health().events;
    assert_eq!(ids(&keeper.health()), ["big"]);
    assert_eq!(sent[0].subject, "ü".repeat(MAX_SUBJECT / 2));
    assert_eq!(sent[0].raw.len(), MAX_RAW);
    assert_eq!(
        log.lines(),
        [
            "error: a record of sel is not held: its event_id is 0 bytes, not 1 to 64",
            "error: a record of sel is not held: its event_id is 65 bytes, not 1 to 64",
        ]
    );
    assert_eq!(
        Queue::default().hold(event("", pb::HardwareSeverity::Info, 0, "a")),
        Err(Refused::EventId { len: 0 })
    );
}

/// Proves past 256 records the `info` records of the source of the oldest `info` record that
/// has two or more are folded into one `info` record that counts them, the others kept whole.
///
/// Catches: a queue that grows without bound on corrected errors, or that folds the wrong ones.
#[test]
fn past_256_the_info_records_of_one_source_are_folded_into_one() {
    let mut queue = Queue::default();
    for n in 0..250u64 {
        queue
            .hold(event(
                &format!("w{n}"),
                pb::HardwareSeverity::Warning,
                n,
                "sel",
            ))
            .expect("held");
    }
    for n in 0..6u64 {
        queue
            .hold(event(
                &format!("a{n}"),
                pb::HardwareSeverity::Info,
                1000 + n,
                "rasdaemon",
            ))
            .expect("held");
    }
    queue
        .hold(event("b0", pb::HardwareSeverity::Info, 10, "journal"))
        .expect("held");
    assert_eq!(queue.records.len(), MAX_HELD - 4);
    let folded: Vec<_> = queue
        .records
        .iter()
        .filter(|r| r.code == FOLDED_CODE)
        .collect();
    assert_eq!(folded.len(), 1);
    let fold = folded[0];
    assert_eq!(
        (
            fold.count,
            fold.time_unix,
            fold.severity,
            fold.source.as_str()
        ),
        (6, 1000, i32::from(pb::HardwareSeverity::Info), "rasdaemon")
    );
    assert!(fold.event_id.starts_with("fold:rasdaemon:1000:"));
    assert_eq!(
        fold.message,
        "6 info records of rasdaemon folded into one while held"
    );
    assert!(queue.records.iter().any(|r| r.event_id == "b0"));
    assert_eq!(
        queue
            .records
            .iter()
            .filter(|r| r.event_id.starts_with('w'))
            .count(),
        250
    );
}

/// Proves a fold is folded again with the records that follow it, its count carried, and warning
/// and critical records are never folded or dropped, even past 256 with no `info` record to fold.
///
/// Catches: a fold's count lost when it is folded again, or a warning or critical record lost.
#[test]
fn folds_add_up_and_warnings_and_criticals_are_never_folded() {
    let mut queue = Queue::default();
    for n in 0..MAX_HELD as u64 {
        queue
            .hold(event(
                &format!("i{n}"),
                pb::HardwareSeverity::Info,
                n,
                "sel",
            ))
            .expect("held");
    }
    queue
        .hold(event("i-more", pb::HardwareSeverity::Info, 999, "sel"))
        .expect("held");
    assert_eq!(queue.records.len(), 1);
    assert_eq!(queue.records[0].count, MAX_HELD as u64 + 1);
    let mut hard = Queue::default();
    for n in 0..=MAX_HELD as u64 {
        let severity = if n % 2 == 0 {
            pb::HardwareSeverity::Warning
        } else {
            pb::HardwareSeverity::Critical
        };
        hard.hold(event(&format!("h{n}"), severity, n, "sel"))
            .expect("held");
    }
    assert_eq!(hard.records.len(), MAX_HELD + 1);
    assert!(hard.records.iter().all(|r| r.count == 1));
}

/// Proves the health of a heartbeat holds six records, the oldest, of eight held with every
/// text at its bound and the boot id at its bound, and keeps its bound of 4096 bytes encoded; the
/// two newest wait for a later heartbeat.
///
/// Catches: a health over its bound, which the coordinator refuses whole, every heartbeat.
#[test]
fn six_records_at_their_bounds_keep_the_health_within_its_bound() {
    let dir = Scratch::new("health-fit");
    let (keeper, _) = start(&dir, boot(&"b".repeat(64)), yes());
    let full = |n: u64| pb::HardwareEvent {
        event_id: format!("{n:0>64}"),
        subject: "s".repeat(32),
        source: "s".repeat(16),
        code: "c".repeat(32),
        message: "m".repeat(120),
        raw: "r".repeat(256),
        about: "a".repeat(64),
        ..event("", pb::HardwareSeverity::Warning, n, "sel")
    };
    keeper.record("sel", "", (1..=8).map(full).collect(), NOW);
    let health = keeper.health();
    assert_eq!(check_health(&health), Ok(()));
    let sent: Vec<u64> = health.events.iter().map(|e| e.time_unix).collect();
    assert_eq!(sent, (1..=MAX_EVENTS as u64).collect::<Vec<u64>>());
}

/// Proves each source's state rides in every heartbeat, by source, the last one it said in place
/// of the one before, its source and reason cut to their bounds; and the keeper gives the boot's
/// start to the sources that judge a record before it.
///
/// Catches: a source that reports nothing read as one that found nothing wrong (no state sent),
/// a source listed twice, or a reason past its bound refusing the whole health.
#[test]
fn each_source_says_its_state_in_every_heartbeat() {
    let dir = Scratch::new("health-sources");
    let (keeper, log) = start(&dir, boot("boot-1"), yes());
    assert_eq!(keeper.boot_unix(), 1_795_000_000);
    let state = |source: &str, status: pb::SourceStatus, reason: &str| pb::SourceState {
        source: source.to_owned(),
        status: status.into(),
        reason: reason.to_owned(),
    };
    keeper.source_state(state("sel", pb::SourceStatus::Ok, ""));
    keeper.source_state(state(
        "rasdaemon-and-more",
        pb::SourceStatus::Unsupported,
        "no database",
    ));
    keeper.source_state(state(
        "sel",
        pb::SourceStatus::Unreadable,
        &"the BMC does not answer ".repeat(4),
    ));
    let health = keeper.health();
    assert_eq!(
        health.sources,
        [
            state(
                "rasdaemon-and-mo",
                pb::SourceStatus::Unsupported,
                "no database"
            ),
            state(
                "sel",
                pb::SourceStatus::Unreadable,
                &"the BMC does not answer ".repeat(4)[..MAX_SOURCE_REASON]
            ),
        ]
    );
    assert_eq!(check_health(&health), Ok(()));
    assert!(log.lines().is_empty(), "{:?}", log.lines());
}

/// Proves the keeper keeps at most twelve sources' states: a thirteenth is logged and not kept,
/// and a source already kept still says its new state.
///
/// Catches: a health with more sources than its bound, which the coordinator refuses whole.
#[test]
fn past_twelve_sources_a_new_one_is_logged_and_not_kept() {
    let dir = Scratch::new("health-sources-bound");
    let (keeper, log) = start(&dir, boot("boot-1"), yes());
    let state = |n: usize, status: pb::SourceStatus| pb::SourceState {
        source: format!("source-{n:02}"),
        status: status.into(),
        reason: String::new(),
    };
    for n in 0..=MAX_SOURCES {
        keeper.source_state(state(n, pb::SourceStatus::Ok));
    }
    keeper.source_state(state(0, pb::SourceStatus::Unreadable));
    let sources = keeper.health().sources;
    assert_eq!(sources.len(), MAX_SOURCES);
    assert_eq!(sources[0], state(0, pb::SourceStatus::Unreadable));
    assert_eq!(
        log.lines(),
        ["error: the state of the source source-12 is not kept: 12 sources are"]
    );
}

/// Proves a heartbeat whose six records at their bounds and twelve sources at theirs would pass
/// the health's 4096 bytes carries fewer records, the most severe and oldest first, and the
/// health keeps its bound; the records left wait in the queue.
///
/// Catches: a health over its bound once sources report, which the coordinator refuses whole at
/// every heartbeat, so no record would ever be written.
#[test]
fn sources_at_their_bound_leave_records_for_a_later_heartbeat() {
    let dir = Scratch::new("health-sources-fit");
    let (keeper, _) = start(&dir, boot(&"b".repeat(64)), yes());
    let full = |n: u64| pb::HardwareEvent {
        event_id: format!("{n:0>64}"),
        subject: "s".repeat(32),
        source: "s".repeat(16),
        code: "c".repeat(32),
        message: "m".repeat(120),
        raw: "r".repeat(256),
        about: "a".repeat(64),
        ..event("", pb::HardwareSeverity::Warning, n, "sel")
    };
    keeper.record("sel", "", (1..=8).map(full).collect(), NOW);
    for n in 0..MAX_SOURCES {
        keeper.source_state(pb::SourceState {
            source: format!("{n:s>16}"),
            status: pb::SourceStatus::Unreadable.into(),
            reason: "r".repeat(64),
        });
    }
    let health = keeper.health();
    assert_eq!(check_health(&health), Ok(()));
    assert_eq!(health.sources.len(), MAX_SOURCES);
    let sent: Vec<u64> = health.events.iter().map(|e| e.time_unix).collect();
    assert!(!sent.is_empty() && sent.len() < MAX_EVENTS, "{sent:?}");
    assert_eq!(sent, (1..=sent.len() as u64).collect::<Vec<u64>>());
    assert_eq!(on_file(&dir).held.records.len(), 8);
}

/// Proves a state file that does not parse, or that another machine wrote, is moved aside and
/// read as absent, and a file of no machine id is read by any machine.
///
/// Catches: a cloned state directory's records sent as this host's, or a corrupt file failing
/// the start.
#[test]
fn a_corrupt_or_foreign_file_is_moved_aside() {
    let dir = Scratch::new("health-aside");
    fs::write(dir.path().join(HEALTH_FILE), b"{not json").expect("write");
    let (keeper, log) = start(&dir, boot("boot-1"), yes());
    assert!(!keeper.health().clean_previous_shutdown);
    assert!(dir.path().join(HEALTH_FILE_ASIDE).exists());
    let lines = log.lines();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].starts_with(&format!(
        "error: the health state file was moved aside to {HEALTH_FILE_ASIDE}: it does not parse: "
    )));
    keeper.stopping();
    drop(keeper);
    let other = BootFacts {
        machine_id: "m-2".to_owned(),
        ..boot("boot-2")
    };
    let (moved, log) = start(&dir, other, yes());
    assert!(!moved.health().clean_previous_shutdown);
    assert_eq!(
        log.lines(),
        [format!(
            "error: the health state file was moved aside to {HEALTH_FILE_ASIDE}: machine m-1 \
             wrote it, not this one"
        )]
    );
    moved.stopping();
    drop(moved);
    let unkeyed = BootFacts {
        machine_id: String::new(),
        ..boot("boot-3")
    };
    let (adopted, log) = start(&dir, unkeyed, yes());
    assert!(adopted.health().clean_previous_shutdown);
    assert!(log.lines().is_empty());
    assert_eq!(on_file(&dir).machine_id, "m-2");
}

/// Proves a state file that cannot be read is moved aside, and one that cannot be moved aside is
/// said so; the start goes on either way.
///
/// Catches: a start that fails, or says nothing, on a state file it cannot use.
#[test]
fn a_file_that_cannot_be_read_or_moved_is_said_so() {
    let dir = Scratch::new("health-unreadable");
    fs::create_dir(dir.path().join(HEALTH_FILE)).expect("a directory where the file goes");
    let (_, log) = start(&dir, boot("boot-1"), yes());
    assert!(log.lines()[0].contains("it cannot be read: "));
    let stuck = Scratch::new("health-stuck");
    fs::write(stuck.path().join(HEALTH_FILE), b"{not json").expect("write");
    fs::create_dir(stuck.path().join(HEALTH_FILE_ASIDE)).expect("a directory");
    fs::write(stuck.path().join(HEALTH_FILE_ASIDE).join("x"), b"x").expect("write");
    let (_, log) = start(&stuck, boot("boot-1"), yes());
    let lines = log.lines();
    assert!(lines[0].starts_with("error: the health state file is not read: it does not parse: "));
    assert!(lines[0].contains("; it could not be moved aside: "));
}

/// Proves a crash in the middle of a write keeps the file as it was before it, and the records
/// it held.
///
/// Catches: a write that is not whole, which a crash would leave half written.
#[test]
fn a_crash_mid_write_keeps_the_old_file() {
    let dir = Scratch::new("health-crash");
    let (keeper, _) = start(&dir, boot("boot-1"), yes());
    keeper.record(
        "sel",
        "",
        vec![event("kept", pb::HardwareSeverity::Critical, NOW, "sel")],
        NOW,
    );
    let before = on_file(&dir);
    let mut changed = before.clone();
    changed.held = Queue::default();
    let stopped = health_state::write(dir.path(), &changed, &|_| true);
    assert!(stopped.is_err());
    assert_eq!(on_file(&dir), before);
}

/// Proves a state file that cannot be written is logged, and what the daemon holds is still sent.
///
/// Catches: records lost, or a heartbeat failed, when the state directory cannot be written.
#[test]
fn a_file_that_cannot_be_written_is_logged_and_the_records_still_sent() {
    let dir = Scratch::new("health-unwritable");
    let gone = dir.path().join("gone");
    let log = Arc::new(RecordingLog::default());
    let keeper = HealthKeeper::open(&gone, boot("boot-1"), Box::new(Says(yes())), log.clone());
    keeper.record(
        "sel",
        "",
        vec![event("held", pb::HardwareSeverity::Critical, NOW, "sel")],
        NOW,
    );
    assert_eq!(ids(&keeper.health()), ["held"]);
    let lines = log.lines();
    assert_eq!(lines.len(), 2);
    assert!(
        lines
            .iter()
            .all(|l| l.starts_with("error: the health state file is not written: "))
    );
}

/// Proves a file of an earlier or later release reads: a field it lacks takes its default, and
/// one this release does not know is passed over.
///
/// Catches: a downgrade or an upgrade that loses the held records and the marker.
#[test]
fn a_file_with_fields_missing_or_unknown_reads() {
    let dir = Scratch::new("health-fields");
    fs::write(
        dir.path().join(HEALTH_FILE),
        br#"{"boot_id":"boot-1","marker":"clean","later":{"x":1}}"#,
    )
    .expect("write");
    let (keeper, log) = start(&dir, boot("boot-2"), yes());
    assert!(keeper.health().clean_previous_shutdown);
    assert!(log.lines().is_empty());
}

/// Proves a marker this release does not know (a later release's) reads as not clean, and the
/// records the file holds are kept rather than the file moved aside.
///
/// Catches: a file of a later release moved aside whole, its held records lost, for one new
/// marker value.
#[test]
fn an_unknown_marker_reads_as_not_clean_and_keeps_the_records() {
    let dir = Scratch::new("health-unknown-marker");
    fs::write(
        dir.path().join(HEALTH_FILE),
        br#"{"boot_id":"boot-1","marker":"halted","held":{"records":[{"event_id":"kept","severity":3}],"next_seq":1}}"#,
    )
    .expect("write");
    let (keeper, log) = start(&dir, boot("boot-2"), yes());
    let health = keeper.health();
    assert!(!health.clean_previous_shutdown);
    assert_eq!(ids(&health), ["kept"]);
    assert!(log.lines().is_empty());
    assert!(!dir.path().join(HEALTH_FILE_ASIDE).exists());
}

/// Proves the boot of a Linux or OpenRC host is read from its root: the boot id of the clock,
/// `btime` of `/proc/stat` and `/etc/machine-id`; with neither file, 0 and no machine id.
///
/// Catches: the boot time or machine id read from the wrong file, or a missing one failing the
/// start.
#[test]
fn a_linux_boot_is_read_from_its_root() {
    let root = Scratch::new("health-root");
    fs::create_dir_all(root.path().join("proc")).expect("proc");
    fs::create_dir_all(root.path().join("etc")).expect("etc");
    fs::write(root.path().join(PROC_STAT), "cpu 1 2\nbtime 1795000000\n").expect("stat");
    fs::write(root.path().join(MACHINE_ID), "0123abcd\n").expect("machine-id");
    let clock = FakeClock::new();
    let sysctl = FakeLaunchctl::new();
    for kind in [PlatformKind::Linux, PlatformKind::Openrc] {
        assert_eq!(
            BootFacts::read(kind, root.path(), &*clock, &sysctl),
            BootFacts {
                boot_id: Some("boot-1".to_owned()),
                boot_unix: 1_795_000_000,
                machine_id: "0123abcd".to_owned(),
            }
        );
    }
    let empty = Scratch::new("health-root-empty");
    clock.lose_boot_id();
    assert_eq!(
        BootFacts::read(PlatformKind::Linux, empty.path(), &*clock, &sysctl),
        BootFacts::default()
    );
    assert!(sysctl.calls().is_empty());
}

/// Proves a Mac's boot time is the `sec` `sysctl -n kern.boottime` prints when it exits 0, and 0
/// when it exits otherwise or does not run; a Mac has no machine id.
///
/// Catches: a failed `sysctl`'s output read as the boot time.
#[test]
fn a_macs_boot_time_is_read_through_sysctl() {
    let root = Scratch::new("health-mac");
    let clock = FakeClock::new();
    let args: Vec<String> = ["-n", "kern.boottime"].map(str::to_owned).to_vec();
    let sysctl = FakeLaunchctl::new();
    sysctl.then(
        &args,
        Ok(LaunchctlOutput::success(
            "{ sec = 1795176765, usec = 482911 }",
        )),
    );
    sysctl.then(
        &args,
        Ok(LaunchctlOutput {
            code: Some(1),
            stdout: b"{ sec = 5, usec = 0 }".to_vec(),
            stderr: Vec::new(),
        }),
    );
    sysctl.then(
        &args,
        Err(LaunchctlError::TimedOut {
            args: args.join(" "),
        }),
    );
    let read = || BootFacts::read(PlatformKind::Macos, root.path(), &*clock, &sysctl);
    assert_eq!(
        read(),
        BootFacts {
            boot_id: Some("boot-1".to_owned()),
            boot_unix: 1_795_176_765,
            machine_id: String::new(),
        }
    );
    assert_eq!(read().boot_unix, 0);
    assert_eq!(read().boot_unix, 0);
}
