//! The daemon's hardware sources (`hostkeeper_daemon::hardware_sources`) and what the keeper
//! (`hostkeeper_daemon::host_health::HealthKeeper::read`) makes of their reads: a worker reads,
//! waits its period and stops when told; each source gets a worker of its own; the sources of each
//! kind of host; a source's state in every heartbeat, its reason and name cut to their bounds, at
//! most twelve; a read with no mark keeping the mark it had, and one with neither a mark nor a
//! record leaving no first read; and the health kept within its bound when the states crowd the
//! records. The sources are scripted fakes; no host file or program is read.

mod support;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use hostkeeper_daemon::hardware_sources::{
    HardwareSource, SourceRead, at_host, for_host, poll, run, start, start_host,
};
use hostkeeper_daemon::host_health::{BootFacts, HealthKeeper};
use hostkeeper_daemon::shutdown_probe::{GoingDown, ShutdownProbe};
use hostkeeper_launchd::PlatformKind;
use hostkeeper_link::testing::{FakeClock, ManualDispatch, RecordingLog, ScriptedSleeper};
use hostkeeper_link::{HostHealthSource, Sleeper, StopFlag, UnixClock};
use hostkeeper_proto::health::{
    MAX_CODE, MAX_EVENT_ID, MAX_EVENTS, MAX_MESSAGE, MAX_RAW, MAX_SOURCE, MAX_SOURCE_REASON,
    MAX_SOURCES, MAX_SUBJECT, check_health,
};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use hostkeeper_steps::Clock;
use support::Scratch;

/// The second the tests read at.
const NOW: u64 = 1_795_662_000;

/// The period of the scripted source.
const EVERY: u64 = 30;

/// A probe that never says the system is going down.
struct Staying;

impl ShutdownProbe for Staying {
    fn going_down(&self) -> GoingDown {
        GoingDown::No("the tests stay up".to_owned())
    }
}

fn keeper(dir: &Scratch) -> Arc<HealthKeeper> {
    Arc::new(HealthKeeper::open(
        dir.path(),
        BootFacts::default(),
        Box::new(Staying),
        Arc::new(RecordingLog::default()),
    ))
}

/// What the scripted source was asked: the mark and the second of each read.
type Asked = Arc<Mutex<Vec<(Option<String>, u64)>>>;

/// A source that answers its scripted reads in order, then reads `ok` with no record.
struct Scripted {
    name: &'static str,
    reads: VecDeque<SourceRead>,
    asked: Asked,
}

impl Scripted {
    fn new(name: &'static str, reads: Vec<SourceRead>) -> (Scripted, Asked) {
        let asked = Asked::default();
        let source = Scripted {
            name,
            reads: reads.into(),
            asked: Arc::clone(&asked),
        };
        (source, asked)
    }
}

impl HardwareSource for Scripted {
    fn name(&self) -> &'static str {
        self.name
    }

    fn every_secs(&self) -> u64 {
        EVERY
    }

    fn read(&mut self, mark: Option<&str>, now_unix: u64) -> SourceRead {
        self.asked
            .lock()
            .expect("asked")
            .push((mark.map(str::to_owned), now_unix));
        self.reads
            .pop_front()
            .unwrap_or_else(|| SourceRead::ok("steady".to_owned(), Vec::new()))
    }
}

fn event(id: &str, severity: pb::HardwareSeverity) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id: id.to_owned(),
        time_unix: NOW,
        severity: severity.into(),
        state: pb::HardwareEventState::Event.into(),
        source: "fake".to_owned(),
        code: "io_error".to_owned(),
        message: format!("record {id}"),
        ..pb::HardwareEvent::default()
    }
}

fn state(source: &str, status: pb::SourceStatus, reason: &str) -> pb::SourceState {
    pb::SourceState {
        source: source.to_owned(),
        status: status.into(),
        reason: reason.to_owned(),
    }
}

#[test]
fn a_worker_reads_waits_its_period_and_stops_when_told() {
    let dir = Scratch::new("sources-run");
    let keeper = keeper(&dir);
    let clock = FakeClock::new(NOW, 0);
    let (mut source, asked) = Scripted::new(
        "fake",
        vec![SourceRead::ok(
            "m1".to_owned(),
            vec![event("a", pb::HardwareSeverity::Warning)],
        )],
    );
    let sleeper = ScriptedSleeper::new(2);
    run(&mut source, &keeper, &clock, &sleeper);
    assert_eq!(sleeper.waits(), [EVERY, EVERY, EVERY]);
    assert_eq!(
        *asked.lock().expect("asked"),
        [
            (None, NOW),
            (Some("m1".to_owned()), NOW),
            (Some("steady".to_owned()), NOW)
        ]
    );
    assert_eq!(keeper.health().events.len(), 1);
}

