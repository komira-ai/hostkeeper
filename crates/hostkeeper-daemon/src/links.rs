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
//! [`crate::link_adapters`] ([`Links::heartbeater`]). On a Linux host it also gives the repository
//! sets the coordinator pushes to [`crate::repository_push::RepositoryPush`].
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
//! An enroller that cannot be made (the coordinator's address refused, the pki directory locked
//! or unreadable) is made again, for the life of the process: after a wait on
//! [`Links::enroll_sleeper`] of [`OPEN_RETRY_FIRST_SECS`], doubled after each failure up to
//! [`OPEN_RETRY_MAX_SECS`]. Each failure is noted in [`Links::renewal`], the enrollment loop's
//! state that every heartbeat reports ([`crate::link_adapters::DaemonRenewal`]), with the end of
//! the current certificate and the error of the enroller's last step, which the enroller notes
//! once it runs.
//!
//! On a Linux host, the enrollment loop also writes the console's issue file after each step,
//! and after each failure to make the enroller ([`Links::console`], [`crate::console`]): the
//! request's state, the coordinator and the key hash, for the login prompt of the machine's
//! console.
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
use hostkeeper_enroll::{Enroller, RETRY_SECS};
use hostkeeper_link::{
    CertSource, ClientIdentity, CoordinatorLink, Dispatch, GrpcLinkError, GrpcMtlsLink, Heartbeat,
    Heartbeater, JsonMtlsLink, LinkError, LoopConfig, NegotiatedLink, Platform, Ports, Reply,
    RepositorySink, Sleeper, StopFlag, TcpDial, TcpTransport, Tick, UnixClock,
};
use hostkeeper_steps::Clock;

use crate::console::ConsoleIssue;
use crate::executor::Daemon;
use crate::link_adapters::{
    DaemonHalts, DaemonObserver, DaemonOrders, DaemonRecords, DaemonRenewal, DaemonTrustedRoot,
};
use crate::node::NodeState;
use crate::notify::Watchdog;
use crate::repository_push::RepositoryPush;

pub use hostkeeper_enroll::RenewalState;
pub use hostkeeper_link::Log;

/// Makes the enroller of the enrollment loop, on the loop's own worker: the enroller's transport
/// may own an async runtime, which must be made, used and dropped outside the daemon's. An
/// `Err` says why there is no enroller yet; the loop logs it, notes it, and makes it again after
/// its wait ([`OPEN_RETRY_FIRST_SECS`]).
pub type MakeEnroller = Box<dyn FnMut() -> Result<Enroller, String> + Send>;

/// The line the enrollment loop logs when it has no enroller, before the reason.
pub const NO_ENROLLMENT: &str = "error: the enrollment loop cannot start yet: ";

/// The wait, in seconds, after the first failure to make the enroller: the pace of the
/// enroller's own steps ([`RETRY_SECS`]).
pub const OPEN_RETRY_FIRST_SECS: u64 = RETRY_SECS;

/// The longest wait, in seconds, between two tries to make the enroller: five minutes, so a host
/// whose enroller could not be made (its coordinator briefly away, its clock wrong at boot) tries
/// again within five minutes of the cause going away, and one that cannot make it at all tries
/// twelve times an hour.
pub const OPEN_RETRY_MAX_SECS: u64 = 300;

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
    /// The unix clock of the deferrals, and of the last answered heartbeat `Status` reports.
    pub clock: Arc<dyn UnixClock>,
    /// Where both loops' lines go.
    pub log: Arc<dyn Log>,
    /// The heartbeat loop's wait between two heartbeats.
    pub sleeper: Box<dyn Sleeper + Send>,
    /// The enrollment loop's wait between two tries to make its enroller; [`Tick::Stop`] ends the
    /// loop.
    pub enroll_sleeper: Box<dyn Sleeper + Send>,
    /// What the enrollment loop last said of the certificate, which every heartbeat reports.
    pub renewal: RenewalState,
    /// Seconds between two heartbeats (`--heartbeat-secs`).
    pub period: NonZeroU64,
    /// Starts each loop, and each order of the heartbeat loop, on a worker of its own.
    pub dispatch: Arc<dyn Dispatch>,
    /// Stops the loops; the enrollment loop reads it before each step. The production sleeper
    /// reads the same flag.
    pub stop: StopFlag,
    /// Told after each heartbeat that the heartbeat loop is alive.
    pub watchdog: Box<dyn Watchdog>,
    /// The console's issue file, which the enrollment loop writes after each step and each
    /// failure to make its enroller; none on a host whose console shows none (a Mac).
    pub console: Option<ConsoleIssue>,
}

impl Links {
    /// Starts the enrollment loop, then the heartbeat loop, each on a worker of
    /// [`Links::dispatch`]. Each loop writes what [`Daemon::status`] reports of it into the
    /// daemon's [`crate::node::NodeState`]: the enrollment loop the host's enrollment after each
    /// step, the heartbeat loop its link after each heartbeat, at the second of [`Links::clock`].
    pub fn start(self, daemon: &Arc<Daemon>) {
        let heartbeater = self.heartbeater(daemon);
        let Links {
            mut enroller,
            clock,
            log,
            sleeper,
            enroll_sleeper,
            renewal,
            dispatch,
            stop,
            watchdog,
            mut console,
            ..
        } = self;
        let enrolling = Arc::clone(daemon);
        let beat_log = Arc::clone(&log);
        dispatch.start(Box::new(move || {
            let loop_ports = EnrollLoop {
                log: log.as_ref(),
                node: &enrolling.ctx().node,
                renewal: &renewal,
                sleeper: &*enroll_sleeper,
                stop: &stop,
            };
            loop_ports.run(&mut enroller, &mut console);
        }));
        let beating = Arc::clone(daemon);
        dispatch.start(Box::new(move || {
            let node = &beating.ctx().node;
            heartbeater.run(&*sleeper, &mut |outcome| {
                node.beat(outcome, clock.unix_secs());
                alive(&*watchdog, beat_log.as_ref());
            });
        }));
    }

