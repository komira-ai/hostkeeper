//! The rules of the steps of `set-desired.yml` that call the coordinator, and of the two steps
//! around them: the request step that writes what the set sends, and the outputs step that
//! reads what the calls left. [`set_desired`](super::set_desired) applies them.
//!
//! A call step ([`CALLS`]) is refused when its script is not one command; that command does not
//! run [`PROGRAM`]; before the method it does not pass each flag of [`CALL_GLOBALS`] and its own
//! `--response-file` exactly once with exactly its value, or passes any other flag, a token input
//! of [`TOKEN_INPUTS`] among them (a call takes the job's own OIDC token, by
//! `--github-oidc-audience`, and no token is ever a file or stdin of the job); it does not call
//! `releases <method>`, or that is not a command of hkctl's table (`hostkeeper_proto::ctl`); a flag
//! after the method is not one of that command; or it does not end by sending its diagnostics to
//! its own file (`2> <file>`), the one the outputs step reads; or it does not pass each flag of
//! its `fields` after the method exactly once with exactly its value (the set: the service and
//! [`REQUEST_FILE`], which the request step writes; the wait: the service, the version and
//! [`WAIT_SECS`]); or a variable of [`CALL_ENV`] its command names is not given in its `env` as
//! that input.
//!
//! The outputs step is refused when its `env` does not give each of [`OUTPUTS_ENV`] exactly or
//! has no [`OUTPUTS_PROGRAM_ENV`], or its one command does not bind each variable of
//! [`OUTPUTS_ARGS`] and run that program into `$GITHUB_OUTPUT`: the workflow lint's tests run the
//! program with exactly those variables.

use hostkeeper_hkctl::is_served;
use hostkeeper_proto::ctl::{COMMANDS, Command, Input};

use super::lint::entry;
use super::set_desired::{CLIENT_STEP, PROGRAM, WAIT_STEP, logical_lines, text, values, words};
use super::yaml::Node;

/// One call step: its `id`, the method of `releases` it calls, the files of its answer and of
/// its diagnostics, and the flags of the method it passes with their values.
pub struct Call {
    pub step: &'static str,
    pub method: &'static str,
    pub response: &'static str,
    pub err: &'static str,
    pub fields: &'static [(&'static str, &'static str)],
}