#[test]
fn each_source_gets_a_worker_of_its_own() {
    let dir = Scratch::new("sources-start");
    let keeper = keeper(&dir);
    let clock: Arc<dyn UnixClock> = Arc::new(FakeClock::new(NOW, 0));
    let (first, first_asked) = Scripted::new("one", Vec::new());
    let (second, second_asked) =
        Scripted::new("two", vec![SourceRead::unreadable("timed out".to_owned())]);
    let dispatch = ManualDispatch::default();
    let sleeper = || -> Box<dyn Sleeper + Send> { Box::new(ScriptedSleeper::new(0)) };
    start(
        vec![Box::new(first), Box::new(second)],
        &keeper,
        &clock,
        &sleeper,
        &dispatch,
    );
    assert_eq!(dispatch.pending(), 2);
    assert!(dispatch.run_next());
    assert!(dispatch.run_next());
    assert_eq!(first_asked.lock().expect("asked").len(), 1);
    assert_eq!(second_asked.lock().expect("asked").len(), 1);
    assert_eq!(
        keeper.health().sources,
        [
            state("one", pb::SourceStatus::Ok, ""),
            state("two", pb::SourceStatus::Unreadable, "timed out")
        ]
    );
}

#[test]
fn a_linux_host_reads_smart_hwmon_and_the_sel_and_a_mac_nothing_yet() {
    let names = |kind| -> Vec<&'static str> {
        for_host(
            kind,
            Path::new("/nonexistent"),
            Path::new("/nonexistent/smartctl"),
        )
        .iter()
        .map(|s| s.name())
        .collect()
    };
    assert_eq!(names(PlatformKind::Linux), ["smart", "hwmon", "sel"]);
    assert_eq!(names(PlatformKind::Openrc), ["smart", "hwmon", "sel"]);
    assert!(names(PlatformKind::Macos).is_empty());
}

#[test]
fn the_host_sources_are_started_on_the_dispatch_and_read_nothing_before() {
    let dir = Scratch::new("sources-at-host");
    let keeper = keeper(&dir);
    let clock: Arc<dyn UnixClock> = Arc::new(FakeClock::new(NOW, 0));
    let sleeper = || -> Box<dyn Sleeper + Send> { Box::new(ScriptedSleeper::new(0)) };
    let dispatch = ManualDispatch::default();
    at_host(PlatformKind::Linux, &keeper, &clock, &sleeper, &dispatch);
    assert_eq!(dispatch.pending(), 3);
    let mac = ManualDispatch::default();
    at_host(PlatformKind::Macos, &keeper, &clock, &sleeper, &mac);
    assert_eq!(mac.pending(), 0);
    assert!(keeper.health().sources.is_empty(), "nothing read yet");
}

#[test]
fn a_read_with_no_mark_and_no_record_leaves_no_first_read() {
    let dir = Scratch::new("sources-no-mark");
    let keeper = keeper(&dir);
    let read = SourceRead::unsupported("absent".to_owned());
    assert_eq!(keeper.read("fake", read, NOW), 0);
    assert_eq!(keeper.mark("fake"), None);
    assert_eq!(
        keeper.health().sources,
        [state("fake", pb::SourceStatus::Unsupported, "absent")]
    );
}

#[test]
fn a_read_with_records_and_no_mark_keeps_the_mark_it_had() {
    let dir = Scratch::new("sources-keep-mark");
    let keeper = keeper(&dir);
    let fresh = SourceRead {
        mark: None,
        events: vec![event("first", pb::HardwareSeverity::Warning)],
        ..SourceRead::unreadable("partly".to_owned())
    };
    assert_eq!(keeper.read("fake", fresh.clone(), NOW), 1);
    assert_eq!(
        keeper.mark("fake").as_deref(),
        Some(""),
        "no mark before: empty"
    );
    assert_eq!(
        keeper.read("fake", SourceRead::ok("m7".to_owned(), Vec::new()), NOW),
        0
    );
    let again = SourceRead {
        events: vec![event("second", pb::HardwareSeverity::Warning)],
        ..fresh
    };
    assert_eq!(keeper.read("fake", again, NOW), 1);
    assert_eq!(keeper.mark("fake").as_deref(), Some("m7"));
    assert_eq!(keeper.health().events.len(), 2);
}

