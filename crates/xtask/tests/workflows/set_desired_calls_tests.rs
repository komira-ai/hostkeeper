//! The rules of [`set_desired_calls`](super::set_desired_calls) and the guards, order and
//! environment rules of [`set_desired`](super::set_desired) that the two calls brought: each case
//! adds one defect to [`CLIENT`] (or the real file) and reads the exact problems the lint
//! reports. [`CLIENT`]'s steps are, from 1: download, checksum, attestation, commit, install,
//! version, tailnet, ca, request, set-desired, wait, outputs.

use super::lint::entry;
use super::set_desired::{ALWAYS_IF, CALL_STEPS, ORDERED, WAIT_IF};
use super::set_desired_calls::{CALLS, OUTPUTS_ENV, REQUEST_ARGS, command};
use super::set_desired_tests::{CLIENT, CLIENT_BLOCK, assert_client, moved, real, step_ids};
use super::with_in;
use super::yaml::{self, Node, Value};

/// The set step's line of its global flag `--github-oidc-audience`, and the wait step's.
const AUDIENCE: &str = "            --github-oidc-audience \"$AUDIENCE\" \\\n";
/// The set step's response flag.
const SET_RESPONSE: &str = "            --response-file \"$RUNNER_TEMP/hkctl-set.json\" \\\n";
/// The wait step's bound.
const WAIT_SECS: &str = "            --wait-secs \"${MAX_POLLS}0\" \\\n";

/// [`CLIENT`] with `from` replaced by `to` in its outputs step only.
fn outputs_with(from: &str, to: &str) -> String {
    let start = CLIENT
        .find("      - id: outputs\n")
        .expect("the outputs step");
    let (head, tail) = CLIENT.split_at(start);
    assert_eq!(tail.matches(from).count(), 1, "{from:?}");
    format!("{head}{}", tail.replacen(from, to, 1))
}

/// [`CLIENT`] with `from` replaced by `to` in its set step only.
fn set_with(from: &str, to: &str) -> String {
    assert!(CLIENT_BLOCK.contains(from), "{from:?}");
    with_in(CLIENT, CLIENT_BLOCK, &CLIENT_BLOCK.replacen(from, to, 1))
}

#[test]
fn the_real_file_keeps_every_rule_of_the_calls() {
    // Catches: the real workflow drifting from the rules, a step of the order missing from it,
    // and the two methods it calls missing from hkctl's command table.
    let text = real();
    assert_client(&text, &[]);
    assert_client(CLIENT, &[]);
    let ids = step_ids(&text);
    let at = |id: &str| ids.iter().position(|s| s == id).expect(id);
    for pair in ORDERED.windows(2) {
        assert!(at(pair[0]) < at(pair[1]), "{pair:?} in {ids:?}");
    }
    assert_eq!(ids.last().map(String::as_str), Some("outputs"));
    for call in &CALLS {
        let found = command(call.method).expect("a command of the table");
        assert_eq!(found.service, "Releases");
    }
    assert!(command("no-such-method").is_none());
}

