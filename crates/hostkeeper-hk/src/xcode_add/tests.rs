//! The parts of `hk xcode add` that are functions of their input: the name an archive is found
//! by, and the progress lines. The whole run is in tests/cli/xcode_add.rs.

use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

use super::find::{named, parts};
use super::progress::{Progress, of_text, size_text};
use super::reuse::release_of;

/// Proves: a version's parts drop the trailing zeros and compare as numbers, and what is no version
/// (empty, a letter, a sign, an empty part, a number too large) has none.
#[test]
fn a_version_is_numbers_without_trailing_zeros() {
    assert_eq!(parts("27.2"), Some(vec![27, 2]));
    assert_eq!(parts("27.2.0"), Some(vec![27, 2]));
    assert_eq!(parts("27.0.0"), Some(vec![27]));
    assert_eq!(parts("0"), Some(vec![]));
    assert!(parts("27.10") > parts("27.9"));
    assert!(parts("27.2.1") > parts("27.2"));
    for bad in ["", "27.", "27.2b", "-1", "99999999999999999999.1"] {
        assert_eq!(parts(bad), None, "{bad}");
    }
}

/// Proves: the name an archive is found by is `Xcode`, `_` or `-`, a version and `.xip`, and nothing
/// else: a tag, a build, another case, another suffix, a missing separator or a missing version
/// are not it.
#[test]
fn only_the_plain_name_is_an_archive_name() {
    assert_eq!(named("Xcode_27.2.xip"), Some(vec![27, 2]));
    assert_eq!(named("Xcode-26.1.1.xip"), Some(vec![26, 1, 1]));
    assert_eq!(named("Xcode_27.2.0.xip"), Some(vec![27, 2]));
    for bad in [
        "Xcode_27.3_Release_Candidate.xip",
        "Xcode-27.2.0+27B52.xip",
        "xcode_27.2.xip",
        "Xcode_27.2.XIP",
        "Xcode_27.2.xip.part",
        "Xcode27.2.xip",
        "Xcode_.xip",
        "Xcode_27.2",
        "Xcode 27.2.xip",
    ] {
        assert_eq!(named(bad), None, "{bad}");
    }
}

/// Proves: a size is told in the unit of its own magnitude, B without decimals and KB, MB and GB
/// with two, powers of 1000.
#[test]
fn a_size_is_told_in_its_own_unit() {
    assert_eq!(size_text(0), "0 B");
    assert_eq!(size_text(999), "999 B");
    assert_eq!(size_text(1000), "1.00 KB");
    assert_eq!(size_text(200_000), "200.00 KB");
    assert_eq!(size_text(6_200_000), "6.20 MB");
    assert_eq!(size_text(11_920_523_904), "11.92 GB");
    assert_eq!(size_text(5_000_000_000_000), "5000.00 GB");
}

/// A clock that moves 2 seconds with every reading.
fn clock() -> Duration {
    use std::cell::Cell;
    thread_local!(static NOW: Cell<u64> = const { Cell::new(0) });
    NOW.with(|now| {
        let reading = now.get();
        now.set(reading + 2);
        Duration::from_secs(reading)
    })
}

/// Proves: off a terminal a line is written when a tenth is passed and not again within it, with
/// the sizes in the unit of the total, the whole percent and the rate from the clock; a jump over
/// several tenths is one line; the end of the archive is a line; and nothing is written to end it.
#[test]
fn a_log_gets_one_line_per_tenth() {
    let mut out = Vec::new();
    let mut progress = Progress::new("uploading", 999, false, clock);
    progress.at(50, &mut out);
    progress.at(99, &mut out);
    progress.at(100, &mut out);
    progress.at(150, &mut out);
    progress.at(450, &mut out);
    progress.at(999, &mut out);
    progress.at(999, &mut out);
    progress.finish(&mut out);
    assert_eq!(
        String::from_utf8(out).expect("text"),
        "hk: uploading 100 of 999 B (10%), 50 B/s\n\
         hk: uploading 450 of 999 B (45%), 112 B/s\n\
         hk: uploading 999 of 999 B (100%), 166 B/s\n"
    );
}

