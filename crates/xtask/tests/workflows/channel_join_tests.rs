//! The tailnet join of the job `channel` of `release.yml` ([`super::desired`]) and of every
//! caller of the CI client ([`super::callers::JOIN_FROM_VARS`]): the repository's files pass, the
//! channel job's join is the CI client's join (the same action at the same commit, the same
//! Tailscale tarball), and each rule is shown to refuse its defect by putting the defect into the
//! repository's file and reading the exact problems the lint reports. What the join does in a run
//! of the job is in `channel_run_tests.rs`.

use super::callers::{CALLERS, HELLO_FILE, JOIN_FROM_VARS};
use super::desired::{
    CHANNEL_STEPS, HOST_AT, HOST_STEP, JOIN_AT, JOIN_IF, JOIN_USES, STEP_KEYS, WAIT_AT,
};
use super::lint::{ID_TOKEN_ALLOWED, entries, entry, lint};
use super::set_desired;
use super::yaml::{self, Node, Value};
use super::{with_in, workflows};

/// The repository's workflow `file`.
fn workflow(file: &str) -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == file)
        .map(|(_, text)| text)
        .unwrap_or_else(|| panic!("{file} exists"))
}

/// The steps of the job `job` of the workflow `doc`.
fn steps<'a>(doc: &'a Node, job: &str) -> &'a [Node] {
    match entry(doc, "jobs")
        .and_then(|jobs| entry(jobs, job))
        .and_then(|j| entry(j, "steps"))
        .map(|s| &s.value)
    {
        Some(Value::Seq(items)) => items,
        _ => panic!("the job {job} has steps"),
    }
}

/// The text of the scalar `key` of `node`.
fn text<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    entry(node, key).and_then(Node::as_str)
}

/// The problems of the repository's release.yml with the one occurrence of `from` replaced by
/// `to`.
fn release_problems(from: &str, to: &str) -> Vec<String> {
    lint("release.yml", &with_in(&workflow("release.yml"), from, to))
}

/// The join step of the channel job in the repository's release.yml, from its first line to the
/// comment before the wait.
fn join_text(text: &str) -> &str {
    let start = text
        .find("      - name: Join the tailnet\n")
        .expect("the channel job has the join");
    let end = start
        + text[start..]
            .find("      # curl's own retries")
            .expect("the wait follows the join");
    &text[start..end]
}

/// The problem a changed step `n` (from 1) of the job channel is reported with.
fn step_problem(n: usize, got: &str) -> String {
    format!(
        "release.yml: job channel step {n} is `{got}`, not `{}`",
        CHANNEL_STEPS[n - 1]
    )
}

#[test]
fn the_channel_job_joins_the_tailnet_right_before_the_wait_only_with_a_client_id() {
    // Catches: the rules' own indexes drifting from the steps they name: the host step, the join
    // right after it, then the wait, and the join's `if:` written as anything but the client ID
    // being set.
    assert!(CHANNEL_STEPS[HOST_AT].starts_with(&format!("if {JOIN_IF} run ")));
    assert!(
        CHANNEL_STEPS[JOIN_AT].starts_with(&format!("if {JOIN_IF} uses tailscale/github-action "))
    );
    assert!(CHANNEL_STEPS[WAIT_AT].starts_with("run curl "));
    assert!(
        CHANNEL_STEPS[WAIT_AT]
            .contains("env RELEASE_CHANNEL=${{ vars.HOSTKEEPER_RELEASE_CHANNEL }}")
    );
    assert!(CHANNEL_STEPS[JOIN_AT].contains(&format!(
        "with ping=${{{{ steps.{HOST_STEP}.outputs.hostname }}}}"
    )));
    assert_eq!(JOIN_IF, "${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID != '' }}");
    // No other step of the job has an `if:`, and the key is not one a step may have otherwise.
    let guarded: Vec<usize> = (0..CHANNEL_STEPS.len())
        .filter(|&at| CHANNEL_STEPS[at].starts_with("if "))
        .collect();
    assert_eq!(guarded, [HOST_AT, JOIN_AT]);
    assert!(!STEP_KEYS.contains(&"if"));
    assert!(ID_TOKEN_ALLOWED.contains(&("release.yml", "channel")));
}

#[test]
fn the_channel_join_is_the_ci_clients_join() {
    // Catches: the channel job's join moved to another commit of the action, or another
    // Tailscale version or tarball digest, than the CI client's join, so the two joins of one
    // release run differ.
    let client = yaml::load(&workflow(set_desired::FILE)).expect("set-desired.yml parses");
    let client_join = steps(&client, set_desired::JOB)
        .iter()
        .find(|s| text(s, "id") == Some(set_desired::JOIN_STEP))
        .expect("the CI client's join");
    let release = yaml::load(&workflow("release.yml")).expect("release.yml parses");
    let channel_join = &steps(&release, "channel")[JOIN_AT];
    assert_eq!(text(client_join, "uses"), Some(JOIN_USES));
    assert_eq!(text(channel_join, "uses"), Some(JOIN_USES));
    let with_of = |step: &Node, key: &str| {
        entry(step, "with")
            .and_then(|w| text(w, key))
            .map(str::to_owned)
    };
    for key in ["version", "sha256sum", "use-cache"] {
        assert!(with_of(client_join, key).is_some(), "{key}");
        assert_eq!(
            with_of(channel_join, key),
            with_of(client_join, key),
            "{key}"
        );
    }
    // The tag too, through the one repository variable both pass.
    let tags: Vec<&str> = entries(entry(channel_join, "with").expect("with"))
        .iter()
        .filter(|(k, _)| k.as_str() == Some("tags"))
        .filter_map(|(_, v)| v.as_str())
        .collect();
    assert_eq!(tags, ["${{ vars.HOSTKEEPER_TAILNET_TAG }}"]);
}

