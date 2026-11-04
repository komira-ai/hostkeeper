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
    CHANNEL, Failing, FakeClock, FakeFetcher, HOST, Serve, config, deliver, digest, full, index_at,
    index_json, listing, order, package_at, published, scratch, serve, sha256_hex,
};

const ARCHIVE: &[u8] = b"the bytes of tierstore 1.5.0";

/// The URL of the release index of the tests' order.
fn at_index() -> String {
    index_at("tierstore", "1.5.0")
}

/// The URL of the tests' order's package for [`HOST`].
fn at_package() -> String {
    package_at("tierstore", "1.5.0")
}

/// The bounds of these tests: 64 bytes, a fetch timeout of 50 s.
fn bounds() -> Bounds {
    Bounds {
        max_package_bytes: 64,
        fetch_timeout_secs: 50,
        ..Bounds::default()
    }
}

/// Fetches `order` from `fetcher` for [`HOST`] with `bounds` into `root`, at the clock's time.
fn fetch(
    fetcher: &FakeFetcher,
    clock: &FakeClock,
    bounds: &Bounds,
    root: &Path,
    order: &PackageOrder,
) -> Result<StagedArchive, PackageFailure> {
    fetch_into(fetcher, clock, bounds, root, order, HOST, stage::plain)
}

fn fetch_into(
    fetcher: &FakeFetcher,
    clock: &FakeClock,
    bounds: &Bounds,
    root: &Path,
    order: &PackageOrder,
    platform: Platform,
    writer: OpenWriter,
) -> Result<StagedArchive, PackageFailure> {
    let channel = config(CHANNEL, &"0".repeat(64)).release_channel;
    let env = FetchEnv {
        fetcher,
        clock,
        bounds,
        release_channel: &channel,
        packages_root: root,
        platform,
    };
    fetch_with(&env, order, writer)
}

