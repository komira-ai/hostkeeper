//! `docs/operations/maintenance.md`, the runbook of the maintenance update of Linux hosts and
//! Macs, against the command table `hk` is generated from, the protos' descriptors, the daemon's
//! detector of device-management settings and the bootstrap's keys.
//!
//! - Every `hk maintenance <verb>` the runbook names, on a fenced command line or in an inline
//!   code span, is a method of the command table, and every method of the table's `maintenance`
//!   service is called by a fenced command line: a renamed, added or misspelled verb fails here.
//!   So are `hk xcode-push` and every method of the table's `xcode` service (`list`, `remove`).
//! - The first column of each of its tables is exactly the names the code defines, each once: the
//!   group's settings (`## The group's settings`) the fields of `MaintenancePolicy`, a run's states
//!   and a host's steps the values of `MaintenanceState` and `MaintenanceHostStep`, the refusals
//!   (`## Refusals`) the values of `MaintenanceRefusal`, the alerts (`## Alerts`) the three
//!   alert kinds of a maintenance run and the two of the Xcode store, each a value of `AlertKind`,
//!   and why a host is left out (`### Why a host is left out`) the values of `MaintenanceSkip`.
//! - The table of what device management must not set (`### What device management must not
//!   set`) is exactly the names the daemon's detector reports, in its order
//!   (`hostkeeper_daemon::maintenance_mdm::reported`), and the table of the update account's keys
//!   (`### The update account's keys`) exactly the bootstrap's keys, in their order
//!   (`hostkeeper_bootstrap::update_account::KEYS`).
//! - A token written after "refused" is a refusal of `Maintenance` or of `HostGroups`, and one
//!   written after "alert" a value of `AlertKind`.
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

/// The table of why a host is left out of a run.
const SKIPS: &str = "### Why a host is left out";

/// The table of what device management must not set on a Mac.
const MDM: &str = "### What device management must not set";

/// The table of the update account's keys.
const ACCOUNT_KEYS: &str = "### The update account's keys";

/// The alert kinds the alerts table holds: the three a maintenance run raises and the two of the
/// Xcode store.
const RUNBOOK_ALERTS: [&str; 5] = [
    "updates_due",
    "maintenance_halted",
    "maintenance_not_back",
    "xcode_release_waiting",
    "xcode_version_taken",
];

/// The calls the runbook must make that are not `hk maintenance`: the push and the store's.
const XCODE_CALLS: [&str; 3] = ["xcode-push", "xcode list", "xcode remove"];

/// The verbs of the command table's `maintenance` service, as `maintenance <verb>`.
fn verbs() -> Vec<String> {
    COMMANDS
        .iter()
        .filter(|c| c.service_command == "maintenance")
        .map(|c| format!("maintenance {}", c.method_command))
        .collect()
}

