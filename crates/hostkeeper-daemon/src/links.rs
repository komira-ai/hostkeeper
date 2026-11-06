//! The daemon's two loops that talk to its coordinator (docs/design/hostkeeper-design.md,
//! sections 5.7, 6.5 and 8.4): the enrollment loop of `hostkeeper-enroll`, which gets the host
//! certificate and renews it under the pki directory, and the heartbeat loop of
//! `hostkeeper-link`, which reads the certificate of the pki directory's `current` link at every
//! heartbeat. So the generation the enrollment loop writes is the one the next heartbeat
//! presents, and a host with no certificate yet beats nothing: its loop says once what it waits
//! for, and tries again every period.
//!
//! [`Links`] is everything the two loops need besides the daemon. [`Links::start`] starts them,
//! the enrollment loop first, each on a worker of its [`Dispatch`]; [`crate::start::serve`] calls
//! it once the boot work is over and the readiness notice was sent, so neither loop runs while
//! the boot pass does. The heartbeat loop works on the daemon through the four adapters of
//! [`crate::link_adapters`] ([`Links::heartbeater`]).
//!
//! The watchdog: after each heartbeat, whatever it came to, the heartbeat loop tells the service
//! manager it is alive ([`Links::watchdog`], `WATCHDOG=1` under systemd), and nothing else sends
//! that notice: no thread or timer of its own. So a heartbeat that never returns stops the
//! notices, and systemd, past the unit's `WatchdogSec=`, restarts the daemon. A notice that
//! cannot be sent is logged, and the loop goes on.
//!
//! The loops stop when [`Links::stop`] is set: the enrollment loop before its next step, the
//! heartbeat loop at the end of its wait (`hostkeeper_link::ClockSleeper`). An order running on
//! a worker goes on; its verdict is in its step record.
//!
//! The heartbeat loop's link to the coordinator is [`HostLink`]: the [`NegotiatedLink`] of
//! [`coordinator_link`] (the JSON route and gRPC, both over mutual TLS), made at the first
//! heartbeat the loop sends, on the loop's own worker.

use std::io;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_config::names::Version;
use hostkeeper_enroll::Enroller;
use hostkeeper_link::{
    CertSource, ClientIdentity, CoordinatorLink, Dispatch, GrpcLinkError, GrpcMtlsLink, Heartbeat,
    Heartbeater, JsonMtlsLink, LinkError, LoopConfig, NegotiatedLink, Platform, Ports, Reply,
    Sleeper, StopFlag, TcpDial, TcpTransport, Tick, UnixClock,
};
use hostkeeper_steps::Clock;

use crate::executor::Daemon;
use crate::link_adapters::{DaemonHalts, DaemonObserver, DaemonOrders, DaemonRecords};
use crate::notify::Watchdog;

pub use hostkeeper_link::Log;

/// Makes the enroller of the enrollment loop, on the loop's own worker: the enroller's transport
/// may own an async runtime, which must be made, used and dropped outside the daemon's. An
/// `Err` says why there is no enrollment; the loop logs it and ends.
pub type MakeEnroller = Box<dyn FnOnce() -> Result<Enroller, String> + Send>;

/// The line the enrollment loop logs when it has no enroller, before the reason.
pub const NO_ENROLLMENT: &str = "error: the enrollment loop does not run: ";

/// The line the heartbeat loop logs when its watchdog notice was not sent, before the error.
pub const NO_WATCHDOG_NOTICE: &str = "error: the watchdog notice was not sent: ";

/// What the two loops need besides the daemon.
pub struct Links {
    /// The enrollment loop's enroller.
    pub enroller: MakeEnroller,
    /// The heartbeat loop's one outbound call.
    pub link: Arc<dyn CoordinatorLink>,
    /// The host's client certificate and key, read at every heartbeat.
    pub certs: Arc<dyn CertSource>,
    /// The unix clock of the deferrals.
    pub clock: Arc<dyn UnixClock>,
    /// Where both loops' lines go.
    pub log: Arc<dyn Log>,
    /// The heartbeat loop's wait between two heartbeats.
    pub sleeper: Box<dyn Sleeper + Send>,
    /// Seconds between two heartbeats (`--heartbeat-secs`).
    pub period: NonZeroU64,
    /// Starts each loop, and each order of the heartbeat loop, on a worker of its own.
    pub dispatch: Arc<dyn Dispatch>,
    /// Stops the loops; the enrollment loop reads it before each step. The production sleeper
    /// reads the same flag.
    pub stop: StopFlag,
    /// Told after each heartbeat that the heartbeat loop is alive.
    pub watchdog: Box<dyn Watchdog>,
}