/// The two calls, in the order they run.
pub const CALLS: [Call; 2] = [
    Call {
        step: CLIENT_STEP,
        method: "set-desired",
        response: "$RUNNER_TEMP/hkctl-set.json",
        err: "$RUNNER_TEMP/hkctl-set.err",
        fields: &[("--service", "$SERVICE"), ("--request", REQUEST_FILE)],
    },
    Call {
        step: WAIT_STEP,
        method: "wait-converged",
        response: "$RUNNER_TEMP/hkctl-wait.json",
        err: "$RUNNER_TEMP/hkctl-wait.err",
        fields: &[
            ("--service", "$SERVICE"),
            ("--version", "$VERSION"),
            WAIT_SECS,
        ],
    },
];
/// What a call step's `env` gives each variable its command names, exactly: the caller's inputs.
pub const CALL_ENV: [(&str, &str); 5] = [
    ("SERVICE", "${{ inputs.service }}"),
    ("VERSION", "${{ inputs.version }}"),
    ("COORDINATOR_URL", "${{ inputs.coordinator_url }}"),
    ("AUDIENCE", "${{ inputs.audience }}"),
    ("MAX_POLLS", MAX_POLLS_INPUT),
];
/// The service of hkctl both calls are methods of.
pub const SERVICE: &str = "releases";
/// The global flags each call passes before the service, with their values: the private
/// listener, the coordinator's CA as the one trusted root, and the job's own OIDC token for the
/// caller's audience.
pub const CALL_GLOBALS: [(&str, &str); 3] = [
    ("--endpoint", "$COORDINATOR_URL"),
    ("--ca-file", "$RUNNER_TEMP/hkctl-coordinator-ca.pem"),
    ("--github-oidc-audience", "$AUDIENCE"),
];
/// The flag that puts a call's answer in its file, so that stdout carries only workflow
/// commands (hkctl's `::add-mask::`).
pub const RESPONSE_FLAG: &str = "--response-file";
/// hkctl's other token inputs, which no call passes.
pub const TOKEN_INPUTS: [&str; 2] = ["--token-file", "--token-stdin"];
/// The flags of hkctl that take no value.
const SWITCHES: [&str; 1] = ["--token-stdin"];
/// The request of the set, which the request step writes.
pub const REQUEST_FILE: &str = "$RUNNER_TEMP/hkctl-request.json";
/// How the wait step bounds its hold: `max_polls` with a 0 appended, ten times it, as text.
pub const WAIT_SECS: (&str, &str) = ("--wait-secs", "${MAX_POLLS}0");
/// The value of `MAX_POLLS` in the wait step.
pub const MAX_POLLS_INPUT: &str = "${{ inputs.max_polls }}";
/// The variable of the outputs step's `env` that holds its jq program.
pub const OUTPUTS_PROGRAM_ENV: &str = "OUTPUTS_PROGRAM";
/// The rest of the outputs step's `env`, exactly: what the program names, the outcome of each
/// call, and each file a call leaves, or `/dev/null` when it left none.
pub const OUTPUTS_ENV: [(&str, &str); 9] = [
    ("SERVICE", "${{ inputs.service }}"),
    ("VERSION", "${{ inputs.version }}"),
    ("WAIT", "${{ inputs.wait }}"),
    ("SET_OUTCOME", "${{ steps.set-desired.outcome }}"),
    ("WAIT_OUTCOME", "${{ steps.wait.outcome }}"),
    (
        "SET_BODY",
        "${{ steps.set-desired.outcome == 'success' && format('{0}/hkctl-set.json', runner.temp) || '/dev/null' }}",
    ),
    (
        "SET_ERR",
        "${{ steps.set-desired.outcome == 'failure' && format('{0}/hkctl-set.err', runner.temp) || '/dev/null' }}",
    ),
    (
        "WAIT_BODY",
        "${{ steps.wait.outcome == 'success' && format('{0}/hkctl-wait.json', runner.temp) || '/dev/null' }}",
    ),
    (
        "WAIT_ERR",
        "${{ steps.wait.outcome == 'failure' && format('{0}/hkctl-wait.err', runner.temp) || '/dev/null' }}",
    ),
];
/// The variables the outputs step's jq binds, in order: how (`--arg`, a text; `--rawfile`, a
/// file's text), the variable's name, and the `env` it comes from.
pub const OUTPUTS_ARGS: [(&str, &str, &str); 9] = [
    ("--arg", "service", "$SERVICE"),
    ("--arg", "version", "$VERSION"),
    ("--arg", "wait", "$WAIT"),
    ("--arg", "set_outcome", "$SET_OUTCOME"),
    ("--arg", "wait_outcome", "$WAIT_OUTCOME"),
    ("--rawfile", "set_body", "$SET_BODY"),
    ("--rawfile", "set_err", "$SET_ERR"),
    ("--rawfile", "wait_body", "$WAIT_BODY"),
    ("--rawfile", "wait_err", "$WAIT_ERR"),
];
/// How the outputs step's one command ends: the program, its output appended to the job's.
pub const OUTPUTS_END: [&str; 3] = ["$OUTPUTS_PROGRAM", ">>", "$GITHUB_OUTPUT"];

/// The command of hkctl's table for `releases <method>`.
pub fn command(method: &str) -> Option<&'static Command> {
    COMMANDS
        .iter()
        .find(|c| is_served(c) && c.service_command == SERVICE && c.method_command == method)
}

/// The flags `command` takes after its method: `--request`, and each field's flags.
fn field_flags(command: &Command) -> Vec<String> {
    let mut flags = vec!["--request".to_owned()];
    for f in command.fields {
        match f.input {
            Input::Flag => flags.push(format!("--{}", f.flag)),
            Input::SecretFile => {
                flags.push(format!("--{}-file", f.flag));
                flags.push(format!("--{}-stdin", f.flag));
            }
            Input::RequestFile => {}
        }
    }
    flags
}