#[test]
fn a_source_state_is_cut_to_its_bounds_and_at_most_twelve_are_sent() {
    let dir = Scratch::new("sources-bounds");
    let keeper = keeper(&dir);
    let long = "s".repeat(MAX_SOURCE + 4);
    let reason = format!("{}é", "r".repeat(MAX_SOURCE_REASON - 1));
    keeper.read(&long, SourceRead::unreadable(reason), NOW);
    let sources = keeper.health().sources;
    assert_eq!(sources[0].source, "s".repeat(MAX_SOURCE));
    assert_eq!(
        sources[0].reason,
        "r".repeat(MAX_SOURCE_REASON - 1),
        "cut at a character boundary"
    );
    for n in 0..MAX_SOURCES + 2 {
        keeper.read(
            &format!("src{n:02}"),
            SourceRead::unsupported(String::new()),
            NOW,
        );
    }
    let sources = keeper.health().sources;
    assert_eq!(sources.len(), MAX_SOURCES);
    assert_eq!(sources[0].source, "src00", "in the order of their tokens");
}

/// A record with every text at its bound.
fn widest(n: usize) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id: format!("{n:0>width$}", width = MAX_EVENT_ID),
        subject: "s".repeat(MAX_SUBJECT),
        code: "c".repeat(MAX_CODE),
        message: "m".repeat(MAX_MESSAGE),
        raw: "r".repeat(MAX_RAW),
        about: "a".repeat(MAX_EVENT_ID),
        source: "f".repeat(MAX_SOURCE),
        ..event("x", pb::HardwareSeverity::Critical)
    }
}

#[test]
fn the_health_carries_fewer_records_when_the_states_would_break_its_bound() {
    let dir = Scratch::new("sources-fit");
    let keeper = keeper(&dir);
    let records: Vec<pb::HardwareEvent> = (0..MAX_EVENTS).map(widest).collect();
    keeper.read("fake", SourceRead::ok("m".to_owned(), records), NOW);
    let full = keeper.health();
    assert_eq!(full.events.len(), MAX_EVENTS);
    assert_eq!(check_health(&full), Ok(()));
    for n in 0..MAX_SOURCES {
        let reason = "r".repeat(MAX_SOURCE_REASON);
        keeper.read(
            &format!("{n:0>width$}", width = MAX_SOURCE),
            SourceRead::unreadable(reason),
            NOW,
        );
    }
    let crowded = keeper.health();
    assert_eq!(crowded.sources.len(), MAX_SOURCES);
    assert!(
        crowded.events.len() < MAX_EVENTS,
        "{}",
        crowded.events.len()
    );
    assert!(!crowded.events.is_empty());
    assert_eq!(check_health(&crowded), Ok(()));
    assert_eq!(
        crowded.events[..],
        full.events[..crowded.events.len()],
        "the records left are the first of the order"
    );
}

#[test]
fn poll_hands_the_read_to_the_keeper_at_the_clocks_second() {
    let dir = Scratch::new("sources-poll");
    let keeper = keeper(&dir);
    let clock = FakeClock::new(NOW + 5, 0);
    let (mut source, asked) = Scripted::new(
        "fake",
        vec![SourceRead::ok(
            "m".to_owned(),
            vec![event("p", pb::HardwareSeverity::Info)],
        )],
    );
    assert_eq!(poll(&mut source, &keeper, &clock), 1);
    assert_eq!(*asked.lock().expect("asked"), [(None, NOW + 5)]);
    assert_eq!(keeper.mark("fake").as_deref(), Some("m"));
}

#[test]
fn the_daemon_starts_its_host_sources_only_when_it_keeps_a_health_state() {
    let dir = Scratch::new("sources-start-host");
    let keeper = keeper(&dir);
    let fake = Arc::new(FakeClock::new(NOW, 0));
    let clock: Arc<dyn UnixClock> = fake.clone();
    let waits: Arc<dyn Clock> = fake;
    let stop = StopFlag::default();
    let none = ManualDispatch::default();
    start_host(PlatformKind::Linux, None, &clock, &waits, &stop, &none);
    assert_eq!(none.pending(), 0);
    let linux = ManualDispatch::default();
    start_host(
        PlatformKind::Linux,
        Some(&keeper),
        &clock,
        &waits,
        &stop,
        &linux,
    );
    assert_eq!(linux.pending(), 3, "one worker per source, none run yet");
}