#[test]
fn a_channel_wait_without_the_join_is_refused() {
    // Catches: the channel job as it was before the join, whose wait runs on a runner off the
    // tailnet when the client ID is set; and the join moved after the wait.
    let release = workflow("release.yml");
    let join = join_text(&release).to_owned();
    let mut want = vec![
        "release.yml: job channel has 11 steps, not the 12 of desired::CHANNEL_STEPS".to_owned(),
    ];
    let shapes = CHANNEL_STEPS;
    // Without the join every later step moves up by one: step n holds the shape of step n + 1.
    for (n, got) in shapes.iter().enumerate().skip(JOIN_AT + 1) {
        want.push(step_problem(n, got));
    }
    want.push(format!(
        "release.yml: job channel step {} does not use `{JOIN_USES}`, the CI client's join",
        JOIN_AT + 1
    ));
    assert_eq!(release_problems(&join, ""), want);
    let compare = "      - name: The channel serves the published index, byte for byte\n";
    let moved = with_in(
        &with_in(&release, &join, ""),
        compare,
        &format!("{join}{compare}"),
    );
    assert_eq!(
        lint("release.yml", &moved),
        [
            format!(
                "release.yml: job channel step {} has `if`, which a step of it may not",
                WAIT_AT + 1
            ),
            step_problem(JOIN_AT + 1, shapes[WAIT_AT]),
            step_problem(WAIT_AT + 1, shapes[JOIN_AT]),
            format!(
                "release.yml: job channel step {} does not use `{JOIN_USES}`, the CI client's join",
                JOIN_AT + 1
            ),
        ]
    );
}

#[test]
fn a_join_that_runs_on_another_condition_is_refused() {
    // Catches: a join that runs with no client ID (and fails the job where the CI client would
    // not join), one that never runs, and one keyed on another variable.
    let line = format!("        if: {JOIN_IF}\n        uses: tailscale");
    for to in [
        "        uses: tailscale".to_owned(),
        "        if: ${{ false }}\n        uses: tailscale".to_owned(),
        "        if: ${{ vars.HOSTKEEPER_TAILNET_COORDINATOR != '' }}\n        uses: tailscale"
            .to_owned(),
    ] {
        let got = release_problems(&line, &to);
        assert_eq!(got.len(), 1, "{to:?}: {got:?}");
        assert!(
            got[0].starts_with(&format!(
                "release.yml: job channel step {} is `",
                JOIN_AT + 1
            )),
            "{got:?}"
        );
    }
}

#[test]
fn a_join_at_another_commit_or_of_another_action_is_refused() {
    // Catches: the join's action moved off the CI client's commit (the shape alone strips the
    // commit), and a fork of the action.
    let pin = "uses: tailscale/github-action@d1b6cd204f8dceda5b3eaad7f1f767be390056cd # v4.2.0\n        with:\n          oauth-client-id: ${{ vars.";
    let other = "uses: tailscale/github-action@0000000000000000000000000000000000000000 # v4.2.0\n        with:\n          oauth-client-id: ${{ vars.";
    assert_eq!(
        release_problems(pin, other),
        [format!(
            "release.yml: job channel step {} does not use `{JOIN_USES}`, the CI client's join",
            JOIN_AT + 1
        )]
    );
    let fork = "uses: example/github-action@d1b6cd204f8dceda5b3eaad7f1f767be390056cd # v4.2.0\n        with:\n          oauth-client-id: ${{ vars.";
    let got = release_problems(pin, fork);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got[0].contains(
        "is `if ${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID != '' }} uses example/github-action "
    ));
}

#[test]
fn a_join_that_can_fail_quietly_or_takes_a_stored_credential_is_refused() {
    // Catches: a failed join the wait goes on after, and a join given an auth key or OAuth
    // secret in place of the OIDC token.
    let use_cache = "          use-cache: false\n          ping: ${{ steps.channel-host";
    assert_eq!(
        release_problems(
            "        id: tailnet\n",
            "        id: tailnet\n        continue-on-error: true\n",
        ),
        [format!(
            "release.yml: job channel step {} has `continue-on-error`, which a step of it may not",
            JOIN_AT + 1
        )]
    );
    for key in ["authkey", "oauth-secret"] {
        let to = format!(
            "          use-cache: false\n          {key}: ${{{{ secrets.TS }}}}\n          ping: ${{{{ steps.channel-host"
        );
        let got = release_problems(use_cache, &to);
        assert_eq!(got.len(), 1, "{key}: {got:?}");
        assert!(
            got[0].contains(&format!(" with {key}=${{{{ secrets.TS }}}} ")),
            "{got:?}"
        );
    }
}

