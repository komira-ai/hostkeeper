use std::cell::Cell;
use std::fs::File;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, symlink};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_hook::{Class, Outcome};

use super::*;
use crate::ports::{Got, Limit, NetError, Sink};
use crate::testing::{
    Failing, FakeClock, FakeFetcher, Serve, config, deliver, full, listing, order, scratch, serve,
};

const ARCHIVE: &[u8] = b"the bytes of tierstore 1.5.0";
const URL: &str = "https://releases.example/tierstore/tierstore-1.5.0.tar.zst";

/// The bounds of these tests: 64 bytes, a fetch timeout of 50 s.
fn bounds() -> Bounds {
    Bounds {
        max_package_bytes: 64,
        fetch_timeout_secs: 50,
        ..Bounds::default()
    }
}

/// Fetches `order` from `fetcher` with `bounds` into `root`, at the clock's time.
fn fetch(
    fetcher: &FakeFetcher,
    clock: &FakeClock,
    bounds: &Bounds,
    root: &Path,
    order: &PackageOrder,
) -> Result<StagedArchive, PackageFailure> {
    fetch_into(fetcher, clock, bounds, root, order, stage::plain)
}

fn fetch_into(
    fetcher: &FakeFetcher,
    clock: &FakeClock,
    bounds: &Bounds,
    root: &Path,
    order: &PackageOrder,
    writer: OpenWriter,
) -> Result<StagedArchive, PackageFailure> {
    let channel = config("https://releases.example", &"0".repeat(64)).release_channel;
    let env = FetchEnv {
        fetcher,
        clock,
        bounds,
        release_channel: &channel,
        packages_root: root,
    };
    fetch_with(&env, order, writer)
}

fn tierstore() -> PackageOrder {
    order("tierstore", "1.5.0", ARCHIVE)
}

fn deferred(reason: &'static str) -> PackageFailure {
    PackageFailure {
        outcome: Outcome::Deferred,
        class: Class::Environment,
        reason,
    }
}

fn failed(reason: &'static str) -> PackageFailure {
    PackageFailure {
        outcome: Outcome::Failure,
        class: Class::Release,
        reason,
    }
}

/// The names in the service's directory.
fn left(root: &Path) -> Vec<String> {
    listing(&root.join("tierstore"))
}

/// Catches a URL built any other way than the design's: the channel, the service, then the
/// service and version with `.tar.zst`, whether or not the channel ends in `/`, under a channel
/// with a path, and with a version whose dots and `+` stay inside its one segment.
#[test]
fn fetch_requests_exactly_the_channel_service_version_url() {
    let scratch = scratch("fetch-url");
    for channel in ["https://releases.example", "https://releases.example/"] {
        let fetcher = FakeFetcher::new().route(URL, serve(ARCHIVE));
        let channel = config(channel, &"0".repeat(64)).release_channel;
        let env = FetchEnv {
            fetcher: &fetcher,
            clock: &FakeClock::at(1),
            bounds: &bounds(),
            release_channel: &channel,
            packages_root: scratch.path(),
        };
        fetch_package(&env, &tierstore()).expect("fetched");
        let urls: Vec<String> = fetcher.calls().into_iter().map(|call| call.url).collect();
        assert_eq!(urls, [URL]);
    }
    let with_path = config("https://releases.example/pkg/", &"0".repeat(64)).release_channel;
    let odd = order("svc_2-b", "1..2+x", b"");
    assert_eq!(
        package_url(&with_path, &odd.service, &odd.version).as_str(),
        "https://releases.example/pkg/svc_2-b/svc_2-b-1..2+x.tar.zst"
    );
}

/// Catches a staged archive that is not where later steps look for it, not the bytes, or
/// readable by others: the file is in `packages/<service>/` (made when missing), holds the
/// bytes, is mode 0600, and carries the order's digest and the byte count. Dropping it removes
/// it.
#[test]
fn a_good_download_is_staged_hashed_and_returned() {
    let scratch = scratch("fetch-good");
    let fetcher = FakeFetcher::new().route(URL, serve(ARCHIVE));
    let order = tierstore();
    let staged = fetch(
        &fetcher,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &order,
    )
    .expect("fetched");
    let path = staged.file.path().to_owned();
    assert_eq!(
        path.parent(),
        Some(scratch.path().join("tierstore").as_path())
    );
    assert_eq!(staged.dir, scratch.path().join("tierstore"));
    assert!(left(scratch.path())[0].starts_with(".fetch-"));
    assert_eq!(std::fs::read(&path).expect("the staged file"), ARCHIVE);
    let mode = std::fs::metadata(&path).expect("the staged file").mode();
    assert_eq!(mode & 0o7777, 0o600);
    assert_eq!(staged.sha256, order.digest);
    assert_eq!(staged.size, 28);
    assert_eq!(staged.url.as_str(), URL);
    drop(staged);
    assert!(left(scratch.path()).is_empty());
}

