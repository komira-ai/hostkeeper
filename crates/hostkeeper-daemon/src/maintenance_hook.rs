//! `hostkeeper maintenance-hook <prepare|cutover|verify> --params <file>`: the maintenance hook,
//! the OS work of a maintenance run (docs/design/maintenance-updates.md, section 5.2), run the way
//! the step runner runs a hook's step.
//!
//! **Parameters.** The fields of the run's `maintain` order (`hostkeeper.agent.v1.Maintain`) as
//! proto3 canonical JSON, in the file `--params` names, or on standard input when it names `-`
//! ([`MaintainParams`]). No argument carries a parameter, and nothing the hook writes holds one.
//!
//! **Kinds.** Each kind of the run (`macos`, `xcode`, `linux`, `reboot`) is the work of a
//! [`MaintenanceKind`], found in the host platform's [`Registry`] ([`registry_for`]); a kind the
//! platform has no work for fails `unsupported_kind`. On a Linux host (systemd or OpenRC) the
//! `cutover` and the `after_reboot` of `linux` and `reboot` are written
//! ([`crate::maintenance_linux`]), and so is their `verify` ([`LinuxCheck`], the module
//! [`linux`]), and the `prepare` of `linux`, its stage (the module `linux::stage`: the free space
//! checked, then a download alone); on a Mac every step of `macos` ([`MacosKind`]): its
//! `prepare` stages a macOS update or defers, its `cutover` and restart install it with the update
//! account, its `verify` checks the target build, and every step of `xcode` ([`XcodeKind`]) but the
//! `after_reboot` it never asks for: its `prepare` stages the Xcodes of the order, its `cutover`
//! moves them into place, accepts their licences, runs their first launch, selects the default
//! and removes the old Xcodes the order names, and its `verify` checks each; every other step of
//! every kind is `unsupported_kind` until its own change. The steps run each kind of the run in [`Kind::ALL`]'s order, macOS before Xcode,
//! whatever order the parameters give:
//!
//! - `prepare` runs each kind's `prepare`, and ends at the first that does not succeed. It writes
//!   nothing to the run record and reboots nothing.
//! - `cutover` runs each kind's `cutover` once per run, by the run record ([`RunRecord`]): a kind
//!   recorded applied is passed over. A kind whose cutover says it needs a reboot
//!   ([`Cutover::NeedsReboot`]) is recorded with its reboot under way and the boot id it was asked
//!   in, and only then is the reboot asked for: by the kind's own restart when it has one
//!   ([`MaintenanceKind::restart`]: a Mac's update, whose install restarts the Mac, so that the
//!   record is durable before the install starts), else by the host's [`Rebooter`]; the step ends
//!   there, exit 0, reason `reboot_under_way`. The `cutover` run after the reboot finds the boot id changed, asks the
//!   kind for the version the host now runs ([`MaintenanceKind::after_reboot`]), records the kind
//!   applied and goes on with the next kind, never installing the first again. Under the same
//!   boot id, the reboot was asked for and has not happened: it is asked for again, and nothing is
//!   installed again but by a kind's own restart, which is its install. A `reboot` kind after a kind whose reboot the same `cutover` saw finished is
//!   recorded applied without a second reboot.
//! - `verify` checks each kind of the run in turn and ends at the first wrong fact: it fails
//!   `not_applied` for a kind the run record does not hold applied, `not_rebooted` for a kind
//!   applied after a reboot while the host still runs the boot that reboot was asked in, and
//!   otherwise runs the kind's `verify`. A kind not in the run is not checked.
//!
//! **The run record.** The record is the maintenance run record in the directory of the
//! `--params` file ([`StoreRecord`]), where the daemon puts the parameters. With the parameters on
//! standard input there is no such directory and the record is [`Unwired`]: a `cutover` there
//! would install and then fail to record it, so [`host_main`] refuses the step first, before any
//! kind runs, failing `run_record_failed` ([`unwired_record`]); `prepare` and `verify` run. The
//! daemon's sequencer ([`crate::maintenance_run`]) launches `cutover` with the parameters of one
//! kind at a time, and applies a kind whose reboot was under way itself once it reads a new boot
//! id: it launches no `cutover` after the reboot, except on the same boot, to ask for the reboot
//! again.
//!
//! **Outcome.** The exit code is the hook contract's: `0` done, `75` deferred (the kind touched
//! nothing), `1` failed. The last line of standard output is the contract's JSON report: the
//! reason a token, the detail at most [`MAX_DETAIL`] bytes.