#[test]
fn a_call_takes_the_jobs_own_token_and_no_other() {
    // Catches: a call given a token file or stdin (a token the job would have to hold), the
    // fresh OIDC token dropped, and a global flag beyond those a call needs.
    let token_file = "step 10 (`set-desired`) passes `--token-file`: a call takes the job's own OIDC token, by `--github-oidc-audience`";
    let no_audience = "step 10 (`set-desired`) does not pass `--github-oidc-audience \"$AUDIENCE\"` before the method exactly once";
    assert_client(
        &set_with(
            AUDIENCE,
            "            --token-file \"$RUNNER_TEMP/token\" \\\n",
        ),
        &[no_audience, token_file],
    );
    assert_client(
        &with_in(
            CLIENT,
            "            --service \"$SERVICE\" \\\n            --request",
            "            --service \"$SERVICE\" \\\n            --token-stdin \\\n            --request",
        ),
        &[
            "step 10 (`set-desired`) passes `--token-stdin`: a call takes the job's own OIDC token, by `--github-oidc-audience`",
        ],
    );
    // A switch before the method takes no value: the method after it is still read.
    assert_client(
        &set_with(
            SET_RESPONSE,
            &format!("{SET_RESPONSE}            --token-stdin \\\n"),
        ),
        &[
            "step 10 (`set-desired`) passes `--token-stdin`: a call takes the job's own OIDC token, by `--github-oidc-audience`",
        ],
    );
    assert_client(&set_with(AUDIENCE, ""), &[no_audience]);
    // Given twice, or with another value.
    let wait_audience = "step 11 (`wait`) does not pass `--github-oidc-audience \"$AUDIENCE\"` before the method exactly once";
    let waited = |to: &str| {
        let start = CLIENT.find("      - id: wait\n").expect("the wait step");
        let (head, tail) = CLIENT.split_at(start);
        format!("{head}{}", tail.replacen(AUDIENCE, to, 1))
    };
    assert_client(&waited(&format!("{AUDIENCE}{AUDIENCE}")), &[wait_audience]);
    assert_client(
        &waited("            --github-oidc-audience=hostkeeper-ci \\\n"),
        &[wait_audience],
    );
    assert_client(
        &set_with(
            AUDIENCE,
            &format!("{AUDIENCE}            --timeout-secs 5 \\\n"),
        ),
        &["step 10 (`set-desired`) passes `--timeout-secs` before the method, which no call needs"],
    );
    assert_client(
        &set_with(
            "            --endpoint \"$COORDINATOR_URL\" \\\n",
            "            --endpoint https://coordinator.example \\\n",
        ),
        &[
            "step 10 (`set-desired`) does not pass `--endpoint \"$COORDINATOR_URL\"` before the method exactly once",
        ],
    );
    assert_client(
        &set_with(
            "            --ca-file \"$RUNNER_TEMP/hkctl-coordinator-ca.pem\" \\\n",
            "",
        ),
        &[
            "step 10 (`set-desired`) does not pass `--ca-file \"$RUNNER_TEMP/hkctl-coordinator-ca.pem\"` before the method exactly once",
        ],
    );
}

#[test]
fn a_call_is_one_hkctl_command_of_the_table_into_its_own_files() {
    // Catches: a call step that runs more than hkctl, another program, a method renamed or not
    // in the table, a flag the method does not take, its answer printed on stdout (where the
    // token's mask line is) or into another file, and its diagnostics not in the file the
    // outputs step reads.
    let not_one =
        "step 10 (`set-desired`) does not run `$RUNNER_TEMP/hkctl-bin/hkctl` as its one command";
    let first = "          \"$RUNNER_TEMP/hkctl-bin/hkctl\" \\\n            --endpoint";
    assert_client(
        &set_with(first, &format!("          echo calling\n{first}")),
        &[not_one],
    );
    assert_client(
        &with_in(
            CLIENT,
            CLIENT_BLOCK,
            &CLIENT_BLOCK.replace(
                "$RUNNER_TEMP/hkctl-bin/hkctl",
                "$RUNNER_TEMP/hkctl-bin/hostkeeper",
            ),
        ),
        &[not_one],
    );
    assert_client(
        &with_in(CLIENT, "releases set-desired \\", "releases set-desire \\"),
        &["step 10 (`set-desired`) does not call `releases set-desired`"],
    );
    assert_client(
        &with_in(CLIENT, "releases wait-converged \\", "releases converge \\"),
        &["step 11 (`wait`) does not call `releases wait-converged`"],
    );
    assert_client(
        &with_in(
            CLIENT,
            WAIT_SECS,
            &format!("{WAIT_SECS}            --max-polls \"$MAX_POLLS\" \\\n"),
        ),
        &[
            "step 11 (`wait`) passes `--max-polls`, which is not a flag of `releases wait-converged`",
        ],
    );
    assert_client(
        &set_with(SET_RESPONSE, ""),
        &[
            "step 10 (`set-desired`) does not pass `--response-file \"$RUNNER_TEMP/hkctl-set.json\"` before the method exactly once",
        ],
    );
    // The response flag after the method is a flag of no method.
    assert_client(
        &with_in(
            &set_with(SET_RESPONSE, ""),
            "            --service \"$SERVICE\" \\\n            --request",
            &format!("            --service \"$SERVICE\" \\\n{SET_RESPONSE}            --request"),
        ),
        &[
            "step 10 (`set-desired`) does not pass `--response-file \"$RUNNER_TEMP/hkctl-set.json\"` before the method exactly once",
            "step 10 (`set-desired`) passes `--response-file`, which is not a flag of `releases set-desired`",
        ],
    );
    let no_err = "step 10 (`set-desired`) does not end with `2> \"$RUNNER_TEMP/hkctl-set.err\"`, the file the outputs step reads";
    assert_client(
        &with_in(
            CLIENT,
            "--request \"$RUNNER_TEMP/hkctl-request.json\" \\\n            2> \"$RUNNER_TEMP/hkctl-set.err\"\n",
            "--request \"$RUNNER_TEMP/hkctl-request.json\"\n",
        ),
        &[no_err],
    );
    assert_client(
        &with_in(
            CLIENT,
            "2> \"$RUNNER_TEMP/hkctl-set.err\"",
            "2> \"$RUNNER_TEMP/hkctl-wait.err\"",
        ),
        &[no_err],
    );
    assert_client(
        &with_in(CLIENT, "2> \"$RUNNER_TEMP/hkctl-set.err\"", "2>&1"),
        &[no_err],
    );
}

