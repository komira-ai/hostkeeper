//! The burst of hostkeeper's own update: the one rule that says a binary has failed its starts
//! ([`should_fall_back`]), read the same for systemd's start limit and for the fallback on macOS,
//! and the start log the macOS fallback reads it from.

mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use hostkeeper_steps::selfupdate::{START_LIMIT_BURST, START_LIMIT_WINDOW_SECS, main_dropin};
use hostkeeper_steps::starts::{MAX_STARTS, STARTS_FILE};
use hostkeeper_steps::{DaemonStart, Stamp, StartLog, StartLogError, should_fall_back};
use serde_json::{Value, json};
use support::Scratch;

fn at(boot: &str, mono_secs: u64) -> Stamp {
    Stamp {
        boot_id: boot.to_owned(),
        mono_secs,
    }
}

fn starts(monos: &[u64]) -> Vec<Stamp> {
    monos.iter().map(|mono| at("b", *mono)).collect()
}

/// The value of `key=` in the systemd text `text`.
fn setting(text: &str, key: &str) -> u64 {
    text.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("{key} in {text}"))
}

/// Catches the two systems deciding the burst apart, or a second copy of the rule: systemd's
/// start limit, read from the drop-in it is given (`StartLimitBurst=`, `StartLimitIntervalSec=`),
/// and the rule the fallback on macOS applies ([`START_LIMIT_BURST`] within
/// [`START_LIMIT_WINDOW_SECS`]) are the same numbers through the same function, and give the
/// table's verdicts: two starts in the window, no; three in the window, yes; three spread over
/// more than the window, no; and the edges of the window and of a reboot.
#[test]
fn should_fall_back_agrees_for_systemd_and_launchd_starts() {
    let dropin = main_dropin("hostkeeper-fallback.service", START_LIMIT_BURST);
    let systemd = |starts: &[Stamp]| {
        let burst = setting(&dropin, "StartLimitBurst");
        let window = setting(&dropin, "StartLimitIntervalSec");
        should_fall_back(starts, u32::try_from(burst).expect("a burst"), window)
    };
    let launchd =
        |starts: &[Stamp]| should_fall_back(starts, START_LIMIT_BURST, START_LIMIT_WINDOW_SECS);
    assert_eq!((START_LIMIT_BURST, START_LIMIT_WINDOW_SECS), (3, 300));

    let mut rebooted = starts(&[100, 110]);
    rebooted.push(at("c", 20));
    let table: Vec<(&str, Vec<Stamp>, bool)> = vec![
        ("no start", Vec::new(), false),
        ("two starts in the window", starts(&[100, 110]), false),
        ("three starts in the window", starts(&[100, 110, 120]), true),
        (
            "three starts over more than the window",
            starts(&[100, 300, 500]),
            false,
        ),
        (
            "the first start just inside the window",
            starts(&[100, 200, 399]),
            true,
        ),
        (
            "the first start at the window's end",
            starts(&[100, 200, 400]),
            false,
        ),
        (
            "old starts and three new ones",
            starts(&[0, 10, 1000, 1010, 1020]),
            true,
        ),
        (
            "two starts before a reboot do not count after it",
            rebooted,
            false,
        ),
        (
            "a later start on a clock that went back is not counted",
            vec![at("b", 900), at("b", 100), at("b", 110), at("b", 120)],
            true,
        ),
        (
            "a start after the latest does not count",
            vec![at("b", 100), at("b", 500), at("b", 110)],
            false,
        ),
    ];
    for (case, starts, want) in table {
        assert_eq!(systemd(&starts), want, "systemd: {case}");
        assert_eq!(launchd(&starts), want, "launchd: {case}");
    }
    assert!(
        !should_fall_back(&starts(&[1, 2, 3]), 0, 300),
        "a burst of 0"
    );
    assert!(should_fall_back(&starts(&[1]), 1, 300), "a burst of 1");
}

fn start(version: &str, mono: u64) -> DaemonStart {
    DaemonStart {
        version: version.to_owned(),
        at: at("b", mono),
    }
}

