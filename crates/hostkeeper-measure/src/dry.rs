//! The dry run: what [`crate::runner::run`] would do, printed, with nothing run.

use std::io::Write;

use crate::fill::{check_files, describe, render, render_json, render_until};
use crate::params::Params;
use crate::plan::{Action, OnTimeout, Pick, Plan};
use crate::runner::RunError;

/// An argument as a shell would need it: quoted when it holds a space or a quote.
fn word(arg: &str) -> String {
    if arg.is_empty() || arg.contains(|c: char| c.is_whitespace() || "'\"\\$`".contains(c)) {
        format!("'{}'", arg.replace('\'', "'\\''"))
    } else {
        arg.to_owned()
    }
}

/// Prints what [`crate::runner::run`] would do with the same plan and parameters, and runs
/// nothing: no `hkctl`, no file written, no line read from the operator. It reads the files the
/// parameters name, to check them as the run does.
pub fn dry_run(plan: &Plan, params: &Params, out: &mut dyn Write) -> Result<(), RunError> {
    params.check(plan)?;
    check_files(plan, params)?;
    let mut text = format!(
        "plan {}: {}\nhosts: {}\nrepetitions: {}\nevery call is `hkctl --endpoint <endpoint> --ca-file <ca file> --token-file <token file> <command>`\n",
        plan.id, plan.title, plan.hosts.says, plan.repetitions
    );
    for k in 1..=plan.repetitions {
        let hosts = match plan.hosts.pick {
            Pick::Service => format!(
                "hkctl hosts list (the hosts of {})",
                plan.hosts.service.as_deref().unwrap_or_default()
            ),
            Pick::Live => "hkctl hosts list (the live hosts)".to_owned(),
            Pick::None => "no hosts: the steps find them".to_owned(),
        };
        let audit = if plan.reads_audit() {
            ", then hkctl audit list --newest true --limit 1"
        } else {
            ""
        };
        text.push_str(&format!("repetition {k}:\n  start: {hosts}{audit}\n"));
        for step in &plan.steps {
            let line = match &step.action {
                Action::Call {
                    args,
                    request,
                    audit,
                } => {
                    let args = args
                        .iter()
                        .map(|a| Ok(word(&render(params, a, k, None, true)?)))
                        .collect::<Result<Vec<_>, RunError>>()?;
                    let request = match request {
                        Some(r) => format!(
                            " --request <file: {}>",
                            render_json(params, r, k, None, true)?
                        ),
                        None => String::new(),
                    };
                    let audit = audit
                        .as_ref()
                        .map(|a| format!("; stamped by its audit entry {a}"))
                        .unwrap_or_default();
                    format!("call `hkctl {}{request}`{audit}", args.join(" "))
                }
                Action::Operator { says } => {
                    format!("ask the operator: {}", render(params, says, k, None, true)?)
                }
                Action::Watch(w) => format!(
                    "watch every {} s, at most {} s ({}): {}",
                    w.poll_secs,
                    w.timeout_secs,
                    match w.on_timeout {
                        OnTimeout::EndRepetition => "a timeout ends the repetition",
                        OnTimeout::Continue => "a timeout goes on",
                    },
                    describe(&render_until(params, &w.until, k, None, true)?)
                ),
            };
            text.push_str(&format!("  {}: {line}\n", step.id));
        }
    }
    text.push_str("dry run: nothing was called or written and the operator was asked nothing; the parameter files were read to check them\n");
    out.write_all(text.as_bytes()).map_err(RunError::Console)
}
