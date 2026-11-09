//! The rule of [`super::one_command`]: every `run:` of `release.yml` is one command on one line.
//! The cases put a script into [`super::CLEAN`] (the rule is applied to it directly) and into the
//! repository's own `release.yml` (through the whole lint).

use super::lint::lint;
use super::one_command::problems;
use super::{CLEAN, with, workflows, yaml};

/// The step of [`CLEAN`] each case replaces (line 16).
const STEP: &str = "      - run: cargo test --workspace --locked\n";

fn found(text: &str) -> Vec<String> {
    problems(&yaml::load(text).expect("loads"))
}

fn multi_line(line: usize) -> String {
    format!(
        "line {line}: `run:` has more than one line: a step is one command (split it into steps, or move the logic into a `cargo xtask` verb)"
    )
}

fn several(line: usize) -> String {
    format!(
        "line {line}: `run:` runs more than one command: a step is one command (split it into steps, or move the logic into a `cargo xtask` verb)"
    )
}

#[test]
fn a_step_of_one_command_on_one_line_passes() {
    // Catches: a rule that refuses the steps release.yml is made of: a call with quoted
    // arguments, a command substitution inside an argument, a redirection to a file.
    for run in [
        "cargo test --workspace --locked",
        "cargo xtask channels rpm sign --signed-at \"$(git show --no-patch --format=%ct HEAD)\"",
        "cargo xtask release plan > \"$RUNNER_TEMP/plan.json\"",
        "sha256sum --check --strict SHA256SUMS",
    ] {
        assert_eq!(
            found(&with(STEP, &format!("      - run: {run}\n"))),
            Vec::<String>::new(),
            "{run}"
        );
    }
    assert_eq!(found(CLEAN), Vec::<String>::new());
}

#[test]
fn a_block_of_several_lines_and_a_list_of_commands_on_one_line_are_refused() {
    // Catches: the steps of release.yml that checked the sums, then ran the check, in one block,
    // and that wrote the signing key to a file in shell, and the same in one line.
    let block = with(
        STEP,
        "      - run: |\n          umask 077\n          printf '%s' \"$KEY\" > \"$RUNNER_TEMP/key\"\n",
    );
    assert_eq!(found(&block), [multi_line(17)]);
    // A folded scalar of one line is one command; a block of one command and a trailing newline
    // is too.
    let one = with(STEP, "      - run: |\n          cargo test --locked\n");
    assert_eq!(found(&one), Vec::<String>::new());
    for run in [
        "(cd dist && sha256sum --check --strict SHA256SUMS)",
        "cd dist && sha256sum --check --strict SHA256SUMS",
        "umask 077; cargo test",
        "cargo test || cargo test",
    ] {
        let text = with(STEP, &format!("      - run: {run}\n"));
        assert_eq!(found(&text), [several(16)], "{run}");
    }
}

#[test]
fn release_yml_with_a_multi_line_step_is_refused_by_the_lint_and_as_it_is_passes() {
    // Catches: the rule not wired to the file, and a rule that refuses release.yml as it is.
    let release = workflows()
        .into_iter()
        .find(|(file, _)| file == "release.yml")
        .map(|(_, text)| text)
        .expect("release.yml");
    let has = |text: &str| {
        lint("release.yml", text)
            .iter()
            .any(|p| p.contains("`run:` has more than one line"))
    };
    assert!(!has(&release));
    let step = "      - run: rustup toolchain install\n";
    assert!(release.contains(step));
    let broken = release.replacen(
        step,
        "      - run: |\n          rustup toolchain install\n          rustup show\n",
        1,
    );
    assert!(has(&broken));
    // Another workflow's block scalar is not this rule's business.
    let other = with(STEP, "      - run: |\n          a\n          b\n");
    assert!(
        !lint("ci.yml", &other)
            .iter()
            .any(|p| p.contains("`run:` has more than one line"))
    );
}
