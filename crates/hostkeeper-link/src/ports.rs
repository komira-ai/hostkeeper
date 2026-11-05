//! The ports of the loop: everything it talks to, each a trait the daemon implements over the
//! real thing and `testing` over a fake. [`ThreadDispatch`] and [`ClockSleeper`] are the two
//! the crate implements itself, because they need nothing of the daemon.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_steps::Clock;

use crate::beat::{Health, Heartbeat, Liveness, Oversize};
use crate::names::Reason;
use crate::order::{Order, OrderResult, Reply};

/// What a reply was refused for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReplyFault {
    #[error("it holds a key the daemon does not know")]
    UnknownKey,
    #[error("it is longer than the reply bound")]
    OverBound,
    #[error("it is not a reply")]
    Malformed,
}

/// Why a heartbeat got no reply.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// The coordinator could not be reached, or the connection failed.
    #[error("the coordinator could not be reached: {0}")]
    Transport(io::ErrorKind),
    /// The coordinator answered with a refusal, by its token (`forbidden`).
    #[error("the coordinator refused the heartbeat: {}", .0.as_str())]
    Refused(Reason),
    /// The coordinator's answer was not a reply the daemon reads.
    #[error("the coordinator's reply is refused: {0}")]
    BadReply(ReplyFault),
    /// The heartbeat was not sent.
    #[error("the heartbeat is not sent: {0}")]
    TooLarge(Oversize),
    /// The heartbeat was not sent: the link's wire cannot carry one of its facts.
    #[error("the heartbeat is not sent: {0}")]
    Unsendable(Unsendable),
    /// The heartbeat was not sent: the TLS of the link refused the coordinator or this host.
    #[error("the heartbeat is not sent: {0}")]
    Untrusted(TrustFault),
}

/// A fact of a heartbeat the JSON route's wire has no word for: its names and versions have
/// narrower grammars than the daemon's (design 11.8).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Unsendable {
    #[error(
        "the service name {0:?} is not one the JSON heartbeat carries: 1 to 64 bytes of a-z, 0-9 and -"
    )]
    ServiceName(String),
    #[error(
        "the version {0:?} is not one the JSON heartbeat carries: a digit, then letters, digits and . + ~ -, at most 128 bytes"
    )]
    Version(String),
}

/// What the TLS of the link refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TrustFault {
    /// No certificate of the CA bundle has the SHA-256 the daemon config pins, or the bundle
    /// cannot be read: nothing is dialed.
    #[error("no certificate of the CA bundle has the SHA-256 the daemon config pins")]
    NoPinnedCa,
    /// What answered presented a certificate that does not chain to the pinned CA, or is not
    /// for the coordinator's name, or is not a server's.
    #[error("what answered is not the coordinator: its certificate is refused")]
    ServerCertificate,
    /// The host's certificate and key do not make a TLS client identity.
    #[error("the client certificate and key are not a TLS identity")]
    ClientIdentity,
}

/// The coordinator, as the loop reaches it. [`CoordinatorLink::heartbeat`] is the one outbound
/// call the loop makes.
pub trait CoordinatorLink: Send + Sync {
    /// Sends `beat` with the client certificate and key of `identity`, and returns the reply.
    ///
    /// # Errors
    /// The coordinator could not be reached, refused the heartbeat, or answered with something
    /// that is not a reply.
    fn heartbeat(&self, identity: &ClientIdentity, beat: &Heartbeat) -> Result<Reply, LinkError>;
}

/// The host's client certificate and key, as the certificate source read them. The key never
/// shows in a log: `Debug` prints its length only.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientIdentity {
    certificate: Vec<u8>,
    key: Vec<u8>,
}

impl ClientIdentity {
    pub fn new(certificate: Vec<u8>, key: Vec<u8>) -> ClientIdentity {
        ClientIdentity { certificate, key }
    }

    /// The certificate chain, PEM.
    pub fn certificate(&self) -> &[u8] {
        &self.certificate
    }

    /// The private key, PEM.
    pub fn key(&self) -> &[u8] {
        &self.key
    }
}

impl fmt::Debug for ClientIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ClientIdentity {{ certificate: {} bytes, key: redacted }}",
            self.certificate.len()
        )
    }
}

/// What the certificate source has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CertState {
    /// The pair to use for this heartbeat.
    Enrolled(ClientIdentity),
    /// No usable pair: the loop says once what it waits for and makes no call.
    NotEnrolled { waiting_for: PathBuf },
}

