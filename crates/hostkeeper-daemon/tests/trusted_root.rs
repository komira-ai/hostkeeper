//! The Sigstore trusted root's report: its age on a fake unix clock that the tests set and
//! advance (nothing sleeps and nothing reads the real time), the reason it needs attention, and
//! which of the packaged and the host's files is the one `gh` is given.

use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use hostkeeper_daemon::cli::{DEFAULT_TRUSTED_ROOT_MAX_AGE_DAYS, SIGSTORE_TRUSTED_ROOT};
use hostkeeper_daemon::trusted_root::{
    PACKAGED_TRUSTED_ROOT, RootProblem, RootReport, TRUSTED_ROOT_MEDIA_TYPE, TrustedRoot,
    is_trusted_root,
};
use hostkeeper_link::testing::FakeClock;

/// Seconds in a day.
const DAY: u64 = 86_400;

/// When the tests' root files were written, in unix seconds.
const WRITTEN: u64 = 1_800_000_000;

/// A root as `gh attestation trusted-root` writes it: one trusted root per line.
const ROOT: &str = concat!(
    "{\"mediaType\":\"application/vnd.dev.sigstore.trustedroot+json;version=0.1\",\"tlogs\":[]}\n",
    "{\"mediaType\":\"application/vnd.dev.sigstore.trustedroot+json;version=0.1\"}\n",
);

/// The limit of the tests, in days.
const LIMIT_DAYS: u64 = 90;

fn scratch(name: &str) -> hostkeeper_testkit::Scratch {
    hostkeeper_testkit::Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name)
        .expect("make the scratch dir")
}

/// Writes `text` at `path`, its modification time set to `at` unix seconds.
fn write_at(path: &Path, text: &[u8], at: u64) {
    fs::create_dir_all(path.parent().expect("a parent")).expect("make the parent");
    fs::write(path, text).expect("write the root");
    let time = UNIX_EPOCH + Duration::from_secs(at);
    File::options()
        .write(true)
        .open(path)
        .and_then(|file| file.set_times(FileTimes::new().set_modified(time)))
        .expect("set the modification time");
}

/// The packaged and the host's root files under `dir`, and a root over them aged on `clock`.
fn root_under(dir: &Path, clock: &Arc<FakeClock>) -> (PathBuf, PathBuf, TrustedRoot) {
    let packaged = dir.join("packages").join(PACKAGED_TRUSTED_ROOT);
    let installed = dir.join("usr/lib/hostkeeper/trusted_root.json");
    let root = TrustedRoot::new(
        packaged.clone(),
        installed.clone(),
        LIMIT_DAYS,
        Arc::clone(clock) as Arc<dyn hostkeeper_link::UnixClock>,
    );
    (packaged, installed, root)
}

fn report(path: &Path, age_secs: Option<u64>, problem: Option<RootProblem>) -> RootReport {
    RootReport {
        path: path.to_path_buf(),
        age_secs,
        problem,
    }
}

/// Catches an age not counted from the file's time on the injected clock, a root reported stale
/// at or under the limit, one past the limit not reported `trusted_root_stale`, and a limit
/// counted in other units than days: a root ten days old is fine, the same file exactly at the
/// limit is fine, and one second past it, the clock advanced, it is stale.
#[test]
fn a_root_under_the_limit_reports_its_age_and_past_it_reports_stale() {
    let dir = scratch("root-age");
    let clock = Arc::new(FakeClock::new(WRITTEN + 10 * DAY, 0));
    let (packaged, _, root) = root_under(dir.path(), &clock);
    write_at(&packaged, ROOT.as_bytes(), WRITTEN);

    assert_eq!(root.report(), report(&packaged, Some(10 * DAY), None));
    clock.advance(80 * DAY);
    assert_eq!(
        root.report(),
        report(&packaged, Some(LIMIT_DAYS * DAY), None)
    );
    clock.advance(1);
    let stale = root.report();
    assert_eq!(
        stale,
        report(
            &packaged,
            Some(LIMIT_DAYS * DAY + 1),
            Some(RootProblem::Stale)
        )
    );
    assert_eq!(
        stale.problem.map(RootProblem::token),
        Some("trusted_root_stale")
    );
    assert_eq!(root.path(), packaged);
}

