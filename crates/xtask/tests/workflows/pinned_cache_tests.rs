//! The rule of [`super::pinned_cache`]: every pinned download is the middle of five steps,
//! restore from the cache, download on a miss, the one-line note on a failed download, the check
//! against the pin, and the save of a checked file on a miss. Each case puts the five steps of
//! `gh.tar.gz` (or a defect of them) into [`super::CLEAN`] in place of its test step, the check's
//! `run:` on line 27, and reads the exact problems; the last cases run the conditions of the
//! repository's own steps against every outcome.

use std::collections::BTreeMap;

use super::expr::{Context, holds};
use super::lint::{entries, entry};
use super::pinned_cache::{GO_ON, on_checked, on_download_failure, on_miss, saved_key};
use super::yaml::{self, Node, Value};
use super::{assert_problems, lint, with, workflows, workflows_dir};

const CACHE: &str = "55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0";

const RESTORE: &str = "      - id: gh-cache
        uses: actions/cache/restore@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
        with:
          path: ${{ runner.temp }}/gh.tar.gz
          key: pinned-gh-${{ env.GH_VERSION }}-${{ env.GH_SHA256 }}
";

const DOWNLOAD: &str = "      - id: gh-download
        if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}
        run: curl --proto =https --proto-redir =https --tlsv1.2 --fail --retry 60 --retry-delay 10 --retry-max-time 600 --retry-all-errors --silent --show-error --location --output \"$RUNNER_TEMP/gh.tar.gz\" \"https://github.com/cli/cli/releases/download/v$GH_VERSION/gh_${GH_VERSION}_linux_amd64.tar.gz\"
";

const NOTE: &str = "      - if: ${{ !cancelled() && steps.gh-download.outcome == 'failure' }}
        run: echo \"::error::gh.tar.gz is not cached and GitHub did not serve it\"
";

const CHECK: &str = "      - id: gh-check
        run: cargo xtask install-tool --download \"$RUNNER_TEMP/gh.tar.gz\" --sha256 \"$GH_SHA256\" --out target/gh --extract tar.gz --member gh
";

const SAVE: &str = "      - if: ${{ !cancelled() && steps.gh-check.outcome == 'success' && steps.gh-cache.outputs.cache-hit != 'true' }}
        uses: actions/cache/save@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
        with:
          path: ${{ runner.temp }}/gh.tar.gz
          key: ${{ steps.gh-cache.outputs.cache-primary-key }}
";

const WHO: &str = "job test: the check of gh.tar.gz (line 27)";

fn not_restored(key: &str, guard: &str) -> String {
    format!(
        "{WHO}: the third step before it is not `actions/cache/restore@` with `id: gh-cache`, {guard}and `with:` only `path: ${{{{ runner.temp }}}}/gh.tar.gz` and `key: {key}`"
    )
}

const KEY: &str = "pinned-gh-${{ env.GH_VERSION }}-${{ env.GH_SHA256 }}";
const NOT_ON_MISS: &str = "job test: the check of gh.tar.gz (line 27): its download lacks `id: gh-download` and `if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}`";
const NOT_NOTED: &str = "job test: the check of gh.tar.gz (line 27): the step before it is not the note `if: ${{ !cancelled() && steps.gh-download.outcome == 'failure' }}` with a one-line `run: echo \"::error::…\"`";
const NOT_SAVED: &str = "job test: the check of gh.tar.gz (line 27): the step after it is not `actions/cache/save@` with `if: ${{ !cancelled() && steps.gh-check.outcome == 'success' && steps.gh-cache.outputs.cache-hit != 'true' }}` and `with:` only `path: ${{ runner.temp }}/gh.tar.gz` and `key: ${{ steps.gh-cache.outputs.cache-primary-key }}`";
/// The release download on line 23, which no check reads.
const UNCHECKED: &str = "job test: the release download (line 23) is not the download of a pinned check: put `cargo xtask install-tool --download \"$RUNNER_TEMP/<file>\" --sha256 \"$<PIN>\"` two steps after it, in the five steps of a pinned download";

/// [`super::CLEAN`] whose test step is `steps`.
fn job(steps: &str) -> String {
    with("      - run: cargo test --workspace --locked\n", steps)
}

/// The five steps.
fn all() -> String {
    [RESTORE, DOWNLOAD, NOTE, CHECK, SAVE].concat()
}

/// The five steps, `from` replaced by `to` once in their text.
fn five(from: &str, to: &str) -> String {
    let all = all();
    assert_eq!(
        all.matches(from).count(),
        1,
        "{from:?} is not in the steps once"
    );
    job(&all.replacen(from, to, 1))
}