fn tierstore() -> PackageOrder {
    order("tierstore", "1.5.0")
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

/// The URLs `fetcher` was asked for, in order.
fn urls(fetcher: &FakeFetcher) -> Vec<String> {
    fetcher.calls().into_iter().map(|call| call.url).collect()
}

/// A fake channel whose index names [`ARCHIVE`] for [`HOST`] and whose package is `package`.
fn with_package(package: Serve) -> FakeFetcher {
    FakeFetcher::new()
        .route(
            &at_index(),
            serve(index_json("tierstore", "1.5.0", &[(HOST, ARCHIVE)]).as_bytes()),
        )
        .route(&at_package(), package)
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

/// Answers every request with the network error `error`.
fn failing(error: NetError) -> Serve {
    Box::new(move |_: &mut dyn Sink, _: &Limit| Err(error))
}

/// Catches a URL built any other way than the layout's: the index and then the host's package,
/// each at `<channel>/<service>/<version>/`, whether or not the channel ends in `/`, under a
/// channel with a path, and with a version whose dots and `+` stay inside its one segment.
#[test]
fn fetch_requests_exactly_the_index_and_the_platform_package_urls() {
    let scratch = scratch("fetch-url");
    for channel in [CHANNEL, "https://releases.example/"] {
        let fetcher = published("tierstore", "1.5.0", ARCHIVE);
        let channel = config(channel, &"0".repeat(64)).release_channel;
        let env = FetchEnv {
            fetcher: &fetcher,
            clock: &FakeClock::at(1),
            bounds: &bounds(),
            release_channel: &channel,
            packages_root: scratch.path(),
            platform: HOST,
        };
        fetch_package(&env, &tierstore()).expect("fetched");
        assert_eq!(urls(&fetcher), [at_index(), at_package()]);
    }
    let with_path = config("https://releases.example/pkg/", &"0".repeat(64)).release_channel;
    let odd = order("svc_2-b", "1..2+x");
    assert_eq!(
        index_url(&with_path, &odd.service, &odd.version).as_str(),
        "https://releases.example/pkg/svc_2-b/1..2+x/index.json"
    );
    assert_eq!(
        package_url(&with_path, &odd.service, &odd.version, HOST).as_str(),
        "https://releases.example/pkg/svc_2-b/1..2+x/svc_2-b-1..2+x-linux-x86_64.tar.zst"
    );
}

/// Catches a staged archive that is not where later steps look for it, not the bytes, or
/// readable by others: the package is in `packages/<service>/` (made when missing), holds the
/// bytes, is mode 0600, and carries the host's platform, the index entry's digest, the byte
/// count and the package URL. The index's staging file is gone, and dropping the package removes
/// it.
#[test]
fn a_good_download_is_staged_hashed_and_returned() {
    let scratch = scratch("fetch-good");
    let fetcher = published("tierstore", "1.5.0", ARCHIVE);
    let staged = fetch(
        &fetcher,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
    )
    .expect("fetched");
    let path = staged.file.path().to_owned();
    assert_eq!(
        path.parent(),
        Some(scratch.path().join("tierstore").as_path())
    );
    assert_eq!(staged.dir, scratch.path().join("tierstore"));
    let name = path.file_name().expect("a name").to_string_lossy();
    assert_eq!(left(scratch.path()), [&*name]);
    assert!(name.starts_with(".fetch-"));
    assert_eq!(std::fs::read(&path).expect("the staged file"), ARCHIVE);
    let mode = std::fs::metadata(&path).expect("the staged file").mode();
    assert_eq!(mode & 0o7777, 0o600);
    assert_eq!(staged.platform, HOST);
    assert_eq!(staged.sha256, digest(&sha256_hex(ARCHIVE)));
    assert_eq!(staged.size, 28);
    assert_eq!(staged.url.as_str(), at_package());
    drop(staged);
    assert!(left(scratch.path()).is_empty());
}

/// Catches the hash or the size not being compared with the index's entry, or the refused file
/// staying: other bytes of the entry's length, and a body that is shorter and ends cleanly, are
/// `package_digest_mismatch`, a release failure, and nothing is left in the service's directory.
#[test]
fn a_changed_artifact_for_a_version_is_refused() {
    let scratch = scratch("fetch-changed");
    let fetcher = with_package(serve(b"the bytes of tierstore 1.5.1"));
    let refused = fetch(
        &fetcher,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
    )
    .expect_err("other bytes");
    assert_eq!(refused, failed(PACKAGE_DIGEST_MISMATCH));
    assert_eq!(urls(&fetcher), [at_index(), at_package()]);
    assert!(left(scratch.path()).is_empty());

    let short = with_package(serve(&ARCHIVE[..10]));
    let refused = fetch(
        &short,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
    )
    .expect_err("a short read that ends cleanly");
    assert_eq!(refused, failed(PACKAGE_DIGEST_MISMATCH));
    assert!(left(scratch.path()).is_empty());
}

/// Catches a size cap checked only after the download: an entry whose size is over
/// `max_package_bytes` is refused with only the index requested; a body longer than the entry's
/// size is refused by its length header before any byte is taken, and a body with no length
/// header is stopped at the first chunk past the entry's size (28 bytes, so 32 bytes in chunks
/// of 4). All are `package_digest_mismatch` and leave no file.
#[test]
fn a_package_over_the_size_cap_is_refused_before_it_is_read() {
    let scratch = scratch("fetch-cap");
    let huge = published("tierstore", "1.5.0", &[1_u8; 65]);
    let refused = fetch(
        &huge,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
    )
    .expect_err("an entry over the cap");
    assert_eq!(refused, failed(PACKAGE_DIGEST_MISMATCH));
    assert_eq!(urls(&huge), [at_index()]);
    assert!(left(scratch.path()).is_empty());

    for (announce, taken) in [(true, 0), (false, 32)] {
        let given = Rc::new(Cell::new(u64::MAX));
        let fetcher = with_package(counted(vec![1; 1000], announce, &given));
        let refused = fetch(
            &fetcher,
            &FakeClock::at(1),
            &bounds(),
            scratch.path(),
            &tierstore(),
        )
        .expect_err("longer than the entry says");
        assert_eq!(
            (refused, given.get()),
            (failed(PACKAGE_DIGEST_MISMATCH), taken)
        );
        assert!(left(scratch.path()).is_empty());
    }
}

/// Catches an off-by-one at the cap: a package of exactly `max_package_bytes`, announced or
/// not, whose entry says so, is accepted.
#[test]
fn a_package_exactly_at_the_size_cap_is_accepted() {
    let scratch = scratch("fetch-at-cap");
    let body = vec![9_u8; 64];
    for announce in [true, false] {
        let given = Rc::new(Cell::new(0));
        let fetcher = FakeFetcher::new()
            .route(
                &at_index(),
                serve(index_json("tierstore", "1.5.0", &[(HOST, &body)]).as_bytes()),
            )
            .route(&at_package(), counted(body.clone(), announce, &given));
        let staged = fetch(
            &fetcher,
            &FakeClock::at(1),
            &bounds(),
            scratch.path(),
            &tierstore(),
        )
        .expect("exactly at the cap");
        assert_eq!((staged.size, given.get()), (64, 64));
    }
}

/// Catches a deadline read from the system clock, or not enforced: the fake fetcher moves the
/// injected clock past `fetch_timeout_secs` during the package's transfer, and the next chunk
/// stops it. The order is deferred with `fetch_failed`, nothing is left, and no third request is
/// made. No real time passes.
#[test]
fn a_stalled_fetch_defers_and_frees_the_slot() {
    let scratch = scratch("fetch-stalled");
    let clock = Arc::new(FakeClock::at(1000));
    let stalls = Arc::clone(&clock);
    let given = Rc::new(Cell::new(0));
    let taken = Rc::clone(&given);
    let fetcher = with_package(Box::new(move |sink: &mut dyn Sink, _: &Limit| {
        sink.begin(None).expect("begin");
        sink.chunk(b"the first").expect("in time");
        taken.set(1);
        stalls.advance(51);
        Ok(sink
            .chunk(b" and the rest")
            .map_or(Got::Stopped, |()| Got::Complete))
    }));
    let refused = fetch(&fetcher, &clock, &bounds(), scratch.path(), &tierstore())
        .expect_err("stalled past the deadline");
    assert_eq!(refused, deferred(FETCH_FAILED));
    assert_eq!(given.get(), 1);
    assert_eq!(urls(&fetcher), [at_index(), at_package()]);
    assert!(left(scratch.path()).is_empty());
}

/// Catches a network failure that fails the release: no connection, a status (a 404 too: the
/// channel may not have the version yet), a body cut short, the adapter's own deadline and a
/// read error are all deferred with `fetch_failed`, for the index (the package is then never
/// requested) and for the package; so is an adapter claiming the sink stopped it when it did
/// not. Nothing is left.
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
        let no_index = FakeFetcher::new().route(&at_index(), failing(error));
        let refused = fetch(
            &no_index,
            &FakeClock::at(1),
            &bounds(),
            scratch.path(),
            &tierstore(),
        )
        .expect_err("the index failed");
        assert_eq!(refused, deferred(FETCH_FAILED), "{error:?}");
        assert_eq!(urls(&no_index), [at_index()], "{error:?}");
        assert!(left(scratch.path()).is_empty(), "{error:?}");

        let no_package = with_package(failing(error));
        let refused = fetch(
            &no_package,
            &FakeClock::at(1),
            &bounds(),
            scratch.path(),
            &tierstore(),
        )
        .expect_err("the package failed");
        assert_eq!(refused, deferred(FETCH_FAILED), "{error:?}");
        assert!(left(scratch.path()).is_empty(), "{error:?}");
    }
    let claims = with_package(Box::new(|_: &mut dyn Sink, _: &Limit| Ok(Got::Stopped)));
    let refused = fetch(
        &claims,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
    )
    .expect_err("an unexplained stop");
    assert_eq!(refused, deferred(FETCH_FAILED));
    assert!(left(scratch.path()).is_empty());
}

