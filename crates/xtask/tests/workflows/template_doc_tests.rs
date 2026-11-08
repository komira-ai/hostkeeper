//! [`template_doc`](super::template_doc) on the real `docs/deploy/service-caller.md`: its
//! template keeps every rule, and each case changes the page in one place and reads the exact
//! problems the lint reports.

use std::path::Path;

use super::callers::{CLIENT_COMMIT, CLIENT_RELEASE, TAILNET_TAG};
use super::template_doc::{DOC, HEADING, problems, template};
use super::with_in;

fn real_doc() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(DOC),
    )
    .expect("read docs/deploy/service-caller.md")
}

/// The template's `uses:` of the CI client, as the page has it.
fn client_uses() -> String {
    format!("komira-ai/hostkeeper/.github/workflows/set-desired.yml@{CLIENT_COMMIT}")
}

/// The line, counted from 1 in the template of `doc`, that holds `needle`.
fn line_of(doc: &str, needle: &str) -> usize {
    let text = template(doc).expect("the page has its template");
    text.lines()
        .position(|l| l.contains(needle))
        .map(|i| i + 1)
        .unwrap_or_else(|| panic!("{needle:?} is not in the template"))
}

/// A problem of the template, as [`problems`] names it.
fn of(problem: &str) -> String {
    format!("{DOC}, the template: {problem}")
}

/// The problems of the real page with the one occurrence of `from` replaced by `to`.
#[track_caller]
fn changed(from: &str, to: &str) -> Vec<String> {
    problems(&with_in(&real_doc(), from, to))
}

/// The problems a `set-desired` pinned to `uses` gets, whose line in the page is `line`: the
/// shared pin rule (only when `unpinned`) and the CI client's revision.
fn pin_problems(uses: &str, line: usize, unpinned: bool) -> Vec<String> {
    let mut want = Vec::new();
    if unpinned {
        want.push(of(&format!(
            "line {line}: `uses: {uses}` is not pinned to a 40-digit lowercase commit SHA"
        )));
    }
    want.push(of(&format!(
        "job set-desired must pin set-desired.yml at the CI client's revision {CLIENT_COMMIT}, not `{uses}`"
    )));
    want
}

#[test]
fn a_template_pinned_to_a_branch_or_a_tag_is_refused() {
    // Catches: the template calling the CI client at a ref a push or a moved tag changes, so a
    // service copying it runs code no review saw with its OIDC token.
    let doc = real_doc();
    let line = line_of(&doc, "set-desired.yml@");
    for git_ref in ["main", "v0.1.184", "refs/heads/main"] {
        let uses = format!("komira-ai/hostkeeper/.github/workflows/set-desired.yml@{git_ref}");
        assert_eq!(
            changed(&client_uses(), &uses),
            pin_problems(&uses, line, true),
            "{git_ref}"
        );
    }
}

#[test]
fn the_template_of_the_page_keeps_every_rule() {
    // Catches: the template drifting from the CI client's revision, the caller rules or the
    // shared workflow rules, as the lint of every workflow would catch it in hello-desired.yml.
    assert_eq!(problems(&real_doc()), Vec::<String>::new());
}

#[test]
fn a_template_pinned_to_another_commit_is_refused() {
    // Catches: a full commit that is not the CI client's revision, which the shared pin rule
    // takes; and no SHA at all.
    let doc = real_doc();
    let line = line_of(&doc, "set-desired.yml@");
    let other = "komira-ai/hostkeeper/.github/workflows/set-desired.yml@f04696da7e2bdcbaf9112450ab651a1ab24e61bf";
    assert_eq!(
        changed(&client_uses(), other),
        pin_problems(other, line, false)
    );
    let bare = "komira-ai/hostkeeper/.github/workflows/set-desired.yml";
    assert_eq!(
        changed(&client_uses(), bare),
        [
            of(&format!(
                "line {line}: `uses: {bare}` is not pinned (owner/repo@<40 hex digits>)"
            )),
            of(&format!(
                "job set-desired must pin set-desired.yml at the CI client's revision {CLIENT_COMMIT}, not `{bare}`"
            )),
        ]
    );
}

#[test]
fn a_template_with_another_release_than_the_client_is_refused() {
    // Catches: the version comment, `hostkeeper_version` or `hostkeeper_commit` left behind when
    // the pin moves, so the hkctl downloaded is not the one the pin names.
    let release = format!("v{CLIENT_RELEASE}");
    assert_eq!(
        changed(&format!("# {release}\n"), "# v0.1.183\n"),
        [of(&format!(
            "job set-desired must name the CI client's release `# {release}` after its `uses:`, not `# v0.1.183`"
        ))]
    );
    assert_eq!(
        changed(
            &format!("hostkeeper_version: {CLIENT_RELEASE}"),
            "hostkeeper_version: 0.1.183"
        ),
        [of(&format!(
            "job set-desired must pass `hostkeeper_version: {CLIENT_RELEASE}`, the CI client's revision"
        ))]
    );
    assert_eq!(
        changed(
            &format!("hostkeeper_commit: {CLIENT_COMMIT}"),
            "hostkeeper_commit: f04696da7e2bdcbaf9112450ab651a1ab24e61bf"
        ),
        [of(&format!(
            "job set-desired must pass `hostkeeper_commit: {CLIENT_COMMIT}`, the CI client's revision"
        ))]
    );
}