/// The methods of the command table's `xcode` service, as `xcode <method>`.
fn xcode_methods() -> Vec<String> {
    COMMANDS
        .iter()
        .filter(|c| c.service_command == "xcode")
        .map(|c| format!("xcode {}", c.method_command))
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

/// What is wrong with the order of the table under `heading` whose tokens are the names `code`
/// lists, each once: nothing when the table holds other names (which [`names_problems`] says),
/// else the first row out of `code`'s order.
fn order_problems(doc: &str, heading: &str, code: &[&str]) -> Vec<String> {
    let Ok(tokens) = tokens_under(doc, heading) else {
        return Vec::new();
    };
    if tokens.len() != code.len() {
        return Vec::new();
    }
    tokens
        .iter()
        .zip(code)
        .find(|(token, name)| token.as_str() != **name)
        .map(|(token, name)| {
            vec![format!(
                "`{token}` under `{heading}` stands where the code lists `{name}`: keep the code's order"
            )]
        })
        .unwrap_or_default()
}

/// The table under `heading` against `code`, the names in the order the code lists them: each
/// name once, none missing, none extra, in that order.
fn listed_problems(doc: &str, heading: &str, what: &str, code: &[&str]) -> Vec<String> {
    let names: BTreeSet<String> = code.iter().map(|name| (*name).to_owned()).collect();
    let mut problems = names_problems(doc, heading, what, &names);
    if problems.is_empty() {
        problems.extend(order_problems(doc, heading, code));
    }
    problems
}

/// Every token written after "alert" in `doc` that is no alert kind of `kinds`.
fn alert_mention_problems(doc: &str, kinds: &BTreeSet<String>) -> Vec<String> {
    let marker = "alert `";
    doc.match_indices(marker)
        .filter_map(|(at, _)| {
            let token = doc[at + marker.len()..]
                .split('`')
                .next()
                .unwrap_or_default();
            (!kinds.contains(token))
                .then(|| format!("`{token}` is written as an alert and is none"))
        })
        .collect()
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
    let mut calls = verbs();
    calls.extend(XCODE_CALLS.iter().map(|call| (*call).to_owned()));
    let required: Vec<&str> = calls.iter().map(String::as_str).collect();
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
    let alerts: BTreeSet<String> = RUNBOOK_ALERTS
        .iter()
        .filter(|k| kinds.contains(**k))
        .map(|k| (*k).to_owned())
        .collect();
    problems.extend(names_problems(doc, ALERTS, "alert of a run", &alerts));
    problems.extend(alert_mention_problems(doc, &kinds));
    problems.extend(names_problems(
        doc,
        SKIPS,
        "reason a host is left out",
        &tokens_of(
            "hostkeeper.operator.v1.MaintenanceSkip",
            "MAINTENANCE_SKIP_",
        ),
    ));
    problems.extend(listed_problems(
        doc,
        MDM,
        "setting the detector reports",
        &hostkeeper_daemon::maintenance_mdm::reported(),
    ));
    problems.extend(listed_problems(
        doc,
        ACCOUNT_KEYS,
        "key of the bootstrap",
        &hostkeeper_bootstrap::update_account::KEYS,
    ));
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
/// of the table or an Xcode call uncalled, or lists a setting `MaintenancePolicy` does not have
/// (or leaves one of its fields out); a run state, host step, refusal, alert or reason a host is
/// left out that it lists and the protos do not define, or one they define that it leaves out;
/// and a device-management setting or an update-account key it lists that the daemon's detector
/// or the bootstrap does not have, leaves out, or lists out of their order.
#[test]
fn the_maintenance_runbook_matches_the_code() {
    assert_eq!(problems(&real()), Vec::<String>::new());
    assert_eq!(verbs().len(), 6, "{:?}", verbs());
    assert_eq!(
        xcode_methods(),
        ["xcode list", "xcode remove"],
        "the xcode service's methods, all of which XCODE_CALLS names"
    );
    assert_eq!(hostkeeper_daemon::maintenance_mdm::reported().len(), 16);
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
    // An Xcode call no line makes: both pushes named by a verb hk does not have.
    let push = "<token-file> xcode-push ";
    assert_eq!(doc.matches(push).count(), 2);
    let found = problems(&doc.replace(push, "<token-file> xcode-upload "));
    assert_eq!(found.len(), 3, "{found:?}");
    for parse in &found[..2] {
        assert!(
            parse.ends_with("no service xcode-upload; 'hk --help' lists them"),
            "{found:?}"
        );
    }
    assert_eq!(found[2], "no command line calls `hk xcode-push`");
    // The store's removal left uncalled.
    let remove = "    xcode remove --sha256 <sha256>";
    let found = edit(remove, "    xcode list --sha256 <sha256>");
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found[0].ends_with("xcode list has no flag --sha256"),
        "{found:?}"
    );
    assert_eq!(found[1], "no command line calls `hk xcode remove`");
    // An alert of the store dropped from the alerts table.
    assert_eq!(
        edit("| `xcode_version_taken` | warning |", "| warning |"),
        ["the code's `xcode_version_taken` is missing under `## Alerts`"]
    );
    // A token written as an alert that is none.
    assert_eq!(
        edit(
            "warning alert `xcode_release_waiting`",
            "warning alert `xcode_waiting`"
        ),
        ["`xcode_waiting` is written as an alert and is none"]
    );
    // A reason a host is left out that the protos do not define, and the one it replaced.
    assert_eq!(
        edit("| `group_changed` |", "| `moved` |"),
        [
            "`moved` under `### Why a host is left out` is no reason a host is left out of the code",
            "the code's `group_changed` is missing under `### Why a host is left out`",
        ]
    );
    // A device-management setting the detector does not report, one it reports left out, one
    // listed twice, and two listed out of the detector's order.
    assert_eq!(
        edit("| `Beta` |", "| `BetaEnrollment` |"),
        [
            "`BetaEnrollment` under `### What device management must not set` is no setting the detector reports of the code",
            "the code's `Beta` is missing under `### What device management must not set`",
        ]
    );
    let catalog =
        "| `CatalogURL` | `com.apple.SoftwareUpdate` | another software-update catalog |\n";
    assert_eq!(
        edit(catalog, ""),
        ["the code's `CatalogURL` is missing under `### What device management must not set`"]
    );
    assert_eq!(
        edit(catalog, &format!("{catalog}{catalog}")),
        ["`CatalogURL` is listed twice under `### What device management must not set`"]
    );
    let pre =
        "| `AllowPreReleaseInstallation` | `com.apple.SoftwareUpdate` | pre-release installs |\n";
    assert_eq!(
        edit(&format!("{catalog}{pre}"), &format!("{pre}{catalog}")),
        [
            "`AllowPreReleaseInstallation` under `### What device management must not set` stands where the code lists `CatalogURL`: keep the code's order"
        ]
    );
    // An update-account key the bootstrap does not take, and two keys out of its order.
    assert_eq!(
        edit("| `admin_name` |", "| `admin_user` |"),
        [
            "`admin_user` under `### The update account's keys` is no key of the bootstrap of the code",
            "the code's `admin_name` is missing under `### The update account's keys`",
        ]
    );
    let name = "| `update_account_name` | the update account's name, `hkupdate` |\n";
    let password = "| `update_account_password` | its password: not empty, no line break |\n";
    assert_eq!(
        edit(&format!("{name}{password}"), &format!("{password}{name}")),
        [
            "`update_account_password` under `### The update account's keys` stands where the code lists `update_account_name`: keep the code's order"
        ]
    );
}

/// The order check of [`listed_problems`] says nothing of a table [`names_problems`] already
/// refuses: one that is gone, or one of another length.
#[test]
fn the_order_check_leaves_other_defects_to_the_names_check() {
    let code = ["a", "b"];
    assert_eq!(
        order_problems("# Other\n", "### T", &code),
        Vec::<String>::new()
    );
    let short = "### T\n\n| `b` |\n";
    assert_eq!(order_problems(short, "### T", &code), Vec::<String>::new());
    assert_eq!(
        listed_problems(short, "### T", "name", &code),
        ["the code's `a` is missing under `### T`"]
    );
    let swapped = "### T\n\n| `b` |\n| `a` |\n";
    assert_eq!(
        listed_problems(swapped, "### T", "name", &code),
        ["`b` under `### T` stands where the code lists `a`: keep the code's order"]
    );
    let kept = "### T\n\n| `a` |\n| `b` |\n";
    assert_eq!(
        listed_problems(kept, "### T", "name", &code),
        Vec::<String>::new()
    );
}