impl Links {
    /// Starts the enrollment loop, then the heartbeat loop, each on a worker of
    /// [`Links::dispatch`].
    pub fn start(self, daemon: &Arc<Daemon>) {
        let heartbeater = self.heartbeater(daemon);
        let Links {
            enroller,
            log,
            sleeper,
            period,
            dispatch,
            stop,
            watchdog,
            ..
        } = self;
        let beat_log = Arc::clone(&log);
        dispatch.start(Box::new(move || enroll(enroller, log.as_ref(), &stop)));
        dispatch.start(Box::new(move || {
            beat(
                heartbeater,
                &*sleeper,
                period,
                &*watchdog,
                beat_log.as_ref(),
            )
        }));
    }

    /// The heartbeat loop over `daemon`: its executor, held halts, installed services and
    /// observer are the daemon's ([`crate::link_adapters`]); its step-record directory, where the
    /// deferrals live, is the daemon's state directory; it announces this build's platform and
    /// the daemon's own version.
    pub fn heartbeater(&self, daemon: &Arc<Daemon>) -> Heartbeater {
        let ctx = daemon.ctx();
        Heartbeater::new(
            LoopConfig {
                platform: PLATFORM,
                agent_version: Version::from(ctx.own_version),
                state_dir: ctx.dirs.state.clone(),
                period_secs: self.period,
            },
            Ports {
                link: Arc::clone(&self.link),
                certs: Arc::clone(&self.certs),
                records: Arc::new(DaemonRecords::new(Arc::clone(daemon))),
                observer: Arc::new(DaemonObserver::new(Arc::clone(daemon))),
                orders: Arc::new(DaemonOrders::new(Arc::clone(daemon))),
                dispatch: Arc::clone(&self.dispatch),
                halts: Arc::new(DaemonHalts::new(Arc::clone(daemon))),
                clock: Arc::clone(&self.clock),
                log: Arc::clone(&self.log),
            },
        )
    }
}

/// The heartbeat loop: a heartbeat now and after every `period` seconds, until `sleeper` says
/// stop, each followed by the watchdog notice. An order running on a worker when it stops goes
/// on; its verdict is in its step record.
fn beat(
    mut heartbeater: Heartbeater,
    sleeper: &dyn Sleeper,
    period: NonZeroU64,
    watchdog: &dyn Watchdog,
    log: &dyn Log,
) {
    loop {
        heartbeater.beat_once();
        if let Err(e) = watchdog.alive() {
            log.line(&format!("{NO_WATCHDOG_NOTICE}{e}"));
        }
        if sleeper.sleep(period.get()) == Tick::Stop {
            return;
        }
    }
}

/// The enrollment loop: the enroller made, then its steps until `stop` is set; without an
/// enroller, the reason logged once ([`NO_ENROLLMENT`]).
fn enroll(make: MakeEnroller, log: &dyn Log, stop: &StopFlag) {
    match make() {
        Ok(mut enroller) => {
            enroller.run_loop(&mut |line: &str| log.line(line), &mut || !stop.is_stopped())
        }
        Err(why) => log.line(&format!("{NO_ENROLLMENT}{why}")),
    }
}

/// The platform this daemon was built for.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const PLATFORM: Platform = Platform::LinuxX86_64;

/// The platform this daemon was built for.
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub const PLATFORM: Platform = Platform::LinuxArm64;

/// The platform this daemon was built for.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const PLATFORM: Platform = Platform::DarwinArm64;

