use std::cell::Cell;
use std::os::unix::fs::{MetadataExt, symlink};
use std::rc::Rc;

use super::*;
use crate::ports::{Got, NetError, Sink, Url};
use crate::testing::{
    Failing, FakeClock, FakeFetcher, deliver, listing, scratch, serve, sha256_hex,
};

const URL: &str = "https://releases.example/svc/svc-1.tar.zst";

/// A job for `URL` into `dir`, with `cap` bytes and a deadline 100 s from the clock's now.
fn job<'a>(fetcher: &'a FakeFetcher, clock: &'a FakeClock, dir: &'a Path, url: &'a Url) -> Job<'a> {
    Job {
        fetcher,
        clock,
        dir,
        url,
        cap: 10,
        deadline: clock.stamp(clock.mono_secs() + 100),
    }
}

fn denied(_: &File) -> Box<dyn Write + '_> {
    Box::new(Failing(io::ErrorKind::PermissionDenied))
}

/// The names in `dir` that a staging file has.
fn staging_files(dir: &Path) -> Vec<String> {
    listing(dir)
        .into_iter()
        .filter(|name| name.starts_with(PREFIX))
        .collect()
}

/// Catches a staging file opened through a name that already exists: a link at the first name
/// is skipped, its target is not touched, and the file is made under the next name, mode 0600.
/// When every name exists nothing is opened and the error is `AlreadyExists`; a directory that
/// is missing is its own error.
#[test]
fn the_staging_file_is_never_created_through_an_existing_path() {
    let scratch = scratch("stage-exclusive");
    let dir = scratch.path().join("svc");
    std::fs::create_dir(&dir).expect("the service directory");
    let outside = scratch.path().join("outside");
    std::fs::write(&outside, b"keep").expect("the link target");
    symlink(&outside, dir.join(".fetch-taken")).expect("a link at the first name");

    let names = [".fetch-taken", ".fetch-free"].map(str::to_owned);
    let (mut file, temp) = create(&dir, names.to_vec()).expect("the second name");
    assert_eq!(temp.path(), dir.join(".fetch-free"));
    file.write_all(b"new").expect("write");
    assert_eq!(std::fs::read(&outside).expect("the target"), b"keep");
    let mode = std::fs::metadata(temp.path()).expect("the file").mode();
    assert_eq!(mode & 0o7777, 0o600);

    let taken = [".fetch-taken", ".fetch-free"].map(str::to_owned);
    let refused = create(&dir, taken.to_vec()).expect_err("every name exists");
    assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&outside).expect("the target"), b"keep");
    assert_eq!(
        std::fs::read(dir.join(".fetch-free")).expect("the file"),
        b"new"
    );

    let missing = create(&scratch.path().join("absent"), names_of(&["x"])).expect_err("no dir");
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);

    drop(temp);
    assert_eq!(listing(&dir), [".fetch-taken"]);
}

fn names_of(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// Catches two staging files of one process sharing a name: each name has the prefix the orphan
/// sweep looks for, and no two are alike.
#[test]
fn staging_names_have_the_prefix_and_never_repeat() {
    let first = names();
    let second = names();
    assert_eq!(first.len(), 16);
    let mut all: Vec<&String> = first.iter().chain(&second).collect();
    assert!(
        all.iter().all(|name| name.starts_with(".fetch-")),
        "{all:?}"
    );
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 32);
}

