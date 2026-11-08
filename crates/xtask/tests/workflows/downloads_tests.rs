//! The rule of [`super::downloads`]: every `curl` of a `run:` carries the HTTPS and failure
//! guards and the retry options, and no `run:` downloads with `wget` or `gh release download`.
//! Each case puts one `run:` into [`CLEAN`] (its first line, line 17) and reads the exact problems.

use super::{CLEAN, assert_problems, lint, with_in, workflows_dir};

/// The options every download carries, as the workflows write them.
const FLAGS: &str = "--proto =https --proto-redir =https --tlsv1.2 --fail --retry 5 --retry-delay 5 --retry-max-time 300 --retry-all-errors";

/// [`CLEAN`] whose one `run:` is the block scalar `body` (each line indented under `run: |`).
fn run(body: &str) -> String {
    let indented: String = body.lines().map(|l| format!("          {l}\n")).collect();
    with_in(
        CLEAN,
        "      - run: cargo test --workspace --locked\n",
        &format!("      - run: |\n{indented}"),
    )
}

/// A download with `options` in place of [`FLAGS`].
fn curl(options: &str) -> String {
    run(&format!(
        "curl {options} --silent --location --output \"$RUNNER_TEMP/a.tar.gz\" \"https://github.com/o/r/releases/download/v1/a.tar.gz\""
    ))
}

/// Proves: a download with every guard and retry option passes, written on one line, across
/// lines joined by `\`, inside `$(...)` with a pipe after it, after `&&`, and with other whole
/// numbers for the retries (`release.yml`'s wait for the release channel).
/// Catches: a rule that refuses the shapes the workflows use.
#[test]
fn a_download_with_every_guard_and_retry_option_passes_in_every_shape() {
    assert_problems(&curl(FLAGS), &[]);
    assert_problems(
        &run(
            "curl --proto =https --proto-redir =https \\\n  --tlsv1.2 --fail \\\n  --retry 5 --retry-delay 5 --retry-max-time 300 --retry-all-errors \\\n  --output a https://example.com/a",
        ),
        &[],
    );
    assert_problems(
        &run(&format!(
            "digest=$(curl {FLAGS} https://example.com/a.sha256 | cut -d ' ' -f 1)"
        )),
        &[],
    );
    assert_problems(
        &run(&format!("mkdir -p d && curl {FLAGS} https://example.com/a")),
        &[],
    );
    assert_problems(
        &curl(
            "--proto =https --proto-redir =https --tlsv1.2 --fail --max-time 30 --retry 45 --retry-delay 40 --retry-max-time 1800 --retry-all-errors",
        ),
        &[],
    );
}

/// Proves: a download without any one of the guards or the retry options is refused, naming
/// exactly what it lacks, and one without all of them names each.
/// Catches: a new download that fails a run on GitHub's next 504, or one that follows a redirect
/// to plain HTTP, accepts TLS below 1.2, or keeps an HTTP error page as the file.
#[test]
fn a_download_without_a_guard_or_a_retry_option_is_refused() {
    for (drop, lacks) in [
        ("--proto =https ", "`--proto =https`"),
        ("--proto-redir =https ", "`--proto-redir =https`"),
        ("--tlsv1.2 ", "`--tlsv1.2`"),
        ("--fail ", "`--fail`"),
        ("--retry 5 ", "`--retry <n>`"),
        ("--retry-delay 5 ", "`--retry-delay <n>`"),
        ("--retry-max-time 300 ", "`--retry-max-time <n>`"),
        (" --retry-all-errors", "`--retry-all-errors`"),
    ] {
        let options = FLAGS.replacen(drop, "", 1);
        assert_ne!(options, FLAGS, "{drop:?} is not in the flags");
        assert_problems(
            &curl(&options),
            &[&format!("line 17: a curl lacks {lacks}")],
        );
    }
    assert_problems(
        &curl("--silent"),
        &[
            "line 17: a curl lacks `--proto =https`, `--proto-redir =https`, `--tlsv1.2`, `--fail`, `--retry-all-errors`, `--retry <n>`, `--retry-delay <n>`, `--retry-max-time <n>`",
        ],
    );
}

