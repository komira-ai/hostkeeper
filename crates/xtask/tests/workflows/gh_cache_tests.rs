//! The rule of [`super::gh_cache`]: every download of the pinned gh is the middle of five steps,
//! restore from the cache, download on a miss, the one-line note on a failed download, the check
//! against the pin, and the save on a miss. Each case puts the five steps (or a defect of them)
//! into [`super::CLEAN`] in place of its test step, the download's `run:` on line 23, and reads the exact
//! problems; the last cases run the conditions of the repository's own steps against every
//! outcome of the cache and the download.

use std::collections::BTreeMap;

use super::expr::{Context, holds};
use super::gh_cache::{ON_DOWNLOAD_FAILURE, ON_MISS};
use super::lint::entry;
use super::yaml::{self, Node, Value};
use super::{PRIVILEGED_WORKFLOW, assert_problems, lint, with, workflows, workflows_dir};

const CACHE: &str = "55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0";

const RESTORE: &str = "      - id: gh-cache
        uses: actions/cache/restore@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
        with:
          path: ${{ runner.temp }}/gh.tar.gz
          key: gh-${{ env.GH_VERSION }}-${{ env.GH_SHA256 }}
";

const DOWNLOAD: &str = "      - id: gh-download
        if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}
        run: curl --proto =https --proto-redir =https --tlsv1.2 --fail --retry 5 --retry-delay 5 --retry-max-time 300 --retry-all-errors --silent --show-error --location --output \"$RUNNER_TEMP/gh.tar.gz\" \"https://github.com/cli/cli/releases/download/v$GH_VERSION/gh_${GH_VERSION}_linux_amd64.tar.gz\"
";

const NOTE: &str = "      - if: ${{ !cancelled() && steps.gh-download.outcome == 'failure' }}
        run: echo \"::error::gh $GH_VERSION is not cached and GitHub did not serve it\"
";

const CHECK: &str = "      - run: cargo xtask install-tool --download \"$RUNNER_TEMP/gh.tar.gz\" --sha256 \"$GH_SHA256\" --out target/gh --extract tar.gz --member gh
";

const SAVE: &str = "      - if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}
        uses: actions/cache/save@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
        with:
          path: ${{ runner.temp }}/gh.tar.gz
          key: ${{ steps.gh-cache.outputs.cache-primary-key }}
";

/// The problem of each of the five roles, for the download on line 23.
const WHO: &str = "job test: the gh download (line 23)";
const NOT_RESTORED: &str = "job test: the gh download (line 23): the step before it is not `actions/cache/restore@` with `id: gh-cache`, `path: ${{ runner.temp }}/gh.tar.gz` and `key: gh-${{ env.GH_VERSION }}-${{ env.GH_SHA256 }}`";
const NOT_ON_MISS: &str = "job test: the gh download (line 23) lacks `id: gh-download` and `if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}`";
const NOT_NOTED: &str = "job test: the gh download (line 23): the step after it is not the note `if: ${{ !cancelled() && steps.gh-download.outcome == 'failure' }}` with a one-line `run: echo \"::error::…\"`";
const NOT_CHECKED: &str = "job test: the gh download (line 23): the second step after it is not the check `cargo xtask install-tool --download \"$RUNNER_TEMP/gh.tar.gz\" --sha256 \"$<NAME>\"` with no `if:`";
const NOT_SAVED: &str = "job test: the gh download (line 23): the third step after it is not `actions/cache/save@` with `if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}`, `path: ${{ runner.temp }}/gh.tar.gz` and `key: ${{ steps.gh-cache.outputs.cache-primary-key }}`";

/// [`super::CLEAN`] whose test step is `steps`.
fn job(steps: &str) -> String {
    with("      - run: cargo test --workspace --locked\n", steps)
}

