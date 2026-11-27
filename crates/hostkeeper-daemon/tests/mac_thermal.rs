//! A Mac's thermal pressure (`hostkeeper_daemon::mac_thermal`): `pmset -g therm` and
//! `powermetrics --samplers thermal -n 1` read from fixtures written from the format each prints
//! (hand-written samples, each to be replaced by a capture from a Mac), the rule judged sample by
//! sample into whole records (five samples at heavy assert, trapping escalates, ten minutes
//! nominal deassert), `powermetrics` run only under a limit and at most every five minutes, and
//! every failure as the source's state; the source driven as the daemon drives it into a keeper
//! over a scratch state directory. The programs are scripted fakes: nothing runs a program, and
//! the clock is the test's.

mod support;

use std::fs;
use std::sync::Arc;

use hostkeeper_daemon::hardware_sources::{HardwareSource, SourceRead, poll};
use hostkeeper_daemon::host_health::{BootFacts, HealthKeeper};
use hostkeeper_daemon::mac_thermal::{
    Assertion, HEAVY_SAMPLES, NOMINAL_SECS, PMSET_ARGS, POWERMETRICS_ARGS, POWERMETRICS_EVERY_SECS,
    Pressure, SOURCE, THERMAL_EVERY_SECS, Therm, ThermalMark, ThermalSource, judge, parse_pmset,
    parse_powermetrics,
};
use hostkeeper_daemon::shutdown_probe::{GoingDown, ShutdownProbe};
use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_link::HostHealthSource;
use hostkeeper_link::testing::{FakeClock, RecordingLog};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use support::Scratch;

/// The second the tests read at.
const NOW: u64 = 1_795_662_000;

/// What `pmset -g therm` of the nominal fixture is read as.
const NOMINAL_LINES: &str = "Note: No thermal warning level has been recorded; Note: No \
                             performance warning level has been recorded; CPU_Scheduler_Limit \
                             \t= 100; CPU_Speed_Limit \t= 100";

/// What `pmset -g therm` of the limited fixture is read as.
const LIMITED_LINES: &str = "Thermal warning level set to 2.; Performance warning level set to \
                             1.; CPU_Scheduler_Limit \t= 100; CPU_Speed_Limit \t= 72";

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/mac_health/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|a| (*a).to_owned()).collect()
}

fn printed(text: &str) -> Result<LaunchctlOutput, LaunchctlError> {
    Ok(LaunchctlOutput::success(text))
}

/// `powermetrics`' answer at `level`.
fn measured(level: &str) -> String {
    fixture("powermetrics-heavy.txt").replace(
        "Current pressure level: Heavy",
        &format!("Current pressure level: {level}"),
    )
}

/// The record of the rule.
fn record(
    event_id: &str,
    at: u64,
    severity: pb::HardwareSeverity,
    state: pb::HardwareEventState,
    message: &str,
    raw: &str,
    about: &str,
) -> pb::HardwareEvent {
    pb::HardwareEvent {
        event_id: event_id.to_owned(),
        time_unix: at,
        of_previous_boot: false,
        component: pb::HardwareComponent::Thermal.into(),
        subject: "thermal pressure".to_owned(),
        severity: severity.into(),
        state: state.into(),
        source: "thermal".to_owned(),
        code: "thermal_pressure".to_owned(),
        message: message.to_owned(),
        raw: raw.to_owned(),
        about: about.to_owned(),
    }
}

/// Judges `levels`, one every [`THERMAL_EVERY_SECS`] from `from`, each with the `raw` `"s<n>"`:
/// every record, in order.
fn samples(mark: &mut ThermalMark, from: u64, levels: &[Pressure]) -> Vec<pb::HardwareEvent> {
    let mut events = Vec::new();
    for (n, level) in levels.iter().enumerate() {
        let at = from + n as u64 * THERMAL_EVERY_SECS;
        events.extend(judge(mark, *level, &format!("s{n}"), at));
    }
    events
}

/// What a scripted program answers.
type Answer = Result<LaunchctlOutput, LaunchctlError>;

/// No record.
fn none() -> Vec<pb::HardwareEvent> {
    Vec::new()
}

struct Never;

impl ShutdownProbe for Never {
    fn going_down(&self) -> GoingDown {
        GoingDown::No("a test".to_owned())
    }
}