#[test]
fn a_template_with_another_tailnet_tag_is_refused() {
    // Catches: a tag written into the template, a fallback to the CI client's default, or no
    // tag, instead of the repository variable that holds tag:ci.
    let line = format!("      tailnet_tag: {TAILNET_TAG}\n");
    let want = [of(&format!(
        "job set-desired must pass `tailnet_tag: {TAILNET_TAG}`, the tag the tailnet's trust credential issues"
    ))];
    for to in [
        "      tailnet_tag: tag:ci\n",
        "      tailnet_tag: ${{ vars.HOSTKEEPER_TAILNET_TAG || 'tag:hk-ci' }}\n",
        "",
    ] {
        assert_eq!(changed(&line, to), want, "{to:?}");
    }
}

#[test]
fn a_template_with_other_rights_or_secrets_is_refused() {
    // Catches: a calling job that grants a write scope beside its token, drops `id-token: write`
    // (GitHub then issues no token), or hands secrets to the CI client, which takes none.
    let rights = "    permissions:\n      contents: read\n      id-token: write\n";
    assert_eq!(
        changed(
            rights,
            "    permissions:\n      contents: write\n      id-token: write\n"
        ),
        [
            of(
                "job set-desired: `contents: write` is not allowed in docs/deploy/service-caller.md"
            ),
            of("job set-desired: permissions must be exactly contents: read and id-token: write"),
        ]
    );
    assert_eq!(
        changed(rights, "    permissions:\n      contents: read\n"),
        [of(
            "job set-desired: permissions must be exactly contents: read and id-token: write"
        )]
    );
    assert_eq!(
        changed(
            "    with:\n      service: my-service\n",
            "    secrets:\n      token: x\n    with:\n      service: my-service\n"
        ),
        [of("job set-desired passes secrets to set-desired.yml")]
    );
}

#[test]
fn a_template_that_runs_on_another_trigger_or_branch_is_refused() {
    // Catches: a template that sets a version on a push or a schedule, which the allow-list entry
    // of the checklist (`workflow_dispatch` only) refuses, or a job a dispatch from another
    // branch runs.
    assert_eq!(
        changed(
            "on:\n  workflow_dispatch:\n",
            "on:\n  push:\n  workflow_dispatch:\n"
        ),
        [of(
            "docs/deploy/service-caller.md: on must be workflow_dispatch only"
        )]
    );
    let gate = "    if: ${{ github.ref == 'refs/heads/main' }}\n    runs-on: ubuntu-24.04\n";
    assert_eq!(
        changed(gate, "    runs-on: ubuntu-24.04\n"),
        [of(
            "docs/deploy/service-caller.md: job release needs the exact `if: ${{ github.ref == 'refs/heads/main' }}`"
        )]
    );
    let caller_gate = "    needs: [release]\n    if: ${{ github.ref == 'refs/heads/main' }}\n";
    assert_eq!(
        changed(
            caller_gate,
            "    needs: [release]\n    if: ${{ always() }}\n"
        ),
        [
            of("job set-desired needs the exact `if: ${{ github.ref == 'refs/heads/main' }}`"),
            of(
                "docs/deploy/service-caller.md: job set-desired needs the exact `if: ${{ github.ref == 'refs/heads/main' }}`"
            ),
        ]
    );
}

#[test]
fn a_template_whose_version_input_is_not_a_required_string_is_refused() {
    // Catches: a dispatch with no version, or one of another type, reaching the release job.
    let want = [of(
        "docs/deploy/service-caller.md: input version must be a required string",
    )];
    assert_eq!(
        changed(
            "        type: string\n        required: true\n",
            "        type: string\n        required: false\n"
        ),
        want
    );
    assert_eq!(
        changed(
            "        type: string\n        required: true\n",
            "        type: number\n        required: true\n"
        ),
        want
    );
}

#[test]
fn a_template_for_a_real_service_name_is_refused() {
    // Catches: the public template naming a service of its own instead of the placeholder.
    assert_eq!(
        changed("      service: my-service\n", "      service: tierstore\n"),
        [of(
            "docs/deploy/service-caller.md: job set-desired must pass `service: my-service`"
        )]
    );
}