/// Catches a service directory that is a link (or not a directory) being written through, and
/// a missing one not being made: `make_dir` makes a missing directory, accepts an existing one,
/// and refuses a link to a directory, a file, and a path under a file.
#[test]
fn the_service_directory_is_made_or_must_be_a_real_directory() {
    let scratch = scratch("stage-dir");
    let dir = scratch.path().join("svc");
    assert!(!is_dir(&dir).expect("missing"));
    make_dir(&dir).expect("made");
    assert!(is_dir(&dir).expect("a directory"));
    make_dir(&dir).expect("already there");

    let link = scratch.path().join("link");
    symlink(&dir, &link).expect("a link to the directory");
    let refused = make_dir(&link).expect_err("a link");
    assert_eq!(
        refused.to_string(),
        format!("{} is not a directory", link.display())
    );

    let file = scratch.path().join("file");
    std::fs::write(&file, b"x").expect("a file");
    assert_eq!(
        is_dir(&file).expect_err("a file").to_string(),
        format!("{} is not a directory", file.display())
    );
    let under = is_dir(&file.join("x")).expect_err("under a file");
    assert_eq!(under.kind(), io::ErrorKind::NotADirectory);
    let parent_missing = make_dir(&scratch.path().join("a/b")).expect_err("no parent");
    assert_eq!(parent_missing.kind(), io::ErrorKind::NotFound);
}

/// Catches a download whose file or digest is wrong: the bytes the fetcher gives are in the
/// staging file, and its SHA-256 and size are theirs.
#[test]
fn a_download_holds_the_bytes_their_digest_and_their_count() {
    let scratch = scratch("stage-download");
    let url = Url::new(URL.to_owned());
    let fetcher = FakeFetcher::new().route(URL, serve(b"ten bytes!"));
    let clock = FakeClock::at(5);
    let got = download(&job(&fetcher, &clock, scratch.path(), &url), plain).expect("downloaded");
    assert_eq!(got.sha256, sha256_hex(b"ten bytes!"));
    assert_eq!(got.size, 10);
    assert_eq!(
        std::fs::read(got.file.path()).expect("the file"),
        b"ten bytes!"
    );
    let calls = fetcher.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].url, URL);
    assert_eq!(calls[0].limit.connect_timeout, CONNECT_TIMEOUT);
    assert_eq!(calls[0].limit.deadline, clock.stamp(105));
    drop(got);
    assert!(staging_files(scratch.path()).is_empty());
}

/// Runs `download` with `serve` and returns its fault; no staging file may be left.
fn fault_of(name: &str, serve: crate::testing::Serve, writer: OpenWriter) -> Fault {
    let scratch = scratch(name);
    let url = Url::new(URL.to_owned());
    let fetcher = FakeFetcher::new().route(URL, serve);
    let clock = FakeClock::at(5);
    let fault =
        download(&job(&fetcher, &clock, scratch.path(), &url), writer).expect_err("a fault");
    assert!(staging_files(scratch.path()).is_empty(), "{name}");
    fault
}

/// Catches a cap checked after the download, or not at all: an announced length over the cap
/// stops the transfer before any byte, and so does the first chunk that passes it; exactly the
/// cap is accepted.
#[test]
fn the_cap_stops_the_transfer_at_the_header_or_the_first_byte_over() {
    let given = Rc::new(Cell::new(u64::MAX));
    let seen = Rc::clone(&given);
    let announced: crate::testing::Serve = Box::new(move |sink: &mut dyn Sink, _: &Limit| {
        let (got, bytes) = deliver(sink, b"eleven byte", true, 4);
        seen.set(bytes);
        Ok(got)
    });
    assert!(matches!(
        fault_of("stage-cap-head", announced, plain),
        Fault::TooLarge
    ));
    assert_eq!(given.get(), 0);

    let seen = Rc::clone(&given);
    let streamed: crate::testing::Serve = Box::new(move |sink: &mut dyn Sink, _: &Limit| {
        let (got, bytes) = deliver(sink, &[7; 30], false, 4);
        seen.set(bytes);
        Ok(got)
    });
    assert!(matches!(
        fault_of("stage-cap-body", streamed, plain),
        Fault::TooLarge
    ));
    assert_eq!(given.get(), 12);
}

/// Catches a write error that is ignored, or reported as something else: the first write that
/// fails stops the transfer and is the fault, with its kind.
#[test]
fn a_write_that_fails_stops_the_transfer_with_its_error() {
    let fault = fault_of("stage-write", serve(b"abc"), denied);
    let Fault::Storage(error) = fault else {
        panic!("not a storage fault: {fault:?}");
    };
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(!is_full(&error));
    assert!(is_full(&io::Error::from(io::ErrorKind::StorageFull)));
}