/// Catches a missing root reported with an age, as fine, or under the token of another problem,
/// and a missing root that hands `gh` a path other than the host's: with neither file present
/// the report is `trusted_root_missing` with no age, and `gh` is given the host's path.
#[test]
fn a_missing_root_reports_missing_and_no_age() {
    let dir = scratch("root-missing");
    let clock = Arc::new(FakeClock::new(WRITTEN, 0));
    let (_, installed, root) = root_under(dir.path(), &clock);
    let missing = root.report();
    assert_eq!(
        missing,
        report(&installed, None, Some(RootProblem::Missing))
    );
    assert_eq!(
        missing.problem.map(RootProblem::token),
        Some("trusted_root_missing")
    );
    assert_eq!(root.path(), installed);
}

/// Catches a file that is not a trusted root taken for one, or reported under another reason
/// than `trusted_root_unparseable`, and an unparseable root whose age is dropped or whose
/// staleness hides it: each malformed file reports its age and `trusted_root_unparseable`,
/// also when it is past the limit, and so does a root that cannot be read (a directory).
#[test]
fn a_malformed_or_unreadable_root_reports_unparseable_with_its_age() {
    let malformed: [&[u8]; 9] = [
        b"",
        b"\n\n",
        b"{\"mediaType\":",
        b"not json",
        b"[1, 2]",
        b"{\"tlogs\":[]}",
        b"{\"mediaType\":7}",
        b"{\"mediaType\":\"application/vnd.dev.sigstore.bundle.v0.3+json\"}",
        b"{\"mediaType\":\"application/vnd.dev.sigstore.trustedroot+json\"}\n{}\n",
    ];
    for (n, text) in malformed.iter().enumerate() {
        let dir = scratch(&format!("root-malformed-{n}"));
        let clock = Arc::new(FakeClock::new(WRITTEN + DAY, 0));
        let (packaged, _, root) = root_under(dir.path(), &clock);
        write_at(&packaged, text, WRITTEN);
        let unparseable = report(&packaged, Some(DAY), Some(RootProblem::Unparseable));
        assert_eq!(root.report(), unparseable, "{n}");
        clock.advance(LIMIT_DAYS * DAY);
        assert_eq!(
            root.report().problem,
            Some(RootProblem::Unparseable),
            "{n}: unparseable wins over stale"
        );
    }
    assert_eq!(RootProblem::Unparseable.token(), "trusted_root_unparseable");

    let dir = scratch("root-unreadable");
    let clock = Arc::new(FakeClock::new(WRITTEN, 0));
    let (packaged, _, root) = root_under(dir.path(), &clock);
    fs::create_dir_all(&packaged).expect("a directory where the root should be");
    let unreadable = root.report();
    assert_eq!(unreadable.path, packaged);
    assert_eq!(unreadable.problem, Some(RootProblem::Unparseable));
}