#[test]
fn the_set_sends_the_request_and_the_wait_is_bounded_by_max_polls() {
    // Catches: the set sending another file than the request step writes, the request step
    // writing elsewhere, and the wait held without its bound, with a bound that is not ten times
    // `max_polls`, or with `max_polls` taken from anything but its input.
    let request = "            --request \"$RUNNER_TEMP/hkctl-request.json\" \\\n";
    let no_request = "step 10 (`set-desired`) does not pass `--request \"$RUNNER_TEMP/hkctl-request.json\"` exactly once";
    assert_client(&with_in(CLIENT, request, ""), &[no_request]);
    assert_client(
        &with_in(CLIENT, request, &format!("{request}{request}")),
        &[no_request],
    );
    assert_client(
        &with_in(
            CLIENT,
            "'{}' > \"$RUNNER_TEMP/hkctl-request.json\"",
            "'{}' > \"$RUNNER_TEMP/request.json\"",
        ),
        &[
            "the request step does not end by writing `\"$RUNNER_TEMP/hkctl-request.json\"`, the file the set step reads",
        ],
    );
    let no_bound = "step 11 (`wait`) does not pass `--wait-secs \"${MAX_POLLS}0\"` exactly once";
    for changed in [
        String::new(),
        "            --wait-secs 1800 \\\n".to_owned(),
        "            --wait-secs \"$MAX_POLLS\" \\\n".to_owned(),
        format!("            --wait-secs 1 \\\n{WAIT_SECS}"),
    ] {
        assert_client(&with_in(CLIENT, WAIT_SECS, &changed), &[no_bound]);
    }
    assert_client(
        &with_in(
            CLIENT,
            "MAX_POLLS: ${{ inputs.max_polls }}",
            "MAX_POLLS: 180",
        ),
        &["step 11 (`wait`)'s `MAX_POLLS` is not `${{ inputs.max_polls }}`"],
    );
    assert_client(
        &with_in(CLIENT, "        type: number\n", "        type: string\n"),
        &["no input `max_polls` of `type: number`"],
    );
}