/// Catches the hash not being compared, or the refused file staying: other bytes for the version
/// are `digest_mismatch`, a release failure, and nothing is left in the service's directory.
#[test]
fn a_changed_artifact_for_a_version_is_refused() {
    let scratch = scratch("fetch-changed");
    let fetcher = FakeFetcher::new().route(URL, serve(b"other bytes of the same length!"));
    let refused = fetch(
        &fetcher,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
    )
    .expect_err("other bytes");
    assert_eq!(refused, failed(DIGEST_MISMATCH));
    assert!(left(scratch.path()).is_empty());

    let short = FakeFetcher::new().route(URL, serve(&ARCHIVE[..10]));
    let refused = fetch(
        &short,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
    )
    .expect_err("a short read that ends cleanly");
    assert_eq!(refused, failed(DIGEST_MISMATCH));
    assert!(left(scratch.path()).is_empty());
}

/// Serves `body` (announced or not) in chunks of 4 bytes, and records in `given` how many bytes
/// the sink took before it stopped the transfer.
fn counted(body: Vec<u8>, announce: bool, given: &Rc<Cell<u64>>) -> Serve {
    let given = Rc::clone(given);
    Box::new(move |sink: &mut dyn Sink, _: &Limit| {
        let (got, bytes) = deliver(sink, &body, announce, 4);
        given.set(bytes);
        Ok(got)
    })
}

/// Catches a size cap checked only after the download: a length header over
/// `max_package_bytes` is refused before any byte is taken, and a body with no length header is
/// stopped at the first chunk that passes the cap (64 bytes, so 68 bytes in chunks of 4 at
/// most). Both are `digest_mismatch` and leave no file.
#[test]
fn a_package_over_the_size_cap_is_refused_before_it_is_read() {
    let scratch = scratch("fetch-cap");
    let given = Rc::new(Cell::new(u64::MAX));
    let headed = FakeFetcher::new().route(URL, counted(vec![1; 65], true, &given));
    let refused = fetch(
        &headed,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
    )
    .expect_err("over the cap by its header");
    assert_eq!((refused, given.get()), (failed(DIGEST_MISMATCH), 0));
    assert!(left(scratch.path()).is_empty());

    let streamed = FakeFetcher::new().route(URL, counted(vec![1; 1000], false, &given));
    let refused = fetch(
        &streamed,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
    )
    .expect_err("over the cap as it streams");
    assert_eq!((refused, given.get()), (failed(DIGEST_MISMATCH), 68));
    assert!(left(scratch.path()).is_empty());
}

/// Catches an off-by-one at the cap: a package of exactly `max_package_bytes`, announced or
/// not, is accepted.
#[test]
fn a_package_exactly_at_the_size_cap_is_accepted() {
    let scratch = scratch("fetch-at-cap");
    let body = vec![9_u8; 64];
    let order = order("tierstore", "1.5.0", &body);
    for announce in [true, false] {
        let given = Rc::new(Cell::new(0));
        let fetcher = FakeFetcher::new().route(URL, counted(body.clone(), announce, &given));
        let staged = fetch(
            &fetcher,
            &FakeClock::at(1),
            &bounds(),
            scratch.path(),
            &order,
        )
        .expect("exactly at the cap");
        assert_eq!((staged.size, given.get()), (64, 64));
    }
}