/// Catches a choice of file that keeps an old root over a newer one: a host root written after
/// the packaged one is used (an operator's copy after a rotation), the packaged one when it is
/// newer or written in the same second, either one alone when it is the only one; and an age
/// counted from the file not chosen.
#[test]
fn the_root_written_last_is_used_and_the_packaged_one_on_a_tie() {
    let dir = scratch("root-choice");
    let clock = Arc::new(FakeClock::new(WRITTEN + 100 * DAY, 0));
    let (packaged, installed, root) = root_under(dir.path(), &clock);

    write_at(&installed, ROOT.as_bytes(), WRITTEN);
    assert_eq!(root.path(), installed, "the host's root alone");
    assert_eq!(
        root.report(),
        report(&installed, Some(100 * DAY), Some(RootProblem::Stale))
    );

    write_at(&packaged, ROOT.as_bytes(), WRITTEN + 20 * DAY);
    assert_eq!(root.path(), packaged, "the packaged root is newer");
    assert_eq!(root.report(), report(&packaged, Some(80 * DAY), None));

    write_at(&installed, ROOT.as_bytes(), WRITTEN + 20 * DAY);
    assert_eq!(root.path(), packaged, "a tie goes to the packaged root");

    write_at(&installed, ROOT.as_bytes(), WRITTEN + 30 * DAY);
    assert_eq!(root.path(), installed, "the host's root is newer");
    assert_eq!(root.report(), report(&installed, Some(70 * DAY), None));

    fs::remove_file(&installed).expect("remove the host's root");
    assert_eq!(root.path(), packaged, "the packaged root alone");
}

/// Catches an age that wraps around when the clock is behind the file's time (a clock set back,
/// or a file from a host with a clock ahead), and a limit in days that overflows when turned
/// into seconds: the age is 0 and the root is fine, and the largest limit never goes stale.
#[test]
fn a_clock_behind_the_file_gives_age_zero_and_a_huge_limit_does_not_overflow() {
    let dir = scratch("root-clock-behind");
    let clock = Arc::new(FakeClock::new(WRITTEN - DAY, 0));
    let (packaged, installed, root) = root_under(dir.path(), &clock);
    write_at(&packaged, ROOT.as_bytes(), WRITTEN);
    assert_eq!(root.report(), report(&packaged, Some(0), None));

    let forever = TrustedRoot::new(
        packaged.clone(),
        installed,
        u64::MAX,
        Arc::clone(&clock) as Arc<dyn hostkeeper_link::UnixClock>,
    );
    clock.set_unix(u64::MAX);
    assert_eq!(
        forever.report(),
        report(&packaged, Some(u64::MAX - WRITTEN), None)
    );
}

/// Catches a parse that refuses what `gh attestation trusted-root` writes (one compact root per
/// line, or one root written over several lines) or accepts a stream with one value that is not
/// a root among roots; and a media type other than the one Sigstore names trusted roots by.
#[test]
fn a_stream_of_trusted_roots_parses_on_one_line_each_or_over_several() {
    assert_eq!(
        TRUSTED_ROOT_MEDIA_TYPE,
        "application/vnd.dev.sigstore.trustedroot+json"
    );
    assert!(is_trusted_root(ROOT.as_bytes()));
    assert!(is_trusted_root(
        b"{\n  \"mediaType\": \"application/vnd.dev.sigstore.trustedroot+json;version=0.1\"\n}\n"
    ));
    assert!(!is_trusted_root(
        format!("{ROOT}{{\"mediaType\":\"application/json\"}}\n").as_bytes()
    ));
    assert!(!is_trusted_root(format!("{ROOT}}}").as_bytes()));
}

/// Catches a committed root that the daemon would report unparseable, and a packaged path or a
/// default limit that moved without the docs and the package: the root the release packages
/// parses, the daemon reads it under `hostkeeper/current/files/` of the packages directory, the
/// host's root stays at `/usr/lib/hostkeeper/trusted_root.json`, and the limit is 90 days.
#[test]
fn the_committed_root_parses_and_the_paths_and_limit_are_the_documented_ones() {
    let committed = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/sigstore/trusted_root.json"),
    )
    .expect("the committed root");
    assert!(is_trusted_root(&committed));
    assert_eq!(
        PACKAGED_TRUSTED_ROOT,
        "hostkeeper/current/files/trusted_root.json"
    );
    assert_eq!(
        SIGSTORE_TRUSTED_ROOT,
        "/usr/lib/hostkeeper/trusted_root.json"
    );
    assert_eq!(DEFAULT_TRUSTED_ROOT_MAX_AGE_DAYS.get(), 90);
}
