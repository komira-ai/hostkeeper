//! The rules of [`set_desired_tag`](super::set_desired_tag): the tag check of the real
//! `set-desired.yml`, run with jaq on the tags a caller may give and those it may not, and the
//! lint on [`CLIENT`], each case adding one defect to it and reading the exact problems.

use super::lint::entry;
use super::set_desired::{JOB, JOIN_STEP};
use super::set_desired_outputs_tests::{Ended, run};
use super::set_desired_tag::{TAG_DEFAULT, TAG_INPUT, TAG_STEP, TAG_VALUE, tag_program};
use super::set_desired_tests::{CLIENT, JOIN_BLOCK, assert_client, moved, real, step_ids};
use super::with_in;
use super::yaml::{self, Node};

/// The check step of [`CLIENT`], whole.
const TAG_BLOCK: &str = "      - id: tailnet-tag
        env:
          TAILNET_TAG: ${{ inputs.tailnet_tag }}
        run: jq --null-input --arg tag \"$TAILNET_TAG\" 'empty'
";

/// The input of [`CLIENT`] that names the tag.
const TAG_INPUT_BLOCK: &str =
    "      tailnet_tag:\n        type: string\n        default: tag:hk-ci\n";

/// The error the real check ends with on a tag it refuses.
const REFUSED: &str = "tailnet_tag is not tag: followed by a Tailscale tag name (a lower-case letter, then lower-case letters, digits and -)";

/// The problem of an input that is missing, retyped or given another default.
const NO_INPUT: &str = "no input `tailnet_tag` of `type: string` with `default: tag:hk-ci`";

/// The check of the real workflow, run on `tag` as the step's `--arg tag`.
fn check(tag: &str) -> (Vec<String>, Ended) {
    let doc = yaml::load(&real()).expect("the workflow parses");
    let program = tag_program(&doc).expect("the tag step has its program");
    run(program, &[("tag", tag.to_owned())])
}

#[test]
fn the_check_accepts_a_tailscale_tag() {
    // The default, the tailnet's existing CI tag, and the shortest and longest shapes of a name:
    // one letter, and letters, digits and dashes after it (a dash last included).
    for tag in [
        TAG_DEFAULT,
        "tag:ci",
        "tag:a",
        "tag:ci-runner-2",
        "tag:a-",
        "tag:x0",
    ] {
        assert_eq!(check(tag), (vec![], Ended::Done), "{tag:?}");
    }
}

#[test]
fn the_check_refuses_every_tag_that_is_not_a_tailscale_tag() {
    // Catches a check that accepts an empty tag, a name without its `tag:` prefix, upper-case
    // letters, a name that starts with a digit or a dash, characters outside letters, digits and
    // `-`, two tags in one value (which the action would pass on as two), and a tag with a line
    // break or a space before or after it (`$` of a regex matches before a final newline; the
    // check's `\z` does not).
    for tag in [
        "",
        "tag:",
        "hk-ci",
        "TAG:ci",
        "tag:CI",
        "tag:Ci",
        "tag:1ci",
        "tag:-ci",
        "tag:ci_x",
        "tag:ci.x",
        "tag:hk:ci",
        "tag:ci,tag:admin",
        "tag:ci tag:admin",
        "tag:ci\n",
        "\ntag:ci",
        "tag:ci\ntag:admin",
        " tag:ci",
        "tag:ci ",
        "tag:cé",
    ] {
        assert_eq!(
            check(tag),
            (vec![], Ended::Failed(REFUSED.to_owned())),
            "{tag:?}"
        );
    }
}

#[test]
fn the_tag_defaults_to_hk_ci_and_the_join_passes_it() {
    // Catches: the input missing, required, of another type or with another default (every
    // caller that names no tag would join as another tag, or not at all); and the join passing
    // a fixed tag or another input instead of the caller's.
    let doc = yaml::load(&real()).expect("parses");
    let input = entry(&doc, "on")
        .and_then(|on| entry(on, "workflow_call"))
        .and_then(|c| entry(c, "inputs"))
        .and_then(|i| entry(i, TAG_INPUT))
        .expect("the tag input");
    let field = |key: &str| entry(input, key).and_then(Node::as_str);
    assert_eq!(
        (field("type"), field("required"), field("default")),
        (Some("string"), Some("false"), Some("tag:hk-ci"))
    );
    assert_client(CLIENT, &[]);
    assert_client(&with_in(CLIENT, TAG_INPUT_BLOCK, ""), &[NO_INPUT]);
    for (from, to) in [
        ("        default: tag:hk-ci\n", "        default: tag:ci\n"),
        ("        default: tag:hk-ci\n", "        default: \"\"\n"),
        ("        default: tag:hk-ci\n", ""),
        ("        type: string\n", "        type: number\n"),
    ] {
        let changed = with_in(CLIENT, TAG_INPUT_BLOCK, &TAG_INPUT_BLOCK.replace(from, to));
        assert_client(&changed, &[NO_INPUT]);
    }
    let tags = "          tags: ${{ inputs.tailnet_tag }}\n";
    let no_tag = "the join step `tailnet` (step 8) does not pass `tags: ${{ inputs.tailnet_tag }}`";
    for to in [
        "          tags: tag:hk-ci\n",
        "          tags: ${{ inputs.tailnet_audience }}\n",
        "",
    ] {
        assert_client(&with_in(CLIENT, tags, to), &[no_tag]);
    }
}