/// Proves: the five steps pass; so do the pin as `${GH_SHA256}`, the pin and the URL's version
/// from the steps' own `env:` (artifacts.yml's matrix pins), a URL that is one variable (the
/// previous release), and the five steps of a job that goes on after a failed step, each guarded
/// by `!cancelled()` (ci.yml's `lint`).
/// Catches: a rule that refuses the shapes the workflows use.
#[test]
fn the_five_steps_pass_in_every_shape_the_workflows_use() {
    assert_problems(&job(&all()), &[]);
    assert_problems(&five("\"$GH_SHA256\"", "\"${GH_SHA256}\""), &[]);
    let matrix = all()
        .replace(KEY, "pinned-gh-${{ matrix.v }}-${{ matrix.gh-sha256 }}")
        .replace("$GH_SHA256", "$SHA256")
        .replace(
            "      - id: gh-check\n",
            "      - id: gh-check\n        env:\n          SHA256: ${{ matrix.gh-sha256 }}\n",
        )
        .replace(
            "      - id: gh-download\n",
            "      - id: gh-download\n        env:\n          GH_VERSION: ${{ matrix.v }}\n",
        );
    assert_problems(&job(&matrix), &[]);
    let url = all()
        .replace(KEY, "pinned-gh-${{ env.GH_URL }}-${{ env.GH_SHA256 }}")
        .replace(
            "\"https://github.com/cli/cli/releases/download/v$GH_VERSION/gh_${GH_VERSION}_linux_amd64.tar.gz\"",
            "\"$GH_URL\"",
        );
    assert_problems(&job(&url), &[]);
    assert_problems(&job(&go_on()), &[]);
}

/// The five steps of a job that goes on after a failed step.
fn go_on() -> String {
    all()
        .replace(
            "      - id: gh-cache\n",
            "      - id: gh-cache\n        if: ${{ !cancelled() }}\n",
        )
        .replace(
            "if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}",
            "if: ${{ !cancelled() && steps.gh-cache.outputs.cache-hit != 'true' }}",
        )
        .replace(
            "      - id: gh-check\n",
            "      - id: gh-check\n        if: ${{ !cancelled() }}\n",
        )
}

/// Proves: a release download that no check follows is refused: alone, before another step,
/// and however it names its output (`-o`, `--output=`, `-O`, `--remote-name`,
/// `--remote-name-all`) or its URL (a whole `$NAME` whose value the step's, the job's or the
/// workflow's `env:` sets to a release asset). A download of this repository's own releases
/// (set-desired.yml's and hello-desired.yml's, by their tags), a `$NAME` no `env:` sets, and a
/// check of a file no curl of the job writes (a trusted root gh prints) are not pinned downloads.
/// Catches: a new pinned tool downloaded without the cache, in any spelling of curl's output or
/// URL, and a rule that holds a step that downloads no pinned file to it.
#[test]
fn a_release_download_without_its_check_is_refused() {
    let refused = |line: usize| {
        format!(
            "job test: the release download (line {line}) is not the download of a pinned check: put `cargo xtask install-tool --download \"$RUNNER_TEMP/<file>\" --sha256 \"$<PIN>\"` two steps after it, in the five steps of a pinned download"
        )
    };
    assert_problems(&job(DOWNLOAD), &[&refused(18)]);
    assert_problems(
        &job(&format!(
            "{DOWNLOAD}      - run: tar -xzf \"$RUNNER_TEMP/gh.tar.gz\"\n"
        )),
        &[&refused(18)],
    );
    let curl = |rest: &str| {
        format!(
            "      - run: curl --proto =https --proto-redir =https --tlsv1.2 --fail --retry 60 --retry-delay 10 --retry-max-time 600 --retry-all-errors {rest}\n"
        )
    };
    let asset = "https://github.com/o/r/releases/download/v1/a.tgz";
    for output in [
        "-o \"$RUNNER_TEMP/a.tgz\"",
        "--output=\"$RUNNER_TEMP/a.tgz\"",
        "-O",
        "--remote-name",
        "--remote-name-all",
    ] {
        assert_problems(&job(&curl(&format!("{output} {asset}"))), &[&refused(16)]);
    }
    let by_name = curl("--output \"$RUNNER_TEMP/a.tgz\" \"$A_URL\"");
    let step_env = by_name.replacen(
        "      - run:",
        &format!("      - env:\n          A_URL: {asset}\n        run:"),
        1,
    );
    assert_problems(&job(&step_env), &[&refused(18)]);
    let job_env = with_in_clean(
        "    permissions:\n      contents: read\n    steps:\n",
        &format!(
            "    permissions:\n      contents: read\n    env:\n      A_URL: {asset}\n    steps:\n"
        ),
        &by_name,
    );
    assert_problems(&job_env, &[&refused(18)]);
    let workflow_env = with_in_clean(
        "permissions: {}\n",
        &format!("permissions: {{}}\nenv:\n  A_URL: \"${{A}}{asset}\"\n"),
        &by_name,
    );
    assert_problems(&workflow_env, &[&refused(18)]);
    for not_pinned in [
        curl("--output \"$RUNNER_TEMP/a.tgz\" \"$A_URL\""),
        curl("--remote-name \"https://github.com/komira-ai/hostkeeper/releases/download/$TAG/a\""),
        curl("--remote-name-all \"https://github.com/$GITHUB_REPOSITORY/releases/download/v$R/a\""),
        "      - run: cargo xtask install-tool --download \"$RUNNER_TEMP/root.json\" --sha256 \"$ROOT_SHA256\" --out target/root.json\n".to_owned(),
    ] {
        assert_problems(&job(&not_pinned), &[]);
    }
}