/// Proves: a `--proto` or `--proto-redir` with another value than `=https`, also as a second
/// one after `=https` (curl takes the last), a retry count or time of 0 or not a whole number,
/// and a valued option with nothing after it are each refused with their own message.
/// Catches: a guard written and then undone, or a retry that never retries.
#[test]
fn a_guard_or_a_retry_option_with_another_value_is_refused() {
    let swap = |from: &str, to: &str| curl(&FLAGS.replacen(from, to, 1));
    assert_problems(
        &swap("--tlsv1.2", "--proto all --tlsv1.2"),
        &["line 17: a curl's `--proto` is `all`, not `=https`"],
    );
    assert_problems(
        &swap("--proto-redir =https", "--proto-redir =http"),
        &["line 17: a curl's `--proto-redir` is `=http`, not `=https`"],
    );
    assert_problems(
        &swap("--retry 5", "--retry 0"),
        &["line 17: a curl's `--retry` is `0`, not a whole number above 0"],
    );
    assert_problems(
        &swap("--retry-delay 5", "--retry-delay 5s"),
        &["line 17: a curl's `--retry-delay` is `5s`, not a whole number above 0"],
    );
    assert_problems(
        &run(&format!(
            "curl {} --retry-max-time",
            FLAGS.replacen(" --retry-max-time 300", "", 1)
        )),
        &["line 17: a curl's `--retry-max-time` is ``, not a whole number above 0"],
    );
    assert_problems(
        &run(&format!(
            "curl {}",
            FLAGS.replacen("--proto-redir =https", "--proto-redir", 1)
        )),
        &["line 17: a curl's `--proto-redir` is `--tlsv1.2`, not `=https`"],
    );
}

/// Proves: the options of a curl end at `|`, `||`, `&&`, `;` and `)`: an option after them is
/// another command's; a second curl on the line is held to the rule on its own, and so is a curl
/// in backquotes.
/// Catches: a curl that passes on another command's words, or a second curl the rule skips.
#[test]
fn each_curl_of_a_line_is_held_to_its_own_options() {
    let short = FLAGS.replacen(" --retry-all-errors", "", 1);
    let lacks = "line 17: a curl lacks `--retry-all-errors`";
    for end in ["|", "||", "&&", ";", ")"] {
        assert_problems(
            &run(&format!(
                "curl {short} https://example.com/a {end} echo --retry-all-errors"
            )),
            &[lacks],
        );
    }
    assert_problems(
        &run(&format!(
            "curl {FLAGS} https://example.com/a && curl {short} https://example.com/b"
        )),
        &[lacks],
    );
    assert_problems(
        &run(&format!("x=`curl {short} https://example.com/a`")),
        &[lacks],
    );
}

/// Proves: a comment line, a word that only contains `curl`, and a quoted `curl` are not
/// downloads, and a `run:` that is not a scalar (`defaults.run`) is not read as one.
/// Catches: a rule that refuses a workflow for its comments or for another tool's name.
#[test]
fn a_comment_or_a_word_that_only_contains_curl_is_not_a_download() {
    assert_problems(
        &run("# curl https://example.com/a\n  # wget https://example.com/a"),
        &[],
    );
    assert_problems(
        &run("curly --fetch a\necho \"curl\" libcurl curl-config"),
        &[],
    );
    assert_problems(
        &with_in(
            CLEAN,
            "jobs:\n",
            "defaults:\n  run:\n    shell: bash\njobs:\n",
        ),
        &[],
    );
}

/// Proves: `wget`, also in `$(...)`, and `gh release download` are refused, and another `gh
/// release` verb is not.
/// Catches: a download the curl rule cannot see, which carries no retries.
#[test]
fn a_download_with_wget_or_gh_release_download_is_refused() {
    assert_problems(
        &run("wget https://example.com/a"),
        &["line 17: `wget` downloads without curl's guards and retries: use curl"],
    );
    assert_problems(
        &run("x=$(wget -qO- https://example.com/a)"),
        &["line 17: `wget` downloads without curl's guards and retries: use curl"],
    );
    assert_problems(
        &run("gh release download v1 --repo o/r"),
        &["line 17: `gh release download` downloads without curl's guards and retries: use curl"],
    );
    assert_problems(&run("gh release list --repo o/r\ngh release"), &[]);
}

/// Proves: `artifacts.yml` with the retry options taken off the download of cargo-cyclonedx (the
/// step a GitHub 504 failed on main) fails the lint on that step's line, and passes with them.
/// Catches: a rule that does not read the repository's own workflows.
#[test]
fn the_cyclonedx_download_of_artifacts_yml_without_its_retries_is_refused() {
    let text = std::fs::read_to_string(workflows_dir().join("artifacts.yml")).expect("read");
    assert_eq!(lint("artifacts.yml", &text), Vec::<String>::new());
    let download = "--fail --retry 5 --retry-delay 5 --retry-max-time 300 --retry-all-errors --silent --show-error --location --output \"$RUNNER_TEMP/cyclonedx.tar.xz\"";
    let at = text.find(download).expect("the cyclonedx download");
    let line = text[..at].lines().count();
    let changed = text.replacen(
        download,
        "--fail --silent --show-error --location --output \"$RUNNER_TEMP/cyclonedx.tar.xz\"",
        1,
    );
    assert_eq!(
        lint("artifacts.yml", &changed),
        vec![format!(
            "line {line}: a curl lacks `--retry-all-errors`, `--retry <n>`, `--retry-delay <n>`, `--retry-max-time <n>`"
        )]
    );
}