/// Catches the deadline read once, or from another clock: a chunk that arrives after the
/// deadline stops the transfer, and a transfer that ends after it is refused although every
/// byte arrived.
#[test]
fn the_deadline_is_checked_on_every_chunk_and_at_the_end() {
    let clock = std::sync::Arc::new(FakeClock::at(5));
    let scratch = scratch("stage-deadline");
    let url = Url::new(URL.to_owned());
    let late = std::sync::Arc::clone(&clock);
    let fetcher = FakeFetcher::new().route(
        URL,
        Box::new(move |sink: &mut dyn Sink, _: &Limit| {
            sink.begin(None).expect("begin");
            sink.chunk(b"a").expect("in time");
            late.advance(100);
            Ok(sink.chunk(b"b").map_or(Got::Stopped, |()| Got::Complete))
        }),
    );
    let fault = download(&job(&fetcher, &clock, scratch.path(), &url), plain).expect_err("late");
    assert!(matches!(fault, Fault::Deadline), "{fault:?}");

    let after_last = std::sync::Arc::clone(&clock);
    let fetcher = FakeFetcher::new().route(
        URL,
        Box::new(move |sink: &mut dyn Sink, _: &Limit| {
            let (got, _) = deliver(sink, b"ab", true, 1);
            after_last.advance(100);
            Ok(got)
        }),
    );
    let fault = download(&job(&fetcher, &clock, scratch.path(), &url), plain).expect_err("late");
    assert!(matches!(fault, Fault::Deadline), "{fault:?}");
    assert!(staging_files(scratch.path()).is_empty());
}

/// Catches a network error turned into success, and an adapter's claim that the sink stopped it
/// being believed when the sink did not: both are faults.
#[test]
fn a_network_error_or_an_unexplained_stop_is_a_fault() {
    let refused: crate::testing::Serve =
        Box::new(|_: &mut dyn Sink, _: &Limit| Err(NetError::Connect));
    assert!(matches!(fault_of("stage-net", refused, plain), Fault::Net));
    let claims: crate::testing::Serve = Box::new(|_: &mut dyn Sink, _: &Limit| Ok(Got::Stopped));
    assert!(matches!(
        fault_of("stage-unexplained", claims, plain),
        Fault::Unexplained
    ));
}

/// Catches a later reason replacing the first: an adapter that keeps calling the sink after it
/// stopped gets `Stop` again, and the fault is the first reason (the cap, not the write error
/// of a chunk sent after it).
#[test]
fn the_first_reason_to_stop_is_kept() {
    let ignores_stop: crate::testing::Serve = Box::new(|sink: &mut dyn Sink, _: &Limit| {
        assert_eq!(sink.begin(Some(11)), Err(Stop));
        assert_eq!(sink.chunk(b"x"), Err(Stop));
        Ok(Got::Complete)
    });
    assert!(matches!(
        fault_of("stage-first", ignores_stop, denied),
        Fault::TooLarge
    ));
}

/// Catches a read-back of the staging file that reads past its cap or follows a link: only the
/// first `cap` bytes are returned (all of them when the file is shorter), and a link is refused
/// with an I/O error (`ELOOP`), not read.
#[test]
fn read_back_reads_at_most_cap_bytes_and_refuses_a_link() {
    let scratch = scratch("stage-read-back");
    let file = scratch.path().join("staged");
    std::fs::write(&file, b"0123456789").expect("the staged file");
    assert_eq!(read_back(&file, 4).expect("the first bytes"), b"0123");
    assert_eq!(read_back(&file, 100).expect("all bytes"), b"0123456789");

    let link = scratch.path().join("link");
    symlink(&file, &link).expect("a link");
    let refused = read_back(&link, 100).expect_err("a link");
    assert_eq!(refused.raw_os_error(), Some(libc::ELOOP));
}