#[test]
fn only_the_two_calls_run_on_past_a_failure_and_the_outputs_run_always() {
    // Catches: a call whose failure stops the job before the outputs step reads it, any other
    // step the job runs on past, a wait run when the set failed or when the caller does not
    // wait, and an outputs step that does not run after a failure.
    let continues = "        continue-on-error: true\n";
    let set = "      - id: set-desired\n";
    assert_client(
        &with_in(CLIENT, &format!("{set}{continues}"), set),
        &[
            "step 10 (`set-desired`) does not have `continue-on-error: true`: the outputs step reads how it ended",
        ],
    );
    let wait_if = format!("        if: {WAIT_IF}\n");
    assert_client(
        &with_in(
            CLIENT,
            &format!("{wait_if}{continues}"),
            &format!("{wait_if}        continue-on-error: false\n"),
        ),
        &[
            "step 11 (`wait`) does not have `continue-on-error: true`: the outputs step reads how it ended",
        ],
    );
    let download = "      - id: download\n";
    assert_client(
        &with_in(CLIENT, download, &format!("{download}{continues}")),
        &["step 1 (`download`) has `continue-on-error`, which could run the job on past it"],
    );
    let always = format!("        if: {ALWAYS_IF}\n");
    assert_client(
        &with_in(CLIENT, &always, &format!("{always}{continues}")),
        &["step 12 (`outputs`) has `continue-on-error`, which could run the job on past it"],
    );
    for to in [
        String::new(),
        "        if: ${{ inputs.wait }}\n".to_owned(),
        "        if: ${{ steps.set-desired.outcome == 'success' }}\n".to_owned(),
    ] {
        assert_client(
            &with_in(CLIENT, &wait_if, &to),
            &[&format!("step 11 (`wait`) does not have `if: {WAIT_IF}`")],
        );
    }
    let no_always = format!("step 12 (`outputs`) does not have `if: {ALWAYS_IF}`");
    for to in ["", "        if: ${{ !cancelled() }}\n"] {
        assert_client(&with_in(CLIENT, &always, to), &[&no_always]);
    }
    let request = "      - id: request\n";
    assert_client(
        &with_in(
            CLIENT,
            request,
            &format!("{request}        if: ${{{{ always() }}}}\n"),
        ),
        &["step 9 (`request`) has `if`, which could run the job on past it"],
    );
    // A second call step of the same id is one problem, not a stray `continue-on-error` too.
    assert_client(
        &with_in(
            CLIENT,
            CLIENT_BLOCK,
            &format!("{CLIENT_BLOCK}{CLIENT_BLOCK}"),
        ),
        &["more than one step has `id: set-desired`"],
    );
    for id in CALL_STEPS {
        let doc = yaml::load(&real()).expect("parses");
        let step = steps(&doc)
            .into_iter()
            .find(|s| entry(s, "id").and_then(Node::as_str) == Some(id))
            .expect(id);
        assert_eq!(
            entry(&step, "continue-on-error").and_then(Node::as_str),
            Some("true")
        );
    }
}

#[test]
fn the_calls_come_after_what_they_read_and_the_outputs_come_last() {
    // Catches: the set before the request or the CA it reads, the wait before the set, and the
    // outputs step before a call it reports, or missing.
    let request = moved(
        CLIENT,
        "      - id: request\n",
        "      - id: set-desired\n",
        "      - id: wait\n",
    );
    assert_client(
        &request,
        &[
            "the set step `set-desired` (step 9) comes before `request` (step 10), whose file it reads",
        ],
    );
    let wait_first = moved(
        CLIENT,
        "      - id: wait\n",
        "      - id: outputs\n",
        "      - id: set-desired\n",
    );
    assert_client(
        &wait_first,
        &["the wait step `wait` (step 10) comes before the set step `set-desired` (step 11)"],
    );
    let outputs = &CLIENT[CLIENT.find("      - id: outputs\n").expect("outputs")..];
    let without = with_in(CLIENT, outputs, "");
    assert_client(&without, &["no step has `id: outputs`"]);
    let outputs_early = with_in(
        &without,
        "      - id: wait\n",
        &format!("{outputs}      - id: wait\n"),
    );
    assert_client(
        &outputs_early,
        &["the outputs step `outputs` (step 11) is not the last step"],
    );
    let late = format!("{CLIENT}      - id: after\n        run: echo done\n");
    assert_client(
        &late,
        &["the outputs step `outputs` (step 12) is not the last step"],
    );
}

#[test]
fn no_step_hands_a_token_or_an_environment_on() {
    // Catches: a step after the verification whose `env` names the token request (a call step
    // passing it on), and a step that writes GITHUB_ENV, by which a value would reach the
    // environment of every step after it.
    let wait_env = "          MAX_POLLS: ${{ inputs.max_polls }}\n";
    assert_client(
        &with_in(
            CLIENT,
            wait_env,
            &format!("{wait_env}          URL: ${{{{ env.ACTIONS_ID_TOKEN_REQUEST_URL }}}}\n"),
        ),
        &["step 11 (`wait`)'s `env` names the OIDC token request"],
    );
    assert_client(
        &with_in(
            CLIENT,
            "        run: printf '%s\\n' \"$CA\" > \"$RUNNER_TEMP/hkctl-coordinator-ca.pem\"\n",
            "        run: echo \"CA=$RUNNER_TEMP/ca.pem\" >> \"$GITHUB_ENV\"\n",
        ),
        &["step 8 (`ca`) writes `GITHUB_ENV`, which sets the environment of the steps after it"],
    );
    for (script, word) in [
        ("while true; do sleep 10; done", "while"),
        ("until false; do sleep 10; done", "until"),
        ("for poll in 1 2 3; do sleep 10; done", "for"),
    ] {
        assert_client(
            &with_in(
                CLIENT,
                "        run: jq --null-input --arg version",
                &format!(
                    "        run: |\n          {script}\n          jq --null-input --arg version"
                ),
            ),
            &[&format!(
                "step 9 (`request`) has a `{word}` loop: the wait is one call the coordinator holds"
            )],
        );
    }
}