/// Catches a connect timeout other than the design's 30 s, or a deadline that is not
/// `fetch_timeout_secs` from the start of the fetch on the injected clock, or that differs
/// between the two requests: both carry the one limit.
#[test]
fn a_connect_timeout_of_30_seconds_is_passed_to_the_adapter() {
    let scratch = scratch("fetch-limit");
    let fetcher = published("tierstore", "1.5.0", ARCHIVE);
    let clock = FakeClock::at(700);
    fetch(&fetcher, &clock, &bounds(), scratch.path(), &tierstore()).expect("fetched");
    let limit = Limit {
        connect_timeout: Duration::from_secs(30),
        deadline: clock.stamp(750),
    };
    let limits: Vec<Limit> = fetcher.calls().into_iter().map(|call| call.limit).collect();
    assert_eq!(limits, [limit.clone(), limit]);
}

fn read_only(_: &File) -> Box<dyn Write + '_> {
    Box::new(Failing(io::ErrorKind::ReadOnlyFilesystem))
}

thread_local! {
    /// How many staging files this test's thread has opened a writer for.
    static OPENED: Cell<u32> = const { Cell::new(0) };
}

/// A writer that writes to the file for the first staging file (the index) and is `then` for
/// every later one (the package).
fn first_then(file: &File, then: OpenWriter) -> Box<dyn Write + '_> {
    let n = OPENED.get();
    OPENED.set(n + 1);
    if n == 0 {
        stage::plain(file)
    } else {
        then(file)
    }
}