/// [`super::CLEAN`] with `from` replaced by `to`, and its test step by `step`.
fn with_in_clean(from: &str, to: &str, step: &str) -> String {
    super::with_in(&job(step), from, to)
}

/// Proves: `continue-on-error`, `shell` or `working-directory` on any of the five steps, or a
/// step's own `RUNNER_TEMP`, is refused, naming the step and each key it sets.
/// Catches: a check made to pass a file that failed it, or to read a file other than the one
/// restored and saved, and a download whose failure the job goes on past.
#[test]
fn a_step_key_that_undoes_the_five_steps_is_refused() {
    let note = "      - if: ${{ !cancelled() && steps.gh-download.outcome == 'failure' }}\n";
    let save = "      - if: ${{ !cancelled() && steps.gh-check.outcome == 'success' && steps.gh-cache.outputs.cache-hit != 'true' }}\n";
    for (head, role) in [
        ("      - id: gh-cache\n", "restore"),
        ("      - id: gh-download\n", "download"),
        (note, "note"),
        ("      - id: gh-check\n", "check"),
        (save, "save"),
    ] {
        for (key, set) in [
            ("continue-on-error: true", "continue-on-error"),
            ("shell: bash", "shell"),
            ("working-directory: crates", "working-directory"),
            (
                "env:\n          RUNNER_TEMP: /elsewhere",
                "env: RUNNER_TEMP",
            ),
        ] {
            let text = five(head, &format!("{head}        {key}\n"));
            let check = text
                .find("run: cargo xtask install-tool")
                .expect("the check");
            let line = text[..check].lines().count();
            assert_problems(
                &text,
                &[&format!(
                    "job test: the check of gh.tar.gz (line {line}): its {role} step sets `{set}`"
                )],
            );
        }
    }
    assert_problems(
        &five(
            "      - id: gh-check\n",
            "      - id: gh-check\n        continue-on-error: true\n        shell: bash\n",
        ),
        &[
            "job test: the check of gh.tar.gz (line 29): its check step sets `continue-on-error`, `shell`",
        ],
    );
}

/// Proves: a check right after its download, the first steps of the job, lacks each other step
/// and its id, and its download (no second step before it) is a release download of no check.
/// Catches: a pinned download whose file is used unchecked or uncached, at the edge of the
/// job's steps.
#[test]
fn a_check_right_after_its_download_lacks_each_other_step() {
    let bare = format!("{}{}", DOWNLOAD.replace("      - id: gh-download\n        if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}\n", "      - "), CHECK.replace("      - id: gh-check\n        ", "      - "));
    let who = "job test: the check of gh.tar.gz (line 17)";
    assert_problems(
        &job(&bare),
        &[
            &format!("{who} lacks `id: gh-check`"),
            &format!(
                "{who}: the second step before it is not one `curl` with `--output \"$RUNNER_TEMP/gh.tar.gz\"`"
            ),
            &format!(
                "{who}: the third step before it is not `actions/cache/restore@` with `id: gh-cache`, and `with:` only `path: ${{{{ runner.temp }}}}/gh.tar.gz` and `key: pinned-gh-<version>-${{{{ env.GH_SHA256 }}}}`"
            ),
            &format!(
                "{who}: the step before it is not the note `if: ${{{{ !cancelled() && steps.gh-download.outcome == 'failure' }}}}` with a one-line `run: echo \"::error::…\"`"
            ),
            &format!(
                "{who}: the step after it is not `actions/cache/save@` with `if: ${{{{ !cancelled() && steps.gh-check.outcome == 'success' && steps.gh-cache.outputs.cache-hit != 'true' }}}}` and `with:` only `path: ${{{{ runner.temp }}}}/gh.tar.gz` and `key: ${{{{ steps.gh-cache.outputs.cache-primary-key }}}}`"
            ),
            "job test: the release download (line 16) is not the download of a pinned check: put `cargo xtask install-tool --download \"$RUNNER_TEMP/<file>\" --sha256 \"$<PIN>\"` two steps after it, in the five steps of a pinned download",
        ],
    );
}