#[test]
fn the_outputs_step_reads_exactly_the_files_the_calls_leave() {
    // Catches: an outputs step reading a file a call does not write, or a file when the call did
    // not leave one, its program missing, its output printed to the log instead of the job's
    // outputs, and a variable its program reads not bound.
    for (key, value) in OUTPUTS_ENV {
        let line = format!("          {key}: {value}\n");
        assert_client(
            &outputs_with(&line, ""),
            &[&format!("the outputs step's `{key}` is not `{value}`")],
        );
    }
    let set_body = "          SET_BODY: ${{ steps.set-desired.outcome == 'success' && format('{0}/hkctl-set.json', runner.temp) || '/dev/null' }}\n";
    assert_client(
        &with_in(
            CLIENT,
            set_body,
            "          SET_BODY: ${{ runner.temp }}/hkctl-set.json\n",
        ),
        &[&format!(
            "the outputs step's `SET_BODY` is not `{}`",
            OUTPUTS_ENV[5].1
        )],
    );
    assert_client(
        &with_in(
            CLIENT,
            "          OUTPUTS_PROGRAM: '\"reason=converged\"'\n",
            "",
        ),
        &["the outputs step has no `OUTPUTS_PROGRAM` in its `env`"],
    );
    let end = "\"$OUTPUTS_PROGRAM\" >> \"$GITHUB_OUTPUT\"";
    let no_run = "the outputs step does not run `jq ... $OUTPUTS_PROGRAM >> $GITHUB_OUTPUT` as its one command";
    assert_client(&with_in(CLIENT, end, "\"$OUTPUTS_PROGRAM\""), &[no_run]);
    assert_client(
        &with_in(CLIENT, end, "\"$OUTPUTS_PROGRAM\" > \"$GITHUB_OUTPUT\""),
        &[no_run],
    );
    assert_client(
        &with_in(
            CLIENT,
            "          jq --null-input --raw-output \\\n",
            "          gojq --null-input --raw-output \\\n",
        ),
        &[no_run],
    );
    assert_client(
        &with_in(
            CLIENT,
            "            --rawfile set_err \"$SET_ERR\" \\\n",
            "            --arg set_err \"$SET_ERR\" \\\n",
        ),
        &["the outputs step does not pass `--rawfile set_err \"$SET_ERR\"`"],
    );
}

#[test]
fn no_script_holds_an_expression_and_the_release_inputs_are_optional() {
    // Catches: an input written into a script (where the runner would interpolate it as code)
    // rather than read through `env`, and the package digest or the index inputs required,
    // which would refuse a caller that uses the other way.
    let doc = yaml::load(&real()).expect("parses");
    for step in steps(&doc) {
        let script = entry(&step, "run")
            .and_then(Node::as_str)
            .unwrap_or_default();
        assert!(!script.contains("${{"), "{script}");
    }
    let inputs = entry(&doc, "on")
        .and_then(|on| entry(on, "workflow_call"))
        .and_then(|c| entry(c, "inputs"))
        .expect("inputs");
    for name in ["package_digest", "release_index", "release_index_digest"] {
        let input = entry(inputs, name).expect(name);
        assert_eq!(
            entry(input, "required").and_then(Node::as_str),
            Some("false"),
            "{name}"
        );
    }
}