mod kind;
pub mod linux;
pub mod macos;
mod params;
mod ports;
mod registry;
mod store;
pub mod xcode;

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, ValueEnum};
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_hook::{Class, Outcome, Report, is_reason_token};
use hostkeeper_launchd::{Platform, PlatformKind};
use hostkeeper_placed::{RealFs, Roots};
use hostkeeper_proto::maintenance::MAX_HALTED_REASON;
use hostkeeper_steps::{BootId, BootIdSource};

use crate::Placement;
use crate::adapters::Adapters;
use crate::cli;
use crate::maintenance_linux::kinds::{LinuxKind, RebootKind};
use crate::maintenance_linux::rebooter::HostRebooter;
use crate::maintenance_own::PlacedPackages;

pub use kind::{Cutover, KindError, MaintenanceKind, REASON_UNSUPPORTED_KIND, Unsupported};
pub use linux::LinuxCheck;
pub use macos::MacosKind;
pub use params::{Kind, MAX_PARAMS_BYTES, MaintainParams, ParamsError};
pub use ports::{KindState, RebootError, Rebooter, RecordError, RunRecord, Unwired};
pub use registry::Registry;
pub use store::StoreRecord;
pub(crate) use store::record_kind;
pub use xcode::XcodeKind;

/// The `--params` that names standard input.
pub const STDIN_PARAMS: &str = "-";
/// The most bytes of the report's detail: the bound of the reason a run record keeps
/// (`MaintenanceRunState.halted_reason`).
pub const MAX_DETAIL: usize = MAX_HALTED_REASON;

/// The reason of parameters that could not be read or are not a valid `Maintain`.
pub const REASON_PARAMS_INVALID: &str = "params_invalid";
/// The reason of a run record that could not be read or written.
pub const REASON_RUN_RECORD_FAILED: &str = "run_record_failed";
/// The reason of a boot id that could not be read.
pub const REASON_BOOT_ID_UNREADABLE: &str = "boot_id_unreadable";
/// The reason of a reboot that could not be asked for.
pub const REASON_REBOOT_FAILED: &str = "reboot_failed";
/// The reason of a `cutover` that ended by asking for a reboot (exit 0).
pub const REASON_REBOOT_UNDER_WAY: &str = "reboot_under_way";
/// The reason of a `verify` of a kind the run record does not hold applied.
pub const REASON_NOT_APPLIED: &str = "not_applied";
/// The reason of a `verify` of a kind applied after a reboot while the host still runs the boot
/// that reboot was asked in.
pub const REASON_NOT_REBOOTED: &str = "not_rebooted";
/// The reason given in place of a kind's reason that is not a token.
pub const REASON_KIND_FAILED: &str = "kind_failed";

/// A step of the maintenance hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum HookStep {
    /// Stage each kind while the host serves.
    Prepare,
    /// Apply each kind on the drained host, rebooting for a kind that needs it.
    Cutover,
    /// Check that each kind is applied.
    Verify,
}

/// The arguments of `hostkeeper maintenance-hook`.
#[derive(Clone, Debug, PartialEq, Eq, Args)]
pub struct HookArgs {
    /// The step to run.
    #[arg(value_enum)]
    pub step: HookStep,
    /// The file of the run's parameters (the `maintain` order's fields as JSON), `-` for
    /// standard input.
    #[arg(long)]
    pub params: PathBuf,
    /// The seconds a package manager's program waits at most for the host's package manager's
    /// lock, from 1 to 3600, as the daemon's flag of the same name: on top of the deadline of
    /// each program the Linux install runs.
    #[arg(
        long,
        default_value_t = cli::DEFAULT_PACKAGE_LOCK_WAIT_SECS,
        value_parser = clap::value_parser!(u64).range(1..=cli::MAX_PACKAGE_LOCK_WAIT_SECS),
    )]
    pub package_lock_wait_secs: u64,
}

/// What the hook works through: the kinds of the host, its run record, its reboot and its boot
/// id.
pub struct HookPorts<'a> {
    pub kinds: &'a Registry,
    pub record: &'a dyn RunRecord,
    pub rebooter: &'a dyn Rebooter,
    pub boot_id: &'a dyn BootIdSource,
}

