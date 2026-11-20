//! The launch of one step of a maintenance turn, and how its end reads as the run record's
//! [`Outcome`].
//!
//! - **The maintenance hook's steps** (stage, apply, check): `<this binary> maintenance-hook
//!   <prepare|cutover|verify> --params <file>` through the host's launcher as an additive step of
//!   the service `hostkeeper`, unit `updater-hostkeeper-<step>-<try>`, with no bound (an OS
//!   update is never cut). The params file is beside the run record: the run's parameters
//!   ([`super::PARAMS_FILE`]) for `prepare` and `verify`, and for `cutover` the same parameters
//!   naming the one kind applied ([`CUTOVER_PARAMS_FILE`]). Exit `0` is [`Outcome::Ok`], `75`
//!   [`Outcome::Deferred`], anything else [`Outcome::Failed`].
//! - **A service's `drain` or `resume`**: the step the manifest of the service's `current`
//!   release names in `[hook] drain` or `[hook] resume`, from a tree that still has the digest of
//!   its seal, through [`run_maintenance_step`], unit `updater-<service>-<drain|resume>-<try>`. Its
//!   params file (`<service>.params`, beside the record) holds the manifest's `[params]`, the
//!   service and the run's id, and for a `drain` with a deadline the deadline in unix seconds; the
//!   `drain` is bounded by exactly the run's `drain_deadline_secs` (a deadline of 0 by one
//!   second), and is unbounded without one. A service whose release names no such step is not
//!   touched: the step is [`Outcome::Ok`].
//!
//! A unit of the same name left by an earlier run is stopped before the launch. A launcher that
//! fails is [`Outcome::Failed`], its unit stopped. The reason of an outcome names the step's exit
//! and the hook's reason and detail, never a parameter.

use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use hostkeeper_config::manifest::Expected;
use hostkeeper_config::names::ServiceName;
use hostkeeper_hook::{
    DRAIN_DEADLINE_KEY, EXIT_DEFERRED, EXIT_OK, Params, RUN_ID_KEY, Report, SERVICE_KEY, Scalar,
    Step,
};
use hostkeeper_proto::FILE_DESCRIPTOR_SET;
use hostkeeper_proto::hostkeeper::agent::v1::Maintain;
use hostkeeper_steps::maintenance::{
    Kind, Launch, MaintenanceCall, MaintenanceOutcome, MaintenanceRecord, MaintenanceStep, Outcome,
    run_maintenance_step,
};
use hostkeeper_steps::{AdditiveCall, Bound, Layout, Seal, Staged, StepExit, read_seal};
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, SerializeOptions};

use super::params_path;
use crate::executor::Ctx;
use crate::files;
use crate::installed;
use crate::maintenance_hook::MaintainParams;
use crate::maintenance_state::wire_kind;
use crate::verbs::force::SELF_SERVICE;

/// The params file of the hook's `cutover`: the run's parameters naming the one kind applied.
pub const CUTOVER_PARAMS_FILE: &str = "maintain-cutover.json";
/// The subcommand of this binary that is the maintenance hook.
pub const HOOK_SUBCOMMAND: &str = "maintenance-hook";
/// The message the run's parameters are the JSON of.
const MAINTAIN_MESSAGE: &str = "hostkeeper.agent.v1.Maintain";

/// Launches `step` of the turn `record` (the record of its start written) and waits for its end.
/// `dir` is the run record's directory; `params` the run's parameters.
pub fn run(
    ctx: &Ctx,
    dir: &Path,
    record: &MaintenanceRecord,
    step: &Launch,
    params: &Result<MaintainParams, String>,
) -> Outcome {
    let ran = match step {
        Launch::Stage => hook_step(ctx, record, Step::Prepare, &params_path(dir)),
        Launch::Check => hook_step(ctx, record, Step::Verify, &params_path(dir)),
        Launch::Apply(kind) => cutover_params(dir, params, *kind)
            .and_then(|path| hook_step(ctx, record, Step::Cutover, &path)),
        Launch::Drain(service) => {
            service_step(ctx, dir, record, service, MaintenanceStep::Drain, params)
        }
        Launch::Resume(service) => {
            service_step(ctx, dir, record, service, MaintenanceStep::Resume, params)
        }
    };
    ran.unwrap_or_else(Outcome::Failed)
}

/// Writes the run's parameters (`maintain`, as proto3 JSON) to [`super::PARAMS_FILE`] in `dir`,
/// durably, mode 0600.
pub fn write_params(dir: &Path, maintain: &Maintain) -> Result<(), String> {
    let json = to_json(maintain)?;
    files::write_atomic(dir, super::PARAMS_FILE, &json, &|_| false).map_err(text)
}