fn keeper(dir: &Scratch) -> Arc<HealthKeeper> {
    Arc::new(HealthKeeper::open(
        dir.path(),
        BootFacts {
            boot_id: Some("boot-1".to_owned()),
            boot_unix: NOW - 3600,
            machine_id: String::new(),
        },
        Box::new(Never),
        Arc::new(RecordingLog::default()),
    ))
}

/// Proves both fixtures are read: no warning level and limits of 100 is no limit; a warning level
/// above 0 or a speed limit under 100 is one; the lines read are kept for the record's `raw`.
///
/// Catches: a Mac throttled for heat read as unlimited (no sample taken), or an idle one read as
/// limited (powermetrics run every minute).
#[test]
fn the_pmset_fixtures_are_read() {
    assert_eq!(
        parse_pmset(&fixture("pmset-nominal.txt")),
        Ok(Therm {
            limited: false,
            lines: NOMINAL_LINES.to_owned(),
        })
    );
    assert_eq!(
        parse_pmset(&fixture("pmset-limited.txt")),
        Ok(Therm {
            limited: true,
            lines: LIMITED_LINES.to_owned(),
        })
    );
}

/// Proves each gauge on its own: a warning level of 0 limits nothing and of 1 limits; a speed or
/// scheduler limit of 100 limits nothing and of 99 limits; the notes alone are no limit; output
/// with none of these lines, or a number that does not read, is refused with its reason.
///
/// Catches: one gauge's threshold off by one, or a broken answer read as nominal.
#[test]
fn each_pmset_gauge_is_judged_at_its_bound() {
    let limited = |text: &str| parse_pmset(text).map(|therm| therm.limited);
    assert_eq!(limited("Thermal warning level set to 0."), Ok(false));
    assert_eq!(limited("Thermal warning level set to 1."), Ok(true));
    assert_eq!(limited("Performance warning level set to 0."), Ok(false));
    assert_eq!(limited("Performance warning level set to 4."), Ok(true));
    assert_eq!(limited("CPU_Speed_Limit = 100"), Ok(false));
    assert_eq!(limited("CPU_Speed_Limit = 99"), Ok(true));
    assert_eq!(limited("\tCPU_Scheduler_Limit \t= 100"), Ok(false));
    assert_eq!(limited("\tCPU_Scheduler_Limit \t= 99"), Ok(true));
    assert_eq!(
        limited("Note: No thermal warning level has been recorded"),
        Ok(false)
    );
    assert_eq!(
        parse_pmset("Note: No CPU power status has been recorded\nCPU_Available_CPUs = 12\n"),
        Err("pmset printed no thermal state".to_owned())
    );
    assert_eq!(
        parse_pmset(""),
        Err("pmset printed no thermal state".to_owned())
    );
    assert_eq!(
        parse_pmset("Thermal warning level set to high."),
        Err("pmset printed \"Thermal warning level set to high.\", which has no number".to_owned())
    );
    assert_eq!(
        parse_pmset("CPU_Speed_Limit = "),
        Err("pmset printed \"CPU_Speed_Limit =\", which has no number".to_owned())
    );
}

/// Proves the level of `powermetrics` is read in any case, every level macOS names, and that
/// output with no level, or a level this release does not know, is refused with its reason.
///
/// Catches: a heavy Mac read as another level, or a broken answer read as a level.
#[test]
fn the_powermetrics_level_is_read() {
    assert_eq!(
        parse_powermetrics(&fixture("powermetrics-heavy.txt")),
        Ok(Pressure::Heavy)
    );
    for level in [
        Pressure::Nominal,
        Pressure::Moderate,
        Pressure::Heavy,
        Pressure::Trapping,
        Pressure::Sleeping,
    ] {
        assert_eq!(Pressure::from_word(level.word()), Some(level));
        assert_eq!(
            parse_powermetrics(&measured(&level.word().to_ascii_uppercase())),
            Ok(level)
        );
    }
    assert_eq!(
        parse_powermetrics("**** Thermal pressure ****\n"),
        Err("powermetrics printed no pressure level".to_owned())
    );
    assert_eq!(
        parse_powermetrics(&measured("Scorching")),
        Err("powermetrics printed the level \"Scorching\"".to_owned())
    );
}