/// How a step of the hook ended: its outcome, and the reason and detail of its report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ending {
    pub outcome: Outcome,
    pub reason: Option<&'static str>,
    pub detail: Option<String>,
}

impl Ending {
    /// Done, with nothing to say.
    fn done() -> Ending {
        Ending {
            outcome: Outcome::Success,
            reason: None,
            detail: None,
        }
    }

    /// Failed for `reason`.
    fn failed(reason: &'static str, detail: String) -> Ending {
        Ending {
            outcome: Outcome::Failure,
            reason: Some(reason),
            detail: Some(detail),
        }
    }

    /// The report line of this ending: the reason, a token or [`REASON_KIND_FAILED`]; the detail
    /// cut to [`MAX_DETAIL`] bytes; class `environment` for a deferral, which says the host's
    /// condition rather than the work.
    pub fn report(&self) -> Report {
        let class = match self.outcome {
            Outcome::Deferred => Class::Environment,
            _ => Class::Release,
        };
        Report {
            class,
            reason: self.reason.map(token),
            detail: self.detail.as_deref().map(bounded),
            ..Report::default()
        }
    }
}

impl From<KindError> for Ending {
    fn from(error: KindError) -> Ending {
        match error {
            KindError::Deferred { reason, detail } => Ending {
                outcome: Outcome::Deferred,
                reason: Some(reason),
                detail: Some(detail),
            },
            KindError::Failed { reason, detail } => Ending::failed(reason, detail),
        }
    }
}

/// `reason` when it is a token, [`REASON_KIND_FAILED`] otherwise.
fn token(reason: &str) -> String {
    match is_reason_token(reason) {
        true => reason.to_owned(),
        false => REASON_KIND_FAILED.to_owned(),
    }
}

/// `detail`, cut to at most [`MAX_DETAIL`] bytes at a character's boundary.
fn bounded(detail: &str) -> String {
    let mut end = detail.len().min(MAX_DETAIL);
    while !detail.is_char_boundary(end) {
        end -= 1;
    }
    detail[..end].to_owned()
}

/// The kinds of a host of `platform`, for a build of version `own` (`major.minor.patch`):
/// [`Registry::for_platform`]'s, with `linux` and `reboot` of a Linux host (systemd or OpenRC) put
/// in place of their stubs: staged, installed and rebooted by [`LinuxKind`] and [`RebootKind`],
/// the stage and the install each waiting up to `lock_wait_secs` for the package manager's lock and leaving untouched the
/// packages recorded in the daemon's `placed.json`, and checked by the host's [`LinuxCheck`]
/// ([`linux::kinds`]). When `own` is not a version (which the daemon itself refuses to start
/// with), or the build has no adapters for the platform, their `verify` stays `unsupported_kind`.
/// On a Mac, `macos` ([`MacosKind`]), whose `prepare` stages a macOS update and whose other steps
/// install and check it, keeping the install's plan in `run_dir` (the directory of the run's
/// parameters; `None` for parameters on standard input, where no `cutover` runs), and `xcode`
/// ([`XcodeKind`]), whose `prepare` stages the order's Xcodes and whose `cutover` and `verify`
/// install and check them; `reboot` keeps its stub.
pub fn registry_for(
    platform: &Platform,
    lock_wait_secs: u64,
    own: &str,
    run_dir: Option<&Path>,
) -> Registry {
    let registry = Registry::for_platform(platform.kind);
    match platform.kind {
        PlatformKind::Linux | PlatformKind::Openrc => {
            let placement = Placement {
                roots: Roots::new(
                    Path::new(hostkeeper_placed::roots::STATE_DIR),
                    &platform.unit_dir,
                    Path::new(cli::DEFAULT_PACKAGES_DIR),
                ),
                fs: Arc::new(RealFs::new(Path::new("/"))),
            };
            let packages = Box::new(PlacedPackages::new(&placement));
            let linux_work = Box::new(LinuxKind::at_host(lock_wait_secs, packages));
            let reboot_work = Box::new(RebootKind::at_host());
            match (
                HostkeeperVersion::parse(own),
                Adapters::for_platform(platform),
            ) {
                (Ok(own), Ok(adapters)) => linux::kinds(
                    LinuxCheck::at_host(adapters, own, lock_wait_secs),
                    linux_work,
                    reboot_work,
                ),
                _ => registry
                    .with(Kind::Linux, linux_work)
                    .with(Kind::Reboot, reboot_work),
            }
        }
        PlatformKind::Macos => registry
            .with(Kind::Macos, Box::new(MacosKind::at_host(run_dir)))
            .with(Kind::Xcode, Box::new(XcodeKind::at_host())),
    }
}

