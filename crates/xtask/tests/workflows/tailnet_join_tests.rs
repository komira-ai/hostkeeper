//! The tailnet and the OIDC token of hostkeeper's own deploy: the job `targets` of `release.yml`
//! ([`super::desired`]) waits for no release channel, so it joins no tailnet and holds no
//! `id-token: write`, and every caller of the CI client joins the tailnet when the client ID is
//! set ([`super::callers::JOIN_FROM_VARS`]). Each rule is shown to refuse its defect by putting
//! the defect into the repository's file and reading the exact problems the lint reports. What
//! the job does in a run is in `targets_run_tests.rs`.

use super::callers::{CALLERS, HELLO_FILE, JOIN_FROM_VARS};
use super::desired::{JOIN_IF, TARGETS_STEPS};
use super::lint::{ID_TOKEN_ALLOWED, entry, lint};
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

/// The problem a changed step `n` (from 1) of the job targets is reported with.
fn step_problem(n: usize, got: &str) -> String {
    format!(
        "release.yml: job targets step {n} is `{got}`, not `{}`",
        TARGETS_STEPS[n - 1]
    )
}

/// The step of the job targets that writes the targets, which the inserted steps go before.
const TARGETS_STEP: &str = "      - name: The targets of the release\n";

/// The steps the job channel had before the targets step, from the join to the index outputs:
/// the join of the tailnet, the wait for the channel, the byte comparison and the index written
/// out.
const CHANNEL_WAIT: &str = r#"      - name: Join the tailnet
        id: tailnet
        if: ${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID != '' }}
        uses: tailscale/github-action@d1b6cd204f8dceda5b3eaad7f1f767be390056cd # v4.2.0
        with:
          oauth-client-id: ${{ vars.HOSTKEEPER_TAILNET_CLIENT_ID }}
          audience: ${{ vars.HOSTKEEPER_TAILNET_AUDIENCE }}
          tags: ${{ vars.HOSTKEEPER_TAILNET_TAG }}
          version: 1.94.2
          sha256sum: c6f99a5d774c7783b56902188d69e9756fc3dddfb08ac6be4cb2585f3fecdc32
          use-cache: false
      - name: Wait until the release channel serves the version's index
        env:
          RELEASE_CHANNEL: ${{ vars.HOSTKEEPER_RELEASE_CHANNEL }}
          VERSION: ${{ steps.newest.outputs.version }}
        run: curl --proto =https --proto-redir =https --tlsv1.2 --fail --silent --show-error --max-time 30 --retry 45 --retry-delay 40 --retry-max-time 1800 --retry-all-errors --output "$RUNNER_TEMP/served-index.json" "$RELEASE_CHANNEL/hostkeeper/$VERSION/index.json"
      - name: The channel serves the published index, byte for byte
        run: cmp dist/index.json "$RUNNER_TEMP/served-index.json"
      - name: The release index and its SHA-256
        id: index
        run: printf 'release_index=%s\nrelease_index_digest=%s\n' "$(< dist/index.json)" "$(sha256sum dist/index.json | cut -d ' ' -f 1)" >> "$GITHUB_OUTPUT"
"#;

#[test]
fn no_job_of_release_yml_holds_id_token_and_the_targets_job_reads_the_repository_alone() {
    // Catches: the OIDC scope the old channel job's join held left on the targets job, which
    // joins nothing, or granted to another job of release.yml, such as `verify`, which runs the
    // downloaded binaries.
    assert!(
        ID_TOKEN_ALLOWED
            .iter()
            .all(|(file, _)| *file != "release.yml")
    );
    let release = workflow("release.yml");
    for job in ["  verify:\n", "  targets:\n"] {
        let at = release.find(job).expect("release.yml has the job");
        let perms = release[at..]
            .find("      contents: read\n")
            .map(|p| at + p)
            .expect("the job reads the repository");
        let mut text = release.clone();
        text.insert_str(perms, "      id-token: write\n");
        let name = job.trim().trim_end_matches(':');
        let mut want = vec![format!(
            "job {name}: `id-token: write` is not allowed in release.yml"
        )];
        if name == "targets" {
            want.push("release.yml: job targets must hold exactly `contents: read`".to_owned());
        }
        assert_eq!(lint("release.yml", &text), want, "{name}");
    }
}