/// The five steps, `from` replaced by `to` once in their text.
fn five(from: &str, to: &str) -> String {
    let all = [RESTORE, DOWNLOAD, NOTE, CHECK, SAVE].concat();
    if from.is_empty() {
        return job(&all);
    }
    assert_eq!(
        all.matches(from).count(),
        1,
        "{from:?} is not in the steps once"
    );
    job(&all.replacen(from, to, 1))
}

/// Proves: the five steps pass, also with the check's SHA-256 from its own `env:` (artifacts.yml's
/// matrix pin, `${{ matrix.gh-sha256 }}`, in the key) and as `${GH_SHA256}`, and a curl of
/// another release with no cache steps is not a gh download.
/// Catches: a rule that refuses the shapes the workflows use.
#[test]
fn the_five_steps_pass_with_the_pin_from_the_job_or_the_step() {
    assert_problems(&five("", ""), &[]);
    assert_problems(&five("\"$GH_SHA256\"", "\"${GH_SHA256}\""), &[]);
    let matrix = [
        RESTORE.replace("${{ env.GH_SHA256 }}", "${{ matrix.gh-sha256 }}"),
        DOWNLOAD.to_owned(),
        NOTE.to_owned(),
        CHECK.replace("$GH_SHA256", "$SHA256").replace(
            "      - run:",
            "      - env:\n          SHA256: ${{ matrix.gh-sha256 }}\n        run:",
        ),
        SAVE.to_owned(),
    ]
    .concat();
    assert_problems(&job(&matrix), &[]);
    assert_problems(&job(&DOWNLOAD.replace("cli/cli", "goreleaser/nfpm")), &[]);
}

/// Proves: a gh download with no step before it, or as the last step, is refused for each of the
/// four steps it lacks, and the problem of an absent check names the key unknown.
/// Catches: a download that fails the job on GitHub's next outage, or whose archive is used
/// unchecked, at the edge of the job's steps.
#[test]
fn a_gh_download_alone_lacks_each_of_the_other_four_steps() {
    assert_problems(
        &job(DOWNLOAD),
        &[
            NOT_CHECKED.replace("line 23", "line 18").as_str(),
            "job test: the gh download (line 18): the step before it is not `actions/cache/restore@` with `id: gh-cache`, `path: ${{ runner.temp }}/gh.tar.gz` and `key: gh-${{ env.GH_VERSION }}-<the check's SHA-256>`",
            NOT_NOTED.replace("line 23", "line 18").as_str(),
            NOT_SAVED.replace("line 23", "line 18").as_str(),
        ],
    );
}

/// Proves: a restore that is not the restore action (the full `actions/cache`, which would save
/// at the job's end, checked or not), has another id, another path, or a key without the version
/// or the check's SHA-256, is refused.
/// Catches: a cache under a key another pin shares, a file the download does not write, or a save
/// that does not wait for the check.
#[test]
fn a_restore_of_another_shape_is_refused() {
    for (from, to) in [
        ("actions/cache/restore@", "actions/cache@"),
        ("id: gh-cache", "id: cache"),
        (
            "path: ${{ runner.temp }}/gh.tar.gz\n          key: gh-",
            "path: ${{ runner.temp }}/gh.zip\n          key: gh-",
        ),
        ("key: gh-${{ env.GH_VERSION }}-", "key: gh-"),
        (
            "key: gh-${{ env.GH_VERSION }}-${{ env.GH_SHA256 }}",
            "key: gh-${{ env.GH_VERSION }}-${{ env.NFPM_SHA256 }}",
        ),
    ] {
        assert_problems(&five(from, to), &[NOT_RESTORED]);
    }
    // The key of a check whose SHA-256 is another variable is that variable's.
    assert_problems(
        &five("--sha256 \"$GH_SHA256\"", "--sha256 \"$NFPM_SHA256\""),
        &[&NOT_RESTORED.replace("env.GH_SHA256", "env.NFPM_SHA256")],
    );
}

