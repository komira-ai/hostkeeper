//! Fakes of every port, for this crate's tests and the daemon's. Each is a handle: clones share
//! one script, so a test keeps a clone to script answers and to read what was asked after it gave
//! the other to an [`crate::Observer`].
//!
//! A scripted answer is steady (given on every call) or queued (given once, before the steady
//! one). Asked something it has no answer for, a fake answers as the host would for a thing that
//! is not there: an unknown unit is not found, an unknown pid is an error, an unknown URL does
//! not answer, a version with no seal has no digest.

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::error::{ObserveError, RunError};
use crate::health::{Connect, Duplex};
use crate::ports::{
    ActiveState, CommandOutput, CommandRunner, Deadline, ExeStat, ExpectedExe, HealthClient,
    HealthReply, ProcessIdentity, ProcessProbe, ServiceManager, UnitState,
};
use crate::url::HealthUrl;

fn lock<T>(shared: &Mutex<T>) -> MutexGuard<'_, T> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Answers per key: queued ones first, then the steady one.
#[derive(Debug)]
struct Script<K, T> {
    queued: BTreeMap<K, VecDeque<T>>,
    steady: BTreeMap<K, T>,
}

impl<K, T> Default for Script<K, T> {
    fn default() -> Script<K, T> {
        Script {
            queued: BTreeMap::new(),
            steady: BTreeMap::new(),
        }
    }
}

impl<K: Ord, T: Clone> Script<K, T> {
    fn next(&mut self, key: &K) -> Option<T> {
        let queued = self.queued.get_mut(key).and_then(VecDeque::pop_front);
        queued.or_else(|| self.steady.get(key).cloned())
    }

    fn queue(&mut self, key: K, answer: T) {
        self.queued.entry(key).or_default().push_back(answer);
    }
}

#[derive(Debug, Default)]
struct ManagerScript {
    answers: Script<String, Result<UnitState, ObserveError>>,
    calls: Vec<String>,
}

/// A service manager that answers what it is told, per unit and per call.
#[derive(Clone, Debug, Default)]
pub struct FakeServiceManager(Arc<Mutex<ManagerScript>>);

impl FakeServiceManager {
    /// `unit` is in `active` with `main_pid` on every call from now on.
    pub fn set(&self, unit: &str, active: ActiveState, main_pid: Option<u32>) {
        let state = UnitState {
            active,
            sub: format!("{active:?}").to_lowercase(),
            main_pid,
        };
        lock(&self.0)
            .answers
            .steady
            .insert(unit.to_owned(), Ok(state));
    }

    /// The next call for `unit` answers `answer`, once.
    pub fn then(&self, unit: &str, answer: Result<UnitState, ObserveError>) {
        lock(&self.0).answers.queue(unit.to_owned(), answer);
    }

    /// Every call for `unit` fails from now on.
    pub fn fail(&self, unit: &str) {
        let failure = ObserveError::Manager {
            unit: unit.to_owned(),
            why: "failed on command".to_owned(),
        };
        lock(&self.0)
            .answers
            .steady
            .insert(unit.to_owned(), Err(failure));
    }

    /// The units asked about, in order.
    pub fn calls(&self) -> Vec<String> {
        lock(&self.0).calls.clone()
    }
}

impl ServiceManager for FakeServiceManager {
    fn unit_state(&self, unit: &str) -> Result<UnitState, ObserveError> {
        let mut script = lock(&self.0);
        script.calls.push(unit.to_owned());
        let not_found = UnitState {
            active: ActiveState::NotFound,
            sub: "dead".to_owned(),
            main_pid: None,
        };
        script
            .answers
            .next(&unit.to_owned())
            .unwrap_or(Ok(not_found))
    }
}

#[derive(Debug, Default)]
struct ProcessScript {
    identity: Script<u32, Result<ProcessIdentity, ObserveError>>,
    stat: BTreeMap<u32, Result<ExeStat, ObserveError>>,
    digest: BTreeMap<u32, Result<[u8; 32], ObserveError>>,
    digest_calls: usize,
    calls: Vec<&'static str>,
}

/// A process table that answers what it is told, and counts the digests taken.
#[derive(Clone, Debug, Default)]
pub struct FakeProcessProbe(Arc<Mutex<ProcessScript>>);

impl FakeProcessProbe {
    /// `pid` started at `start_time` and runs an executable of `stat` whose digest is `digest`,
    /// on every call from now on.
    pub fn set(&self, pid: u32, start_time: u64, stat: ExeStat, digest: [u8; 32]) {
        let mut script = lock(&self.0);
        let identity = ProcessIdentity { pid, start_time };
        script.identity.steady.insert(pid, Ok(identity));
        script.stat.insert(pid, Ok(stat));
        script.digest.insert(pid, Ok(digest));
    }