/// Proves: a restore that is not the restore action (the full `actions/cache`, which saves at
/// the job's end, checked or not), has another id, another path, a `restore-keys` (which restores
/// another version's file), an `if:` the check has not, or a key without the version or with
/// another pin, is refused; the key follows the check's `--sha256` variable.
/// Catches: a cache entry another pin shares, a file the download does not write, or a restore
/// that a failed step before it skips in a job that goes on.
#[test]
fn a_restore_of_another_shape_is_refused() {
    let plain = not_restored(KEY, "");
    for (from, to) in [
        ("actions/cache/restore@", "actions/cache@"),
        ("- id: gh-cache", "- id: cache"),
        (
            "path: ${{ runner.temp }}/gh.tar.gz\n          key: pinned",
            "path: ${{ runner.temp }}/gh.zip\n          key: pinned",
        ),
        (
            "key: pinned-gh-${{ env.GH_VERSION }}-${{ env.GH_SHA256 }}\n",
            "key: pinned-gh-${{ env.GH_VERSION }}-${{ env.GH_SHA256 }}\n          restore-keys: pinned-gh-\n",
        ),
        ("key: pinned-gh-${{ env.GH_VERSION }}-", "key: pinned-gh-"),
        (
            "key: pinned-gh-${{ env.GH_VERSION }}-${{ env.GH_SHA256 }}",
            "key: pinned-gh-${{ env.GH_VERSION }}-${{ env.NFPM_SHA256 }}",
        ),
        (
            "      - id: gh-cache\n",
            "      - id: gh-cache\n        if: ${{ !cancelled() }}\n",
        ),
    ] {
        // A line added before the check moves it to line 28.
        let lines = to.lines().count() - from.lines().count();
        let want = plain.replace("line 27", &format!("line {}", 27 + lines));
        assert_problems(&five(from, to), &[&want]);
    }
    assert_problems(
        &five("--sha256 \"$GH_SHA256\"", "--sha256 \"$NFPM_SHA256\""),
        &[&not_restored(
            "pinned-gh-${{ env.GH_VERSION }}-${{ env.NFPM_SHA256 }}",
            "",
        )],
    );
    // In a job that goes on, a restore without the check's `if:`.
    let unguarded = go_on().replacen(
        "      - id: gh-cache\n        if: ${{ !cancelled() }}\n",
        "      - id: gh-cache\n",
        1,
    );
    assert_problems(
        &job(&unguarded),
        &[&not_restored(KEY, "`if: ${{ !cancelled() }}`, ").replace("line 27", "line 28")],
    );
}

/// Proves: a download that runs on a cache hit too (no `if:`, or a job's `!cancelled()` alone),
/// has another id, or whose URL names no variable, is refused; so is a release download that is
/// not one curl to `$RUNNER_TEMP/<file>` (no check then reads its file).
/// Catches: a job that still fails when GitHub is down though the pin is cached, a note that
/// never runs, or a key with no version.
#[test]
fn a_download_that_runs_on_a_hit_or_has_no_version_is_refused() {
    let line = |s: &str| s.replace("line 27", "line 26");
    assert_problems(
        &five(
            "        if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}\n        run: curl",
            "        run: curl",
        ),
        &[&line(NOT_ON_MISS)],
    );
    assert_problems(
        &five(
            "if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}\n        run: curl",
            "if: ${{ !cancelled() }}\n        run: curl",
        ),
        &[NOT_ON_MISS],
    );
    assert_problems(&five("- id: gh-download", "- id: download"), &[NOT_ON_MISS]);
    assert_problems(
        &five(
            "\"https://github.com/cli/cli/releases/download/v$GH_VERSION/gh_${GH_VERSION}_linux_amd64.tar.gz\"",
            "https://github.com/cli/cli/releases/download/v2/gh.tar.gz",
        ),
        &["job test: the check of gh.tar.gz (line 27): its download's URL names no variable"],
    );
    // A download in a job that goes on, with the condition of a job that does not.
    let plain_miss = go_on().replacen(
        "if: ${{ !cancelled() && steps.gh-cache.outputs.cache-hit != 'true' }}\n        run: curl",
        "if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}\n        run: curl",
        1,
    );
    assert_problems(
        &job(&plain_miss),
        &[
            "job test: the check of gh.tar.gz (line 29): its download lacks `id: gh-download` and `if: ${{ !cancelled() && steps.gh-cache.outputs.cache-hit != 'true' }}`",
        ],
    );
    for (from, to) in [
        ("--output \"$RUNNER_TEMP/gh.tar.gz\"", "--output gh.tar.gz"),
        (
            "--output \"$RUNNER_TEMP/gh.tar.gz\"",
            "--output \"$RUNNER_TEMP/d/gh.tar.gz\"",
        ),
        ("run: curl", "run: mkdir -p d && curl"),
    ] {
        assert_problems(&five(from, to), &[UNCHECKED]);
    }
}

