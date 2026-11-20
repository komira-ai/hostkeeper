//! `hostkeeper maintenance-hook <prepare|cutover|verify> --params <file>`: the maintenance hook,
//! the OS work of a maintenance run (docs/design/maintenance-updates.md, section 5.2), run the way
//! the step runner runs a hook's step.
//!
//! **Parameters.** The fields of the run's `maintain` order (`hostkeeper.agent.v1.Maintain`) as
//! proto3 canonical JSON, in the file `--params` names, or on standard input when it names `-`
//! ([`MaintainParams`]). No argument carries a parameter, and nothing the hook writes holds one.
//!
//! **Kinds.** Each kind of the run (`macos`, `xcode`, `linux`, `reboot`) is the work of a
//! [`MaintenanceKind`], found in the host platform's [`Registry`]; a kind the platform has no work
//! for, or whose work is not written yet, fails `unsupported_kind`. Written so far: on a Mac, the
//! `macos` kind's `prepare` ([`MacosKind`]), which stages a macOS update or defers. The steps run
//! each kind of the run in [`Kind::ALL`]'s order, macOS before Xcode, whatever order the
//! parameters give:
//!
//! - `prepare` runs each kind's `prepare`, and ends at the first that does not succeed. It writes
//!   nothing to the run record and reboots nothing.
//! - `cutover` runs each kind's `cutover` once per run, by the run record ([`RunRecord`]): a kind
//!   recorded applied is passed over. A kind whose cutover says it needs a reboot
//!   ([`Cutover::NeedsReboot`]) is recorded with its reboot under way and the boot id it was asked
//!   in, and only then is the reboot asked for ([`Rebooter`]); the step ends there, exit 0, reason
//!   `reboot_under_way`. The `cutover` run after the reboot finds the boot id changed, asks the
//!   kind for the version the host now runs ([`MaintenanceKind::after_reboot`]), records the kind
//!   applied and goes on with the next kind, never installing the first again. Under the same
//!   boot id, the reboot was asked for and has not happened: it is asked for again, and nothing is
//!   installed again. A `reboot` kind after a kind whose reboot the same `cutover` saw finished is
//!   recorded applied without a second reboot.
//! - `verify` fails `not_applied` for a kind the run record does not hold applied, and otherwise
//!   runs each kind's `verify`.
//!
//! **Outcome.** The exit code is the hook contract's: `0` done, `75` deferred (the kind touched
//! nothing), `1` failed. The last line of standard output is the contract's JSON report: the
//! reason a token, the detail at most [`MAX_DETAIL`] bytes.

mod kind;
mod macos;
mod params;
mod ports;
mod registry;

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use hostkeeper_hook::{Class, Outcome, Report, is_reason_token};
use hostkeeper_launchd::Platform;
use hostkeeper_proto::maintenance::MAX_HALTED_REASON;
use hostkeeper_steps::{BootId, BootIdSource};

pub use kind::{Cutover, KindError, MaintenanceKind, REASON_UNSUPPORTED_KIND, Unsupported};
pub use macos::MacosKind;
pub use params::{Kind, MAX_PARAMS_BYTES, MaintainParams, ParamsError};
pub use ports::{KindState, RebootError, Rebooter, RecordError, RunRecord, Unwired};
pub use registry::Registry;

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

/// `hostkeeper maintenance-hook` on this host: the kinds of its platform with the work written so
/// far ([`Registry::at_host`]), the boot id of the system the binary was built for, and no run
/// record or reboot yet ([`Unwired`]); the parameters from the file `args` names or from the
/// process's standard input, the report to its standard output.
pub fn host_main(args: &HookArgs) -> Outcome {
    let kinds = Registry::at_host(Platform::host().kind);
    let boot_id = crate::adapters::host_boot_id();
    let ports = HookPorts {
        kinds: &kinds,
        record: &Unwired,
        rebooter: &Unwired,
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
            KindState::Applied => {}
            KindState::RebootUnderWay { boot } => {
                let now = boot_id(ports.boot_id)?;
                if now == boot {
                    return Err(reboot(kind, ports.rebooter));
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
                        return Err(reboot(kind, ports.rebooter));
                    }
                },
            },
        }
    }
    Ok(Ending::done())
}

/// Each kind's `verify`, once the run record holds every kind applied.
fn verify(params: &MaintainParams, ports: &HookPorts<'_>) -> Result<Ending, Ending> {
    for kind in params.kinds() {
        let state = ports
            .record
            .state(params.run_id(), kind)
            .map_err(record_failed)?;
        if state != KindState::Applied {
            return Err(Ending::failed(
                REASON_NOT_APPLIED,
                format!("{kind} is not recorded applied"),
            ));
        }
        ports.kinds.get(kind)?.verify(params)?;
    }
    Ok(Ending::done())
}

/// Asks for the reboot `kind` needs: the step ends done, `reboot_under_way`, or failed when the
/// reboot could not be asked for.
fn reboot(kind: Kind, rebooter: &dyn Rebooter) -> Ending {
    match rebooter.reboot() {
        Ok(()) => Ending {
            outcome: Outcome::Success,
            reason: Some(REASON_REBOOT_UNDER_WAY),
            detail: Some(format!("{kind} needs a reboot: rebooting")),
        },
        Err(error) => Ending::failed(REASON_REBOOT_FAILED, error.to_string()),
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
