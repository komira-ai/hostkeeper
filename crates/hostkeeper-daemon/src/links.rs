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
//! sets the coordinator pushes to [`crate::repository_push::RepositoryPush`]; on every host it
//! gives the host's facts the coordinator sends to the facts file ([`FactsFile`]) and the operator keys
//! it sends to the host's [`Links::operator_keys`], and the host prerequisites it sends to the host's
//! [`Links::prerequisites`].
//!
//! The watchdog: after each heartbeat, whatever it came to, the heartbeat loop tells the service
//! manager it is alive ([`Links::watchdog`], `WATCHDOG=1` under systemd), and nothing else sends
//! that notice: no thread or timer of its own. So a heartbeat that never returns stops the
//! notices, and systemd, past the unit's `WatchdogSec=`, restarts the daemon. A notice that
//! cannot be sent is logged, and the loop goes on.
//!
//! On a Mac the heartbeat loop also checks, after each heartbeat and its notice, that the daemon's
//! own launchd job is not marked disabled ([`crate::own_job`]): a job that is gets enabled again
//! and is alerted, and a check that fails is alerted, the loop going on either way.
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
//! On every host, the enrollment loop also keeps the host's identity copy current after each step
//! ([`Links::identity`], [`crate::host_identity`]): the certificate and key of the pki directory's
//! `current` generation, readable by the group `hostkeeper-identity`, for the services of the
//! host. A failure there is logged and never reaches the enroller.
//!
//! On a Linux host, the enrollment loop also writes the console's issue file after each step,
//! and after each failure to make the enroller ([`Links::console`], [`crate::console`]): the
//! request's state, the coordinator and the key hash, for the login prompt of the machine's
//! console.
//!
//! The heartbeat loop's link to the coordinator is [`HostLink`]: the [`NegotiatedLink`] of
//! [`coordinator_link`] (the JSON route and gRPC, both over mutual TLS), made at the first
//! heartbeat the loop sends, on the loop's own worker. Both dial the coordinator's name where its
//! reach says ([`crate::coordinator_address`]): the addresses it looks up to, the one last
//! reached (kept in the state directory across a restart), and the config's `coordinator_ip`.

use std::io;
use std::net::SocketAddr;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_config::names::Version;
use hostkeeper_enroll::{Enroller, RETRY_SECS};
use hostkeeper_link::HostHealthSource;
use hostkeeper_link::{
    BmcSource, CertSource, ClientIdentity, CoordinatorLink, Dispatch, FullDiskAccessSource,
    GrpcLinkError, GrpcMtlsLink, Heartbeat, Heartbeater, JsonMtlsLink, LinkError, Lookup,
    LoopConfig, MaintainSink, MaintenanceSource, NegotiatedLink, OperatorKeysSink, Platform, Ports,
    PrerequisitesSink, Reach, RemoteLoginProbeSource, Reply, RepositorySink, SerialConsoleSource,
    Sleeper, SshSink, StopFlag, TcpDial, TcpTransport, Tick, UnixClock,
};
use hostkeeper_observe::ProcessRunner;
use hostkeeper_observe::command::MAX_STDOUT;
use hostkeeper_steps::Clock;

