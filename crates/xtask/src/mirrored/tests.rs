use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;
use crate::coordinator_update::public::{Get, Head};
use crate::release::ports::PortError;
use crate::testdir::TestDir;

/// An empty directory of its own for one test, removed when it drops.
pub(crate) fn scratch(name: &str) -> TestDir {
    let exe = std::env::current_exe().expect("test binary");
    TestDir::under(
        &exe.parent().expect("deps").join("xtask-mirrored-unit"),
        name,
    )
}

/// What the fake answers for one URL.
#[derive(Clone)]
pub(crate) enum Answer {
    /// A `200` with this body.
    Body(Vec<u8>),
    /// This status, no body.
    Status(u16),
    /// No answer: this reason (a DNS failure, a refused connection).
    Fail(String),
    /// These bytes of the body, then the connection breaks with this reason.
    Cut(Vec<u8>, String),
    /// The `.part` file at this path is taken away, then no answer.
    Remove(PathBuf),
}

/// The web, scripted per URL; a URL it was given no answer for does not resolve. It records
/// each GET's URL and limit.
#[derive(Default)]
pub(crate) struct FakeWeb {
    pub(crate) answers: BTreeMap<String, Answer>,
    pub(crate) gets: RefCell<Vec<(String, Option<u64>)>>,
}

impl FakeWeb {
    pub(crate) fn with(answers: &[(&str, Answer)]) -> FakeWeb {
        FakeWeb {
            answers: answers
                .iter()
                .map(|(url, answer)| ((*url).to_owned(), answer.clone()))
                .collect(),
            gets: RefCell::default(),
        }
    }

    /// The URLs asked, in order.
    pub(crate) fn urls(&self) -> Vec<String> {
        self.gets
            .borrow()
            .iter()
            .map(|(url, _)| url.clone())
            .collect()
    }
}

impl Web for FakeWeb {
    fn get(&self, get: &Get<'_>, body: &mut dyn Write) -> Result<Head, PortError> {
        assert_eq!(get.accept, "*/*");
        assert_eq!(get.if_none_match, None);
        self.gets.borrow_mut().push((get.url.to_owned(), get.limit));
        let failed = |why: &str| PortError::new(format!("GET {}", get.url), why);
        match self.answers.get(get.url) {
            Some(Answer::Body(bytes)) => {
                body.write_all(bytes).expect("write the body");
                Ok(Head {
                    status: 200,
                    ..Head::default()
                })
            }
            Some(Answer::Status(status)) => Ok(Head {
                status: *status,
                ..Head::default()
            }),
            Some(Answer::Fail(why)) => Err(failed(why)),
            Some(Answer::Cut(bytes, why)) => {
                body.write_all(bytes).expect("write the body");
                Err(failed(why))
            }
            Some(Answer::Remove(path)) => {
                std::fs::remove_file(path).expect("remove the .part file");
                Err(failed("gone"))
            }
            None => Err(failed("dns error: failed to lookup address information")),
        }
    }
}

/// A pause that records what it was asked and never waits.
#[derive(Default)]
pub(crate) struct Pauses(pub(crate) RefCell<Vec<Duration>>);

impl Pauses {
    pub(crate) fn pause(&self) -> impl Fn(Duration) + '_ {
        |d| self.0.borrow_mut().push(d)
    }
}

const FILE: &[u8] = b"the pinned file's bytes";

fn pin(bytes: Option<u64>) -> Pin {
    Pin {
        name: "file.bin".to_owned(),
        sha256: Sha256::of(FILE),
        bytes,
    }
}

fn urls() -> Vec<String> {
    vec![
        "https://first.example/file.bin".to_owned(),
        "https://second.example/file.bin".to_owned(),
    ]
}