/// Proves: a download that runs on a cache hit too (no `if:`, or another one) or has another id
/// is refused; so is one that is not one curl to `$RUNNER_TEMP/<file>`, alone of all problems.
/// Catches: a job that still fails when GitHub is down though the pin is cached, a note that
/// never runs, or an archive the cache steps do not name.
#[test]
fn a_download_that_runs_on_a_hit_or_writes_elsewhere_is_refused() {
    assert_problems(
        &five(
            "        if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}\n        run: curl",
            "        run: curl",
        ),
        &[NOT_ON_MISS.replace("line 23", "line 22").as_str()],
    );
    assert_problems(&five("- id: gh-download", "- id: download"), &[NOT_ON_MISS]);
    let elsewhere = format!("{WHO} is not one curl with `--output \"$RUNNER_TEMP/<file>\"`");
    for (from, to) in [
        ("--output \"$RUNNER_TEMP/gh.tar.gz\"", "--output gh.tar.gz"),
        (
            "--output \"$RUNNER_TEMP/gh.tar.gz\"",
            "--output \"$RUNNER_TEMP/\"",
        ),
        (
            "--output \"$RUNNER_TEMP/gh.tar.gz\"",
            "--output \"$RUNNER_TEMP/d/gh.tar.gz\"",
        ),
        ("--output \"$RUNNER_TEMP/gh.tar.gz\"", "--remote-name"),
        ("run: curl", "run: mkdir -p d && curl"),
    ] {
        assert_problems(&five(from, to), &[&elsewhere]);
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
            "        run: echo \"::error::gh $GH_VERSION is not cached and GitHub did not serve it\"\n",
            "        run: |\n          echo \"::error::gh is not cached\"\n          echo and GitHub did not serve it\n",
        ),
        ("run: echo \"::error::", "run: echo \"::warning::"),
    ] {
        assert_problems(&five(from, to), &[NOT_NOTED]);
    }
}

/// Proves: a check with an `if:` (skipped on a hit, so a cached archive is used unchecked), of
/// another file, after another command, or with a SHA-256 that is not a
/// variable, is refused, and the restore's key is then not judged; a step between the download
/// and the check is refused as each shifted step.
/// Catches: an archive from the cache or the download used before it is checked against the pin.
#[test]
fn a_check_that_may_not_run_or_checks_something_else_is_refused() {
    for (from, to) in [
        (
            "      - run: cargo xtask install-tool",
            "      - if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}\n        run: cargo xtask install-tool",
        ),
        (
            "--download \"$RUNNER_TEMP/gh.tar.gz\" --sha256",
            "--download \"$RUNNER_TEMP/gh.zip\" --sha256",
        ),
        (
            "      - run: cargo xtask install-tool",
            "      - run: true && cargo xtask install-tool",
        ),
        (
            "--sha256 \"$GH_SHA256\"",
            "--sha256 77b9e297e94c50336823d0f84de7ef1be60cba704601fa0d98f129f47a94987b",
        ),
        ("--sha256 \"$GH_SHA256\"", "--sha256 \"$\""),
        ("--sha256 \"$GH_SHA256\"", "--sha256 \"${GH-SHA}\""),
    ] {
        assert_problems(&five(from, to), &[NOT_CHECKED]);
    }
    // gh's archive unpacked before the check: the note, the check and the save each one late.
    let early = format!(
        "{RESTORE}{DOWNLOAD}      - run: tar -xzf \"$RUNNER_TEMP/gh.tar.gz\"\n{NOTE}{CHECK}{SAVE}"
    );
    assert_problems(&job(&early), &[NOT_CHECKED, NOT_NOTED, NOT_SAVED]);
}