/// Proves: a note with another condition (`failure()`, which also runs after an earlier step's
/// failure), more than one line, or another command, is refused.
/// Catches: a cold cache with GitHub down failing without saying why.
#[test]
fn a_note_of_another_shape_is_refused() {
    for (from, to) in [
        (
            "      - if: ${{ !cancelled() && steps.gh-download.outcome == 'failure' }}",
            "      - if: ${{ failure() }}",
        ),
        (
            "        run: echo \"::error::gh.tar.gz is not cached and GitHub did not serve it\"\n",
            "        run: |\n          echo \"::error::gh is not cached\"\n          echo and GitHub did not serve it\n",
        ),
        ("run: echo \"::error::", "run: echo \"::warning::"),
    ] {
        let lines = to.lines().count() - from.lines().count();
        let want = NOT_NOTED.replace("line 27", &format!("line {}", 27 + lines));
        assert_problems(&five(from, to), &[&want]);
    }
}

/// Proves: a check with an `if:` other than a job's `!cancelled()` (skipped on a hit, so a
/// cached file is used unchecked), without its id, or with a SHA-256 that is not one variable,
/// is refused; a step between the download and the check shifts each step out of its place.
/// Catches: a file from the cache or the download used before it is checked against the pin.
#[test]
fn a_check_that_may_not_run_or_checks_no_pin_is_refused() {
    assert_problems(
        &five(
            "      - id: gh-check\n",
            "      - id: gh-check\n        if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}\n",
        ),
        &[
            &format!(
                "{} has an `if:` other than `{GO_ON}`",
                WHO.replace("27", "28")
            ),
            &not_restored(
                KEY,
                "`if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}`, ",
            )
            .replace("line 27", "line 28"),
        ],
    );
    assert_problems(
        &five(
            "      - id: gh-check\n        run",
            "      - id: check\n        run",
        ),
        &[&format!("{WHO} lacks `id: gh-check`")],
    );
    for sha in [
        "77b9e297e94c50336823d0f84de7ef1be60cba704601fa0d98f129f47a94987b",
        "\"$\"",
        "\"${GH-SHA}\"",
        "\"x$GH_SHA256\"",
        "\"$GH_SHA256.x\"",
    ] {
        assert_problems(
            &five("--sha256 \"$GH_SHA256\"", &format!("--sha256 {sha}")),
            &[&format!("{WHO}: its `--sha256` is not one variable")],
        );
    }
    // The file unpacked before the check: the note and the curl each one step late.
    let early = format!(
        "{RESTORE}{DOWNLOAD}      - run: tar -xzf \"$RUNNER_TEMP/gh.tar.gz\"\n{NOTE}{CHECK}{SAVE}"
    );
    let who = "job test: the check of gh.tar.gz (line 28)";
    assert_problems(
        &job(&early),
        &[
            &format!(
                "{who}: the second step before it is not one `curl` with `--output \"$RUNNER_TEMP/gh.tar.gz\"`"
            ),
            &format!(
                "{who}: the third step before it is not `actions/cache/restore@` with `id: gh-cache`, and `with:` only `path: ${{{{ runner.temp }}}}/gh.tar.gz` and `key: pinned-gh-<version>-${{{{ env.GH_SHA256 }}}}`"
            ),
            "job test: the release download (line 23) is not the download of a pinned check: put `cargo xtask install-tool --download \"$RUNNER_TEMP/<file>\" --sha256 \"$<PIN>\"` two steps after it, in the five steps of a pinned download",
        ],
    );
}