/// The ending of a `cutover`, which a hook with no run record ([`Unwired`]: its parameters on
/// standard input) refuses before any kind runs: failed `run_record_failed`, saying the host keeps
/// no run record. `None` for the other steps.
pub fn unwired_record(step: HookStep) -> Option<Ending> {
    (step == HookStep::Cutover).then(|| record_failed(RecordError::NotWired))
}

/// `hostkeeper maintenance-hook` on this host: the kinds of its platform ([`registry_for`]), the
/// boot id of the system the binary was built for, the platform's reboot ([`HostRebooter`]; none
/// on a Mac) and the run record beside the parameters file ([`StoreRecord::beside_params`]); with
/// the parameters on standard input there is no record ([`Unwired`]), so a `cutover` is refused
/// ([`unwired_record`]) before its parameters are read. The parameters come from the file `args`
/// names or from the process's standard input, the report goes to its standard output.
pub fn host_main(args: &HookArgs) -> Outcome {
    let store_record = StoreRecord::beside_params(&args.params);
    if let Some(ending) = unwired_record(args.step).filter(|_| store_record.is_none()) {
        let _unwritten = ending.report().write_to(&mut io::stdout().lock());
        return ending.outcome;
    }
    let platform = Platform::host();
    let run_dir = match args.params == Path::new(STDIN_PARAMS) {
        true => None,
        false => args.params.parent(),
    };
    let kinds = registry_for(
        &platform,
        args.package_lock_wait_secs,
        env!("CARGO_PKG_VERSION"),
        run_dir,
    );
    let boot_id = crate::adapters::host_boot_id();
    let host_rebooter = HostRebooter::for_platform(platform.kind);
    let rebooter = host_rebooter
        .as_ref()
        .map_or(&Unwired as &dyn Rebooter, |rebooter| {
            rebooter as &dyn Rebooter
        });
    let ports = HookPorts {
        kinds: &kinds,
        record: store_record
            .as_ref()
            .map_or(&Unwired as &dyn RunRecord, |r| r as &dyn RunRecord),
        rebooter,
        boot_id: boot_id.as_ref(),
    };
    main_with(
        args,
        &ports,
        &mut io::stdin().lock(),
        &mut io::stdout().lock(),
    )
}

/// Runs the step `args` names over `ports`, with the parameters of the file `args` names or of
/// `stdin`; writes the report line to `stdout` and returns the outcome. A report that cannot be
/// written leaves the outcome as it is: the exit code is what the daemon judges.
pub fn main_with(
    args: &HookArgs,
    ports: &HookPorts<'_>,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> Outcome {
    let ending = match read_params(&args.params, stdin) {
        Ok(params) => run_step(args.step, &params, ports),
        Err(error) => Ending::failed(REASON_PARAMS_INVALID, error.to_string()),
    };
    let _unwritten = ending.report().write_to(stdout);
    ending.outcome
}

/// The parameters of the file `path`, or of `stdin` when `path` is [`STDIN_PARAMS`].
fn read_params(path: &Path, stdin: &mut dyn Read) -> Result<MaintainParams, ParamsError> {
    if path == Path::new(STDIN_PARAMS) {
        return MaintainParams::read(stdin);
    }
    let mut file = File::open(path)
        .map_err(|error| ParamsError::Read(format!("{}: {error}", path.display())))?;
    MaintainParams::read(&mut file)
}

/// Runs `step` of the run `params` over `ports`.
pub fn run_step(step: HookStep, params: &MaintainParams, ports: &HookPorts<'_>) -> Ending {
    let ended = match step {
        HookStep::Prepare => prepare(params, ports.kinds),
        HookStep::Cutover => cutover(params, ports),
        HookStep::Verify => verify(params, ports),
    };
    match ended {
        Ok(ending) | Err(ending) => ending,
    }
}

/// Each kind's `prepare`, to the first that does not succeed.
fn prepare(params: &MaintainParams, kinds: &Registry) -> Result<Ending, Ending> {
    for kind in params.kinds() {
        kinds.get(kind)?.prepare(params)?;
    }
    Ok(Ending::done())
}