/// Proves: on a terminal the line is rewritten (a carriage return, the text, the clear-to-end
/// sequence) each time the whole percent changes and not otherwise, and a newline ends it once; an
/// empty archive is 100% at once; more bytes than the total never go above 100%; and a progress
/// that never drew ends with nothing.
#[test]
fn a_terminal_gets_one_line_rewritten_per_percent() {
    let mut out = Vec::new();
    let mut progress = Progress::new("hashing", 400, true, clock);
    progress.at(1, &mut out);
    progress.at(3, &mut out);
    progress.at(4, &mut out);
    progress.at(999, &mut out);
    progress.at(999, &mut out);
    progress.finish(&mut out);
    progress.finish(&mut out);
    assert_eq!(
        String::from_utf8(out).expect("text"),
        "\rhashing 1 of 400 B (0%), 0 B/s\x1b[K\
         \rhashing 4 of 400 B (1%), 1 B/s\x1b[K\
         \rhashing 400 of 400 B (100%), 66 B/s\x1b[K\n"
    );
    let mut out = Vec::new();
    let mut empty = Progress::new("hashing", 0, true, clock);
    empty.finish(&mut out);
    assert!(out.is_empty());
    empty.at(0, &mut out);
    assert_eq!(
        String::from_utf8(out).expect("text"),
        "\rhashing 0 of 0 B (100%), 0 B/s\x1b[K"
    );
}

/// Proves: a record is used only by its owner: the same record, with the user running `hk` being
/// another user than its owner, gives no release.
#[test]
fn a_record_of_another_owner_is_not_used() {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    let dir = hostkeeper_testkit::Scratch::under(&target.join("tmp"), "hk-record-owner")
        .expect("a scratch directory");
    let path = dir.path().join("record.json");
    let sha = "a".repeat(64);
    std::fs::write(
        &path,
        format!(
            "{{\"archive\":{{\"sha256\":\"{sha}\",\"size\":5}},\"verdict\":\"pass\",\
             \"expansion\":{{\"version\":\"27.2\",\"build\":\"27B52\"}}}}"
        ),
    )
    .expect("a record");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("a mode");
    let owner = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(&path).expect("a file"));
    assert_eq!(
        release_of(&path, owner, &sha, 5),
        Some(("27.2".to_owned(), "27B52".to_owned()))
    );
    assert_eq!(release_of(&path, owner + 1, &sha, 5), None);
}

/// Proves: a clock that has not moved since the start gives a line with no rate.
#[test]
fn a_clock_that_has_not_moved_gives_no_rate() {
    let mut out = Vec::new();
    let mut progress = Progress::new("uploading", 10, false, || Duration::ZERO);
    progress.at(10, &mut out);
    assert_eq!(
        String::from_utf8(out).expect("text"),
        "hk: uploading 10 of 10 B (100%)\n"
    );
}

/// Proves: a progress that resumes at a byte says no line for the tenths before it, counts the
/// rate over the bytes sent since, and a size is told as `<done> of <total> <unit>` in the unit of
/// the total (never more than the total).
#[test]
fn a_resumed_progress_skips_the_tenths_passed_and_rates_the_bytes_sent_since() {
    assert_eq!(of_text(7_310_000_000, 11_920_000_000), "7.31 of 11.92 GB");
    assert_eq!(of_text(0, 999), "0 of 999 B");
    assert_eq!(of_text(5000, 999), "999 of 999 B");
    let mut out = Vec::new();
    let mut progress = Progress::new("uploading", 1000, false, clock).from(400);
    progress.at(450, &mut out);
    progress.at(500, &mut out);
    progress.at(1000, &mut out);
    assert_eq!(
        String::from_utf8(out).expect("text"),
        "hk: uploading 0.50 of 1.00 KB (50%), 50 B/s\n\
         hk: uploading 1.00 of 1.00 KB (100%), 150 B/s\n"
    );
}