/// Proves: a save on a hit too (only the miss), on every outcome, on GitHub's implicit success
/// (it would skip the save in a job that goes on), as the full cache action, with another path,
/// another key, or more under `with:`, and a save before the check, are refused.
/// Catches: a cache entry written for a file the check refused, or under a key no restore
/// looks up.
#[test]
fn a_save_of_another_shape_is_refused() {
    let save_if = "      - if: ${{ !cancelled() && steps.gh-check.outcome == 'success' && steps.gh-cache.outputs.cache-hit != 'true' }}\n        uses";
    for (from, to) in [
        (
            save_if,
            "      - if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}\n        uses",
        ),
        (save_if, "      - if: ${{ always() }}\n        uses"),
        (save_if, "      - uses"),
        ("actions/cache/save@", "actions/cache@"),
        (
            "path: ${{ runner.temp }}/gh.tar.gz\n          key: ${{ steps",
            "path: ${{ runner.temp }}/gh\n          key: ${{ steps",
        ),
        (
            "key: ${{ steps.gh-cache.outputs.cache-primary-key }}",
            "key: pinned-gh-${{ env.GH_VERSION }}",
        ),
        (
            "key: ${{ steps.gh-cache.outputs.cache-primary-key }}\n",
            "key: ${{ steps.gh-cache.outputs.cache-primary-key }}\n          upload-chunk-size: 1\n",
        ),
    ] {
        assert_problems(&five(from, to), &[NOT_SAVED]);
    }
    let first = format!("{RESTORE}{DOWNLOAD}{NOTE}{SAVE}{CHECK}");
    let who = "job test: the check of gh.tar.gz (line 32)";
    assert_problems(
        &job(&first),
        &[
            &format!(
                "{who}: the second step before it is not one `curl` with `--output \"$RUNNER_TEMP/gh.tar.gz\"`"
            ),
            &format!(
                "{who}: the third step before it is not `actions/cache/restore@` with `id: gh-cache`, and `with:` only `path: ${{{{ runner.temp }}}}/gh.tar.gz` and `key: pinned-gh-<version>-${{{{ env.GH_SHA256 }}}}`"
            ),
            &format!(
                "{who}: the step before it is not the note `if: ${{{{ !cancelled() && steps.gh-download.outcome == 'failure' }}}}` with a one-line `run: echo \"::error::…\"`"
            ),
            &format!(
                "{who}: the step after it is not `actions/cache/save@` with `if: ${{{{ !cancelled() && steps.gh-check.outcome == 'success' && steps.gh-cache.outputs.cache-hit != 'true' }}}}` and `with:` only `path: ${{{{ runner.temp }}}}/gh.tar.gz` and `key: ${{{{ steps.gh-cache.outputs.cache-primary-key }}}}`"
            ),
            "job test: the release download (line 23) is not the download of a pinned check: put `cargo xtask install-tool --download \"$RUNNER_TEMP/<file>\" --sha256 \"$<PIN>\"` two steps after it, in the five steps of a pinned download",
        ],
    );
}

/// One pinned download of the repository: its workflow, job, file, and its job's steps with the
/// index of its restore.
struct Site {
    file: String,
    job: String,
    archive: String,
    steps: Vec<Node>,
    restore: usize,
}

/// Every pinned download of the repository's workflows: each restore of the cache action whose
/// id is `<stem>-cache` and whose path is a file of `${{ runner.temp }}`.
fn sites() -> Vec<Site> {
    let mut found = Vec::new();
    for (file, text) in workflows() {
        let doc = yaml::load(&text).expect("a workflow");
        for (name, job) in entry(&doc, "jobs").map_or(&[][..], entries) {
            let Some(Value::Seq(steps)) = entry(job, "steps").map(|s| s.value.clone()) else {
                continue;
            };
            for (at, step) in steps.iter().enumerate() {
                let restores = entry(step, "uses")
                    .and_then(Node::as_str)
                    .is_some_and(|u| u.starts_with("actions/cache/restore@"));
                let archive = entry(step, "with")
                    .and_then(|w| entry(w, "path"))
                    .and_then(Node::as_str)
                    .and_then(|p| p.strip_prefix("${{ runner.temp }}/"));
                if let (true, Some(archive)) = (restores, archive) {
                    found.push(Site {
                        file: file.clone(),
                        job: name.as_str().unwrap_or_default().to_owned(),
                        archive: archive.to_owned(),
                        steps: steps.clone(),
                        restore: at,
                    });
                }
            }
        }
    }
    found
}