/// Proves: a save on every outcome (no `if:`), before the check, of another path, or under a key
/// other than the restore's, is refused, and so is the full cache action in its place.
/// Catches: a cache entry written for an archive the check refused, or under a key no restore
/// looks up.
#[test]
fn a_save_of_another_shape_is_refused() {
    let save_if = "      - if: ${{ steps.gh-cache.outputs.cache-hit != 'true' }}\n        uses: actions/cache/save@";
    for (from, to) in [
        (save_if, "      - uses: actions/cache/save@"),
        (
            save_if,
            "      - if: ${{ always() }}\n        uses: actions/cache/save@",
        ),
        ("actions/cache/save@", "actions/cache@"),
        (
            "path: ${{ runner.temp }}/gh.tar.gz\n          key: ${{ steps",
            "path: ${{ runner.temp }}/gh\n          key: ${{ steps",
        ),
        (
            "key: ${{ steps.gh-cache.outputs.cache-primary-key }}",
            "key: gh-${{ env.GH_VERSION }}",
        ),
    ] {
        assert_problems(&five(from, to), &[NOT_SAVED]);
    }
    let first = format!("{RESTORE}{DOWNLOAD}{NOTE}{SAVE}{CHECK}");
    assert_problems(&job(&first), &[NOT_CHECKED, NOT_SAVED]);
}

/// Every step sequence of the repository's workflows that downloads gh, as `(file, job, steps)`.
fn gh_jobs() -> Vec<(String, String, Vec<Node>)> {
    let mut found = Vec::new();
    for (file, text) in workflows() {
        let doc = yaml::load(&text).expect("a workflow");
        for (name, job) in entry(&doc, "jobs").map_or(&[][..], super::lint::entries) {
            let Some(Value::Seq(steps)) = entry(job, "steps").map(|s| s.value.clone()) else {
                continue;
            };
            let downloads = steps.iter().any(|s| {
                entry(s, "run")
                    .and_then(Node::as_str)
                    .is_some_and(|r| r.contains(super::gh_cache::GH_RELEASES))
            });
            if downloads {
                found.push((
                    file.clone(),
                    name.as_str().unwrap_or_default().to_owned(),
                    steps,
                ));
            }
        }
    }
    found
}

/// Proves: the jobs that download gh are the eight of the four workflows that build or test with
/// it (both builds of `artifacts.yml`, the Linux and macOS device jobs of `privileged.yml` and of
/// their copies in `pr-device.yml`, and both network-install jobs), and `privileged.yml` with the
/// restore of its Linux job taken out fails the lint on that job's download.
/// Catches: a job that downloads gh and slips past the rule, and a rule that does not read the
/// repository's own workflows.
#[test]
fn every_gh_download_of_the_repository_is_held_to_the_rule() {
    let jobs: Vec<(String, String)> = gh_jobs()
        .into_iter()
        .map(|(file, job, _)| (file, job))
        .collect();
    let want = [
        ("artifacts.yml", "build"),
        ("artifacts.yml", "build-macos"),
        ("net-install.yml", "net-install"),
        ("net-install.yml", "net-install-arm64"),
        ("pr-device.yml", "macos-device"),
        ("pr-device.yml", "privileged"),
        ("privileged.yml", "privileged"),
        ("privileged.yml", "macos-device"),
    ];
    let mut want: Vec<(String, String)> = want
        .iter()
        .map(|(f, j)| ((*f).to_owned(), (*j).to_owned()))
        .collect();
    let mut sorted = jobs.clone();
    sorted.sort();
    want.sort();
    assert_eq!(sorted, want);

    let text = std::fs::read_to_string(workflows_dir().join(PRIVILEGED_WORKFLOW)).expect("read");
    assert_eq!(lint(PRIVILEGED_WORKFLOW, &text), Vec::<String>::new());
    let restore = format!(
        "      - name: Restore the pinned gh archive from the cache\n        id: gh-cache\n        uses: actions/cache/restore@{CACHE}\n        with:\n          path: ${{{{ runner.temp }}}}/gh.tar.gz\n          key: gh-${{{{ env.GH_VERSION }}}}-${{{{ env.GH_SHA256 }}}}\n"
    );
    assert_eq!(text.matches(&restore).count(), 1, "privileged's restore");
    let changed = text.replacen(&restore, "", 1);
    let download = changed
        .find("--output \"$RUNNER_TEMP/gh.tar.gz\"")
        .expect("the download");
    let line = changed[..download].lines().count();
    assert_eq!(
        lint(PRIVILEGED_WORKFLOW, &changed),
        vec![format!(
            "job privileged: the gh download (line {line}): the step before it is not `actions/cache/restore@` with `id: gh-cache`, `path: ${{{{ runner.temp }}}}/gh.tar.gz` and `key: gh-${{{{ env.GH_VERSION }}}}-${{{{ env.GH_SHA256 }}}}`"
        )]
    );
}

