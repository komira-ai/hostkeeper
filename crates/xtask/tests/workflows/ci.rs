//! The rules of `ci.yml` ([`FILE`]) beyond those every workflow keeps ([`super::lint`]): the
//! shape of its sharded tests and of the gate that merges them.
//!
//! - Its jobs are exactly [`JOBS`], and only [`GATE_JOB`] has the check name [`CHECK`], the
//!   required check.
//! - [`SHARD_JOB`] is a matrix of exactly `shard: [1, …, SHARDS]` with `fail-fast: false`, on
//!   [`RUNNER`], with its own `postgres` service. Its one step that runs `llvm-cov nextest` is
//!   exactly [`test_run`] (the partition `hash:${{ matrix.shard }}/SHARDS`, so the matrix runs
//!   every half nextest makes and each test once) under `LLVM_PROFILE_FILE_NAME` =
//!   [`PROFILE_FILE_NAME`] (each shard's profiles named after it). After it the job uploads its
//!   profiles ([`PROFILES_PATH`] as [`PROFILES_ARTIFACT`]) and, in the first shard alone
//!   ([`FIRST_SHARD_IF`]), packs the instrumented binaries ([`PACK`]) and uploads the pack
//!   ([`BINARIES_PATH`] as [`BINARIES_ARTIFACT`]).
//! - [`GATE_JOB`] is named [`CHECK`], needs [`SHARD_JOB`] alone, runs under [`GATE_IF`] on
//!   [`RUNNER`], and its first step is [`SHARD_FAILED_RUN`] under [`SHARD_FAILED_IF`]: it runs
//!   whatever the shards did and fails unless every one passed. Before [`REPORT`] it downloads the
//!   profiles of every shard into [`PROFILES_DIR`] and the binaries, which it unpacks
//!   ([`UNPACK`]); then [`GATE`] runs. [`REPORT`] and [`GATE`] are the commands of the one job
//!   before the shards, unchanged.
//! - No job or step of [`SHARD_JOB`] or [`GATE_JOB`] has `continue-on-error`, which would turn a
//!   failed test or gate into a pass; and neither the workflow nor those jobs has `defaults`, nor
//!   a step of theirs `shell` or `working-directory` ([`STEP_KEYS_REFUSED`]), which would change
//!   what a pinned `run` does (`shell: echo {0}` makes `exit 1` pass).
//!
//! A profile or a binary that does not reach the gate can only lower what it measures, so the
//! hand-over is pinned to fail early, in this lint, and not to keep a pass. `ci_tests.rs` shows each
//! rule refusing its defect, and evaluates the gate's conditions against every result of the
//! shards.

use super::lint::{entries, entry};
use super::yaml::{self, Node, Value};