#[test]
fn the_calls_name_the_callers_service_and_version_and_the_outputs_reach_the_caller() {
    // Catches: a call that sets or waits on another service or version than the caller's (a
    // literal, another input, the flag dropped), a variable a call names given from anything but
    // its input, and the outputs the step writes not reaching the job's or the caller's outputs.
    let service = "            --service \"$SERVICE\" \\\n";
    assert_client(
        &set_with(service, "            --service hello \\\n"),
        &["step 10 (`set-desired`) does not pass `--service \"$SERVICE\"` exactly once"],
    );
    let version = "            --version \"$VERSION\" \\\n";
    let no_version = "step 11 (`wait`) does not pass `--version \"$VERSION\"` exactly once";
    assert_client(&with_in(CLIENT, version, ""), &[no_version]);
    assert_client(
        &with_in(CLIENT, version, "            --version \"$OTHER\" \\\n"),
        &[no_version],
    );
    assert_client(
        &set_with(
            "          SERVICE: ${{ inputs.service }}\n",
            "          SERVICE: ${{ inputs.version }}\n",
        ),
        &["step 10 (`set-desired`)'s `SERVICE` is not `${{ inputs.service }}`"],
    );
    assert_client(
        &set_with("          AUDIENCE: ${{ inputs.audience }}\n", ""),
        &["step 10 (`set-desired`)'s `AUDIENCE` is not `${{ inputs.audience }}`"],
    );
    let wait_version = "          VERSION: ${{ inputs.version }}\n          COORDINATOR_URL";
    assert_client(
        &with_in(
            CLIENT,
            wait_version,
            "          VERSION: ${{ inputs.service }}\n          COORDINATOR_URL",
        ),
        &["step 11 (`wait`)'s `VERSION` is not `${{ inputs.version }}`"],
    );
    assert_client(
        &with_in(
            CLIENT,
            "      reason: ${{ steps.outputs.outputs.reason }}\n",
            "      reason: ${{ steps.set-desired.outputs.reason }}\n",
        ),
        &["job set-desired: the output `reason` is not `${{ steps.outputs.outputs.reason }}`"],
    );
    assert_client(
        &with_in(
            CLIENT,
            "      converged: ${{ steps.outputs.outputs.converged || 'false' }}\n",
            "      converged: ${{ steps.outputs.outputs.converged }}\n",
        ),
        &[
            "job set-desired: the output `converged` is not `${{ steps.outputs.outputs.converged || 'false' }}`",
        ],
    );
    assert_client(
        &with_in(
            CLIENT,
            "        value: ${{ jobs.set-desired.outputs.desired_version }}\n",
            "        value: ${{ inputs.version }}\n",
        ),
        &[
            "the output `desired_version` of `workflow_call` is not `${{ jobs.set-desired.outputs.desired_version }}`",
        ],
    );
}

/// The steps of the one job of `doc`.
fn steps(doc: &Node) -> Vec<Node> {
    let job = entry(doc, "jobs")
        .and_then(|jobs| entry(jobs, "set-desired"))
        .expect("the job");
    match entry(job, "steps").map(|s| &s.value) {
        Some(Value::Seq(steps)) => steps.clone(),
        _ => panic!("the job has no steps"),
    }
}

#[test]
fn the_request_takes_each_part_of_the_release_from_its_own_input() {
    // Catches: a part of the release (the targets above all) left out of the request with no
    // error: its variable not passed to the program, passed under another name, or read from
    // another input or from nothing, so that a caller's targets or index never reach the
    // coordinator.
    // The request step's `env`, whole: VERSION is in the call steps' too.
    let block: String = REQUEST_ARGS
        .iter()
        .map(|(_, var, want)| format!("          {var}: {want}\n"))
        .collect();
    let block = format!("      - id: request\n        env:\n{block}");
    // Its command line, whole: the outputs step passes `--arg version` too.
    let args: String = REQUEST_ARGS
        .iter()
        .map(|(name, var, _)| format!(" --arg {name} \"${var}\""))
        .collect();
    let line = format!("        run: jq --null-input{args} '{{}}'");
    for (name, var, want) in REQUEST_ARGS {
        let env = format!("          {var}: {want}\n");
        let arg = format!(" --arg {name} \"${var}\"");
        let not_env = format!("the request step's `{var}` is not `{want}`");
        let not_arg = format!("the request step does not pass `--arg {name} \"${var}\"`");
        let other = format!("          {var}: ${{{{ inputs.service }}}}\n");
        assert_client(
            &with_in(CLIENT, &block, &block.replace(&env, "")),
            &[&not_env],
        );
        assert_client(
            &with_in(CLIENT, &block, &block.replace(&env, &other)),
            &[&not_env],
        );
        assert_client(
            &with_in(CLIENT, &line, &line.replace(&arg, "")),
            &[&not_arg],
        );
        assert_client(
            &with_in(
                CLIENT,
                &line,
                &line.replace(&arg, &format!(" --arg {name}_text \"${var}\"")),
            ),
            &[&not_arg],
        );
        assert_client(
            &with_in(
                CLIENT,
                &line,
                &line.replace(&arg, &format!(" --arg {name} \"$OTHER\"")),
            ),
            &[&not_arg],
        );
    }
}