    /// The next identity read of `pid` answers `answer`, once.
    pub fn then_identity(&self, pid: u32, answer: Result<ProcessIdentity, ObserveError>) {
        lock(&self.0).identity.queue(pid, answer);
    }

    /// Every stat of `pid`'s executable answers `answer` from now on.
    pub fn set_stat(&self, pid: u32, answer: Result<ExeStat, ObserveError>) {
        lock(&self.0).stat.insert(pid, answer);
    }

    /// Every digest of `pid`'s executable answers `answer` from now on.
    pub fn set_digest(&self, pid: u32, answer: Result<[u8; 32], ObserveError>) {
        lock(&self.0).digest.insert(pid, answer);
    }

    /// How many digests were taken.
    pub fn digest_calls(&self) -> usize {
        lock(&self.0).digest_calls
    }

    /// Every call, in order: `identity`, `exe_stat` or `exe_digest`.
    pub fn calls(&self) -> Vec<&'static str> {
        lock(&self.0).calls.clone()
    }
}

/// The answer for a pid nothing was scripted for.
fn no_such_process(pid: u32) -> ObserveError {
    ObserveError::Process {
        pid,
        why: "no such process".to_owned(),
    }
}

impl ProcessProbe for FakeProcessProbe {
    fn identity(&self, pid: u32) -> Result<ProcessIdentity, ObserveError> {
        let mut script = lock(&self.0);
        script.calls.push("identity");
        let answer = script.identity.next(&pid);
        answer.unwrap_or_else(|| Err(no_such_process(pid)))
    }

    fn exe_stat(&self, pid: u32) -> Result<ExeStat, ObserveError> {
        let mut script = lock(&self.0);
        script.calls.push("exe_stat");
        let answer = script.stat.get(&pid).cloned();
        answer.unwrap_or_else(|| Err(no_such_process(pid)))
    }

    fn exe_digest(&self, pid: u32) -> Result<[u8; 32], ObserveError> {
        let mut script = lock(&self.0);
        script.digest_calls += 1;
        script.calls.push("exe_digest");
        let answer = script.digest.get(&pid).cloned();
        answer.unwrap_or_else(|| Err(no_such_process(pid)))
    }
}

#[derive(Debug, Default)]
struct HealthScript {
    answers: Script<String, Result<HealthReply, ObserveError>>,
    requests: Vec<String>,
}

/// A health client that answers what it is told, per URL, and records every URL asked.
#[derive(Clone, Debug, Default)]
pub struct FakeHealthClient(Arc<Mutex<HealthScript>>);

impl FakeHealthClient {
    /// Every GET of `url` answers `status` from now on.
    pub fn answer(&self, url: &str, status: u16) {
        let reply = HealthReply {
            status,
            body_bytes_read: 0,
        };
        lock(&self.0)
            .answers
            .steady
            .insert(url.to_owned(), Ok(reply));
    }

    /// The next GET of `url` answers `answer`, once.
    pub fn then(&self, url: &str, answer: Result<HealthReply, ObserveError>) {
        lock(&self.0).answers.queue(url.to_owned(), answer);
    }

    /// The URLs asked, in order.
    pub fn requests(&self) -> Vec<String> {
        lock(&self.0).requests.clone()
    }
}

impl HealthClient for FakeHealthClient {
    fn get(&self, url: &HealthUrl, _deadline: Deadline) -> Result<HealthReply, ObserveError> {
        let text = url.to_string();
        let mut script = lock(&self.0);
        script.requests.push(text.clone());
        let answer = script.answers.next(&text);
        answer.unwrap_or_else(|| {
            Err(ObserveError::Unreachable {
                url: text,
                why: "nothing listens".to_owned(),
            })
        })
    }
}

/// Digests by (service, version).
type Seals = BTreeMap<(String, String), [u8; 32]>;

/// The sealed executable digests, per service and version.
#[derive(Clone, Debug, Default)]
pub struct FakeExpectedExe(Arc<Mutex<Seals>>);

impl FakeExpectedExe {
    /// `version` of `service` was sealed with an executable of `digest`.
    pub fn seal(&self, service: &str, version: &str, digest: [u8; 32]) {
        let key = (service.to_owned(), version.to_owned());
        lock(&self.0).insert(key, digest);
    }
}

