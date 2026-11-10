//! The test doubles and builders of this crate's tests: the network ([`FakeTransport`]), the
//! clock ([`FakeClock`]), `gh` ([`bundle::FakeGh`]), and the files a release holds: ISO 9660
//! images with Rock Ridge names ([`iso`]), debs ([`deb`]), attestation bundles ([`bundle`]) and a
//! whole release of both platforms ([`release`]). Everything is made in memory at run time;
//! nothing is fetched and nothing is committed.

pub(crate) mod bundle;
pub(crate) mod deb;
pub(crate) mod iso;
pub(crate) mod release;

use std::collections::BTreeMap;
use std::io::{self, Cursor, Read, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use hostkeeper_package::net::{Response, Timeouts, Transport};
use hostkeeper_package::ports::NetError;
use hostkeeper_steps::{BootId, BootIdError, Clock};
use hostkeeper_testkit::Scratch;
use sha2::{Digest as _, Sha256};

/// The SHA-256 of `bytes`, as 64 lowercase hex digits.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A scratch directory for one test, beside the test binary in the target directory, removed
/// when dropped.
pub(crate) fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let base = exe.parent().expect("its directory").join("pxe-scratch");
    Scratch::under(&base, name).expect("a scratch directory")
}

/// The timeouts the tests pass; the fakes ignore them.
pub(crate) fn timeouts() -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(30),
        total: Duration::from_secs(60),
    }
}

/// A reader that yields its bytes, then fails with `message`.
pub(crate) struct BrokenReader {
    bytes: Cursor<Vec<u8>>,
    message: &'static str,
}

impl BrokenReader {
    pub(crate) fn after(bytes: &[u8], message: &'static str) -> BrokenReader {
        BrokenReader {
            bytes: Cursor::new(bytes.to_vec()),
            message,
        }
    }
}

impl Read for BrokenReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.bytes.read(buffer)? {
            0 => Err(io::Error::other(self.message)),
            read => Ok(read),
        }
    }
}

/// A writer whose writes, or whose flushes, fail with an error kind.
pub(crate) struct Failing {
    write: Option<io::ErrorKind>,
    flush: Option<io::ErrorKind>,
}

impl Failing {
    pub(crate) fn writes(kind: io::ErrorKind) -> Failing {
        Failing {
            write: Some(kind),
            flush: None,
        }
    }

    pub(crate) fn flushes(kind: io::ErrorKind) -> Failing {
        Failing {
            write: None,
            flush: Some(kind),
        }
    }
}

impl Write for Failing {
    fn write(&mut self, chunk: &[u8]) -> io::Result<usize> {
        self.write.map_or(Ok(chunk.len()), |kind| Err(kind.into()))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush.map_or(Ok(()), |kind| Err(kind.into()))
    }
}

/// What the fake network answers for a URL.
#[derive(Clone, Debug)]
pub(crate) enum Serve {
    /// A 200 with this body.
    Body(Vec<u8>),
    /// This status and an empty body.
    Status(u16),
    /// No response.
    Fail(NetError),
    /// A 200 whose body yields these bytes, then fails.
    Broken(Vec<u8>),
}

/// The network of the tests: an answer per URL (404 for any other), and the URLs asked for, in
/// order.
#[derive(Default)]
pub(crate) struct FakeTransport {
    routes: BTreeMap<String, Serve>,
    asked: Mutex<Vec<String>>,
}

impl FakeTransport {
    pub(crate) fn new() -> FakeTransport {
        FakeTransport::default()
    }

    pub(crate) fn route(mut self, url: &str, serve: Serve) -> FakeTransport {
        self.routes.insert(url.to_owned(), serve);
        self
    }

    pub(crate) fn asked(&self) -> Vec<String> {
        self.asked.lock().expect("the log").clone()
    }
}

impl Transport for FakeTransport {
    fn open(&self, url: &str, _timeouts: Timeouts) -> Result<Response, NetError> {
        self.asked.lock().expect("the log").push(url.to_owned());
        let response = |status: u16, body: Box<dyn Read>| Response {
            status,
            content_length: None,
            location: None,
            www_authenticate: None,
            body,
        };
        match self.routes.get(url).cloned().unwrap_or(Serve::Status(404)) {
            Serve::Body(bytes) => Ok(response(200, Box::new(Cursor::new(bytes)))),
            Serve::Status(status) => Ok(response(status, Box::new(io::empty()))),
            Serve::Fail(error) => Err(error),
            Serve::Broken(bytes) => Ok(response(
                200,
                Box::new(BrokenReader::after(&bytes, "the connection was reset")),
            )),
        }
    }
}

/// A monotonic clock that moves only when a test moves it, under one boot.
pub(crate) struct FakeClock {
    secs: AtomicU64,
}

impl FakeClock {
    pub(crate) fn at(secs: u64) -> FakeClock {
        FakeClock {
            secs: AtomicU64::new(secs),
        }
    }

    pub(crate) fn advance(&self, secs: u64) {
        self.secs.fetch_add(secs, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn mono_secs(&self) -> u64 {
        self.secs.load(Ordering::SeqCst)
    }

    fn boot_id(&self) -> Result<BootId, BootIdError> {
        BootId::new("the test clock", "boot-1")
    }

    fn sleep_secs(&self, secs: u64) {
        self.advance(secs);
    }
}
