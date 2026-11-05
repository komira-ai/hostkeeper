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
//! The loops stop when [`Links::stop`] is set: the enrollment loop before its next step, the
//! heartbeat loop at the end of its wait (`hostkeeper_link::ClockSleeper`). An order running on
//! a worker goes on; its verdict is in its step record.

use std::io;
use std::num::NonZeroU64;
use std::sync::Arc;

use hostkeeper_config::names::Version;
use hostkeeper_enroll::Enroller;
use hostkeeper_link::{
    CertSource, ClientIdentity, CoordinatorLink, Dispatch, Heartbeat, Heartbeater, LinkError,
    LoopConfig, Platform, Ports, Reply, Sleeper, StopFlag, UnixClock,
};

use crate::executor::Daemon;
use crate::link_adapters::{DaemonHalts, DaemonObserver, DaemonOrders, DaemonRecords};

pub use hostkeeper_link::Log;

/// Makes the enroller of the enrollment loop, on the loop's own worker: the enroller's transport
/// may own an async runtime, which must be made, used and dropped outside the daemon's. An
/// `Err` says why there is no enrollment; the loop logs it and ends.
pub type MakeEnroller = Box<dyn FnOnce() -> Result<Enroller, String> + Send>;

/// The line the enrollment loop logs when it has no enroller, before the reason.
pub const NO_ENROLLMENT: &str = "error: the enrollment loop does not run: ";

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
            dispatch,
            stop,
            ..
        } = self;
        dispatch.start(Box::new(move || enroll(enroller, log.as_ref(), &stop)));
        dispatch.start(Box::new(move || heartbeater.run(&*sleeper)));
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

/// The coordinator link of a build that has none: every heartbeat ends unsent, as a connection
/// this build does not support.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoLink;

impl CoordinatorLink for NoLink {
    fn heartbeat(&self, _identity: &ClientIdentity, _beat: &Heartbeat) -> Result<Reply, LinkError> {
        Err(LinkError::Transport(io::ErrorKind::Unsupported))
    }
}