/// Catches a deadline read from the system clock, or not enforced: the fake fetcher moves the
/// injected clock past `fetch_timeout_secs` during the transfer, and the next chunk stops it.
/// The order is deferred with `fetch_failed`, nothing is left, and no second fetch is started.
/// No real time passes.
#[test]
fn a_stalled_fetch_defers_and_frees_the_slot() {
    let scratch = scratch("fetch-stalled");
    let clock = Arc::new(FakeClock::at(1000));
    let stalls = Arc::clone(&clock);
    let given = Rc::new(Cell::new(0));
    let taken = Rc::clone(&given);
    let fetcher = FakeFetcher::new().route(
        URL,
        Box::new(move |sink: &mut dyn Sink, _: &Limit| {
            sink.begin(None).expect("begin");
            sink.chunk(b"the first").expect("in time");
            taken.set(1);
            stalls.advance(51);
            Ok(sink
                .chunk(b" and the rest")
                .map_or(Got::Stopped, |()| Got::Complete))
        }),
    );
    let refused = fetch(&fetcher, &clock, &bounds(), scratch.path(), &tierstore())
        .expect_err("stalled past the deadline");
    assert_eq!(refused, deferred(FETCH_FAILED));
    assert_eq!(given.get(), 1);
    assert_eq!(fetcher.calls().len(), 1);
    assert!(left(scratch.path()).is_empty());
}

/// Catches a network failure that fails the release: no connection, a status (a 404 too: the
/// channel may not have the version yet), a body cut short, the adapter's own deadline and a
/// read error are all deferred with `fetch_failed`, and an adapter claiming the sink stopped it
/// when it did not is too. Nothing is left.
#[test]
fn connect_refused_status_error_and_truncated_body_all_defer_with_fetch_failed() {
    let scratch = scratch("fetch-net");
    for error in [
        NetError::Connect,
        NetError::Status(404),
        NetError::Status(500),
        NetError::Truncated,
        NetError::DeadlineExceeded,
        NetError::Io,
    ] {
        let fetcher =
            FakeFetcher::new().route(URL, Box::new(move |_: &mut dyn Sink, _: &Limit| Err(error)));
        let refused = fetch(
            &fetcher,
            &FakeClock::at(1),
            &bounds(),
            scratch.path(),
            &tierstore(),
        )
        .expect_err("a network failure");
        assert_eq!(refused, deferred(FETCH_FAILED), "{error:?}");
        assert!(left(scratch.path()).is_empty(), "{error:?}");
    }
    let claims = FakeFetcher::new().route(
        URL,
        Box::new(|_: &mut dyn Sink, _: &Limit| Ok(Got::Stopped)),
    );
    let refused = fetch(
        &claims,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
    )
    .expect_err("an unexplained stop");
    assert_eq!(refused, deferred(FETCH_FAILED));
}

/// Catches a connect timeout other than the design's 30 s, or a deadline that is not
/// `fetch_timeout_secs` from the start of the fetch on the injected clock.
#[test]
fn a_connect_timeout_of_30_seconds_is_passed_to_the_adapter() {
    let scratch = scratch("fetch-limit");
    let fetcher = FakeFetcher::new().route(URL, serve(ARCHIVE));
    let clock = FakeClock::at(700);
    fetch(&fetcher, &clock, &bounds(), scratch.path(), &tierstore()).expect("fetched");
    let calls = fetcher.calls();
    assert_eq!(calls[0].limit.connect_timeout, Duration::from_secs(30));
    assert_eq!(calls[0].limit.deadline, clock.stamp(750));
}

fn read_only(_: &File) -> Box<dyn Write + '_> {
    Box::new(Failing(std::io::ErrorKind::ReadOnlyFilesystem))
}

/// Catches a full disk reported as `fetch_failed`, or the partial file left behind: a write of
/// the staging file that fails with `StorageFull` (`ENOSPC`) defers with `no_space` and leaves
/// nothing; another write error defers with `fetch_failed`.
#[test]
fn enospc_while_staging_defers_with_no_space_and_leaves_no_partial_file() {
    let scratch = scratch("fetch-enospc");
    let fetcher = FakeFetcher::new().route(URL, serve(ARCHIVE));
    let refused = fetch_into(
        &fetcher,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
        full,
    )
    .expect_err("a full disk");
    assert_eq!(refused, deferred(NO_SPACE));
    assert!(left(scratch.path()).is_empty());

    let refused = fetch_into(
        &fetcher,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
        read_only,
    )
    .expect_err("a read-only disk");
    assert_eq!(refused, deferred(FETCH_FAILED));
    assert!(left(scratch.path()).is_empty());
}