use crate::console::ConsoleIssue;
use crate::coordinator_address::{self, ADDRESS_FILE, host_reach};
use crate::executor::Daemon;
use crate::facts_file::FactsFile;
use crate::host_health::HealthKeeper;
use crate::host_identity::HostIdentity;
use crate::link_adapters::{
    DaemonHalts, DaemonMaintain, DaemonObserver, DaemonOrders, DaemonRecords, DaemonRenewal,
    NoTrustedRoot,
};
use crate::node::NodeState;
use crate::notify::Watchdog;
use crate::repository_push::RepositoryPush;
use crate::ssh_files::SshPaths;
use crate::ssh_host::{SshHost, SystemHost, Tools};
use crate::ssh_host_mac::MacHost;
use crate::ssh_setup::SshSetup;

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
    /// The host's identity copy ([`crate::host_identity`]), which the enrollment loop makes
    /// current after each step; none where a host keeps none.
    pub identity: Option<HostIdentity>,
    /// The Remote Login probe the daemon started ([`crate::remote_login_probe`]), which every
    /// heartbeat reports once it is done; none on any host but a Mac.
    pub remote_login_probe: Option<Arc<dyn RemoteLoginProbeSource>>,
    /// The state of the Full Disk Access grant ([`crate::full_disk_access`]), which every
    /// heartbeat reports; none where a host reports none.
    pub full_disk_access: Option<Arc<dyn FullDiskAccessSource>>,
    /// The host's operator keys ([`crate::operator_keys`]): the managed block of its login
    /// account's `authorized_keys`, which every heartbeat reports and a set the coordinator sends
    /// replaces. A daemon that has one announces `operator-keys-v1`.
    pub operator_keys: Option<Arc<dyn OperatorKeysSink>>,
    /// The host prerequisites of the host's group ([`crate::prerequisites`]), which every
    /// heartbeat reports and a set the coordinator sends replaces. A daemon that has them
    /// announces `prerequisites-v1`.
    pub prerequisites: Option<Arc<dyn PrerequisitesSink>>,
    /// The host's maintenance facts ([`crate::maintenance_facts`]), which every heartbeat to a
    /// coordinator that advertises `maintenance-v1` reports. A daemon that has them announces
    /// `maintenance-v1` and runs `maintain` orders ([`DaemonMaintain`]); none on a host with no
    /// collector, which takes no `maintain` order.
    pub maintenance: Option<Arc<dyn MaintenanceSource>>,
    /// The host's BMC ([`crate::bmc`]), which every heartbeat reports; none on a Mac.
    pub bmc: Option<Arc<dyn BmcSource>>,
    /// The host's serial console baseline ([`crate::serial_baseline`]), which every heartbeat
    /// reports; none on a host whose config names no `serial_console`.
    pub serial_console: Option<Arc<dyn SerialConsoleSource>>,
    /// The host's hardware health ([`crate::host_health`]), which every heartbeat to a coordinator
    /// that advertises `host-health-v1` reports, and whose clean-shutdown marker is judged at
    /// SIGTERM ([`crate::start::serve_until`]); none where a daemon keeps none.
    pub health: Option<Arc<HealthKeeper>>,
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
            mut identity,
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
            loop_ports.run(&mut enroller, &mut console, &mut identity);
        }));
        let beating = Arc::clone(daemon);
        dispatch.start(Box::new(move || {
            let node = &beating.ctx().node;
            heartbeater.run(&*sleeper, &mut |outcome| {
                node.beat(outcome, clock.unix_secs());
                alive(&*watchdog, beat_log.as_ref());
                // After the beat and the notice, so a check never holds either back; it alerts
                // what it finds and never fails.
                beating.ctx().check_own_job();
            });
        }));
    }

    /// The heartbeat loop over `daemon`: its executor, held halts, installed services and observer
    /// are the daemon's ([`crate::link_adapters`]), and it reports no trusted root; its step-record directory, where the
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
                observer: Arc::new(DaemonObserver::with_runs(
                    Arc::clone(daemon),
                    self.health.clone().filter(|_| counts_runs(PLATFORM)),
                    Arc::clone(&self.clock),
                )),
                orders: Arc::new(DaemonOrders::new(Arc::clone(daemon))),
                dispatch: Arc::clone(&self.dispatch),
                halts: Arc::new(DaemonHalts::new(Arc::clone(daemon))),
                renewal: Arc::new(DaemonRenewal::new(self.renewal.clone())),
                trusted_root: Arc::new(NoTrustedRoot),
                clock: Arc::clone(&self.clock),
                log: Arc::clone(&self.log),
                repositories: RepositoryPush::new(
                    Arc::clone(&ctx.repository_ports),
                    ctx.announcement.clone(),
                    Arc::clone(&self.log),
                )
                .map(|push| Arc::new(push) as Arc<dyn RepositorySink>),
                facts: Some(Arc::new(FactsFile::new(
                    ctx.dirs.facts.clone(),
                    PLATFORM,
                    Arc::clone(&self.log),
                ))),
                remote_login_probe: self.remote_login_probe.clone(),
                full_disk_access: self.full_disk_access.clone(),
                operator_keys: self.operator_keys.clone(),
                ssh: ssh_setup(PLATFORM, rustix::process::geteuid().as_raw(), &self.log),
                prerequisites: self.prerequisites.clone(),
                maintenance: self.maintenance.clone(),
                maintain: self.maintenance.as_ref().map(|_| {
                    Arc::new(DaemonMaintain::new(Arc::clone(daemon))) as Arc<dyn MaintainSink>
                }),
                bmc: self.bmc.clone(),
                serial_console: self.serial_console.clone(),
                health: self
                    .health
                    .clone()
                    .map(|keeper| keeper as Arc<dyn HostHealthSource>),
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
    /// before the wait that follows it. `identity`, when there is one, makes the host's identity
    /// copy current after each step, once `node` holds that step's enrollment. The loop ends
    /// when `stop` is set before a try, or the wait says [`Tick::Stop`].
    fn run(
        &self,
        make: &mut MakeEnroller,
        console: &mut Option<ConsoleIssue>,
        identity: &mut Option<HostIdentity>,
    ) {
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
                            if let Some(identity) = identity.as_mut() {
                                identity.after_step(self.log);
                            }
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

/// Whether a daemon of `platform` counts its services' restarts in the `runs` ring of its health
/// state file ([`crate::mac_runs`]) rather than in memory: a Mac's, whose launchd counts a job's
/// runs and no restarts.
pub const fn counts_runs(platform: Platform) -> bool {
    matches!(platform, Platform::DarwinArm64)
}

/// Whether a daemon of `platform` sets sshd up to trust the coordinator's SSH user CA
/// (`ssh-v1`): every platform's daemon does, a Linux one over systemd's or OpenRC's sshd and a
/// Mac's over Remote Login (docs/design/ssh-through-coordinator.md, section 3).
pub const fn sets_ssh_up(platform: Platform) -> bool {
    matches!(
        platform,
        Platform::LinuxX86_64 | Platform::LinuxArm64 | Platform::DarwinArm64
    )
}

/// The sshd setup of a daemon of `platform`, writing as `owner`: over the Linux host on Linux and
/// over the Mac's ([`MacHost`]) on a Mac; none where [`sets_ssh_up`] is false.
pub fn ssh_setup(platform: Platform, owner: u32, log: &Arc<dyn Log>) -> Option<Arc<dyn SshSink>> {
    let runner = Arc::new(ProcessRunner::new(MAX_STDOUT));
    let host: Arc<dyn SshHost> = match platform {
        Platform::DarwinArm64 => Arc::new(MacHost::system(runner, Arc::clone(log))),
        _ => Arc::new(SystemHost::new(runner, Tools::linux())),
    };
    sets_ssh_up(platform).then(|| {
        Arc::new(SshSetup::new(
            host,
            SshPaths::linux(),
            owner,
            Arc::clone(log),
        )) as Arc<dyn SshSink>
    })
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

/// The link to the coordinator `reach` names: a [`NegotiatedLink`] over the JSON route
/// ([`JsonMtlsLink`] through [`TcpTransport`], bounded on `clock`) and gRPC ([`GrpcMtlsLink`]
/// through [`TcpDial`]), both dialing the addresses of `reach`, which they share, so an address
/// one of them reached is the other's too. Both trust the CA bundle of the pki directory `pki`'s
/// current generation whose SHA-256 `config` pins (`ca_sha256`), read again at every heartbeat,
/// and both log to `log`. Nothing is read or dialed here.
///
/// The gRPC link owns a Tokio runtime: the link must be made, called and dropped outside any
/// other runtime ([`HostLink`] makes it on the heartbeat loop's worker).
///
/// # Errors
/// [`GrpcLinkError::Endpoint`]: the coordinator's address is not one the links dial.
/// [`GrpcLinkError::Runtime`]: the gRPC link's runtime cannot be started.
pub fn coordinator_link(
    config: &DaemonConfig,
    pki: &Path,
    reach: &Arc<Reach>,
    clock: Arc<dyn Clock>,
    log: Arc<dyn Log>,
) -> Result<NegotiatedLink, GrpcLinkError> {
    let grpc = GrpcMtlsLink::new(
        config,
        pki,
        Arc::clone(reach),
        Arc::new(TcpDial),
        Arc::clone(&log),
    )?;
    let transport = Arc::new(TcpTransport::new(Arc::clone(&clock)));
    let json = JsonMtlsLink::new(config, pki, Arc::clone(reach), transport, clock, log);
    Ok(NegotiatedLink::new(json, grpc))
}

/// The heartbeat loop's link in the shipped daemon: [`coordinator_link`] over the reach
/// [`host_reach`] makes (looked up through `lookup`, remembering the address of the state
/// directory's [`ADDRESS_FILE`]), made at the first heartbeat and kept. The loop calls the link
/// only once the host has a certificate, so a host not enrolled yet makes nothing; and the call
/// is on the loop's worker, outside the daemon's runtime, where the gRPC link's own runtime may
/// be made and dropped.
///
/// After each heartbeat, an address the reach reached that is not the one the file holds is
/// written to it; a write that fails is logged, once for that address.
///
/// A link that cannot be made leaves that heartbeat unsent, as [`LinkError::Transport`] of the
/// kind [`io::ErrorKind::Other`]; why ([`GrpcLinkError`]) is logged once, and again when it
/// changes. The next heartbeat tries again.
pub struct HostLink {
    config: DaemonConfig,
    pki: PathBuf,
    state_dir: PathBuf,
    lookup: Arc<dyn Lookup>,
    clock: Arc<dyn Clock>,
    log: Arc<dyn Log>,
    state: Mutex<Made>,
}

/// What [`HostLink`] has made so far.
#[derive(Default)]
struct Made {
    link: Option<Linked>,
    /// Why the last try failed, as logged.
    refused: Option<String>,
}

/// The link made, its reach, and the address the state file was last given.
struct Linked {
    link: NegotiatedLink,
    reach: Arc<Reach>,
    kept: Option<SocketAddr>,
}

impl HostLink {
    /// The link [`coordinator_link`] makes of these, once a heartbeat needs it, keeping the
    /// address it reaches in `state_dir`.
    pub fn new(
        config: DaemonConfig,
        pki: PathBuf,
        state_dir: PathBuf,
        lookup: Arc<dyn Lookup>,
        clock: Arc<dyn Clock>,
        log: Arc<dyn Log>,
    ) -> HostLink {
        HostLink {
            config,
            pki,
            state_dir,
            lookup,
            clock,
            log,
            state: Mutex::new(Made::default()),
        }
    }

    fn make(&self) -> Result<Linked, GrpcLinkError> {
        let reach = host_reach(
            &self.config,
            Some(&self.state_dir),
            Arc::clone(&self.lookup),
            Arc::clone(&self.log),
        )?;
        let reach = Arc::new(reach);
        let link = coordinator_link(
            &self.config,
            &self.pki,
            &reach,
            Arc::clone(&self.clock),
            Arc::clone(&self.log),
        )?;
        Ok(Linked {
            link,
            kept: reach.remembered(),
            reach,
        })
    }

    /// One heartbeat over `linked`, then the address it reached kept.
    fn beat(
        &self,
        linked: &mut Linked,
        identity: &ClientIdentity,
        beat: &Heartbeat,
    ) -> Result<Reply, LinkError> {
        let answer = linked.link.heartbeat(identity, beat);
        if let Some(addr) = linked.reach.remembered()
            && linked.kept != Some(addr)
        {
            linked.kept = Some(addr);
            if let Err(error) = coordinator_address::write(&self.state_dir, &linked.reach, addr) {
                self.log.line(&format!(
                    "error: the coordinator's address {addr} is not kept in {}: {error}",
                    self.state_dir.join(ADDRESS_FILE).display()
                ));
            }
        }
        answer
    }
}

impl CoordinatorLink for HostLink {
    fn heartbeat(&self, identity: &ClientIdentity, beat: &Heartbeat) -> Result<Reply, LinkError> {
        // One loop calls the link, one heartbeat at a time: the lock is never waited for.
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(linked) = state.link.as_mut() {
            return self.beat(linked, identity, beat);
        }
        match self.make() {
            Ok(made) => {
                let linked = state.link.insert(made);
                self.beat(linked, identity, beat)
            }
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
