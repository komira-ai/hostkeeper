//! A step's slots filled from the parameters of a repetition, for a run and for a dry run.

use std::path::PathBuf;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::params::Params;
use crate::plan::{Plan, Until};
use crate::runner::RunError;
use crate::template::{self, Slot};

/// Every file a `{file:..}` or `{sha256:..}` slot reads, read once before anything runs.
pub fn check_files(plan: &Plan, params: &Params) -> Result<(), RunError> {
    for step in &plan.steps {
        for text in crate::plan::step_texts(&step.action) {
            for slot in template::slots(text)? {
                if let Some(Slot::File(p) | Slot::Sha256(p)) = Slot::of(&slot) {
                    for k in 1..=plan.repetitions {
                        read_param_file(params, &p, k)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn read_param_file(params: &Params, param: &str, rep: u32) -> Result<String, RunError> {
    let path = PathBuf::from(params.get(param, rep).unwrap_or_default());
    std::fs::read_to_string(&path).map_err(|source| RunError::File {
        param: param.to_owned(),
        path,
        source,
    })
}

/// The value of a step's slot. `dry` prints the host and a file's text by name instead.
pub fn fill(
    params: &Params,
    slot: &str,
    rep: u32,
    host: Option<&str>,
    dry: bool,
) -> Result<String, RunError> {
    Ok(match Slot::of(slot) {
        Some(Slot::Host) if dry => "<host>".to_owned(),
        Some(Slot::Host) => host.unwrap_or_default().to_owned(),
        Some(Slot::Rep) => rep.to_string(),
        Some(Slot::File(p)) if dry => {
            format!("<the text of {}>", params.get(&p, rep).unwrap_or_default())
        }
        Some(Slot::File(p)) => read_param_file(params, &p, rep)?,
        Some(Slot::Sha256(p)) => hex::encode(Sha256::digest(read_param_file(params, &p, rep)?)),
        Some(Slot::Param(p)) => params.get(&p, rep).unwrap_or_default().to_owned(),
        // The plan's check refuses any other slot of a step; a caller of this function may not.
        None => return Err(RunError::Slot(slot.to_owned())),
    })
}

pub fn render(
    params: &Params,
    text: &str,
    rep: u32,
    host: Option<&str>,
    dry: bool,
) -> Result<String, RunError> {
    template::render(text, &mut |slot| fill(params, slot, rep, host, dry))
}

pub fn render_json(
    params: &Params,
    value: &Value,
    rep: u32,
    host: Option<&str>,
    dry: bool,
) -> Result<Value, RunError> {
    Ok(match value {
        Value::String(s) => Value::String(render(params, s, rep, host, dry)?),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| render_json(params, v, rep, host, dry))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), render_json(params, v, rep, host, dry)?)))
                .collect::<Result<_, RunError>>()?,
        ),
        other => other.clone(),
    })
}

/// A step's watch condition with its slots filled.
pub fn render_until(
    params: &Params,
    until: &Until,
    rep: u32,
    host: Option<&str>,
    dry: bool,
) -> Result<Until, RunError> {
    Ok(match until {
        Until::Updated {
            service,
            version,
            healthy,
        } => Until::Updated {
            service: render(params, service, rep, host, dry)?,
            version: render(params, version, rep, host, dry)?,
            healthy: *healthy,
        },
        Until::Health { service, health } => Until::Health {
            service: render(params, service, rep, host, dry)?,
            health: *health,
        },
        other => other.clone(),
    })
}

/// The condition in words, for the console.
pub fn describe(until: &Until) -> String {
    match until {
        Until::Updated {
            service,
            version,
            healthy,
        } => format!(
            "every host lists {service} {version} installed by a succeeded update{}",
            if *healthy { ", healthy" } else { "" }
        ),
        Until::Health { service, health } => {
            format!("every host reports {service} {}", health.gateway())
        }
        Until::Live => "every host is live".to_owned(),
        Until::Back => {
            "the coordinator goes away (a failed call or a host not live) and every host is live again with a newer heartbeat".to_owned()
        }
        Until::Audit { actions, by_name } => format!(
            "an audit entry {}{}",
            actions.join(" or "),
            by_name.as_ref().map(|b| format!(" by {b}")).unwrap_or_default()
        ),
        Until::Beating => "the new host has a first heartbeat and a first serving stamp".to_owned(),
        Until::MdmEnrolled => "a Mac enrolls with the MDM gate".to_owned(),
    }
}