/// The workflow.
pub const FILE: &str = "ci.yml";
/// The format, cargo-deny, clippy and workflow-lint job.
pub const LINT_JOB: &str = "lint";
/// The matrix job of the test shards.
pub const SHARD_JOB: &str = "test-shard";
/// The job that merges the shards' coverage and applies the 99% gate.
pub const GATE_JOB: &str = "coverage";
/// The jobs of [`FILE`], in its order.
pub const JOBS: [&str; 3] = [LINT_JOB, SHARD_JOB, GATE_JOB];
/// The required check of the tests and the gate: [`GATE_JOB`]'s `name`.
pub const CHECK: &str = "test";
/// How many shards the tests are split into.
pub const SHARDS: u32 = 2;
/// The one runner image of the shards and the gate, which reads the binaries the shards build.
pub const RUNNER: &str = "ubuntu-24.04";
/// The `if:` of [`GATE_JOB`]: after the shards, whatever they did, a cancelled run included.
pub const GATE_IF: &str = "${{ always() }}";
/// The `if:` of [`GATE_JOB`]'s first step: a shard did not pass.
pub const SHARD_FAILED_IF: &str = "${{ always() && needs.test-shard.result != 'success' }}";
/// The run of [`GATE_JOB`]'s first step.
pub const SHARD_FAILED_RUN: &str = "exit 1";
/// The name of a shard's profiles, distinct per shard.
pub const PROFILE_FILE_NAME: &str = "hostkeeper-shard-${{ matrix.shard }}-%p-%4m.profraw";
/// The profiles a shard uploads.
pub const PROFILES_PATH: &str = "target/llvm-cov-target/*.profraw";
/// The artifact of a shard's profiles.
pub const PROFILES_ARTIFACT: &str = "coverage-profiles-${{ matrix.shard }}";
/// The artifacts [`GATE_JOB`] downloads the profiles from: every shard's.
pub const PROFILES_PATTERN: &str = "coverage-profiles-*";
/// Where [`GATE_JOB`] puts the profiles: where `llvm-cov report` reads them.
pub const PROFILES_DIR: &str = "target/llvm-cov-target";
/// The `if:` of the steps only the first shard runs.
pub const FIRST_SHARD_IF: &str = "${{ matrix.shard == 1 }}";
/// The first shard's pack of its instrumented binaries.
pub const PACK: &str = "tar --create --zstd --file \"$RUNNER_TEMP/coverage-binaries.tar.zst\" --no-wildcards-match-slash --exclude='*.*' target/llvm-cov-target/debug";
/// The pack, as the upload names it.
pub const BINARIES_PATH: &str = "${{ runner.temp }}/coverage-binaries.tar.zst";
/// The artifact of the pack.
pub const BINARIES_ARTIFACT: &str = "coverage-binaries";
/// Where [`GATE_JOB`] downloads the pack.
pub const BINARIES_DIR: &str = "${{ runner.temp }}";
/// [`GATE_JOB`]'s unpacking of the pack, at the checkout.
pub const UNPACK: &str = "tar --extract --zstd --file \"$RUNNER_TEMP/coverage-binaries.tar.zst\"";
/// The report of the merged profiles.
pub const REPORT: &str = r#"cargo "+$COVERAGE_TOOLCHAIN" llvm-cov report --branch --lcov --disable-default-ignore-filename-regex --ignore-filename-regex '(^|/)(rustc/([0-9a-f]+|[0-9]+\.[0-9]+\.[0-9]+)|tests|benches)/|(^|/)(tests\.rs|[0-9a-zA-Z_-]+[_-]tests\.rs)$|/\.cargo/(registry|git)/|(^|/)target/' --output-path target/coverage/lcov.info"#;
/// The per-crate table and the 99% gate.
pub const GATE: &str = r#"cargo "+$COVERAGE_TOOLCHAIN" run -p coverage-gate --locked -- --lcov target/coverage/lcov.info --root "$GITHUB_WORKSPACE" --summary-file "$GITHUB_STEP_SUMMARY""#;

/// What marks the one step of [`SHARD_JOB`] that runs the tests.
const RUNS_TESTS: &str = "llvm-cov nextest";
const UPLOAD: &str = "actions/upload-artifact@";
const DOWNLOAD: &str = "actions/download-artifact@";

/// The run of the test step: every test of the workspace under coverage, in this shard's half.
pub fn test_run() -> String {
    format!(
        "cargo \"+$COVERAGE_TOOLCHAIN\" llvm-cov nextest --workspace --locked --branch --no-report --no-fail-fast --partition hash:${{{{ matrix.shard }}}}/{SHARDS}"
    )
}

/// The matrix values of [`SHARD_JOB`]: `1` to [`SHARDS`].
pub fn shard_values() -> Vec<String> {
    (1..=SHARDS).map(|k| k.to_string()).collect()
}

/// One line per problem of the shards and the gate in the workflow `text` of [`FILE`].
pub fn problems(text: &str) -> Vec<String> {
    let doc = match yaml::load(text) {
        Ok(doc) => doc,
        Err(e) => return vec![e],
    };
    let jobs = entry(&doc, "jobs").map_or(&[][..], entries);
    let names: Vec<&str> = jobs.iter().filter_map(|(name, _)| name.as_str()).collect();
    let mut problems = Vec::new();
    if entry(&doc, "defaults").is_some() {
        problems.push(
            "defaults is set on the workflow, which changes how every step's run runs".to_owned(),
        );
    }
    if names != JOBS {
        problems.push(format!(
            "jobs are {names:?}, not exactly {JOBS:?} in that order"
        ));
    }
    for (name, job) in jobs {
        let name = name.as_str().unwrap_or_default();
        let check = entry(job, "name").and_then(Node::as_str).unwrap_or(name);
        if name != GATE_JOB && check == CHECK {
            problems.push(format!(
                "job {name}: its check is named {CHECK}, the check of {GATE_JOB} alone"
            ));
        }
    }
    let job = |name: &str| jobs.iter().find(|(k, _)| k.as_str() == Some(name));
    if let Some((_, shard)) = job(SHARD_JOB) {
        problems.extend(shard_problems(shard));
    }
    if let Some((_, gate)) = job(GATE_JOB) {
        problems.extend(gate_problems(gate));
    }
    problems
}

/// The steps of `job`.
pub fn steps(job: &Node) -> &[Node] {
    match entry(job, "steps").map(|s| &s.value) {
        Some(Value::Seq(steps)) => steps,
        _ => &[],
    }
}