    /// The heartbeat loop over `daemon`: its executor, held halts, installed services, observer
    /// and trusted root are the daemon's ([`crate::link_adapters`]); its step-record directory, where the
    /// deferrals live, is the daemon's state directory; it announces this build's platform and
    /// the daemon's own version, and the installers the daemon decided at its start.
    pub fn heartbeater(&self, daemon: &Arc<Daemon>) -> Heartbeater {
        let ctx = daemon.ctx();
        Heartbeater::new(
            LoopConfig {
                platform: PLATFORM,
                agent_version: Version::from(ctx.own_version),
                state_dir: ctx.dirs.state.clone(),
                period_secs: self.period,
                installers: ctx.announcement.names(),
                private_repository: reads_private_repositories(PLATFORM),
            },
            Ports {
                link: Arc::clone(&self.link),
                certs: Arc::clone(&self.certs),
                records: Arc::new(DaemonRecords::new(Arc::clone(daemon))),
                observer: Arc::new(DaemonObserver::new(Arc::clone(daemon))),
                orders: Arc::new(DaemonOrders::new(Arc::clone(daemon))),
                dispatch: Arc::clone(&self.dispatch),
                halts: Arc::new(DaemonHalts::new(Arc::clone(daemon))),
                renewal: Arc::new(DaemonRenewal::new(self.renewal.clone())),
                trusted_root: Arc::new(DaemonTrustedRoot::new(Arc::clone(daemon))),
                clock: Arc::clone(&self.clock),
                log: Arc::clone(&self.log),
                repositories: RepositoryPush::new(
                    Arc::clone(&ctx.repository_ports),
                    ctx.announcement.clone(),
                    Arc::clone(&self.log),
                )
                .map(|push| Arc::new(push) as Arc<dyn RepositorySink>),
            },
        )
    }
}

/// The watchdog notice the heartbeat loop sends after each heartbeat, whatever it came to; a
/// notice that cannot be sent is logged ([`NO_WATCHDOG_NOTICE`]) and the loop goes on.
fn alive(watchdog: &dyn Watchdog, log: &dyn Log) {
    if let Err(e) = watchdog.alive() {
        log.line(&format!("{NO_WATCHDOG_NOTICE}{e}"));
    }
}

/// What the enrollment loop works with besides its enroller.
struct EnrollLoop<'a> {
    log: &'a dyn Log,
    node: &'a NodeState,
    renewal: &'a RenewalState,
    sleeper: &'a dyn Sleeper,
    stop: &'a StopFlag,
}

impl EnrollLoop<'_> {
    /// The enrollment loop: the enroller made, then its steps until `stop` is set, the host's
    /// enrollment after each held in `node` and its certificate's end and error in `renewal`.
    /// While the enroller cannot be made, each failure is noted in `renewal` and logged
    /// ([`NO_ENROLLMENT`], once, and again when the reason changes), and the enroller is made
    /// again after a wait on `sleeper` of [`OPEN_RETRY_FIRST_SECS`], doubled after each failure
    /// up to [`OPEN_RETRY_MAX_SECS`]; `node` is left without an enrollment meanwhile. `console`,
    /// when there is one, shows what each step came to, and each failure to make the enroller,
    /// before the wait that follows it. The loop ends when `stop` is set before a try, or the
    /// wait says [`Tick::Stop`].
    fn run(&self, make: &mut MakeEnroller, console: &mut Option<ConsoleIssue>) {
        let mut wait = OPEN_RETRY_FIRST_SECS;
        let mut logged: Option<String> = None;
        while !self.stop.is_stopped() {
            match make() {
                Ok(mut enroller) => {
                    enroller.run_loop(
                        &mut |line: &str| self.log.line(line),
                        &mut |step, enrollment| {
                            if let Some(console) = console.as_mut() {
                                console.after_step(step, &enrollment, self.log);
                            }
                            self.node.set_enrollment(enrollment);
                        },
                        self.renewal,
                        &mut || !self.stop.is_stopped(),
                    );
                    return;
                }
                Err(why) => {
                    if logged.as_deref() != Some(why.as_str()) {
                        self.log.line(&format!("{NO_ENROLLMENT}{why}"));
                    }
                    self.renewal
                        .failed(format!("the enroller cannot be opened: {why}"));
                    if let Some(console) = console.as_mut() {
                        console.unopened(&why, self.log);
                    }
                    logged = Some(why);
                    if self.sleeper.sleep(wait) == Tick::Stop {
                        return;
                    }
                    wait = wait.saturating_mul(2).min(OPEN_RETRY_MAX_SECS);
                }
            }
        }
    }
}

/// Whether a daemon of `platform` announces `private-repository-v1` whatever its other ports:
/// every Mac daemon does, since its `coordinator` store reads a config package from the
/// coordinator ([`crate::coordinator_store`]). A Linux daemon announces it when it has a
/// repository sink (the link's rule), which is when its mirror reads private repositories.
pub const fn reads_private_repositories(platform: Platform) -> bool {
    matches!(platform, Platform::DarwinArm64)
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