/// Proves five samples in a row at heavy assert one warning (four do not), a moderate or nominal
/// sample between them starts the count again, and a sample after the assertion asserts nothing
/// more.
///
/// Catches: a short spike asserted, a broken run counted as one, or a condition recorded at every
/// sample.
#[test]
fn five_heavy_samples_in_a_row_assert_a_warning() {
    let heavy = Pressure::Heavy;
    let mut mark = ThermalMark::default();
    assert_eq!(samples(&mut mark, NOW, &[heavy; 4]), none());
    assert_eq!(mark.heavy, HEAVY_SAMPLES - 1);

    let mut broken = ThermalMark::default();
    let at = |n: u64| NOW + n * THERMAL_EVERY_SECS;
    let levels = [
        heavy,
        heavy,
        heavy,
        heavy,
        Pressure::Moderate,
        heavy,
        heavy,
        heavy,
        heavy,
        Pressure::Nominal,
        heavy,
        heavy,
        heavy,
        heavy,
        heavy,
        heavy,
    ];
    assert_eq!(
        samples(&mut broken, NOW, &levels),
        vec![record(
            &format!("thermal:heavy:{}", at(14)),
            at(14),
            pb::HardwareSeverity::Warning,
            pb::HardwareEventState::Asserted,
            "Thermal pressure heavy or worse for 5 samples in a row, at worst heavy",
            "s14",
            "",
        )]
    );
    assert_eq!(
        broken.asserted,
        Some(Assertion {
            event_id: format!("thermal:heavy:{}", at(14)),
            critical: false,
        })
    );
    assert_eq!(broken.heavy, 6);
}

/// Proves a run of five with one sample at trapping asserts critical (at worst trapping); a
/// warning then escalates to a critical assertion about it at the first trapping sample, once;
/// and a sleeping sample is critical as trapping is.
///
/// Catches: a trapping Mac (macOS cutting its CPUs) reported as a warning, or escalated at every
/// sample.
#[test]
fn trapping_is_critical_and_escalates_a_warning() {
    let (heavy, trapping) = (Pressure::Heavy, Pressure::Trapping);
    let at = |n: u64| NOW + n * THERMAL_EVERY_SECS;

    let mut mark = ThermalMark::default();
    assert_eq!(
        samples(&mut mark, NOW, &[heavy, trapping, heavy, heavy, heavy]),
        vec![record(
            &format!("thermal:heavy:{}", at(4)),
            at(4),
            pb::HardwareSeverity::Critical,
            pb::HardwareEventState::Asserted,
            "Thermal pressure heavy or worse for 5 samples in a row, at worst trapping",
            "s4",
            "",
        )]
    );
    assert_eq!(judge(&mut mark, Pressure::Sleeping, "s5", at(5)), none());

    let mut warned = ThermalMark::default();
    let first = format!("thermal:heavy:{}", at(4));
    let levels = [heavy, heavy, heavy, heavy, heavy, heavy, trapping, trapping];
    assert_eq!(
        samples(&mut warned, NOW, &levels),
        vec![
            record(
                &first,
                at(4),
                pb::HardwareSeverity::Warning,
                pb::HardwareEventState::Asserted,
                "Thermal pressure heavy or worse for 5 samples in a row, at worst heavy",
                "s4",
                "",
            ),
            record(
                &format!("thermal:trapping:{}", at(6)),
                at(6),
                pb::HardwareSeverity::Critical,
                pb::HardwareEventState::Asserted,
                "Thermal pressure now trapping",
                "s6",
                &first,
            ),
        ]
    );

    let mut asleep = ThermalMark::default();
    let events = samples(&mut asleep, NOW, &[Pressure::Sleeping; 5]);
    assert_eq!(
        events
            .iter()
            .map(|e| (e.severity, e.event_id.clone()))
            .collect::<Vec<_>>(),
        vec![(
            i32::from(pb::HardwareSeverity::Critical),
            format!("thermal:sleeping:{}", at(4))
        )]
    );
}