/// Proves: the pinned downloads of the repository are exactly these, each in the five steps, and
/// ci.yml with the restore of cargo-deny taken out fails the lint on that check's line.
/// Catches: a pinned download that slips past the rule (main's 504s hit gh, cargo-cyclonedx,
/// cargo-llvm-cov, cargo-deny and cargo-nextest), and a rule that does not read the repository's
/// own workflows.
#[test]
fn every_pinned_download_of_the_repository_is_held_to_the_rule() {
    let mut found: Vec<String> = sites()
        .iter()
        .map(|s| format!("{} {} {}", s.file, s.job, s.archive))
        .collect();
    found.sort();
    let mut want = vec![
        "advisories.yml advisories cargo-deny.tar.gz",
        "artifacts.yml build cyclonedx.tar.xz",
        "artifacts.yml build nfpm.tar.gz",
        "artifacts.yml build gh.tar.gz",
        "artifacts.yml build-macos cyclonedx.tar.xz",
        "artifacts.yml build-macos gh.zip",
        "ci.yml lint cargo-deny.tar.gz",
        "ci.yml lint actionlint.tar.gz",
        "ci.yml test-shard cargo-llvm-cov.tar.gz",
        "ci.yml test-shard cargo-nextest.tar.gz",
        "ci.yml coverage cargo-llvm-cov.tar.gz",
        "net-install.yml net-install nfpm.tar.gz",
        "net-install.yml net-install gh.tar.gz",
        "net-install.yml net-install-arm64 gh.tar.gz",
        "pr-device.yml macos-device gh.zip",
        "pr-device.yml macos-device previous-release.tar.gz",
        "pr-device.yml privileged gh.tar.gz",
        "pr-device.yml privileged previous-release.tar.gz",
        "pr-device.yml privileged nfpm.tar.gz",
        "pr-device.yml openrc-device nfpm.tar.gz",
        "privileged.yml privileged gh.tar.gz",
        "privileged.yml privileged previous-release.tar.gz",
        "privileged.yml privileged nfpm.tar.gz",
        "privileged.yml macos-device gh.zip",
        "privileged.yml macos-device previous-release.tar.gz",
        "privileged.yml openrc-device nfpm.tar.gz",
    ];
    want.sort_unstable();
    assert_eq!(found, want);

    let text = std::fs::read_to_string(workflows_dir().join("ci.yml")).expect("read");
    assert_eq!(lint("ci.yml", &text), Vec::<String>::new());
    let restore = format!(
        "      - name: Restore the pinned cargo-deny.tar.gz from the cache\n        id: cargo-deny-cache\n        if: ${{{{ !cancelled() }}}}\n        uses: actions/cache/restore@{CACHE}\n        with:\n          path: ${{{{ runner.temp }}}}/cargo-deny.tar.gz\n          key: pinned-cargo-deny-${{{{ env.DENY_VERSION }}}}-${{{{ env.DENY_SHA256 }}}}\n"
    );
    assert_eq!(
        text.matches(&restore).count(),
        1,
        "lint's restore of cargo-deny"
    );
    let changed = text.replacen(&restore, "", 1);
    let check = changed
        .find("install-tool --download \"$RUNNER_TEMP/cargo-deny.tar.gz\"")
        .expect("the check");
    let line = changed[..check].lines().count();
    assert_eq!(
        lint("ci.yml", &changed),
        vec![format!(
            "job lint: the check of cargo-deny.tar.gz (line {line}): the third step before it is not `actions/cache/restore@` with `id: cargo-deny-cache`, `if: ${{{{ !cancelled() }}}}`, and `with:` only `path: ${{{{ runner.temp }}}}/cargo-deny.tar.gz` and `key: pinned-cargo-deny-${{{{ env.DENY_VERSION }}}}-${{{{ env.DENY_SHA256 }}}}`"
        )]
    );
}

/// What the five steps do in one outcome.
#[derive(Debug, PartialEq)]
struct Run {
    restore: bool,
    download: bool,
    note: bool,
    check: bool,
    save: bool,
    passes: bool,
}

/// Whether a step with condition `cond` runs: GitHub puts `success() &&` before a condition that
/// names no status function, so such a step (and one with no `if:`) runs only when every step
/// before it passed.
fn runs(cond: Option<&str>, failed: bool, names: &BTreeMap<String, String>) -> bool {
    let Some(cond) = cond else {
        return !failed;
    };
    let status = ["always()", "cancelled()", "failure()", "success()"]
        .iter()
        .any(|f| cond.contains(f));
    let holds = holds(
        cond,
        &Context {
            names: names.clone(),
            cancelled: false,
        },
    )
    .expect("a condition");
    holds && (status || !failed)
}