/// Each kind's `cutover`, once per run, with the reboot protocol of the module's header.
fn cutover(params: &MaintainParams, ports: &HookPorts<'_>) -> Result<Ending, Ending> {
    let run_id = params.run_id();
    let record = ports.record;
    // The boot a reboot of this run was seen finished in, by this `cutover`.
    let mut rebooted: Option<BootId> = None;
    for kind in params.kinds() {
        match record.state(run_id, kind).map_err(record_failed)? {
            KindState::Applied | KindState::AppliedAfterReboot { .. } => {}
            KindState::RebootUnderWay { boot } => {
                let now = boot_id(ports.boot_id)?;
                if now == boot {
                    return Err(reboot(kind, params, ports));
                }
                let version = ports.kinds.get(kind)?.after_reboot(params)?;
                record
                    .applied(run_id, kind, &version)
                    .map_err(record_failed)?;
                rebooted = Some(now);
            }
            KindState::Pending => match (kind, &rebooted) {
                (Kind::Reboot, Some(now)) => record
                    .applied(run_id, kind, now.as_str())
                    .map_err(record_failed)?,
                _ => match ports.kinds.get(kind)?.cutover(params)? {
                    Cutover::Applied { version } => record
                        .applied(run_id, kind, &version)
                        .map_err(record_failed)?,
                    Cutover::NeedsReboot => {
                        let boot = boot_id(ports.boot_id)?;
                        record
                            .reboot_under_way(run_id, kind, &boot)
                            .map_err(record_failed)?;
                        return Err(reboot(kind, params, ports));
                    }
                },
            },
        }
    }
    Ok(Ending::done())
}

/// Each kind's check, in the run's order, to the first wrong fact: the kind recorded applied,
/// the host booted again since the reboot it needed was asked for, and the kind's own `verify`.
fn verify(params: &MaintainParams, ports: &HookPorts<'_>) -> Result<Ending, Ending> {
    for kind in params.kinds() {
        let state = ports
            .record
            .state(params.run_id(), kind)
            .map_err(record_failed)?;
        match state {
            KindState::Applied => {}
            KindState::AppliedAfterReboot { asked_in } => {
                if boot_id(ports.boot_id)? == asked_in {
                    return Err(Ending::failed(
                        REASON_NOT_REBOOTED,
                        format!(
                            "{kind} needed a reboot, and the host still runs the boot {} it was \
                             asked in",
                            asked_in.as_str()
                        ),
                    ));
                }
            }
            KindState::Pending | KindState::RebootUnderWay { .. } => {
                return Err(Ending::failed(
                    REASON_NOT_APPLIED,
                    format!("{kind} is not recorded applied"),
                ));
            }
        }
        ports.kinds.get(kind)?.verify(params)?;
    }
    Ok(Ending::done())
}

/// Asks for the reboot `kind` needs, by the kind's own restart when it has one
/// ([`MaintenanceKind::restart`]), else (and for a kind this host holds no work for) by the
/// host's [`Rebooter`]: the step ends done,
/// `reboot_under_way`, or failed when the reboot could not be asked for (the kind's own failure,
/// or `reboot_failed`).
fn reboot(kind: Kind, params: &MaintainParams, ports: &HookPorts<'_>) -> Ending {
    let own = ports
        .kinds
        .get(kind)
        .ok()
        .and_then(|work| work.restart(params));
    let asked = match own {
        Some(restarted) => restarted.map_err(Ending::from),
        None => ports
            .rebooter
            .reboot()
            .map_err(|error| Ending::failed(REASON_REBOOT_FAILED, error.to_string())),
    };
    match asked {
        Ok(()) => Ending {
            outcome: Outcome::Success,
            reason: Some(REASON_REBOOT_UNDER_WAY),
            detail: Some(format!("{kind} needs a reboot: rebooting")),
        },
        Err(ending) => ending,
    }
}

/// This boot's id, or the failure that it could not be read.
fn boot_id(source: &dyn BootIdSource) -> Result<BootId, Ending> {
    source
        .read()
        .map_err(|error| Ending::failed(REASON_BOOT_ID_UNREADABLE, error.to_string()))
}

/// The failure of the run record.
fn record_failed(error: RecordError) -> Ending {
    Ending::failed(REASON_RUN_RECORD_FAILED, error.to_string())
}
