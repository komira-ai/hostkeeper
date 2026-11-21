//! `docs/operations/maintenance.md`, the runbook of the Linux maintenance update, against the
//! command table `hk` is generated from and the protos' descriptors.
//!
//! - Every `hk maintenance <verb>` the runbook names, on a fenced command line or in an inline
//!   code span, is a method of the command table, and every method of the table's `maintenance`
//!   service is called by a fenced command line: a renamed, added or misspelled verb fails here.
//! - The first column of each of its tables is exactly the names the code defines, each once: the
//!   group's settings (`## The group's settings`) the fields of `MaintenancePolicy`, a run's states
//!   and a host's steps the values of `MaintenanceState` and `MaintenanceHostStep`, the refusals
//!   (`## Refusals`) the values of `MaintenanceRefusal`, and the alerts (`## Alerts`) the three
//!   alert kinds of a maintenance run, each a value of `AlertKind`.
//! - A token written after "refused" is a refusal of `Maintenance` or of `HostGroups`.
//!
//! The checks are pure functions of the text, tested on edits of the real runbook.

use std::collections::BTreeSet;

use hostkeeper_proto::ctl::COMMANDS;
use prost_reflect::DescriptorPool;

use super::{command_problems_for, inline_spans, mention_problems, repo_root, tokens_of};

/// The runbook, from the repository root.
const RUNBOOK: &str = "docs/operations/maintenance.md";

/// The table of the group's settings.
const SETTINGS: &str = "## The group's settings";

/// The table of a run's states.
const STATES: &str = "### A run's states";

/// The table of a host's steps.
const STEPS: &str = "### A host's steps";

/// The table of the refusals of `Maintenance`.
const REFUSALS: &str = "## Refusals";

/// The table of the alerts a run raises.
const ALERTS: &str = "## Alerts";

/// The alert kinds a maintenance run raises, which the alerts table holds.
const RUN_ALERTS: [&str; 3] = ["updates_due", "maintenance_halted", "maintenance_not_back"];

/// The verbs of the command table's `maintenance` service, as `maintenance <verb>`.
fn verbs() -> Vec<String> {
    COMMANDS
        .iter()
        .filter(|c| c.service_command == "maintenance")
        .map(|c| format!("maintenance {}", c.method_command))
        .collect()
}

/// The field names of the message `name` of the protos.
fn fields_of(name: &str) -> BTreeSet<String> {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decode");
    let message = pool.get_message_by_name(name).expect("the message");
    message.fields().map(|f| f.name().to_owned()).collect()
}

/// The first-column tokens of the table under `heading`, up to the next heading, or why there
/// are none.
fn tokens_under(doc: &str, heading: &str) -> Result<Vec<String>, String> {
    let mut lines = doc.lines().skip_while(|l| l.trim_end() != heading);
    if lines.next().is_none() {
        return Err(format!("no section `{heading}`"));
    }
    let tokens: Vec<String> = lines
        .take_while(|l| !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("| `"))
        .filter_map(|l| l.split_once('`').map(|(token, _)| token.to_owned()))
        .collect();
    if tokens.is_empty() {
        return Err(format!("the section `{heading}` has no table"));
    }
    Ok(tokens)
}

/// What is wrong with the table under `heading`, against the names `code` defines, each a
/// `what` of the code.
fn names_problems(doc: &str, heading: &str, what: &str, code: &BTreeSet<String>) -> Vec<String> {
    let tokens = match tokens_under(doc, heading) {
        Ok(tokens) => tokens,
        Err(e) => return vec![e],
    };
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for token in &tokens {
        if !code.contains(token) {
            problems.push(format!(
                "`{token}` under `{heading}` is no {what} of the code"
            ));
        }
        if !seen.insert(token.clone()) {
            problems.push(format!("`{token}` is listed twice under `{heading}`"));
        }
    }
    for token in code.difference(&seen) {
        problems.push(format!("the code's `{token}` is missing under `{heading}`"));
    }
    problems
}

/// Every inline `hk maintenance <verb>` span whose verb the command table does not have.
fn span_verb_problems(doc: &str) -> Vec<String> {
    let verbs = verbs();
    inline_spans(doc)
        .iter()
        .filter_map(|span| {
            let mut words = span.split_whitespace();
            match (words.next(), words.next(), words.next()) {
                (Some("hk"), Some("maintenance"), Some(verb)) if !verb.starts_with('-') => {
                    let named = format!("maintenance {verb}");
                    (!verbs.contains(&named))
                        .then(|| format!("`{span}`: the command table has no `hk {named}`"))
                }
                _ => None,
            }
        })
        .collect()
}