/// What the five steps do in one outcome.
#[derive(Debug, PartialEq)]
struct Run {
    download: bool,
    note: bool,
    check: bool,
    save: bool,
    passes: bool,
}

/// Whether the step with condition `cond` runs: GitHub puts `success() &&` before a condition
/// that names no status function, so such a step runs only when every step before it passed.
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

/// What the five steps of `steps`, from the restore at `at`, do when the cache `hit` or not, the
/// download (when it runs) succeeds or not, and the check (when it runs) passes or not.
fn simulate(steps: &[Node], at: usize, hit: bool, download_ok: bool, check_ok: bool) -> Run {
    let cond = |i: usize| entry(&steps[at + i], "if").and_then(Node::as_str);
    let mut names = BTreeMap::from([(
        "steps.gh-cache.outputs.cache-hit".to_owned(),
        if hit { "true" } else { "false" }.to_owned(),
    )]);
    let mut failed = false;
    let download = runs(cond(1), failed, &names);
    let outcome = match (download, download_ok) {
        (false, _) => "skipped",
        (true, true) => "success",
        (true, false) => "failure",
    };
    failed |= outcome == "failure";
    names.insert("steps.gh-download.outcome".to_owned(), outcome.to_owned());
    let note = runs(cond(2), failed, &names);
    let check = runs(cond(3), failed, &names);
    failed |= check && !check_ok;
    let save = runs(cond(4), failed, &names);
    Run {
        download,
        note,
        check,
        save,
        passes: !failed,
    }
}

/// Proves, for each of the eight jobs that download gh, by its own conditions: a cache hit
/// downloads nothing, checks the cached archive and saves nothing; a miss with GitHub up
/// downloads, checks, saves, and passes; a miss with GitHub down (the cold cache in an outage)
/// fails after the note, checking and saving nothing; an archive that fails the check, cached or
/// downloaded, fails the job and is not saved; the note runs in no outcome but the failed
/// download.
/// Catches: a cached pin that still needs GitHub, an archive used or cached unchecked, and an
/// outage that fails a job without the line saying why.
#[test]
fn the_conditions_of_every_gh_download_hold_in_every_outcome() {
    assert!(ON_MISS.contains("cache-hit") && ON_DOWNLOAD_FAILURE.contains("outcome"));
    for (file, job, steps) in gh_jobs() {
        let at = steps
            .iter()
            .position(|s| entry(s, "id").and_then(Node::as_str) == Some("gh-cache"))
            .unwrap_or_else(|| panic!("{file} {job}: the restore"));
        let run = |hit, download_ok, check_ok| simulate(&steps, at, hit, download_ok, check_ok);
        let r = |download, note, check, save, passes| Run {
            download,
            note,
            check,
            save,
            passes,
        };
        let who = format!("{file} {job}");
        for download_ok in [true, false] {
            assert_eq!(
                run(true, download_ok, true),
                r(false, false, true, false, true),
                "{who}: hit"
            );
            assert_eq!(
                run(true, download_ok, false),
                r(false, false, true, false, false),
                "{who}: bad cache"
            );
        }
        assert_eq!(
            run(false, true, true),
            r(true, false, true, true, true),
            "{who}: miss, GitHub up"
        );
        assert_eq!(
            run(false, true, false),
            r(true, false, true, false, false),
            "{who}: miss, bad download"
        );
        for check_ok in [true, false] {
            assert_eq!(
                run(false, false, check_ok),
                r(true, true, false, false, false),
                "{who}: miss, GitHub down"
            );
        }
    }
}