/// The text of `key` of `node`.
fn text<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    entry(node, key).and_then(Node::as_str)
}

/// The text of `with.key` of the step `step`.
fn with<'a>(step: &'a Node, key: &str) -> Option<&'a str> {
    entry(step, "with").and_then(|w| text(w, key))
}

/// Whether `step` runs exactly `run`.
fn runs(step: &Node, run: &str) -> bool {
    text(step, "run").map(str::trim_end) == Some(run)
}

/// Whether `step` uses the action `prefix` (`owner/repo@`) and has the `with` values `want`.
fn uses_with(step: &Node, prefix: &str, want: &[(&str, &str)]) -> bool {
    text(step, "uses").is_some_and(|u| u.starts_with(prefix))
        && want.iter().all(|(k, v)| with(step, k) == Some(v))
}

/// The index of the first of `steps` that `holds`.
fn find(steps: &[Node], holds: impl Fn(&Node) -> bool) -> Option<usize> {
    steps.iter().position(holds)
}

/// The keys of a step that change whether its failure counts (`continue-on-error`) or what its
/// `run` does (`shell`, `working-directory`), refused on every step of the shards and the gate.
pub const STEP_KEYS_REFUSED: [&str; 3] = ["continue-on-error", "shell", "working-directory"];

/// The problems of `who` (`job <name>`) having `continue-on-error` or `defaults` on the job, or
/// a key of [`STEP_KEYS_REFUSED`] on a step.
fn continue_problems(who: &str, job: &Node) -> Vec<String> {
    let mut problems = Vec::new();
    for key in ["continue-on-error", "defaults"] {
        if entry(job, key).is_some() {
            problems.push(format!("{who}: {key} is set on the job"));
        }
    }
    for (at, step) in steps(job).iter().enumerate() {
        for key in STEP_KEYS_REFUSED {
            if entry(step, key).is_some() {
                problems.push(format!("{who}: {key} is set on step {}", at + 1));
            }
        }
    }
    problems
}

fn runner_problem(who: &str, job: &Node) -> Option<String> {
    (text(job, "runs-on") != Some(RUNNER)).then(|| {
        format!("{who}: runs-on must be {RUNNER}, the one image of the shards and the gate")
    })
}

/// Whether `strategy` is exactly `fail-fast: false` and `matrix: {shard: [1, …, SHARDS]}`.
fn strategy_ok(strategy: Option<&Node>) -> bool {
    let Some(strategy) = strategy else {
        return false;
    };
    let matrix = entry(strategy, "matrix");
    let shards = matrix
        .and_then(|m| entry(m, "shard"))
        .map(|s| match &s.value {
            Value::Seq(items) => items.iter().map(Node::as_str).collect::<Option<Vec<_>>>(),
            _ => None,
        });
    let want = shard_values();
    entries(strategy).len() == 2
        && text(strategy, "fail-fast") == Some("false")
        && matrix.is_some_and(|m| entries(m).len() == 1)
        && shards.flatten().is_some_and(|s| s == want)
}

fn shard_problems(job: &Node) -> Vec<String> {
    let who = format!("job {SHARD_JOB}");
    let mut problems = Vec::new();
    if !strategy_ok(entry(job, "strategy")) {
        problems.push(format!(
            "{who}: strategy must be exactly fail-fast: false and the matrix shard: [{}]",
            shard_values().join(", ")
        ));
    }
    problems.extend(runner_problem(&who, job));
    if entry(job, "services")
        .and_then(|s| entry(s, "postgres"))
        .is_none()
    {
        problems.push(format!("{who}: no service postgres"));
    }
    let steps = steps(job);
    let tests: Vec<usize> = (0..steps.len())
        .filter(|&at| text(&steps[at], "run").is_some_and(|r| r.contains(RUNS_TESTS)))
        .collect();
    let test = match tests[..] {
        [at] => Some(at),
        _ => {
            problems.push(format!(
                "{who}: {} steps run {RUNS_TESTS}, not one",
                tests.len()
            ));
            None
        }
    };
    if let Some(at) = test {
        let step = &steps[at];
        if !runs(step, &test_run()) {
            problems.push(format!("{who}: the test step must run `{}`", test_run()));
        }
        if entry(step, "env").and_then(|e| text(e, "LLVM_PROFILE_FILE_NAME"))
            != Some(PROFILE_FILE_NAME)
        {
            problems.push(format!(
                "{who}: the test step must set LLVM_PROFILE_FILE_NAME to `{PROFILE_FILE_NAME}`"
            ));
        }
    }
    // The hand-over is judged after the one test step; with none or two, that message alone.
    if let Some(test) = test {
        let after = |at: Option<usize>, before: usize| at.is_some_and(|at| at > before);
        let profiles = find(steps, |s| {
            uses_with(
                s,
                UPLOAD,
                &[("name", PROFILES_ARTIFACT), ("path", PROFILES_PATH)],
            )
        });
        if !after(profiles, test) {
            problems.push(format!(
                "{who}: no upload of {PROFILES_PATH} as {PROFILES_ARTIFACT} after the test step"
            ));
        }
        let first_shard = |s: &Node| text(s, "if") == Some(FIRST_SHARD_IF);
        let pack = find(steps, |s| runs(s, PACK) && first_shard(s));
        if !after(pack, test) {
            problems.push(format!(
                "{who}: no step `{PACK}` under `{FIRST_SHARD_IF}` after the test step"
            ));
        }
        let binaries = find(steps, |s| {
            uses_with(
                s,
                UPLOAD,
                &[("name", BINARIES_ARTIFACT), ("path", BINARIES_PATH)],
            ) && first_shard(s)
        });
        if !pack.is_some_and(|pack| after(binaries, pack)) {
            problems.push(format!(
                "{who}: no upload of {BINARIES_PATH} as {BINARIES_ARTIFACT} under `{FIRST_SHARD_IF}` after the pack"
            ));
        }
    }
    problems.extend(continue_problems(&who, job));
    problems
}