#[test]
fn the_tag_is_checked_by_one_unconditional_step_before_the_join() {
    // Catches: the check removed, there twice, skippable (`if:`), run on past
    // (`continue-on-error`, `|| true`), placed after the join, or checking another value than
    // the caller's input; in the real file and in a small one.
    let ids = step_ids(&real());
    let at = |id: &str| ids.iter().position(|s| s == id).expect(id);
    assert!(at(TAG_STEP) < at(JOIN_STEP), "{ids:?}");
    assert_client(
        &with_in(CLIENT, TAG_BLOCK, ""),
        &["no step has `id: tailnet-tag`"],
    );
    assert_client(
        &with_in(CLIENT, TAG_BLOCK, &format!("{TAG_BLOCK}{TAG_BLOCK}")),
        &["more than one step has `id: tailnet-tag`"],
    );
    let id = "      - id: tailnet-tag\n";
    assert_client(
        &with_in(
            CLIENT,
            id,
            &format!("{id}        if: ${{{{ inputs.join_tailnet }}}}\n"),
        ),
        &["step 1 (`tailnet-tag`) has `if`, which could run the job on past it"],
    );
    assert_client(
        &with_in(
            CLIENT,
            id,
            &format!("{id}        continue-on-error: true\n"),
        ),
        &["step 1 (`tailnet-tag`) has `continue-on-error`, which could run the job on past it"],
    );
    let after = moved(CLIENT, id, "      - id: download\n", "      - id: ca\n");
    assert_client(
        &after,
        &[
            "the tag step `tailnet-tag` (step 8) comes after the join step `tailnet` (step 7), which reads the tag",
        ],
    );
    let env = "          TAILNET_TAG: ${{ inputs.tailnet_tag }}\n";
    let wrong_env =
        "the tag step `tailnet-tag` (step 1)'s `TAILNET_TAG` is not `${{ inputs.tailnet_tag }}`";
    assert_client(
        &with_in(CLIENT, env, "          TAILNET_TAG: tag:hk-ci\n"),
        &[wrong_env],
    );
    assert_client(
        &with_in(CLIENT, env, "          TAG: ${{ inputs.tailnet_tag }}\n"),
        &[wrong_env],
    );
    let run = "        run: jq --null-input --arg tag \"$TAILNET_TAG\" 'empty'\n";
    let not_one = "the tag step `tailnet-tag` (step 1) does not run `jq --null-input --arg tag \"$TAILNET_TAG\" '<program>'` as its one command";
    for to in [
        "        run: jq --null-input --arg tag \"$TAILNET_TAG\" 'empty' || true\n",
        "        run: jq --null-input --arg tag \"$TAILNET_TAG\"'empty'\n",
        "        run: jq --null-input --arg tag \"$TAILNET_TAG\" empty\n",
        "        run: jq --null-input --arg tag \"$TAILNET_TAG\" 'em'pty'\n",
        "        run: jq --null-input --arg tag \"$TAILNET_TAG\" 'empty'; true\n",
        "        run: |\n          true\n          jq --null-input --arg tag \"$TAILNET_TAG\" 'empty'\n",
        "        run: jq --null-input --arg tag \"$OTHER\" 'empty'\n",
        "        run: |\n          jq --null-input --arg tag \"$TAILNET_TAG\" \\\n            'empty'\n",
        "        run: echo ok\n",
        "",
    ] {
        assert_client(&with_in(CLIENT, run, to), &[not_one]);
    }
}

#[test]
fn no_step_but_the_join_reads_the_tag() {
    // Catches: the tag given to another step, or to every step through the job's or the
    // workflow's `env`, where it would be used without the check; in the real file and in a
    // small one.
    let doc = yaml::load(&real()).expect("parses");
    assert!(
        entry(&doc, "jobs")
            .and_then(|j| entry(j, JOB))
            .and_then(|j| entry(j, "env"))
            .and_then(|e| entry(e, "TAILNET_TAG"))
            .is_none()
    );
    let ca = "      - id: ca\n";
    assert_client(
        &with_in(
            CLIENT,
            ca,
            &format!("{ca}        env:\n          TAG: ${{{{ inputs.tailnet_tag }}}}\n"),
        ),
        &[
            "step 9 (`ca`) names `tailnet_tag`: only the join reads the tag, once `tailnet-tag` checked it",
        ],
    );
    // However the expression spells the read.
    for read in [
        "${{inputs.tailnet_tag}}",
        "${{ inputs['tailnet_tag'] }}",
        "${{ format('{0}', inputs.tailnet_tag) }}",
    ] {
        assert_client(
            &with_in(
                CLIENT,
                ca,
                &format!("{ca}        env:\n          TAG: {read}\n"),
            ),
            &[
                "step 9 (`ca`) names `tailnet_tag`: only the join reads the tag, once `tailnet-tag` checked it",
            ],
        );
    }
    let other = "      - id: other\n        run: echo \"${{ inputs.tailnet_tag }}\"\n";
    assert_client(
        &with_in(CLIENT, TAG_BLOCK, &format!("{other}{TAG_BLOCK}")),
        &[
            "step 1 (`other`) names `tailnet_tag`: only the join reads the tag, once `tailnet-tag` checked it",
        ],
    );
    assert_client(
        &with_in(
            CLIENT,
            "    runs-on: ubuntu-24.04\n",
            "    runs-on: ubuntu-24.04\n    env:\n      TAG: ${{ inputs.tailnet_tag }}\n",
        ),
        &["the job's `env` names `tailnet_tag`"],
    );
    assert_client(
        &with_in(
            CLIENT,
            "permissions: {}\n",
            "permissions: {}\nenv:\n  TAG: ${{ inputs.tailnet_tag }}\n",
        ),
        &["the workflow's `env` names `tailnet_tag`"],
    );
    // The join itself names it, and the check: neither is a problem.
    assert!(JOIN_BLOCK.contains(TAG_VALUE) && TAG_BLOCK.contains(TAG_VALUE));
}