fn package_full(file: &File) -> Box<dyn Write + '_> {
    first_then(file, full)
}

fn package_read_only(file: &File) -> Box<dyn Write + '_> {
    first_then(file, read_only)
}

/// Catches a full disk reported as `fetch_failed`, or the partial file left behind: a write of
/// the staging file, of the index or of the package, that fails with `StorageFull` (`ENOSPC`)
/// defers with `no_space` and leaves nothing; another write error defers with `fetch_failed`.
#[test]
fn enospc_while_staging_defers_with_no_space_and_leaves_no_partial_file() {
    let scratch = scratch("fetch-enospc");
    let fetcher = published("tierstore", "1.5.0", ARCHIVE);
    let writers: [(OpenWriter, PackageFailure); 4] = [
        (full, deferred(NO_SPACE)),
        (read_only, deferred(FETCH_FAILED)),
        (package_full, deferred(NO_SPACE)),
        (package_read_only, deferred(FETCH_FAILED)),
    ];
    for (writer, expected) in writers {
        OPENED.set(0);
        let refused = fetch_into(
            &fetcher,
            &FakeClock::at(1),
            &bounds(),
            scratch.path(),
            &tierstore(),
            HOST,
            writer,
        )
        .expect_err("a disk that fails");
        assert_eq!(refused, expected);
        assert!(left(scratch.path()).is_empty());
    }
}

/// Catches an index that is read from the wrong place, or a failure to read it back being
/// ignored: the staging file of the index is removed while the index is being served, so it
/// cannot be read back, and the order is deferred with `fetch_failed` with the package never
/// requested.
#[test]
fn an_index_that_cannot_be_read_back_defers_with_fetch_failed() {
    let scratch = scratch("fetch-readback");
    let dir = scratch.path().join("tierstore");
    let index = index_json("tierstore", "1.5.0", &[(HOST, ARCHIVE)]).into_bytes();
    let fetcher = FakeFetcher::new().route(
        &at_index(),
        Box::new(move |sink: &mut dyn Sink, _: &Limit| {
            let (got, _) = deliver(sink, &index, true, 4);
            for entry in std::fs::read_dir(&dir).expect("list the directory") {
                std::fs::remove_file(entry.expect("an entry").path()).expect("remove the file");
            }
            Ok(got)
        }),
    );
    let refused = fetch(
        &fetcher,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
    )
    .expect_err("an index that is gone");
    assert_eq!(refused, deferred(FETCH_FAILED));
    assert_eq!(urls(&fetcher), [at_index()]);
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

    let fetcher = published("tierstore", "1.5.0", ARCHIVE);
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

    remove_orphan_fetches(root, &order("absent", "1").service).expect("no directory");
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
    let fetcher = published("tierstore", "1.5.0", ARCHIVE);
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
        failed(PACKAGE_DIGEST_MISMATCH).to_string(),
        "Failure, class release, reason package_digest_mismatch"
    );
    assert_eq!(PackageFailure::release(NO_SPACE), failed(NO_SPACE));
}

/// Catches a fetch that reaches a version the channel does not serve and goes on: the fake
/// channel answers 404 for the index, the fetch is deferred with `fetch_failed`, and the only
/// URL asked is the index's.
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
    assert_eq!(urls(&fetcher), [at_index()]);
    assert!(left(scratch.path()).is_empty());
}