/// Every problem of the runbook.
fn problems(doc: &str) -> Vec<String> {
    let verbs = verbs();
    let required: Vec<&str> = verbs.iter().map(String::as_str).collect();
    let mut problems = command_problems_for(doc, &required);
    problems.extend(span_verb_problems(doc));
    problems.extend(names_problems(
        doc,
        SETTINGS,
        "setting",
        &fields_of("hostkeeper.operator.v1.MaintenancePolicy"),
    ));
    problems.extend(names_problems(
        doc,
        STATES,
        "run state",
        &tokens_of(
            "hostkeeper.operator.v1.MaintenanceState",
            "MAINTENANCE_STATE_",
        ),
    ));
    problems.extend(names_problems(
        doc,
        STEPS,
        "host step",
        &tokens_of(
            "hostkeeper.operator.v1.MaintenanceHostStep",
            "MAINTENANCE_HOST_STEP_",
        ),
    ));
    let refusals = tokens_of(
        "hostkeeper.operator.v1.MaintenanceRefusal",
        "MAINTENANCE_REFUSAL_",
    );
    problems.extend(names_problems(doc, REFUSALS, "refusal", &refusals));
    let kinds = tokens_of("hostkeeper.operator.v1.AlertKind", "ALERT_KIND_");
    let alerts: BTreeSet<String> = RUN_ALERTS
        .iter()
        .filter(|k| kinds.contains(**k))
        .map(|k| (*k).to_owned())
        .collect();
    problems.extend(names_problems(doc, ALERTS, "alert of a run", &alerts));
    let groups = tokens_of(
        "hostkeeper.operator.v1.HostGroupsRefusal",
        "HOST_GROUPS_REFUSAL_",
    );
    problems.extend(mention_problems(
        doc,
        &refusals.union(&groups).cloned().collect(),
    ));
    problems
}

/// The runbook as it is in the repository.
fn real() -> String {
    std::fs::read_to_string(repo_root().join(RUNBOOK)).unwrap_or_else(|e| panic!("{RUNBOOK}: {e}"))
}

/// Catches a runbook that names an `hk maintenance` verb the command table lacks, leaves a verb
/// of the table uncalled, or lists a setting `MaintenancePolicy` does not have (or leaves one of
/// its fields out); and a run state, host step, refusal or alert it lists that the protos do not
/// define, or one they define that it leaves out.
#[test]
fn the_maintenance_runbook_matches_the_code() {
    assert_eq!(problems(&real()), Vec::<String>::new());
    assert_eq!(verbs().len(), 6, "{:?}", verbs());
}

/// The check of [`the_maintenance_runbook_matches_the_code`] refuses each defect, with its
/// reason.
#[test]
fn the_maintenance_runbook_check_refuses_each_defect() {
    let doc = real();
    let edit = |from: &str, to: &str| {
        assert!(doc.contains(from), "{from}");
        problems(&doc.replacen(from, to, 1))
    };
    // A verb the command table does not have, on a command line: the line fails to parse, and
    // the verb it stood for is called by no line.
    let line = "    maintenance put-back --run-id run-1796529600-1 --host-id build-03 \\";
    let found = edit(line, &line.replace("put-back", "putback"));
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found[0].ends_with(
            "the service maintenance has no method putback; 'hk maintenance --help' lists them"
        ),
        "{found:?}"
    );
    assert_eq!(found[1], "no command line calls `hk maintenance put-back`");
    // The same in prose.
    assert_eq!(
        edit("`hk maintenance get`", "`hk maintenance show`"),
        ["`hk maintenance show`: the command table has no `hk maintenance show`"]
    );
    // A setting that is no field of MaintenancePolicy, and the field it replaced.
    assert_eq!(
        edit("| `min_serving` | 1 |", "| `min_up_hosts` | 1 |"),
        [
            "`min_up_hosts` under `## The group's settings` is no setting of the code",
            "the code's `min_serving` is missing under `## The group's settings`",
        ]
    );
    // A setting listed twice.
    assert_eq!(
        edit("| `batch` | 1 |", "| `batch` | 1 |\n| `batch` | 2 |"),
        ["`batch` is listed twice under `## The group's settings`"]
    );
    // A host step dropped, a run state renamed.
    assert_eq!(
        edit("| `not_back` | it was", "| it was"),
        ["the code's `not_back` is missing under `### A host's steps`"]
    );
    assert_eq!(
        edit(
            "| `window_ended` | its window",
            "| `window_over` | its window"
        ),
        [
            "`window_over` under `### A run's states` is no run state of the code",
            "the code's `window_ended` is missing under `### A run's states`",
        ]
    );
    // A refusal the protos do not define, and an alert of no run.
    assert_eq!(
        edit("| `not_in_run` |", "| `no_such_host` |"),
        [
            "`no_such_host` under `## Refusals` is no refusal of the code",
            "the code's `not_in_run` is missing under `## Refusals`",
        ]
    );
    assert_eq!(
        edit(
            "| `maintenance_not_back` | critical |",
            "| `silent_host` | warning |"
        ),
        [
            "`silent_host` under `## Alerts` is no alert of a run of the code",
            "the code's `maintenance_not_back` is missing under `## Alerts`",
        ]
    );
    // A token written as a refusal that is none.
    assert_eq!(
        edit("refused `window_ended`", "refused `window_closed`"),
        ["`window_closed` is written as a refusal and is none"]
    );
    // A table whose section is gone.
    assert_eq!(
        edit("## Refusals\n", "## Refused\n"),
        ["no section `## Refusals`"]
    );
}