/// The flags at the start of `words` (each `--flag value`, `--flag=value` or a switch), and the
/// number of words they take.
fn flags_at<'a>(words: &[&'a str]) -> (Vec<&'a str>, usize) {
    let mut flags = Vec::new();
    let mut i = 0;
    while let Some(w) = words.get(i).filter(|w| w.starts_with("--")) {
        let name = w.split_once('=').map_or(*w, |(name, _)| name);
        flags.push(name);
        i += if w.contains('=') || SWITCHES.contains(&name) {
            1
        } else {
            2
        };
    }
    (flags, i.min(words.len()))
}

/// What is wrong with the call step `step`, named `who`, that makes `call`.
pub fn step_call_problems(who: &str, step: &Node, call: &Call) -> Vec<String> {
    let lines: Vec<String> = logical_lines(text(step, "run").unwrap_or_default())
        .into_iter()
        .filter(|l| !l.trim().is_empty())
        .collect();
    let [line] = lines.as_slice() else {
        return vec![format!("{who} does not run `{PROGRAM}` as its one command")];
    };
    let parts = words(line);
    if parts.first() != Some(&PROGRAM) {
        return vec![format!("{who} does not run `{PROGRAM}` as its one command")];
    }
    let mut problems = Vec::new();
    let (globals, took) = flags_at(&parts[1..]);
    let global_words = &parts[1..1 + took];
    let response = (RESPONSE_FLAG, call.response);
    for (flag, want) in CALL_GLOBALS.iter().chain([&response]) {
        if values(global_words, flag) != [*want] {
            problems.push(format!(
                "{who} does not pass `{flag} \"{want}\"` before the method exactly once"
            ));
        }
    }
    let allowed: Vec<&str> = CALL_GLOBALS
        .iter()
        .map(|(f, _)| *f)
        .chain([RESPONSE_FLAG])
        .collect();
    let rest = &parts[1 + took..];
    let method = rest.get(..2).unwrap_or_default();
    let (fields, field_took) = flags_at(rest.get(2..).unwrap_or_default());
    for flag in globals.iter().chain(&fields) {
        if TOKEN_INPUTS.contains(flag) {
            problems.push(format!(
                "{who} passes `{flag}`: a call takes the job's own OIDC token, by `--github-oidc-audience`"
            ));
        }
    }
    for flag in &globals {
        if !allowed.contains(flag) && !TOKEN_INPUTS.contains(flag) {
            problems.push(format!(
                "{who} passes `{flag}` before the method, which no call needs"
            ));
        }
    }
    if method != [SERVICE, call.method] {
        problems.push(format!("{who} does not call `{SERVICE} {}`", call.method));
        return problems;
    }
    let Some(command) = command(call.method) else {
        problems.push(format!(
            "`{SERVICE} {}` is not a command of hkctl's table",
            call.method
        ));
        return problems;
    };
    let known = field_flags(command);
    for flag in &fields {
        if !known.iter().any(|k| k == flag) && !TOKEN_INPUTS.contains(flag) {
            problems.push(format!(
                "{who} passes `{flag}`, which is not a flag of `{SERVICE} {}`",
                call.method
            ));
        }
    }
    if rest.get(2 + field_took..) != Some(&["2>", call.err][..]) {
        problems.push(format!(
            "{who} does not end with `2> \"{}\"`, the file the outputs step reads",
            call.err
        ));
    }
    let after = rest.get(2..2 + field_took).unwrap_or_default();
    for (flag, want) in call.fields {
        if values(after, flag) != [*want] {
            problems.push(format!(
                "{who} does not pass `{flag} \"{want}\"` exactly once"
            ));
        }
    }
    let env = entry(step, "env");
    for (key, want) in CALL_ENV {
        let named = line.contains(&format!("${key}")) || line.contains(&format!("${{{key}}}"));
        if named && env.and_then(|e| text(e, key)) != Some(want) {
            problems.push(format!("{who}'s `{key}` is not `{want}`"));
        }
    }
    problems
}