/// Catches an index that is believed when it is not the index of the version asked for, or whose
/// size is not bounded: text that is not JSON, an index of another version, and an index over
/// 64 KiB (refused by its length header before any byte is taken, and, with no header, at the
/// first chunk past 64 KiB) all fail the release with `index_invalid`, the package is never
/// requested, and nothing is left.
#[test]
fn an_invalid_index_fails_the_release_with_index_invalid() {
    let scratch = scratch("fetch-bad-index");
    let other_version = index_json("tierstore", "1.4.0", &[(HOST, ARCHIVE)]).into_bytes();
    for body in [b"not json".to_vec(), other_version] {
        let given = Rc::new(Cell::new(0));
        let fetcher = FakeFetcher::new()
            .route(&at_index(), counted(body, true, &given))
            .route(&at_package(), serve(ARCHIVE));
        let refused = fetch(
            &fetcher,
            &FakeClock::at(1),
            &bounds(),
            scratch.path(),
            &tierstore(),
        )
        .expect_err("not this version's index");
        assert_eq!(refused, failed(INDEX_INVALID));
        assert_eq!(urls(&fetcher), [at_index()]);
        assert!(left(scratch.path()).is_empty());
    }
    for (announce, taken) in [(true, 0), (false, 65_540)] {
        let given = Rc::new(Cell::new(u64::MAX));
        let fetcher = FakeFetcher::new()
            .route(&at_index(), counted(vec![b' '; 70_000], announce, &given))
            .route(&at_package(), serve(ARCHIVE));
        let refused = fetch(
            &fetcher,
            &FakeClock::at(1),
            &bounds(),
            scratch.path(),
            &tierstore(),
        )
        .expect_err("an index over 64 KiB");
        assert_eq!((refused, given.get()), (failed(INDEX_INVALID), taken));
        assert_eq!(urls(&fetcher), [at_index()]);
        assert!(left(scratch.path()).is_empty());
    }
}

/// Catches a host that is given another platform's package, or a missing entry that is not
/// refused: an index with only linux-arm64 and macos-arm64 packages, read by a linux-x86_64
/// host, fails the release with `no_package_for_platform`, the package is never requested, and
/// nothing is left.
#[test]
fn a_host_whose_platform_the_index_lacks_is_refused_with_no_package_for_platform() {
    let scratch = scratch("fetch-no-platform");
    let index = index_json(
        "tierstore",
        "1.5.0",
        &[
            (Platform::LinuxArm64, ARCHIVE),
            (Platform::MacosArm64, ARCHIVE),
        ],
    );
    let fetcher = FakeFetcher::new()
        .route(&at_index(), serve(index.as_bytes()))
        .route(&at_package(), serve(ARCHIVE));
    let refused = fetch(
        &fetcher,
        &FakeClock::at(1),
        &bounds(),
        scratch.path(),
        &tierstore(),
    )
    .expect_err("no package for this host");
    assert_eq!(refused, failed(NO_PACKAGE_FOR_PLATFORM));
    assert_eq!(urls(&fetcher), [at_index()]);
    assert!(left(scratch.path()).is_empty());
}

/// Catches a package chosen by anything but `env.platform`: an index with all three platforms,
/// each with different bytes, is fetched for each platform in turn, and the URL requested, the
/// platform, the digest and the bytes staged are that platform's.
#[test]
fn the_package_is_the_one_for_this_hosts_platform() {
    let scratch = scratch("fetch-platform");
    let bodies: [&[u8]; 3] = [b"the arm64 bytes", b"the x86_64 bytes", b"the macos bytes"];
    let packages: Vec<(Platform, &[u8])> = Platform::ALL.into_iter().zip(bodies).collect();
    for (platform, body) in &packages {
        let url = format!(
            "{CHANNEL}/tierstore/1.5.0/tierstore-1.5.0-{}.tar.zst",
            platform.name()
        );
        let fetcher = FakeFetcher::new()
            .route(
                &at_index(),
                serve(index_json("tierstore", "1.5.0", &packages).as_bytes()),
            )
            .route(&url, serve(body));
        let staged = fetch_into(
            &fetcher,
            &FakeClock::at(1),
            &bounds(),
            scratch.path(),
            &tierstore(),
            *platform,
            stage::plain,
        )
        .expect("fetched");
        assert_eq!(urls(&fetcher), [at_index(), url.clone()]);
        assert_eq!(staged.platform, *platform);
        assert_eq!(staged.sha256, digest(&sha256_hex(body)));
        assert_eq!(staged.url.as_str(), url);
        assert_eq!(
            std::fs::read(staged.file.path()).expect("the staged file"),
            *body
        );
    }
}