/// The host's client certificate and key, read again at every heartbeat (design 10: the files
/// under the `current` link of the pki directory, which enrollment writes).
pub trait CertSource: Send + Sync {
    fn current(&self) -> CertState;
}

/// One installed service, as its step record says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedService {
    pub name: ServiceName,
    /// The version the host runs.
    pub version: Version,
    /// The target of an update whose step record is not finished; none when none is.
    pub updating: Option<Version>,
    /// The verdict of the last order the record finished; the loop resends it after a restart.
    pub last_result: Option<OrderResult>,
}

/// The services installed on the host, from their step records: never a list of names from a
/// file or from the configuration.
pub trait ServiceRecords: Send + Sync {
    /// # Errors
    /// The step records cannot be read; the loop then sends no heartbeat.
    fn installed(&self) -> io::Result<Vec<RecordedService>>;
}

/// How a service is now, as the observer saw it (design 5.8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub liveness: Liveness,
    /// The version the observer confirmed running; none when it could not tell.
    pub version: Option<Version>,
    pub health: Health,
    /// Why the health is what it is.
    pub health_detail: Option<String>,
}

/// The service observer: liveness, version and health, observed and not reported by the
/// service.
pub trait ServiceObserver: Send + Sync {
    fn observe(&self, service: &ServiceName) -> Observation;
}

/// The executor. It checks and runs one order and returns its verdict; it may take minutes, and
/// the loop runs it on a worker of its [`Dispatch`]. The try semantics (a resend answered from
/// the record, `try_conflict`, `stale_try`) are the executor's.
pub trait OrderSink: Send + Sync {
    fn execute(&self, order: &Order) -> OrderResult;
}

/// One job for a worker.
pub type Job = Box<dyn FnOnce() + Send>;

/// Starts a job on a worker of its own. A job that is dropped without running is an order
/// that ended with no verdict, and the loop says so.
pub trait Dispatch: Send + Sync {
    fn start(&self, job: Job);
}

/// The production dispatcher: one thread per job.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadDispatch;

impl Dispatch for ThreadDispatch {
    fn start(&self, job: Job) {
        // The thread is not joined: the loop learns the end of the job from the job itself. A
        // thread that cannot be made drops the job, which the loop reads as no verdict.
        let _ = std::thread::Builder::new()
            .name("hostkeeper-order".to_owned())
            .spawn(job);
    }
}

/// The halt generations the daemon holds, persisted beside the step records (design 10, "Held
/// halt generation"), and shared with the executor and `clear_halt`.
pub trait HeldHalts: Send + Sync {
    /// The generation held for `service`; `0` when none is.
    fn held(&self, service: &ServiceName) -> u64;
    /// Raise the generation held for `service` to `generation`; never lowers it.
    ///
    /// # Errors
    /// The store does not take it; the loop logs it and the next reply tries again.
    fn raise(&self, service: &ServiceName, generation: u64) -> io::Result<()>;
}

/// Unix seconds, for the retry time of a deferral, which must outlive a restart and so cannot be
/// a monotonic reading.
pub trait UnixClock: Send + Sync {
    fn unix_secs(&self) -> u64;
}

/// Where the loop's lines go.
pub trait Log: Send + Sync {
    fn line(&self, text: &str);
}

/// What the loop does after a wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    /// Send the next heartbeat.
    Beat,
    /// Stop the loop.
    Stop,
}

/// The wait between two heartbeats.
pub trait Sleeper {
    fn sleep(&self, secs: u64) -> Tick;
}

/// Asks a [`ClockSleeper`] to stop the loop at the end of its wait.
#[derive(Clone, Debug, Default)]
pub struct StopFlag(Arc<AtomicBool>);

impl StopFlag {
    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The production sleeper: waits on the monotonic clock of `hostkeeper-steps`, and stops the
/// loop when its flag is set by the end of the wait.
pub struct ClockSleeper {
    clock: Arc<dyn Clock>,
    stop: StopFlag,
}

impl ClockSleeper {
    pub fn new(clock: Arc<dyn Clock>, stop: StopFlag) -> ClockSleeper {
        ClockSleeper { clock, stop }
    }
}

impl Sleeper for ClockSleeper {
    fn sleep(&self, secs: u64) -> Tick {
        self.clock.sleep_secs(secs);
        if self.stop.is_stopped() {
            Tick::Stop
        } else {
            Tick::Beat
        }
    }
}