/// Proves the condition ends only after ten minutes of nominal samples: at nine it holds, at ten a
/// deassertion about the last assertion at its severity is made, once; a heavy or moderate sample
/// in between starts the ten minutes again; nominal samples with nothing asserted make nothing.
///
/// Catches: a condition ended by one cool sample, never ended, or ended as `info` (which a full
/// queue folds away).
#[test]
fn ten_minutes_nominal_end_the_condition_at_its_severity() {
    let nominal = Pressure::Nominal;
    let at = |n: u64| NOW + n * THERMAL_EVERY_SECS;
    let open = format!("thermal:trapping:{NOW}");
    let critical = || ThermalMark {
        asserted: Some(Assertion {
            event_id: open.clone(),
            critical: true,
        }),
        ..ThermalMark::default()
    };

    let mut mark = critical();
    assert_eq!(samples(&mut mark, NOW, &[nominal; 10]), none());
    assert_eq!(mark.nominal_since, Some(NOW));
    assert_eq!(
        judge(&mut mark, nominal, "s10", NOW + NOMINAL_SECS),
        vec![record(
            &format!("thermal:nominal:{}", NOW + NOMINAL_SECS),
            NOW + NOMINAL_SECS,
            pb::HardwareSeverity::Critical,
            pb::HardwareEventState::Deasserted,
            "Thermal pressure back to nominal for 10 minutes",
            "s10",
            &open,
        )]
    );
    assert_eq!(mark.asserted, None);
    assert_eq!(judge(&mut mark, nominal, "s11", at(11)), none());

    let mut interrupted = critical();
    let levels = [
        nominal,
        nominal,
        nominal,
        nominal,
        nominal,
        Pressure::Heavy,
        nominal,
        nominal,
        nominal,
        nominal,
        nominal,
        nominal,
        nominal,
        nominal,
        nominal,
        nominal,
        Pressure::Moderate,
        nominal,
    ];
    assert_eq!(samples(&mut interrupted, NOW, &levels), none());
    assert_eq!(interrupted.nominal_since, Some(at(17)));
    assert!(interrupted.asserted.is_some());

    let mut warned = ThermalMark {
        asserted: Some(Assertion {
            event_id: open.clone(),
            critical: false,
        }),
        nominal_since: Some(NOW),
        ..ThermalMark::default()
    };
    let ended = judge(&mut warned, nominal, "s", NOW + NOMINAL_SECS);
    assert_eq!(
        ended.iter().map(|e| e.severity).collect::<Vec<_>>(),
        vec![i32::from(pb::HardwareSeverity::Warning)]
    );

    let mut idle = ThermalMark::default();
    assert_eq!(samples(&mut idle, NOW, &[nominal; 20]), none());
    let mut stepped_back = critical();
    stepped_back.nominal_since = Some(NOW + 3600);
    assert_eq!(judge(&mut stepped_back, nominal, "s", NOW), none());
}