impl ExpectedExe for FakeExpectedExe {
    fn digest(&self, service: &str, version: &str) -> Option<[u8; 32]> {
        let key = (service.to_owned(), version.to_owned());
        lock(&self.0).get(&key).copied()
    }
}

#[derive(Debug, Default)]
struct RunnerScript {
    answers: VecDeque<Result<CommandOutput, RunError>>,
    calls: Vec<(Vec<String>, Duration)>,
}

/// A command runner that answers what it is told, in order, and records every argv and deadline.
#[derive(Clone, Debug, Default)]
pub struct FakeCommandRunner(Arc<Mutex<RunnerScript>>);

impl FakeCommandRunner {
    /// The next run answers `answer`.
    pub fn then(&self, answer: Result<CommandOutput, RunError>) {
        lock(&self.0).answers.push_back(answer);
    }

    /// Every run, in order: its argv and its deadline.
    pub fn calls(&self) -> Vec<(Vec<String>, Duration)> {
        lock(&self.0).calls.clone()
    }
}

impl CommandRunner for FakeCommandRunner {
    fn run(&self, argv: &[String], deadline: Duration) -> Result<CommandOutput, RunError> {
        let mut script = lock(&self.0);
        script.calls.push((argv.to_vec(), deadline));
        script.answers.pop_front().unwrap_or(Err(RunError::Empty))
    }
}

/// One step of what a fake peer does when read: give bytes, or fail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chunk {
    Bytes(Vec<u8>),
    Fail(io::ErrorKind),
}

#[derive(Debug, Default)]
struct PeerScript {
    connections: VecDeque<Result<Vec<Chunk>, io::ErrorKind>>,
    connected: Vec<(SocketAddr, Deadline)>,
    written: Vec<u8>,
    served: usize,
    write_failure: Option<io::ErrorKind>,
}

/// An in-memory transport for [`crate::HttpHealthClient`]: each connection plays the chunks it
/// was given, then reports the end of the stream. It records every address connected to, every
/// byte written, and how many bytes were read.
#[derive(Clone, Debug, Default)]
pub struct FakeConnect(Arc<Mutex<PeerScript>>);

impl FakeConnect {
    /// The next connection succeeds and its peer plays `chunks`.
    pub fn reply(&self, chunks: Vec<Chunk>) {
        lock(&self.0).connections.push_back(Ok(chunks));
    }

    /// The next connection fails with `kind`.
    pub fn refuse(&self, kind: io::ErrorKind) {
        lock(&self.0).connections.push_back(Err(kind));
    }

    /// Every write on a connection fails with `kind` from now on.
    pub fn fail_writes(&self, kind: io::ErrorKind) {
        lock(&self.0).write_failure = Some(kind);
    }

    /// Every connection attempt: its address and deadline.
    pub fn connected(&self) -> Vec<(SocketAddr, Deadline)> {
        lock(&self.0).connected.clone()
    }

    /// Every byte written, over every connection.
    pub fn written(&self) -> Vec<u8> {
        lock(&self.0).written.clone()
    }

    /// How many bytes were read, over every connection.
    pub fn served(&self) -> usize {
        lock(&self.0).served
    }
}

impl Connect for FakeConnect {
    fn connect(&self, addr: SocketAddr, deadline: Deadline) -> io::Result<Box<dyn Duplex>> {
        let mut script = lock(&self.0);
        script.connected.push((addr, deadline));
        let chunks = script
            .connections
            .pop_front()
            .unwrap_or(Err(io::ErrorKind::ConnectionRefused))?;
        Ok(Box::new(FakeStream {
            chunks: chunks.into(),
            script: Arc::clone(&self.0),
        }))
    }
}

/// One connection of [`FakeConnect`].
struct FakeStream {
    chunks: VecDeque<Chunk>,
    script: Arc<Mutex<PeerScript>>,
}

impl Read for FakeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.chunks.pop_front() {
            None => Ok(0),
            Some(Chunk::Fail(kind)) => Err(io::Error::from(kind)),
            Some(Chunk::Bytes(mut bytes)) => {
                let n = bytes.len().min(buf.len());
                let rest = bytes.split_off(n);
                buf[..n].copy_from_slice(&bytes);
                if !rest.is_empty() {
                    self.chunks.push_front(Chunk::Bytes(rest));
                }
                lock(&self.script).served += n;
                Ok(n)
            }
        }
    }
}

impl Write for FakeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut script = lock(&self.script);
        let failure = script.write_failure.map(io::Error::from);
        failure.map_or_else(
            || {
                script.written.extend_from_slice(buf);
                Ok(buf.len())
            },
            Err,
        )
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
