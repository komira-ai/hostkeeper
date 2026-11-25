//! The pure parts of the summary: the date text, the order of releases, the rule's boundaries and
//! the words of a stopped call.

use super::read::{Installed, Policy, why};
use super::rule::{State, newest_first, state};
use super::text::utc;
use crate::Stop;
use crate::xcode_add::store::Held;

fn held(version: &str, build: &str, sha: &str) -> Held {
    Held {
        sha256: sha.to_owned(),
        version: version.to_owned(),
        build: build.to_owned(),
    }
}

/// Proves: the date text is the UTC date and minute of a unix time, across the epoch, a leap day,
/// a century that is no leap year and a time late in a day.
#[test]
fn a_unix_time_is_written_as_its_utc_date_and_minute() {
    for (unix, text) in [
        (0, "1970-01-01 00:00 UTC"),
        (951_782_400, "2000-02-29 00:00 UTC"),
        (1_709_164_800, "2024-02-29 00:00 UTC"),
        (1_700_000_000, "2023-11-14 22:13 UTC"),
        (4_107_542_400, "2100-03-01 00:00 UTC"),
        (86_399, "1970-01-01 23:59 UTC"),
    ] {
        assert_eq!(utc(unix), text, "{unix}");
    }
}

/// Proves: releases are ordered by the numbers of the version (27.10 is newer than 27.9, and 27 is
/// 27.0), then by the version's text, then by build, then by SHA-256, newest first, as the
/// coordinator orders them.
#[test]
fn releases_are_ordered_newest_first_as_the_coordinator_orders_them() {
    let mut all = [
        held("27.9", "A", "1"),
        held("27.10", "A", "2"),
        held("27", "B", "3"),
        held("27.0", "B", "4"),
        held("27.0", "A", "5"),
        held("27.0", "A", "0"),
    ];
    all.sort_by(newest_first);
    let order: Vec<(&str, &str, &str)> = all
        .iter()
        .map(|h| (h.version.as_str(), h.build.as_str(), h.sha256.as_str()))
        .collect();
    assert_eq!(
        order,
        [
            ("27.10", "A", "2"),
            ("27.9", "A", "1"),
            ("27.0", "B", "4"),
            ("27.0", "A", "0"),
            ("27.0", "A", "5"),
            ("27", "B", "3"),
        ]
    );
}

/// Proves: with three releases held and `xcode_keep` 2 the second newest is kept and the third is
/// left out saying which two are kept, and with `xcode_keep` 3 the third is kept.
#[test]
fn the_keep_count_is_the_boundary_of_the_releases_a_group_keeps() {
    let archives = [
        held("27.3", "C", "3"),
        held("27.2", "B", "2"),
        held("27.1", "A", "1"),
    ];
    let installed: [Installed; 0] = [];
    let policy = |keep| Policy {
        keep,
        ..Policy::default()
    };
    assert_eq!(
        state("g", &policy(2), &archives, &archives[1], &installed),
        State::Behind("27.3".to_owned())
    );
    assert_eq!(
        state("g", &policy(2), &archives, &archives[2], &installed),
        State::LeftOut("group g keeps only newer releases (27.3, 27.2)".to_owned())
    );
    assert_eq!(
        state("g", &policy(3), &archives, &archives[2], &installed),
        State::Behind("27.3".to_owned())
    );
    assert_eq!(
        state("g", &policy(3), &archives, &archives[0], &installed),
        State::Missing
    );
}

/// Proves: a stopped call is worded as its message, and an exit as its code.
#[test]
fn a_stopped_call_says_its_message_or_its_exit() {
    assert_eq!(why(&Stop::fail("no answer")), "no answer");
    assert_eq!(
        why(&Stop::usage("a call needs --endpoint")),
        "a call needs --endpoint"
    );
    assert_eq!(why(&Stop::Exit(3)), "it exited 3");
}