/// Proves the source as the daemon drives it: an unlimited Mac is sampled nominal without
/// running `powermetrics`; a limited one runs it, at most once in five minutes (the samples
/// between take its level); five such samples hold one warning in the keeper, and ten minutes of
/// unlimited samples after hold its end, with what `pmset` and `powermetrics` printed as `raw`.
/// A sample that changes nothing leaves the mark as it was.
///
/// Catches: `powermetrics` run every minute (or never), or a heavy Mac never reaching the
/// coordinator.
#[test]
fn a_heavy_mac_is_held_then_its_end() {
    let dir = Scratch::new("mac-thermal-cycle");
    let keeper = keeper(&dir);
    let clock = FakeClock::new(NOW, 0);
    let pmset = FakeLaunchctl::new();
    let powermetrics = FakeLaunchctl::new();
    powermetrics.always(&args(&POWERMETRICS_ARGS), printed(&measured("Heavy")));
    let mut source = ThermalSource::new(Box::new(pmset.clone()), Box::new(powermetrics.clone()));
    assert_eq!(source.name(), SOURCE);
    assert_eq!(source.every_secs(), THERMAL_EVERY_SECS);

    pmset.always(&args(&PMSET_ARGS), printed(&fixture("pmset-nominal.txt")));
    assert_eq!(poll(&mut source, &keeper, &clock), 0);
    let calm = keeper.mark(SOURCE);
    assert!(calm.is_some(), "the nominal time began");
    clock.advance(THERMAL_EVERY_SECS);
    assert_eq!(
        source.read(calm.as_deref(), NOW + THERMAL_EVERY_SECS).mark,
        None,
        "nothing changed"
    );
    assert!(powermetrics.calls().is_empty());

    pmset.always(&args(&PMSET_ARGS), printed(&fixture("pmset-limited.txt")));
    let mut held = 0;
    for _ in 0..HEAVY_SAMPLES {
        clock.advance(THERMAL_EVERY_SECS);
        held += poll(&mut source, &keeper, &clock);
    }
    let asserted_at = NOW + 6 * THERMAL_EVERY_SECS;
    let raw = format!("{LIMITED_LINES}; Current pressure level: heavy");
    let open = format!("thermal:heavy:{asserted_at}");
    assert_eq!(held, 1);
    assert_eq!(
        keeper.health().events,
        vec![record(
            &open,
            asserted_at,
            pb::HardwareSeverity::Warning,
            pb::HardwareEventState::Asserted,
            "Thermal pressure heavy or worse for 5 samples in a row, at worst heavy",
            &raw,
            "",
        )]
    );
    assert_eq!(
        powermetrics.calls().len(),
        1,
        "the five samples span four minutes, under the five between two runs"
    );

    pmset.always(&args(&PMSET_ARGS), printed(&fixture("pmset-nominal.txt")));
    let mut ended = 0;
    for _ in 0..=(NOMINAL_SECS / THERMAL_EVERY_SECS) {
        clock.advance(THERMAL_EVERY_SECS);
        ended += poll(&mut source, &keeper, &clock);
    }
    let ended_at = asserted_at + (NOMINAL_SECS / THERMAL_EVERY_SECS + 1) * THERMAL_EVERY_SECS;
    assert_eq!(ended, 1);
    let events = keeper.health().events;
    assert_eq!(
        events.last(),
        Some(&record(
            &format!("thermal:nominal:{ended_at}"),
            ended_at,
            pb::HardwareSeverity::Warning,
            pb::HardwareEventState::Deasserted,
            "Thermal pressure back to nominal for 10 minutes",
            NOMINAL_LINES,
            &open,
        ))
    );
    assert_eq!(powermetrics.calls().len(), 1);
}

/// Proves `powermetrics` runs again once five minutes have passed since its last run, and not a
/// second sooner; and that a clock stepped back before its last run measures again.
///
/// Catches: a cached level kept for good, or `powermetrics` run every minute.
#[test]
fn powermetrics_runs_at_most_every_five_minutes() {
    let pmset = FakeLaunchctl::new();
    pmset.always(&args(&PMSET_ARGS), printed(&fixture("pmset-limited.txt")));
    let powermetrics = FakeLaunchctl::new();
    powermetrics.then(&args(&POWERMETRICS_ARGS), printed(&measured("Heavy")));
    powermetrics.then(&args(&POWERMETRICS_ARGS), printed(&measured("Trapping")));
    powermetrics.then(&args(&POWERMETRICS_ARGS), printed(&measured("Moderate")));
    let mut source = ThermalSource::new(Box::new(pmset), Box::new(powermetrics.clone()));
    let level = |read: SourceRead| {
        let mark: ThermalMark =
            serde_json::from_str(&read.mark.unwrap_or_default()).unwrap_or_default();
        (read.status, mark.worst)
    };
    assert_eq!(
        level(source.read(None, NOW)),
        (pb::SourceStatus::Ok, Some(Pressure::Heavy))
    );
    assert_eq!(
        source.read(None, NOW + POWERMETRICS_EVERY_SECS - 1).events,
        none()
    );
    assert_eq!(powermetrics.calls().len(), 1);
    assert_eq!(
        level(source.read(None, NOW + POWERMETRICS_EVERY_SECS)),
        (pb::SourceStatus::Ok, Some(Pressure::Trapping))
    );
    assert_eq!(powermetrics.calls().len(), 2);
    assert_eq!(
        level(source.read(None, NOW)),
        (pb::SourceStatus::Ok, None),
        "moderate, measured again"
    );
    assert_eq!(powermetrics.calls().len(), 3);
}