/// What the five steps of `site` do when a step before them failed or not, the cache hits or
/// not, the download (when it runs) succeeds or not, and the check (when it runs) passes or not.
fn simulate(site: &Site, failed_before: bool, hit: bool, download_ok: bool, check_ok: bool) -> Run {
    let stem = site.archive.split('.').next().expect("a stem");
    let cond = |i: usize| entry(&site.steps[site.restore + i], "if").and_then(Node::as_str);
    let mut failed = failed_before;
    let mut names = BTreeMap::new();
    let restore = runs(cond(0), failed, &names);
    if restore {
        names.insert(
            format!("steps.{stem}-cache.outputs.cache-hit"),
            if hit { "true" } else { "false" }.to_owned(),
        );
    }
    let outcome = |ran: bool, ok: bool| match (ran, ok) {
        (false, _) => "skipped",
        (true, true) => "success",
        (true, false) => "failure",
    };
    let download = runs(cond(1), failed, &names);
    failed |= download && !download_ok;
    names.insert(
        format!("steps.{stem}-download.outcome"),
        outcome(download, download_ok).to_owned(),
    );
    let note = runs(cond(2), failed, &names);
    // The check passes only a file that is there: restored on a hit, or downloaded.
    let present = (restore && hit) || (download && download_ok);
    let check = runs(cond(3), failed, &names);
    let check_ok = check_ok && present;
    failed |= check && !check_ok;
    names.insert(
        format!("steps.{stem}-check.outcome"),
        outcome(check, check_ok).to_owned(),
    );
    let save = runs(cond(4), failed, &names);
    Run {
        restore,
        download,
        note,
        check,
        save,
        passes: !failed,
    }
}

/// Proves, for each pinned download of the repository, by its own conditions: a hit downloads
/// nothing, checks the cached file and saves nothing; a miss with GitHub up downloads, checks
/// and saves; a miss with GitHub down (the cold cache in an outage) fails after the note,
/// checking and saving nothing; a file that fails the check, cached or downloaded, fails the job
/// and is not saved; the note runs in no outcome but the failed download. After a failed step
/// before them, a job that goes on (its check guarded by `!cancelled()`) does all of this the
/// same; any other runs none of the five.
/// Catches: a cached pin that still needs GitHub, a file used or cached unchecked, an outage
/// that fails a job without the line saying why, and a job that goes on but skips its cache.
#[test]
fn the_conditions_of_every_pinned_download_hold_in_every_outcome() {
    let r = |restore, download, note, check, save, passes| Run {
        restore,
        download,
        note,
        check,
        save,
        passes,
    };
    let sites = sites();
    assert!(!sites.is_empty());
    for site in &sites {
        let stem = site.archive.split('.').next().expect("a stem");
        let check_if = entry(&site.steps[site.restore + 3], "if").and_then(Node::as_str);
        let go_on = check_if == Some(GO_ON);
        assert_eq!(
            entry(&site.steps[site.restore + 1], "if").and_then(Node::as_str),
            Some(on_miss(stem, go_on).as_str())
        );
        assert_eq!(
            entry(&site.steps[site.restore + 2], "if").and_then(Node::as_str),
            Some(on_download_failure(stem).as_str())
        );
        assert_eq!(
            entry(&site.steps[site.restore + 4], "if").and_then(Node::as_str),
            Some(on_checked(stem).as_str())
        );
        assert_eq!(
            entry(&site.steps[site.restore + 4], "with")
                .and_then(|w| entry(w, "key"))
                .and_then(Node::as_str),
            Some(saved_key(stem).as_str())
        );
        let who = format!("{} {} {}", site.file, site.job, site.archive);
        for failed_before in [false, true] {
            let run = |hit, download_ok, check_ok| {
                simulate(site, failed_before, hit, download_ok, check_ok)
            };
            // A job that does not go on after a failed step skips all five, and stays failed.
            if failed_before && !go_on {
                for (hit, download_ok, check_ok) in [(true, true, true), (false, false, false)] {
                    assert_eq!(
                        run(hit, download_ok, check_ok),
                        r(false, false, false, false, false, false),
                        "{who}: after a failure"
                    );
                }
                continue;
            }
            let passes = !failed_before;
            for download_ok in [true, false] {
                assert_eq!(
                    run(true, download_ok, true),
                    r(true, false, false, true, false, passes),
                    "{who}: hit"
                );
                assert_eq!(
                    run(true, download_ok, false),
                    r(true, false, false, true, false, false),
                    "{who}: bad cache"
                );
            }
            assert_eq!(
                run(false, true, true),
                r(true, true, false, true, true, passes),
                "{who}: miss, GitHub up"
            );
            assert_eq!(
                run(false, true, false),
                r(true, true, false, true, false, false),
                "{who}: miss, bad download"
            );
            // GitHub down: in a job that goes on the check still runs (and fails, with no file);
            // in any other it is skipped. Neither saves.
            for check_ok in [true, false] {
                let ran = run(false, false, check_ok);
                assert_eq!(
                    (
                        ran.restore,
                        ran.download,
                        ran.note,
                        ran.check,
                        ran.save,
                        ran.passes
                    ),
                    (true, true, true, go_on, false, false),
                    "{who}: miss, GitHub down"
                );
            }
        }
    }
}