/// Catches an orphan sweep that is too wide, too narrow, or follows a link: before a new fetch
/// the service's `.fetch-*` files and links go (the link's target stays); a `.fetch-*`
/// directory, a `.stage-*` directory, a version directory, `current` and another service's
/// `.fetch-*` stay.
#[test]
fn an_orphan_fetch_file_is_removed_before_a_new_fetch_and_only_that() {
    let scratch = scratch("fetch-orphans");
    let root = scratch.path();
    let dir = root.join("tierstore");
    let other = root.join("other");
    for made in [
        &dir,
        &other,
        &dir.join(".stage-1"),
        &dir.join("1.4.2"),
        &dir.join(".fetch-dir"),
    ] {
        std::fs::create_dir_all(made).expect("a directory");
    }
    let outside = root.join("outside");
    std::fs::write(&outside, b"keep").expect("a file outside");
    std::fs::write(dir.join(".fetch-1-0"), b"partial").expect("an orphan");
    symlink(&outside, dir.join(".fetch-x")).expect("an orphan link");
    symlink("1.4.2", dir.join("current")).expect("current");
    std::fs::write(other.join(".fetch-9-9"), b"theirs").expect("another service's orphan");

    let fetcher = FakeFetcher::new().route(URL, serve(ARCHIVE));
    let staged =
        fetch(&fetcher, &FakeClock::at(1), &bounds(), root, &tierstore()).expect("fetched");
    let staging = staged
        .file
        .path()
        .file_name()
        .expect("a name")
        .to_string_lossy()
        .into_owned();
    let mut kept = vec![
        ".fetch-dir",
        ".stage-1",
        "1.4.2",
        "current",
        staging.as_str(),
    ];
    kept.sort_unstable();
    assert_eq!(listing(&dir), kept);
    assert_eq!(std::fs::read(&outside).expect("the link's target"), b"keep");
    assert_eq!(listing(&other), [".fetch-9-9"]);

    remove_orphan_fetches(root, &order("absent", "1", b"").service).expect("no directory");
}

/// Catches a service directory that is a link being written through or swept: the fetch is
/// deferred with `fetch_failed`, the sweep refuses it, and the link's target is not touched.
#[test]
fn a_service_directory_that_is_a_link_is_not_used() {
    let scratch = scratch("fetch-linked");
    let elsewhere = scratch.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("a directory");
    std::fs::write(elsewhere.join(".fetch-1-1"), b"keep").expect("a file");
    symlink(&elsewhere, scratch.path().join("tierstore")).expect("a linked service directory");
    let fetcher = FakeFetcher::new().route(URL, serve(ARCHIVE));
    let refused = fetch(
        &fetcher,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
    )
    .expect_err("a link");
    assert_eq!(refused, deferred(FETCH_FAILED));
    let swept = remove_orphan_fetches(scratch.path(), &tierstore().service).expect_err("a link");
    assert_eq!(
        swept.to_string(),
        format!(
            "{} is not a directory",
            scratch.path().join("tierstore").display()
        )
    );
    assert_eq!(listing(&elsewhere), [".fetch-1-1"]);
    assert!(fetcher.calls().is_empty());
}

/// Catches a refusal whose text loses its outcome, class or reason.
#[test]
fn a_refusal_reads_as_its_outcome_class_and_reason() {
    assert_eq!(
        deferred(FETCH_FAILED).to_string(),
        "Deferred, class environment, reason fetch_failed"
    );
    assert_eq!(
        failed(DIGEST_MISMATCH).to_string(),
        "Failure, class release, reason digest_mismatch"
    );
    assert_eq!(PackageFailure::release(NO_SPACE), failed(NO_SPACE));
}

/// Catches a fetch that reaches a URL the channel does not serve and goes on: the fake
/// channel answers 404, the fetch is deferred with `fetch_failed`, and the URL asked is the
/// archive's.
#[test]
fn a_version_the_channel_does_not_have_yet_is_deferred() {
    let scratch = scratch("fetch-absent");
    let fetcher = FakeFetcher::new();
    let refused = fetch(
        &fetcher,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
    )
    .expect_err("a 404");
    assert_eq!(refused, deferred(FETCH_FAILED));
    assert_eq!(fetcher.calls()[0].url, URL);
}
