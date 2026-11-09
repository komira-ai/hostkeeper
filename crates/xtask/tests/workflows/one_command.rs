//! The rule of `release.yml`'s steps: every `run:` is one command on one line. A step with
//! several commands (a `run: |` block, `&&` or `;`) hides what each does from the log and from
//! this lint, and a secret handled in shell (`printf "$KEY" > file`) is code no test runs. So the
//! commands are split into steps of their own (with `working-directory:` for a directory), and
//! what is more than a command moves into a verb of `cargo xtask`, which has tests.
//! `one_command_tests.rs` shows each defect refused.

use super::downloads::collect_runs;
use super::xtask_calls::commands;
use super::yaml::Node;

/// The workflow the rule holds for.
pub const FILE: &str = "release.yml";

/// One line per `run:` of the workflow `doc` that is not one command on one line.
pub fn problems(doc: &Node) -> Vec<String> {
    let mut runs = Vec::new();
    collect_runs(doc, &mut runs);
    runs.into_iter()
        .filter_map(|(line, run)| {
            let script = run.trim_end();
            if script.contains('\n') {
                return Some(format!(
                    "line {line}: `run:` has more than one line: a step is one command (split it into steps, or move the logic into a `cargo xtask` verb)"
                ));
            }
            (commands(script).len() > 1).then(|| {
                format!(
                    "line {line}: `run:` runs more than one command: a step is one command (split it into steps, or move the logic into a `cargo xtask` verb)"
                )
            })
        })
        .collect()
}