/// The names in `dir`, sorted.
pub(crate) fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read the directory")
        .map(|e| {
            e.expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn the_first_url_that_gives_the_pinned_bytes_is_kept_with_mode_0644() {
    // Catches: a fetch that does not try the first URL first, keeps a `.part` file, leaves the
    // file with another mode, or pauses before the first round.
    let dir = scratch("first");
    let web = FakeWeb::with(&[(
        "https://first.example/file.bin",
        Answer::Body(FILE.to_vec()),
    )]);
    let pauses = Pauses::default();
    let line = fetch(&urls(), &pin(None), &dir, &web, &pauses.pause()).expect("fetched");
    assert_eq!(
        line,
        format!(
            "ok: https://first.example/file.bin has SHA-256 {}",
            Sha256::of(FILE)
        )
    );
    assert_eq!(std::fs::read(dir.join("file.bin")).expect("the file"), FILE);
    let mode = std::fs::metadata(dir.join("file.bin"))
        .expect("meta")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o644);
    assert_eq!(names(&dir), ["file.bin"]);
    assert_eq!(web.urls(), ["https://first.example/file.bin"]);
    assert_eq!(*pauses.0.borrow(), [Duration::ZERO]);
}

#[test]
fn an_unreachable_or_wrong_first_url_falls_through_to_the_next() {
    // Catches: a fetch that stops at the first failure, keeps bytes of another digest, a
    // non-200 answer or a body cut short, or does not say what failed before the kept one.
    let wrong = Sha256::of(b"other bytes");
    let cases: [(Answer, String); 4] = [
        (
            Answer::Fail("dns error: failed to lookup address information".to_owned()),
            "dns error: failed to lookup address information".to_owned(),
        ),
        (
            Answer::Body(b"other bytes".to_vec()),
            format!("SHA-256 {wrong}, not the pinned one"),
        ),
        (Answer::Status(404), "HTTP 404".to_owned()),
        (
            Answer::Cut(FILE[..4].to_vec(), "connection reset\r\nby peer".to_owned()),
            "connection reset  by peer".to_owned(),
        ),
    ];
    for (first, why) in cases {
        let dir = scratch("fallthrough");
        let web = FakeWeb::with(&[
            ("https://first.example/file.bin", first),
            (
                "https://second.example/file.bin",
                Answer::Body(FILE.to_vec()),
            ),
        ]);
        let pauses = Pauses::default();
        let line = fetch(&urls(), &pin(None), &dir, &web, &pauses.pause()).expect("fetched");
        assert_eq!(
            line,
            format!(
                "ok: https://second.example/file.bin has SHA-256 {}, after 1 failed: \
                 https://first.example/file.bin: {why}",
                Sha256::of(FILE)
            )
        );
        assert_eq!(std::fs::read(dir.join("file.bin")).expect("the file"), FILE);
        assert_eq!(names(&dir), ["file.bin"]);
        assert_eq!(*pauses.0.borrow(), [Duration::ZERO]);
    }
}

#[test]
fn a_pinned_size_is_the_limit_and_another_size_is_refused() {
    // Catches: a size pin not handed to the client as its limit, or a body of another size
    // kept because its digest was not looked at first.
    let dir = scratch("size");
    let short = FILE.len() as u64 - 1;
    let web = FakeWeb::with(&[
        (
            "https://first.example/file.bin",
            Answer::Body(FILE.to_vec()),
        ),
        (
            "https://second.example/file.bin",
            Answer::Body(FILE.to_vec()),
        ),
    ]);
    let pauses = Pauses::default();
    let err = fetch(&urls(), &pin(Some(short)), &dir, &web, &pauses.pause()).unwrap_err();
    let why = format!("{} bytes, not the pinned {short}", FILE.len());
    let attempts: Vec<String> = (0..3)
        .flat_map(|_| urls())
        .map(|url| format!("{url}: {why}"))
        .collect();
    assert_eq!(
        err,
        Error::Unavailable {
            name: "file.bin".to_owned(),
            attempts
        }
    );
    assert!(
        web.gets
            .borrow()
            .iter()
            .all(|(_, limit)| *limit == Some(short))
    );
    let dir = scratch("size-ok");
    let len = FILE.len() as u64;
    fetch(
        &urls(),
        &pin(Some(len)),
        &dir,
        &web,
        &Pauses::default().pause(),
    )
    .expect("fetched");
    assert_eq!(names(&dir), ["file.bin"]);
}

#[test]
fn every_round_failing_is_refused_naming_each_attempt_and_leaves_nothing() {
    // Catches: unbounded retries, rounds without their pauses, an error that does not name
    // every attempt in order, and a `.part` file or a file left behind.
    let dir = scratch("none");
    let web = FakeWeb::default();
    let pauses = Pauses::default();
    let err = fetch(&urls(), &pin(None), &dir, &web, &pauses.pause()).unwrap_err();
    let why = "dns error: failed to lookup address information";
    let attempts: Vec<String> = (0..3)
        .flat_map(|_| urls())
        .map(|url| format!("{url}: {why}"))
        .collect();
    assert_eq!(
        err.to_string(),
        format!(
            "file.bin: no download was the pinned file: {}",
            attempts.join("; ")
        )
    );
    assert_eq!(web.urls().len(), 6);
    assert_eq!(*pauses.0.borrow(), WAITS);
    assert_eq!(names(&dir), Vec::<String>::new());
}

#[test]
fn a_later_round_may_succeed() {
    // Catches: a fetch that gives up after one round.
    struct Flaky {
        web: FakeWeb,
        round: RefCell<usize>,
    }
    impl Web for Flaky {
        fn get(&self, get: &Get<'_>, body: &mut dyn Write) -> Result<Head, PortError> {
            *self.round.borrow_mut() += 1;
            if *self.round.borrow() <= 2 {
                return Err(PortError::new("GET", "timed out"));
            }
            self.web.get(get, body)
        }
    }
    let dir = scratch("later");
    let web = Flaky {
        web: FakeWeb::with(&[(
            "https://first.example/file.bin",
            Answer::Body(FILE.to_vec()),
        )]),
        round: RefCell::new(0),
    };
    let pauses = Pauses::default();
    let line = fetch(&urls(), &pin(None), &dir, &web, &pauses.pause()).expect("fetched");
    assert!(line.ends_with(
        ", after 2 failed: https://first.example/file.bin: timed out; \
         https://second.example/file.bin: timed out"
    ));
    assert_eq!(*pauses.0.borrow(), WAITS[..2]);
}

#[test]
fn a_file_that_cannot_be_written_names_its_path() {
    // Catches: a write failure taken for a failed download (tried again on every URL) or not
    // named: a directory that is not there, a file name taken by a directory, and a `.part`
    // file that vanished before it could be removed.
    let dir = scratch("unwritable");
    let missing = dir.join("missing");
    let web = FakeWeb::with(&[(
        "https://first.example/file.bin",
        Answer::Body(FILE.to_vec()),
    )]);
    let err = fetch(
        &urls(),
        &pin(None),
        &missing,
        &web,
        &Pauses::default().pause(),
    )
    .unwrap_err();
    let part = missing.join("file.bin.part");
    assert!(
        matches!(&err, Error::Write { path, .. } if *path == part),
        "{err}"
    );
    assert!(err.to_string().starts_with(&format!("{part:?}: ")));
    assert_eq!(web.urls(), Vec::<String>::new());

    std::fs::create_dir(dir.join("file.bin")).expect("a directory in the way");
    std::fs::write(dir.join("file.bin").join("x"), b"x").expect("not empty");
    let err = fetch(&urls(), &pin(None), &dir, &web, &Pauses::default().pause()).unwrap_err();
    assert!(
        matches!(&err, Error::Write { path, .. } if *path == dir.join("file.bin")),
        "{err}"
    );

    let dir = scratch("vanished");
    let part = dir.join("file.bin.part");
    let web = FakeWeb::with(&[(
        "https://first.example/file.bin",
        Answer::Remove(part.clone()),
    )]);
    // Each attempt makes the `.part` file again and its answer takes it away, so the removal
    // after the last round finds none.
    let err = fetch(
        &urls()[..1],
        &pin(None),
        &dir,
        &web,
        &Pauses::default().pause(),
    );
    let err = err.unwrap_err();
    assert!(
        matches!(&err, Error::Write { path, .. } if *path == part),
        "{err}"
    );
}

#[test]
fn the_real_client_falls_through_a_404_and_other_bytes_to_the_pinned_file_over_https() {
    // Catches: a fetch that works against the fake only: through the HTTPS client of a host
    // (rustls, the roots of a CA bundle, a server on loopback), a 404 and a 200 of other bytes
    // are each one failed attempt, the pinned bytes are kept, and a size pin is the client's
    // limit, so a longer body is cut off and refused.
    use std::sync::Arc;

    use crate::coordinator_update::https_tests::{Answer as Served, Request, Server};
    use crate::coordinator_update::public::{UreqWeb, roots};

    let server = Server::start(Arc::new(|_: &str, request: &Request| {
        match request.path.as_str() {
            "/a/file.bin" => Served::new(404, "not here"),
            "/b/file.bin" => Served::new(200, "other bytes"),
            _ => Served::new(200, FILE),
        }
    }));
    let dir = scratch("real");
    let pem = dir.join("ca.pem");
    std::fs::write(&pem, server.ca_pem()).expect("the bundle");
    let web = UreqWeb::new(&roots(&pem).expect("the roots"));
    let base = server.url();
    let urls: Vec<String> = ["a", "b", "c"]
        .iter()
        .map(|m| format!("{base}/{m}/file.bin"))
        .collect();
    let into = dir.join("into");
    std::fs::create_dir(&into).expect("into");
    let line = fetch(&urls, &pin(Some(FILE.len() as u64)), &into, &web, &|_| {}).expect("fetched");
    assert_eq!(
        line,
        format!(
            "ok: {base}/c/file.bin has SHA-256 {}, after 2 failed: {base}/a/file.bin: HTTP 404; \
             {base}/b/file.bin: {} bytes, not the pinned {}",
            Sha256::of(FILE),
            "other bytes".len(),
            FILE.len()
        )
    );
    assert_eq!(
        std::fs::read(into.join("file.bin")).expect("the file"),
        FILE
    );

    let short = FILE.len() as u64 - 1;
    let err = fetch(&urls[2..], &pin(Some(short)), &into, &web, &|_| {}).unwrap_err();
    let Error::Unavailable { attempts, .. } = err else {
        panic!("{err}")
    };
    assert_eq!(attempts.len(), 3);
    assert!(
        attempts
            .iter()
            .all(|a| a.starts_with(&format!("{base}/c/file.bin: "))),
        "{attempts:?}"
    );
    assert_eq!(names(&into), ["file.bin"]);
}

#[test]
fn the_hashing_writer_passes_every_byte_on_and_counts_and_hashes_them() {
    // Catches: a writer that hashes or counts other bytes than it wrote, or does not flush its
    // inner writer.
    let mut hashed = Hashed::new(std::io::BufWriter::new(Vec::new()));
    hashed.write_all(&FILE[..5]).expect("write");
    hashed.write_all(&FILE[5..]).expect("write");
    hashed.flush().expect("flush");
    assert_eq!(hashed.inner.get_ref().as_slice(), FILE);
    assert_eq!(hashed.finish(), (FILE.len() as u64, Sha256::of(FILE)));
}