#[test]
fn a_template_that_does_not_set_the_release_jobs_targets_and_index_is_refused() {
    // Catches: targets or an index written into the caller, taken from another job, or dropped,
    // and one package for every platform instead of the targets.
    assert_eq!(
        changed(
            "      targets: ${{ needs.release.outputs.targets }}\n",
            "      targets: '{}'\n"
        ),
        [of(
            "docs/deploy/service-caller.md: job set-desired must pass `targets: ${{ needs.release.outputs.targets }}`"
        )]
    );
    assert_eq!(
        changed(
            "      release_index_digest: ${{ needs.release.outputs.release_index_digest }}\n",
            ""
        ),
        [of(
            "docs/deploy/service-caller.md: job set-desired must pass `release_index_digest: ${{ needs.release.outputs.release_index_digest }}`"
        )]
    );
    assert_eq!(
        changed(
            "      service: my-service\n",
            "      service: my-service\n      package_digest: ${{ needs.release.outputs.digest }}\n"
        ),
        [of(
            "docs/deploy/service-caller.md: job set-desired passes package_digest; the targets name each platform's package"
        )]
    );
}

#[test]
fn a_template_whose_release_job_does_not_write_the_targets_is_refused() {
    // Catches: an output of the release job that is not its step's, and a step `targets` that
    // does not run the targets command, so the targets come from nothing that checked them.
    let outputs = "      targets: ${{ steps.targets.outputs.targets }}\n    env:";
    let got = changed(outputs, "      targets: '{}'\n    env:");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        got[0].starts_with(&of(
            "docs/deploy/service-caller.md: job release outputs are "
        )),
        "{got:?}"
    );
    assert_eq!(
        changed(
            "        run: cargo xtask release targets --version",
            "        run: echo targets={} >> \"$GITHUB_OUTPUT\" # --version"
        ),
        [of(
            "docs/deploy/service-caller.md: job release needs a step `targets` that runs `cargo xtask release targets ...`"
        )]
    );
}

#[test]
fn a_template_that_breaks_a_rule_of_every_workflow_is_refused() {
    // Catches: the template held to the caller rules alone: an action by tag, a token right at
    // the top, and a download that is not retried pass them, and each is a rule of every
    // workflow of this repository.
    let doc = real_doc();
    let checkout = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1";
    assert_eq!(
        changed(checkout, "actions/checkout@v7"),
        [of(&format!(
            "line {}: `uses: actions/checkout@v7` is not pinned to a 40-digit lowercase commit SHA",
            line_of(&doc, checkout)
        ))]
    );
    assert_eq!(
        changed("permissions: {}\n", "permissions:\n  contents: read\n"),
        [of("no top-level `permissions: {}`")]
    );
    assert_eq!(
        changed(" --retry-all-errors", ""),
        [of(&format!(
            "line {}: a curl lacks `--retry-all-errors`",
            line_of(&doc, "run: curl ")
        ))]
    );
}

#[test]
fn a_page_without_its_one_template_is_refused() {
    // Catches: the lint passing a page whose template was renamed, removed, doubled or left
    // open, since it would then check nothing.
    let doc = real_doc();
    let section = format!("\n{HEADING}\n");
    assert_eq!(
        problems(&with_in(&doc, &section, "\n## A template\n")),
        [format!("{DOC} has no section `{HEADING}`")]
    );
    let one = format!("{DOC}: the section `{HEADING}` must hold exactly one ```yaml block");
    assert_eq!(
        problems(&with_in(&doc, "```yaml\n", "```yml\n")),
        std::slice::from_ref(&one)
    );
    let doubled = with_in(
        &doc,
        "\nWhat the lint holds",
        "\n```yaml\nname: x\n```\n\nWhat the lint holds",
    );
    assert_eq!(problems(&doubled), [one]);
    let start = doc.find("```yaml\n").expect("the block opens");
    let open = format!("{}```yaml\nname: x\n", &doc[..start]);
    assert_eq!(
        problems(&open),
        [format!("{DOC}: the template's ```yaml block is not closed")]
    );
}

#[test]
fn a_template_block_ends_at_its_fence_and_the_section_at_the_next_heading() {
    // Catches: a block read past its closing fence into the page's prose, and a section read
    // into the next one, whose yaml block would then count as a second template.
    let doc = real_doc();
    let text = template(&doc).expect("the template");
    assert!(text.starts_with("# Sets the desired version"), "{text}");
    assert!(
        text.ends_with("tailnet_tag: ${{ vars.HOSTKEEPER_TAILNET_TAG }}\n"),
        "{text}"
    );
    let after = with_in(
        &doc,
        "\n## The checklist\n",
        "\n## The checklist\n\n```yaml\nname: x\n```\n",
    );
    assert_eq!(problems(&after), Vec::<String>::new());
}