/// The params file of the `cutover` of `kind`: the run's parameters naming that kind alone.
fn cutover_params(
    dir: &Path,
    params: &Result<MaintainParams, String>,
    kind: Kind,
) -> Result<PathBuf, String> {
    let mut maintain = params.as_ref().map_err(Clone::clone)?.maintain().clone();
    maintain.kinds = vec![i32::from(wire_kind(kind))];
    let json = to_json(&maintain)?;
    files::write_atomic(dir, CUTOVER_PARAMS_FILE, &json, &|_| false).map_err(text)?;
    Ok(dir.join(CUTOVER_PARAMS_FILE))
}

/// `maintain` as proto3 canonical JSON, the form the maintenance hook reads. An `Err` says the
/// compiled descriptor set has no `Maintain`.
fn to_json(maintain: &Maintain) -> Result<Vec<u8>, String> {
    DescriptorPool::decode(FILE_DESCRIPTOR_SET)
        .ok()
        .and_then(|pool| pool.get_message_by_name(MAINTAIN_MESSAGE))
        .and_then(|descriptor| {
            DynamicMessage::decode(descriptor, maintain.encode_to_vec().as_slice()).ok()
        })
        .and_then(|message| {
            let mut writer = serde_json::Serializer::new(Vec::new());
            message
                .serialize_with_options(&mut writer, &SerializeOptions::new())
                .ok()
                .map(|()| writer.into_inner())
        })
        .ok_or_else(|| format!("the compiled descriptor set has no {MAINTAIN_MESSAGE}"))
}

/// The unit of `step` of `service` in the turn of `record`.
fn unit(service: &str, step: Step, record: &MaintenanceRecord) -> String {
    format!("updater-{service}-{}-{}", step.name(), record.order_try)
}

/// Runs the maintenance hook's `step` with the params file `params`.
fn hook_step(
    ctx: &Ctx,
    record: &MaintenanceRecord,
    step: Step,
    params: &Path,
) -> Result<Outcome, String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("this binary's path cannot be read: {error}"))?;
    let unit = unit(SELF_SERVICE, step, record);
    ctx.launcher.stop(&unit).map_err(text)?;
    let hook_args = [HOOK_SUBCOMMAND.to_owned()];
    let ran = ctx.launcher.run_additive(&AdditiveCall {
        service: SELF_SERVICE,
        step: step.name(),
        hook_args: &hook_args,
        params,
        executable: &executable,
        unit: &unit,
        bound: Bound::Unbounded,
    });
    match ran {
        Ok(exit) => Ok(match exit.code {
            Some(EXIT_OK) => Outcome::Ok,
            Some(EXIT_DEFERRED) => Outcome::Deferred(ended(&exit)),
            _ => Outcome::Failed(ended(&exit)),
        }),
        Err(error) => Err(launch_failed(ctx, &unit, &error)),
    }
}

/// Runs `step` of `service`, as its `current` release's manifest names it.
fn service_step(
    ctx: &Ctx,
    dir: &Path,
    record: &MaintenanceRecord,
    service: &ServiceName,
    step: MaintenanceStep,
    params: &Result<MaintainParams, String>,
) -> Result<Outcome, String> {
    let Some(release) = Release::of(ctx, service)? else {
        return Ok(Outcome::Ok);
    };
    let word = match step {
        MaintenanceStep::Drain => release.manifest.hook.drain.clone(),
        MaintenanceStep::Resume => release.manifest.hook.resume.clone(),
    };
    let Some(word) = word else {
        return Ok(Outcome::Ok);
    };
    let deadline = match step {
        MaintenanceStep::Drain => {
            params
                .as_ref()
                .map_err(Clone::clone)?
                .maintain()
                .drain_deadline_secs
        }
        MaintenanceStep::Resume => None,
    };
    let mut hook_params =
        Params::parse_declared(&release.manifest.params.to_string()).map_err(text)?;
    hook_params.insert(SERVICE_KEY, Scalar::Text(service.to_string()));
    hook_params.insert(RUN_ID_KEY, Scalar::Text(record.run_id.clone()));
    let bound = match deadline {
        Some(secs) => {
            let at = ctx.maintenance.unix().unix_secs().saturating_add(secs);
            hook_params.insert(
                DRAIN_DEADLINE_KEY,
                Scalar::Integer(i64::try_from(at).unwrap_or(i64::MAX)),
            );
            Bound::Secs(NonZeroU64::new(secs).unwrap_or(NonZeroU64::MIN))
        }
        None => step.default_bound(),
    };
    let name = format!("{service}.params");
    files::write_atomic(dir, &name, hook_params.to_toml().as_bytes(), &|_| false).map_err(text)?;
    let unit = unit(service.as_str(), step.step(), record);
    ctx.launcher.stop(&unit).map_err(text)?;
    let command = &release.manifest.hook.command;
    let hook = command.first().map(PathBuf::from).unwrap_or_default();
    let hook_args = command.get(1..).unwrap_or_default();
    let executable = release.staged.check(&hook).map_err(text)?;
    let ran = run_maintenance_step(
        &*ctx.launcher,
        &*ctx.clock,
        &MaintenanceCall {
            service: service.as_str(),
            step,
            word: &word,
            hook_args,
            params: &dir.join(&name),
            executable: &executable,
            unit: &unit,
            bound,
        },
    );
    match ran {
        Ok(exit) => Ok(match exit.outcome {
            MaintenanceOutcome::Succeeded => Outcome::Ok,
            MaintenanceOutcome::Deferred if exit.deadline_passed => {
                Outcome::Deferred(format!("the drain deadline passed: {}", ended(&exit.exit)))
            }
            MaintenanceOutcome::Deferred => Outcome::Deferred(ended(&exit.exit)),
            MaintenanceOutcome::Failed => Outcome::Failed(ended(&exit.exit)),
        }),
        Err(error) => Err(launch_failed(ctx, &unit, &error)),
    }
}