#[test]
fn a_targets_job_that_joins_the_tailnet_and_waits_for_the_channel_is_refused() {
    // Catches: the job as it was before A24 (packages-native.md section 10.5): a tailnet join,
    // a wait for the release channel, the comparison of the served index and the index written
    // out, any of which would hold a release that no host fetches from the channel, or fail it.
    let at = TARGETS_STEPS
        .iter()
        .position(|s| s.starts_with("run cargo xtask release targets "))
        .expect("the targets step");
    let got = release_problems(TARGETS_STEP, &format!("{CHANNEL_WAIT}{TARGETS_STEP}"));
    let inserted = 4;
    let mut want = vec![format!(
        "release.yml: job targets step {} has `if`, which a step of it may not",
        at + 1
    )];
    want.push(format!(
        "release.yml: job targets has {} steps, not the {} of desired::TARGETS_STEPS",
        TARGETS_STEPS.len() + inserted,
        TARGETS_STEPS.len()
    ));
    let shapes = [
        format!(
            "if {JOIN_IF} uses tailscale/github-action with audience=${{{{ vars.HOSTKEEPER_TAILNET_AUDIENCE }}}} with oauth-client-id=${{{{ vars.HOSTKEEPER_TAILNET_CLIENT_ID }}}} with sha256sum=c6f99a5d774c7783b56902188d69e9756fc3dddfb08ac6be4cb2585f3fecdc32 with tags=${{{{ vars.HOSTKEEPER_TAILNET_TAG }}}} with use-cache=false with version=1.94.2"
        ),
        r#"run curl --proto =https --proto-redir =https --tlsv1.2 --fail --silent --show-error --max-time 30 --retry 45 --retry-delay 40 --retry-max-time 1800 --retry-all-errors --output "$RUNNER_TEMP/served-index.json" "$RELEASE_CHANNEL/hostkeeper/$VERSION/index.json" env RELEASE_CHANNEL=${{ vars.HOSTKEEPER_RELEASE_CHANNEL }} env VERSION=${{ steps.newest.outputs.version }}"#.to_owned(),
    ];
    // The join and the wait take the places of the targets step and the step after it.
    for (n, got) in shapes.iter().enumerate() {
        want.push(step_problem(at + 1 + n, got));
    }
    assert_eq!(got, want);
    // The committed job has no step that reads the channel or an index.
    let release = yaml::load(&workflow("release.yml")).expect("release.yml parses");
    for step in steps(&release, "targets") {
        let run = text(step, "run").unwrap_or_default();
        assert!(
            !run.contains("index.json") && !run.contains("CHANNEL"),
            "{run}"
        );
        assert!(text(step, "uses").is_none_or(|u| !u.starts_with("tailscale/")));
    }
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
fn a_targets_step_that_writes_an_expression_into_its_script_is_refused() {
    // Catches: the version (or any value) written into a `run:` as `${{ ... }}`, where a value
    // holding shell syntax runs as part of the script, in place of the step's `env:` that the
    // command reads as its flag `--version "$VERSION"`.
    let from = "        env:\n          VERSION: ${{ steps.newest.outputs.version }}\n        run: cargo xtask release targets --version \"$VERSION\" --dir";
    let inline = "        run: cargo xtask release targets --version '${{ steps.newest.outputs.version }}' --dir";
    let at = TARGETS_STEPS
        .iter()
        .position(|s| s.starts_with("run cargo xtask release targets "))
        .expect("the targets step");
    assert_eq!(
        release_problems(from, inline),
        [
            step_problem(
                at + 1,
                "run cargo xtask release targets --version '${{ steps.newest.outputs.version }}' --dir dist --repo \"$GITHUB_REPOSITORY\" --output \"$GITHUB_OUTPUT\"",
            ),
            format!(
                "release.yml: job targets step {} writes an expression into its `run:`; pass the value through `env:`",
                at + 1
            ),
        ]
    );
    // No run of the committed job writes one.
    let release = yaml::load(&workflow("release.yml")).expect("release.yml parses");
    for step in steps(&release, "targets") {
        assert!(!text(step, "run").unwrap_or_default().contains("${{"));
    }
}