/// Catches a start log that loses a start, its version or its order, that grows without end, or
/// that drops a key a newer daemon wrote: absent, it is empty; each start is written durably,
/// mode 0600, and read back in order; only the starts of one version are its starts; past
/// [`MAX_STARTS`] the oldest go; a key it does not know, and a newer schema, are kept.
#[test]
fn the_start_log_keeps_the_last_starts_in_order() {
    let s = Scratch::new("starts-log");
    let mut log = StartLog::load(s.path()).expect("an empty log");
    assert!(log.starts().is_empty());
    assert_eq!(log.path(), s.path().join(STARTS_FILE));
    log.record(start("0.2.0", 10)).expect("written");
    log.record(start("0.1.0", 20)).expect("written");
    log.record(start("0.2.0", 30)).expect("written");

    let path = s.path().join(STARTS_FILE);
    let mode = fs::metadata(&path).expect("the file").permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let written: Value = serde_json::from_slice(&fs::read(&path).expect("read")).expect("JSON");
    assert_eq!(
        written,
        json!({"schema": 1, "starts": [
            {"version": "0.2.0", "at": {"boot_id": "b", "mono_secs": 10}},
            {"version": "0.1.0", "at": {"boot_id": "b", "mono_secs": 20}},
            {"version": "0.2.0", "at": {"boot_id": "b", "mono_secs": 30}},
        ]})
    );
    let log = StartLog::load(s.path()).expect("the log");
    assert_eq!(
        log.starts(),
        [start("0.2.0", 10), start("0.1.0", 20), start("0.2.0", 30)]
    );
    assert_eq!(log.of("0.2.0"), [at("b", 10), at("b", 30)]);
    assert!(log.of("0.3.0").is_empty());

    let mut log = log;
    for mono in 0..(MAX_STARTS as u64) {
        log.record(start("0.4.0", 100 + mono)).expect("written");
    }
    let log = StartLog::load(s.path()).expect("the log");
    assert_eq!(log.starts().len(), MAX_STARTS);
    assert_eq!(log.starts()[0], start("0.4.0", 100));
    assert!(log.of("0.2.0").is_empty(), "the oldest starts went first");

    fs::write(
        &path,
        r#"{"schema":2,"later":{"x":1},"starts":[{"version":"0.5.0","at":{"boot_id":"b","mono_secs":5},"note":"n"}]}"#,
    )
    .expect("a newer log");
    let mut log = StartLog::load(s.path()).expect("a newer log reads");
    assert_eq!(log.starts(), [start("0.5.0", 5)]);
    log.record(start("0.5.0", 6)).expect("written");
    let written: Value = serde_json::from_slice(&fs::read(&path).expect("read")).expect("JSON");
    assert_eq!(written["schema"], json!(2));
    assert_eq!(written["later"], json!({"x": 1}));
    assert_eq!(written["starts"].as_array().map(Vec::len), Some(2));

    fs::write(&path, r#"{"schema":1}"#).expect("a log with no starts");
    assert!(
        StartLog::load(s.path())
            .expect("no starts")
            .starts()
            .is_empty()
    );
}

/// Catches a start log that cannot be used being read as no starts (a binary that keeps failing
/// would then never fall back): text that is not such a log, and a file that cannot be read or
/// written, are errors naming the file.
#[test]
fn a_start_log_that_cannot_be_used_is_an_error_naming_it() {
    let s = Scratch::new("starts-errors");
    let path = s.path().join(STARTS_FILE);
    for text in [
        "not json",
        "[]",
        r#"{"starts":"0.2.0"}"#,
        r#"{"starts":[{"version":"0.2.0"}]}"#,
    ] {
        fs::write(&path, text).expect("write");
        let err = StartLog::load(s.path()).expect_err(text);
        assert!(
            matches!(&err, StartLogError::Garbage { path: p, .. } if *p == path),
            "{text}: {err:?}"
        );
        assert!(
            err.to_string()
                .starts_with(&format!("{} is not a start log: ", path.display())),
            "{err}"
        );
    }
    fs::remove_file(&path).expect("remove");
    fs::create_dir(&path).expect("a directory in its place");
    let err = StartLog::load(s.path()).expect_err("a directory");
    assert!(
        matches!(&err, StartLogError::Io { path: p, .. } if *p == path),
        "{err:?}"
    );
    assert!(
        err.to_string()
            .starts_with(&format!("cannot use the start log {}: ", path.display())),
        "{err}"
    );

    let s = Scratch::new("starts-unwritable");
    let mut log = StartLog::load(s.path()).expect("an empty log");
    fs::create_dir(s.path().join(format!("{STARTS_FILE}.tmp"))).expect("a directory");
    let err = log.record(start("0.2.0", 1)).expect_err("unwritable");
    assert!(
        matches!(&err, StartLogError::Io { path: p, .. } if *p == s.path().join(STARTS_FILE)),
        "{err:?}"
    );
}