/// The installed release of a service: its manifest and its sealed tree.
struct Release {
    manifest: hostkeeper_config::manifest::Manifest,
    staged: Staged,
}

impl Release {
    /// The release `current` names for `service`, checked against its seal; `None` when the
    /// service has no `current`.
    fn of(ctx: &Ctx, service: &ServiceName) -> Result<Option<Release>, String> {
        let package_dir = ctx.dirs.packages.join(service.as_str());
        let Some(version) = crate::boot::current_version(&package_dir)? else {
            return Ok(None);
        };
        let layout = Layout::package(&package_dir);
        let dir = layout.version_path(version.as_str());
        let expected = Expected {
            name: service.clone(),
            version: version.clone(),
            platform: hostkeeper_config::platform::Platform::current(),
        };
        let manifest = crate::target::manifest(&dir, &expected, &ctx.own_version).map_err(text)?;
        let seal = read_seal(&layout.seal_path(version.as_str())).map_err(text)?;
        let seal = Seal::parse(&seal).map_err(text)?;
        let staged = Staged::seal(&dir).map_err(text)?;
        if staged.digest() != seal.tree_digest() {
            return Err(format!("{} is not the tree its seal names", dir.display()));
        }
        Ok(Some(Release { manifest, staged }))
    }
}

/// The services a new turn drains and puts back: each installed service (one with a `current`)
/// whose release names a `drain` and a `resume`, in the order of their names. A service whose
/// release cannot be read is refused rather than passed over, so that no service is left serving
/// while the host it runs on is updated.
pub fn planned_services(ctx: &Ctx) -> Result<Vec<ServiceName>, String> {
    let mut planned = Vec::new();
    for service in installed::service_dirs(&ctx.dirs.packages)
        .map_err(text)?
        .into_values()
    {
        let release = Release::of(ctx, &service).map_err(|why| format!("{service}: {why}"))?;
        let drains = release.is_some_and(|release| {
            release.manifest.hook.drain.is_some() && release.manifest.hook.resume.is_some()
        });
        if drains {
            planned.push(service);
        }
    }
    Ok(planned)
}

/// The words of a step's end: its exit code, and the hook's reason and detail when its last line
/// is a report.
fn ended(exit: &StepExit) -> String {
    let code = match exit.code {
        Some(code) => format!("exit {code}"),
        None => "ended with no exit code".to_owned(),
    };
    let report = exit
        .last_line
        .as_deref()
        .and_then(|line| Report::parse_line(line).ok());
    match report {
        Some(Report {
            reason: Some(reason),
            detail: Some(detail),
            ..
        }) => format!("{code}, {reason}: {detail}"),
        Some(Report {
            reason: Some(reason),
            ..
        }) => format!("{code}, {reason}"),
        _ => code,
    }
}

/// A launcher that failed: its unit stopped, and the words of the failure.
fn launch_failed(ctx: &Ctx, unit: &str, error: &hostkeeper_steps::LaunchError) -> String {
    match ctx.launcher.stop(unit) {
        Ok(()) => format!("the launcher failed: {error}"),
        Err(stop) => {
            format!("the launcher failed: {error}; its unit {unit} was not stopped: {stop}")
        }
    }
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}