/// Proves every failure is the source `unreadable` with its reason, no record and the mark kept:
/// `pmset` not run, exiting 1, ended by a signal, printing nothing read; and, under a limit,
/// `powermetrics` exiting 1 or printing no level.
///
/// Catches: a Mac whose pressure cannot be read shown as nominal, or a failure ending a
/// condition.
#[test]
fn every_failure_is_unreadable_with_its_reason() {
    let limited = || printed(&fixture("pmset-limited.txt"));
    let cases: Vec<(Answer, Option<Answer>, &str)> = vec![
        (
            Err(LaunchctlError::Spawn {
                args: "-g therm".to_owned(),
                why: "no such file".to_owned(),
            }),
            None,
            "pmset did not run: cannot run launchctl -g therm: no such file",
        ),
        (Ok(LaunchctlOutput::exit(1, "")), None, "pmset exited 1"),
        (
            Ok(LaunchctlOutput {
                code: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
            }),
            None,
            "pmset was ended by a signal",
        ),
        (
            printed("usage: pmset"),
            None,
            "pmset printed no thermal state",
        ),
        (
            limited(),
            Some(Ok(LaunchctlOutput::exit(
                1,
                "must be invoked as the superuser",
            ))),
            "powermetrics exited 1",
        ),
        (
            limited(),
            Some(printed("**** Thermal pressure ****\n")),
            "powermetrics printed no pressure level",
        ),
    ];
    let open = serde_json::to_string(&ThermalMark {
        heavy: 7,
        worst: Some(Pressure::Heavy),
        nominal_since: None,
        asserted: Some(Assertion {
            event_id: "thermal:heavy:1".to_owned(),
            critical: false,
        }),
    })
    .expect("a mark");
    for (pmset_answer, powermetrics_answer, why) in cases {
        let pmset = FakeLaunchctl::new();
        pmset.then(&args(&PMSET_ARGS), pmset_answer);
        let powermetrics = FakeLaunchctl::new();
        if let Some(answer) = powermetrics_answer {
            powermetrics.then(&args(&POWERMETRICS_ARGS), answer);
        }
        let mut source = ThermalSource::new(Box::new(pmset), Box::new(powermetrics));
        assert_eq!(
            source.read(Some(&open), NOW),
            SourceRead::unreadable(why.to_owned()),
            "for {why}"
        );
    }
}

/// Proves a mark that does not parse is read as none: the rule starts again from no samples.
///
/// Catches: a state file from a later release, or a damaged mark, stopping the source.
#[test]
fn a_mark_that_does_not_parse_starts_the_rule_again() {
    let pmset = FakeLaunchctl::new();
    pmset.always(&args(&PMSET_ARGS), printed(&fixture("pmset-nominal.txt")));
    let mut source = ThermalSource::new(Box::new(pmset), Box::new(FakeLaunchctl::new()));
    let read = source.read(Some("not json"), NOW);
    let expected = ThermalMark {
        nominal_since: Some(NOW),
        ..ThermalMark::default()
    };
    assert_eq!(
        read,
        SourceRead::ok(
            serde_json::to_string(&expected).expect("a mark"),
            Vec::new()
        )
    );
}

/// Proves a `powermetrics` that fails is not run again within five minutes of its failed run:
/// the samples between are `unreadable` with its reason; five minutes after it, it runs again.
///
/// Catches: a daemon spawning a failing `powermetrics` every minute for as long as macOS limits
/// the Mac.
#[test]
fn a_failing_powermetrics_runs_at_most_every_five_minutes_too() {
    let pmset = FakeLaunchctl::new();
    pmset.always(&args(&PMSET_ARGS), printed(&fixture("pmset-limited.txt")));
    let powermetrics = FakeLaunchctl::new();
    powermetrics.then(
        &args(&POWERMETRICS_ARGS),
        Ok(LaunchctlOutput::exit(1, "must be invoked as the superuser")),
    );
    powermetrics.then(&args(&POWERMETRICS_ARGS), printed(&measured("Heavy")));
    let mut source = ThermalSource::new(Box::new(pmset), Box::new(powermetrics.clone()));
    let failed = SourceRead::unreadable("powermetrics exited 1".to_owned());
    assert_eq!(source.read(None, NOW), failed);
    assert_eq!(source.read(None, NOW + THERMAL_EVERY_SECS), failed);
    assert_eq!(source.read(None, NOW + POWERMETRICS_EVERY_SECS - 1), failed);
    assert_eq!(powermetrics.calls().len(), 1);
    assert_eq!(
        source.read(None, NOW + POWERMETRICS_EVERY_SECS).status,
        pb::SourceStatus::Ok
    );
    assert_eq!(powermetrics.calls().len(), 2);
}