#[test]
fn a_join_that_pings_another_host_or_none_is_refused() {
    // Catches: a join that does not wait for the channel's host (the wait then races the
    // tailnet's spread of the new node), or that pings the coordinator in its place; and the host
    // step's id changed so the ping names a step that wrote nothing.
    let ping = "          ping: ${{ steps.channel-host.outputs.hostname }}\n";
    for to in [
        "",
        "          ping: ${{ vars.HOSTKEEPER_TAILNET_COORDINATOR }}\n",
        "          ping: ${{ vars.HOSTKEEPER_RELEASE_CHANNEL }}\n",
    ] {
        let got = release_problems(ping, to);
        assert_eq!(got.len(), 1, "{to:?}: {got:?}");
        assert!(
            got[0].starts_with(&format!(
                "release.yml: job channel step {} is `",
                JOIN_AT + 1
            )),
            "{got:?}"
        );
    }
    assert_eq!(
        release_problems("        id: channel-host\n", "        id: host\n"),
        [format!(
            "release.yml: job channel step {} does not have `id: {HOST_STEP}`, the host the join pings",
            HOST_AT + 1
        )]
    );
}

#[test]
fn only_the_channel_job_of_release_yml_gets_id_token_for_its_join() {
    // Catches: the OIDC scope the join needs granted to another job of release.yml as well, such
    // as `verify`, which runs the downloaded binaries.
    let verify = "  verify:\n";
    let release = workflow("release.yml");
    let at = release.find(verify).expect("release.yml has verify");
    let perms = release[at..]
        .find("      contents: read\n")
        .map(|p| at + p)
        .expect("verify reads the repository");
    let mut text = release.clone();
    text.insert_str(perms, "      id-token: write\n");
    assert_eq!(
        lint("release.yml", &text),
        ["job verify: `id-token: write` is not allowed in release.yml"]
    );
}

#[test]
fn every_caller_of_the_repository_joins_the_tailnet_when_the_client_id_is_set() {
    // Catches: hello-desired.yml left without the join while the coordinator is served only on
    // the tailnet (its call fails with `no_answer`), and a caller whose join is not keyed on the
    // client ID or takes the federation from other variables.
    assert_eq!(JOIN_FROM_VARS[0], ("join_tailnet", JOIN_IF));
    for c in CALLERS {
        let text = workflow(c.file);
        for (input, want) in JOIN_FROM_VARS {
            assert_eq!(
                text.matches(&format!("      {input}: {want}\n")).count(),
                1,
                "{} {input}",
                c.file
            );
        }
    }
    let hello = workflow(HELLO_FILE);
    for (input, want) in JOIN_FROM_VARS {
        let line = format!("      {input}: {want}\n");
        let refusal = format!(
            "job set-desired must pass `{input}: {want}`, so it joins the tailnet whenever the client ID is set"
        );
        for to in [
            String::new(),
            format!("      {input}: true\n"),
            format!("      {input}: ${{{{ vars.OTHER }}}}\n"),
        ] {
            assert_eq!(
                lint(HELLO_FILE, &with_in(&hello, &line, &to)),
                std::slice::from_ref(&refusal),
                "{to:?}"
            );
        }
    }
}

#[test]
fn a_channel_step_that_writes_an_expression_into_its_script_is_refused() {
    // Catches: the channel's URL (or any value) written into a `run:` as `${{ ... }}`, where a
    // value holding shell syntax runs as part of the script, in place of the step's `env:` that
    // the command reads as its flag `--channel "$RELEASE_CHANNEL"`.
    let from = "        env:\n          RELEASE_CHANNEL: ${{ vars.HOSTKEEPER_RELEASE_CHANNEL }}\n        run: cargo xtask release channel-host --channel \"$RELEASE_CHANNEL\" --output";
    let inline = "        run: cargo xtask release channel-host --channel '${{ vars.HOSTKEEPER_RELEASE_CHANNEL }}' --output";
    let got = release_problems(from, inline);
    assert_eq!(
        got,
        [
            step_problem(
                HOST_AT + 1,
                &format!(
                    "if {JOIN_IF} run cargo xtask release channel-host --channel '${{{{ vars.HOSTKEEPER_RELEASE_CHANNEL }}}}' --output \"$GITHUB_OUTPUT\""
                ),
            ),
            format!(
                "release.yml: job channel step {} writes an expression into its `run:`; pass the value through `env:`",
                HOST_AT + 1
            ),
        ]
    );
    // No run of the committed job writes one.
    let release = yaml::load(&workflow("release.yml")).expect("release.yml parses");
    for step in steps(&release, "channel") {
        assert!(!text(step, "run").unwrap_or_default().contains("${{"));
    }
}