/// Whether `needs` is exactly [`SHARD_JOB`], as a list of one or a scalar.
fn needs_shards(needs: Option<&Node>) -> bool {
    match needs.map(|n| &n.value) {
        Some(Value::Seq(items)) => {
            matches!(&items[..], [only] if only.as_str() == Some(SHARD_JOB))
        }
        Some(_) => needs.and_then(Node::as_str) == Some(SHARD_JOB),
        None => false,
    }
}

fn gate_problems(job: &Node) -> Vec<String> {
    let who = format!("job {GATE_JOB}");
    let mut problems = Vec::new();
    if text(job, "name") != Some(CHECK) {
        problems.push(format!("{who}: name must be {CHECK}, the required check"));
    }
    if !needs_shards(entry(job, "needs")) {
        problems.push(format!("{who}: needs must be [{SHARD_JOB}] alone"));
    }
    if text(job, "if") != Some(GATE_IF) {
        problems.push(format!("{who}: if must be {GATE_IF}"));
    }
    if entry(job, "strategy").is_some() {
        problems.push(format!(
            "{who}: has a strategy, so it is more than one check"
        ));
    }
    problems.extend(runner_problem(&who, job));
    let steps = steps(job);
    let first_fails = steps
        .first()
        .is_some_and(|s| runs(s, SHARD_FAILED_RUN) && text(s, "if") == Some(SHARD_FAILED_IF));
    if !first_fails {
        problems.push(format!(
            "{who}: its first step must be `{SHARD_FAILED_RUN}` under `{SHARD_FAILED_IF}`"
        ));
    }
    let profiles = find(steps, |s| {
        uses_with(
            s,
            DOWNLOAD,
            &[
                ("pattern", PROFILES_PATTERN),
                ("merge-multiple", "true"),
                ("path", PROFILES_DIR),
            ],
        )
    });
    let binaries = find(steps, |s| {
        uses_with(
            s,
            DOWNLOAD,
            &[("name", BINARIES_ARTIFACT), ("path", BINARIES_DIR)],
        )
    });
    let unpack = find(steps, |s| runs(s, UNPACK));
    let report = find(steps, |s| runs(s, REPORT));
    let gate = find(steps, |s| runs(s, GATE));
    let before = |at: Option<usize>, then: usize| at.is_some_and(|at| at < then);
    let Some(report) = report else {
        // The order of the rest is judged against the report; with none, that message alone.
        problems.push(format!("{who}: no step runs `{REPORT}`"));
        problems.extend(continue_problems(&who, job));
        return problems;
    };
    if gate.is_none_or(|gate| gate <= report) {
        problems.push(format!("{who}: no step runs `{GATE}` after the report"));
    }
    if !before(profiles, report) {
        problems.push(format!(
            "{who}: no download of {PROFILES_PATTERN} (merge-multiple) into {PROFILES_DIR} before the report"
        ));
    }
    if !(unpack.is_some_and(|unpack| before(binaries, unpack)) && before(unpack, report)) {
        problems.push(format!(
            "{who}: no download of {BINARIES_ARTIFACT} into {BINARIES_DIR}, then `{UNPACK}`, before the report"
        ));
    }
    problems.extend(continue_problems(&who, job));
    problems
}