/// The variables of the request program, each from its own variable of the step's `env`, which
/// is the caller's input of the same name: a variable not passed, or passed from anything else,
/// leaves that part of the release out of the request with no error.
pub const REQUEST_ARGS: [(&str, &str, &str); 5] = [
    ("version", "VERSION", "${{ inputs.version }}"),
    (
        "package_digest",
        "PACKAGE_DIGEST",
        "${{ inputs.package_digest }}",
    ),
    (
        "release_index",
        "RELEASE_INDEX",
        "${{ inputs.release_index }}",
    ),
    (
        "release_index_digest",
        "RELEASE_INDEX_DIGEST",
        "${{ inputs.release_index_digest }}",
    ),
    ("targets", "TARGETS", "${{ inputs.targets }}"),
];

/// What is wrong with the request step: it does not write [`REQUEST_FILE`], or does not pass
/// each of [`REQUEST_ARGS`] from its input.
pub fn request_problems(step: &Node) -> Vec<String> {
    let run = text(step, "run").unwrap_or_default();
    let mut problems = Vec::new();
    if !run.trim_end().ends_with(&format!("> \"{REQUEST_FILE}\"")) {
        problems.push(format!(
            "the request step does not end by writing `\"{REQUEST_FILE}\"`, the file the set step reads"
        ));
    }
    let env = entry(step, "env");
    let lines = logical_lines(run);
    let parts: Vec<&str> = lines.iter().flat_map(|line| words(line)).collect();
    for (name, var, want) in REQUEST_ARGS {
        if env.and_then(|e| text(e, var)) != Some(want) {
            problems.push(format!("the request step's `{var}` is not `{want}`"));
        }
        // `words` takes the quotes off a word.
        let arg = format!("${var}");
        if !parts.windows(3).any(|w| w == ["--arg", name, arg.as_str()]) {
            problems.push(format!(
                "the request step does not pass `--arg {name} \"${var}\"`"
            ));
        }
    }
    problems
}

/// What is wrong with the outputs step: its `env` or the command that runs its program.
pub fn outputs_problems(step: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    let env = entry(step, "env");
    for (key, want) in OUTPUTS_ENV {
        if env.and_then(|e| text(e, key)) != Some(want) {
            problems.push(format!("the outputs step's `{key}` is not `{want}`"));
        }
    }
    if env.and_then(|e| text(e, OUTPUTS_PROGRAM_ENV)).is_none() {
        problems.push(format!(
            "the outputs step has no `{OUTPUTS_PROGRAM_ENV}` in its `env`"
        ));
    }
    let lines = logical_lines(text(step, "run").unwrap_or_default());
    let parts = match lines.as_slice() {
        [line] => words(line),
        _ => Vec::new(),
    };
    if parts.first() != Some(&"jq") || !parts.ends_with(&OUTPUTS_END) {
        problems.push(format!(
            "the outputs step does not run `jq ... {}` as its one command",
            OUTPUTS_END.join(" ")
        ));
    }
    for (how, name, var) in OUTPUTS_ARGS {
        if !parts.windows(3).any(|w| w == [how, name, var]) {
            problems.push(format!(
                "the outputs step does not pass `{how} {name} \"{var}\"`"
            ));
        }
    }
    problems
}

/// The outputs step's jq program in the workflow `doc`.
pub fn outputs_program(doc: &Node) -> Option<&str> {
    step(doc, "outputs")
        .and_then(|s| entry(s, "env"))
        .and_then(|e| text(e, OUTPUTS_PROGRAM_ENV))
}

/// The request step's jq program in the workflow `doc`: the text between the first and the last
/// `'` of its script.
pub fn request_program(doc: &Node) -> Option<&str> {
    let run = step(doc, "request").and_then(|s| text(s, "run"))?;
    let start = run.find('\'')? + 1;
    let end = run.rfind('\'')?;
    run.get(start..end)
}

/// The step of the one job with `id: id`.
fn step<'a>(doc: &'a Node, id: &str) -> Option<&'a Node> {
    use super::set_desired::JOB;
    use super::yaml::Value;
    let steps = entry(doc, "jobs")
        .and_then(|jobs| entry(jobs, JOB))
        .and_then(|job| entry(job, "steps"));
    match steps.map(|s| &s.value) {
        Some(Value::Seq(steps)) => steps.iter().find(|s| text(s, "id") == Some(id)),
        _ => None,
    }
}