/// The loops' log: each line through the daemon's alert sink, as `hostkeeper daemon: <line>`
/// ([`crate::executor::Ctx::log`]).
pub struct DaemonLog {
    daemon: Arc<Daemon>,
}

impl DaemonLog {
    pub fn new(daemon: Arc<Daemon>) -> DaemonLog {
        DaemonLog { daemon }
    }
}

impl Log for DaemonLog {
    fn line(&self, text: &str) {
        self.daemon.ctx().log(text);
    }
}

/// The link to the coordinator of `config`: a [`NegotiatedLink`] over the JSON route
/// ([`JsonMtlsLink`] through [`TcpTransport`], bounded on `clock`) and gRPC ([`GrpcMtlsLink`]
/// through [`TcpDial`]). Both trust the CA bundle of the pki directory `pki`'s current generation
/// whose SHA-256 `config` pins (`ca_sha256`), read again at every heartbeat, and both log to
/// `log`. Nothing is read or dialed here.
///
/// The gRPC link owns a Tokio runtime: the link must be made, called and dropped outside any
/// other runtime ([`HostLink`] makes it on the heartbeat loop's worker).
///
/// # Errors
/// [`GrpcLinkError::Endpoint`]: `config`'s coordinator address is not one the links dial.
/// [`GrpcLinkError::Runtime`]: the gRPC link's runtime cannot be started.
pub fn coordinator_link(
    config: &DaemonConfig,
    pki: &Path,
    clock: Arc<dyn Clock>,
    log: Arc<dyn Log>,
) -> Result<NegotiatedLink, GrpcLinkError> {
    let grpc = GrpcMtlsLink::new(config, pki, Arc::new(TcpDial), Arc::clone(&log))?;
    let json = JsonMtlsLink::new(config, pki, Arc::new(TcpTransport::new(clock)), log)?;
    Ok(NegotiatedLink::new(json, grpc))
}

/// The heartbeat loop's link in the shipped daemon: [`coordinator_link`], made at the first
/// heartbeat and kept. The loop calls the link only once the host has a certificate, so a host
/// not enrolled yet makes nothing; and the call is on the loop's worker, outside the daemon's
/// runtime, where the gRPC link's own runtime may be made and dropped.
///
/// A link that cannot be made leaves that heartbeat unsent, as [`LinkError::Transport`] of the
/// kind [`io::ErrorKind::Other`]; why ([`GrpcLinkError`]) is logged once, and again when it
/// changes. The next heartbeat tries again.
pub struct HostLink {
    config: DaemonConfig,
    pki: PathBuf,
    clock: Arc<dyn Clock>,
    log: Arc<dyn Log>,
    state: Mutex<Made>,
}

/// What [`HostLink`] has made so far.
#[derive(Default)]
struct Made {
    link: Option<NegotiatedLink>,
    /// Why the last try failed, as logged.
    refused: Option<String>,
}

impl HostLink {
    /// The link [`coordinator_link`] makes of these, once a heartbeat needs it.
    pub fn new(
        config: DaemonConfig,
        pki: PathBuf,
        clock: Arc<dyn Clock>,
        log: Arc<dyn Log>,
    ) -> HostLink {
        HostLink {
            config,
            pki,
            clock,
            log,
            state: Mutex::new(Made::default()),
        }
    }
}

impl CoordinatorLink for HostLink {
    fn heartbeat(&self, identity: &ClientIdentity, beat: &Heartbeat) -> Result<Reply, LinkError> {
        // One loop calls the link, one heartbeat at a time: the lock is never waited for.
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(link) = &state.link {
            return link.heartbeat(identity, beat);
        }
        let made = coordinator_link(
            &self.config,
            &self.pki,
            Arc::clone(&self.clock),
            Arc::clone(&self.log),
        );
        match made {
            Ok(link) => state.link.insert(link).heartbeat(identity, beat),
            Err(error) => {
                let why = error.to_string();
                if state.refused.as_deref() != Some(why.as_str()) {
                    self.log
                        .line(&format!("error: no link to the coordinator: {why}"));
                    state.refused = Some(why);
                }
                Err(LinkError::Transport(io::ErrorKind::Other))
            }
        }
    }
}
