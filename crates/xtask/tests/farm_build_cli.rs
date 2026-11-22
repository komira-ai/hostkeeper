//! `cargo xtask farm-build`, end to end: the verb is run as a process in a git repository of its
//! own, with `--buck2` naming a copy of the fixture (tests/bin/buck2_fixture.rs) in a directory of
//! its own, whose files say what each buck2 call answers; a second copy beside it, named
//! `buildrules`, is the command line that buck2 names, over a table of its own in the repository.
//! Each test reads the verb's exit status and words, the calls the two recorded, and the patch
//! and the job summary the verb wrote. Nothing here reaches a network or a build farm.

#[path = "../src/testdir.rs"]
mod testdir;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sha2::Digest as _;
use testdir::TestDir;

/// What buck2 prints for a key that is set.
const SET: &str = r#"{"buildrules_re.linux_x86_64_properties":"OSFamily=Linux,ISA=x86-64"}"#;
/// A farm build's summary line.
const FARM: &str = "Commands: 474 (cached: 0, remote: 474, local: 0)";
/// The audit call.
const AUDIT: &str = "audit config buildrules_re.linux_x86_64_properties --style json";
/// The build of the command line.
const TOOL: &str = "build buildrules//cli:buildrules --show-full-output";
/// The calls of a run that got as far as the build.
const BUILT: [&str; 4] = [AUDIT, TOOL, "build //:ci", "kill"];
/// The calls of a run that built twice.
const BUILT_TWICE: [&str; 5] = [AUDIT, TOOL, "build //:ci", "build //:ci", "kill"];
/// The calls of a run that stopped at the table.
const STOPPED: [&str; 3] = [AUDIT, TOOL, "kill"];
/// The run and the repository the handoff names.
const RUN: &str = "42";
const REPO: &str = "komira-ai/hostkeeper";
/// The artifact and the file of the patch (crates/xtask/src/farm/table.rs).
const PATCH_ARTIFACT: &str = "buildrules-sync";
const PATCH_FILE: &str = "buildrules-sync.patch";
/// The table of the stand-in, and its one other generated file.
const TABLE: &str = "buildrules.lock.bzl";
const ARCHIVES: &str = "third_party/buildrules/git_archives.tsv";
/// A dependency that gives the workspace a git source.
const GIT_DEP: &str =
    "\n[workspace.dependencies]\nr = { git = \"https://github.com/o/r\", rev = \"0123abc\" }\n";
/// The fix the real command line ends each of its lines with.
const FIX: &str = "run `buildrules sync` and commit the result";

/// The log of a passing build.
const BUILD_LOG: &str = "[t] Waiting on 3 actions\n[t] Commands: 474 (cached: 0, remote: 474, local: 0)\nBUILD SUCCEEDED\n";
/// The answers of a passing build.
const PASSING: [(&str, &str); 2] = [("audit.stdout", SET), ("build.stderr", BUILD_LOG)];

/// Recorded farm logs (crates/xtask/src/farm/cuts/): a build whose one failure was a cut, one with
/// a real failure beside its cuts, and one whose test failed.
const CUT_LOG: &str = include_str!("../src/farm/cuts/build_cut.log");
const CUT_AND_FAILED_LOG: &str = include_str!("../src/farm/cuts/build_cut_and_failed_check.log");
const TEST_FAILED_LOG: &str = include_str!("../src/farm/cuts/test_failed.log");

/// The line the log and the job summary get before the second run of a build cut twice in its
/// recorded log (crates/xtask/src/farm.rs).
const RERUN: &str = "xtask: every failure of `buck2 build //:ci` was a cut connection to the farm \
                     (2 cut messages); running it once more\n";

/// A git repository with a small workspace whose stand-in table is fresh and committed, the
/// fixture's copies in a directory beside it, and a directory for the patch and the summary.
struct Rig {
    repo: TestDir,
    bin: TestDir,
    out: TestDir,
}

/// `git <args>` in `dir`, which must pass: its standard output.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "commit.gpgsign=false"])
        .envs([
            ("GIT_AUTHOR_NAME", "t"),
            ("GIT_AUTHOR_EMAIL", "t@example.com"),
            ("GIT_COMMITTER_NAME", "t"),
            ("GIT_COMMITTER_EMAIL", "t@example.com"),
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A directory with the fixture as `buck2` and as `buildrules`, and `answers` (file name, text).
fn bin(name: &str, answers: &[(&str, &str)]) -> TestDir {
    let bin = TestDir::under(
        Path::new(env!("CARGO_TARGET_TMPDIR")),
        &format!("farm-build-bin-{name}"),
    );
    for program in ["buck2", "buildrules"] {
        std::fs::copy(env!("CARGO_BIN_EXE_xtask-buck2-fixture"), bin.join(program))
            .expect("the fixture");
    }
    for (file, text) in answers {
        std::fs::write(bin.join(file), text).expect("an answer");
    }
    bin
}

impl Rig {
    /// A fresh rig named `name`, its workspace given a git dependency when `git`, with `answers`
    /// for the fixture.
    fn new(name: &str, git_dep: bool, answers: &[(&str, &str)]) -> Rig {
        let base = Path::new(env!("CARGO_TARGET_TMPDIR"));
        let repo = TestDir::under(base, &format!("farm-build-repo-{name}"));
        let out = TestDir::under(base, &format!("farm-build-out-{name}"));
        let bin = bin(name, answers);
        let rig = Rig { repo, bin, out };
        git(&rig.repo, &["init", "--quiet", "-b", "main"]);
        rig.write(
            "Cargo.toml",
            &format!(
                "[workspace]\nmembers = []\n{}",
                if git_dep { GIT_DEP } else { "" }
            ),
        );
        rig.write("Cargo.lock", "version = 4\n");
        rig.write("README.md", "docs\n");
        assert_eq!(rig.buildrules(&rig.repo, "sync"), Some(0));
        std::fs::remove_file(rig.bin.join("buildrules.calls")).expect("calls");
        rig.commit("the table");
        rig
    }

    /// Writes `text` to `rel` in the repository.
    fn write(&self, rel: &str, text: &str) {
        let path = self.repo.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
        std::fs::write(path, text).expect("write");
    }

    /// The text of `rel` in the repository.
    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.repo.join(rel)).expect("read")
    }

    /// Commits everything, as `message`.
    fn commit(&self, message: &str) {
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "--quiet", "-m", message]);
    }

    /// Appends `text` to the root manifest.
    fn append_manifest(&self, text: &str) {
        let manifest = self.read("Cargo.toml");
        self.write("Cargo.toml", &format!("{manifest}{text}"));
    }

    fn buck2(&self) -> PathBuf {
        self.bin.join("buck2")
    }

    fn patch(&self) -> PathBuf {
        self.out.join("patch").join(PATCH_FILE)
    }

    fn summary(&self) -> PathBuf {
        self.out.join("summary.md")
    }

    /// Asks the stand-in buildrules for `verb` in `cwd`: its exit status.
    fn buildrules(&self, cwd: &Path, verb: &str) -> Option<i32> {
        Command::new(self.bin.join("buildrules"))
            .arg(verb)
            .current_dir(cwd)
            .output()
            .expect("run the stand-in")
            .status
            .code()
    }

    /// Asks for a failure of the stand-in buildrules.
    fn defect(&self, defect: &str) {
        std::fs::write(self.bin.join("defect"), defect).expect("defect");
    }

    /// The farm build in the repository, with the handoff's flags.
    fn farm_build(&self) -> Output {
        xtask(
            &self.repo,
            &args(&self.buck2(), &self.patch(), &self.summary()),
        )
    }

    /// The calls buck2 got.
    fn calls(&self) -> Vec<String> {
        calls_in(&self.bin, "calls")
    }

    /// The calls buildrules got.
    fn tool_calls(&self) -> Vec<String> {
        calls_in(&self.bin, "buildrules.calls")
    }
}

/// The flags of a farm build of `//:ci`.
fn args(buck2: &Path, patch: &Path, summary: &Path) -> Vec<String> {
    let path = |p: &Path| p.to_str().expect("utf-8").to_owned();
    [
        "farm-build",
        "--buck2",
        &path(buck2),
        "--target",
        "//:ci",
        "--patch",
        &path(patch),
        "--summary",
        &path(summary),
        "--repo",
        REPO,
        "--run-id",
        RUN,
    ]
    .map(str::to_owned)
    .to_vec()
}

/// `xtask <args>` in `cwd`.
fn xtask(cwd: &Path, args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run xtask")
}

/// The calls a fixture recorded in the file `name` of `dir`, one per line.
fn calls_in(dir: &Path, name: &str) -> Vec<String> {
    std::fs::read_to_string(dir.join(name))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// The sha256 of `text`, in hex.
fn sha256(text: &str) -> String {
    hex::encode(sha2::Sha256::digest(text))
}

/// The command the summary gives.
fn apply() -> String {
    format!(
        "gh run download {RUN} -R {REPO} -n {PATCH_ARTIFACT} -D buck-out/{PATCH_ARTIFACT}-{RUN} \
         && git apply buck-out/{PATCH_ARTIFACT}-{RUN}/{PATCH_FILE}"
    )
}

/// The job summary of a stale table (crates/xtask/src/farm/table.rs, `summary_markdown`).
fn stale_summary(inputs: &[String], files: &[String]) -> String {
    let inputs: String = inputs.iter().map(|i| format!("- {i}\n")).collect();
    let files: String = files.iter().map(|f| format!("- `{f}`\n")).collect();
    format!(
        "## The generated build table is stale\n\n\
         `buildrules check` found inputs of the generated build table changed since it was made:\n\n\
         {inputs}\n\
         `buildrules.lock.bzl`, `buildrules.bzl` and `third_party/buildrules/` are generated from \
         the Cargo files (`Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`) by `buildrules sync`, \
         and nobody edits them by hand. This job ran the pinned `buildrules sync` and uploaded \
         what it changed as the artifact `{PATCH_ARTIFACT}`, a patch of these files:\n\n\
         {files}\n\
         Read it, then apply it in a checkout of the branch, commit and push. Nothing runs on \
         your machine but `gh` and `git`:\n\n\
         ```sh\n{}\n```\n\n\
         If `git apply` refuses it, main has changed the table too: merge main into the branch \
         and push, and the next run carries a new patch.\n",
        apply()
    )
}

#[test]
fn a_farm_build_with_a_fresh_table_prints_its_summary_line_and_stops_buck2_with_status_0() {
    // Catches: the build not run, run before the worker check or the table's check, its output
    // not passed on, its summary not printed, buck2's daemon left running, or a fresh table
    // synced or handed over as a patch.
    let mut answers = PASSING.to_vec();
    answers.push(("build.stdout", "out\n"));
    let d = Rig::new("ok", false, &answers);
    let o = d.farm_build();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), format!("{FARM}\n"));
    assert_eq!(stderr(&o), format!("out\n{BUILD_LOG}"));
    assert_eq!(d.calls(), BUILT);
    assert_eq!(d.tool_calls(), ["check"]);
    assert!(!d.patch().exists());
    assert!(!d.summary().exists());
}

/// The stale table of `d` handed over: the verb fails before the build with the stale inputs
/// `inputs` and the changed files `files` in its words and in the summary (after what the summary
/// held), and the patch it wrote, applied to a clone of the stale commit, makes the table fresh
/// and the farm build pass there.
fn hands_over(d: &Rig, inputs: &[String], files: &[&str]) {
    std::fs::create_dir_all(&*d.out).expect("out");
    std::fs::write(d.summary(), "earlier\n").expect("summary");
    let o = d.farm_build();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stdout(&o), "");
    let files: Vec<String> = files.iter().map(|f| (*f).to_owned()).collect();
    assert_eq!(
        stderr(&o),
        format!(
            "buildrules: wrote {TABLE}\nxtask: the generated build table is stale: {}. \
             buildrules.lock.bzl, buildrules.bzl and third_party/buildrules/ are generated from \
             the Cargo files by `buildrules sync`, which this job ran at the pinned version; what \
             it changed ({}) is the artifact {PATCH_ARTIFACT} of this run, made against this \
             run's merge of the branch into main. Apply it in the branch's checkout, then commit \
             and push (if it does not apply, merge main first): {}\n",
            inputs.join("; "),
            files.join(", "),
            apply()
        )
    );
    assert_eq!(d.calls(), STOPPED);
    assert_eq!(
        d.tool_calls(),
        [
            "check".to_owned(),
            format!("sync --buck2 {}", d.buck2().display()),
            "check".to_owned()
        ]
    );
    assert_eq!(
        std::fs::read_to_string(d.summary()).expect("summary"),
        format!("earlier\n{}", stale_summary(inputs, &files))
    );

    // The patch, on the stale commit as the pull request's branch has it.
    let clone = TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "farm-build-clone");
    let tree = clone.join("tree");
    git(
        &d.repo,
        &["clone", "--quiet", ".", tree.to_str().expect("utf-8")],
    );
    assert_eq!(d.buildrules(&tree, "check"), Some(1), "the clone is stale");
    let applied = Command::new("git")
        .arg("apply")
        .arg(d.patch())
        .current_dir(&tree)
        .output()
        .expect("git apply");
    assert!(applied.status.success(), "{applied:?}");
    assert_eq!(
        d.buildrules(&tree, "check"),
        Some(0),
        "the patch makes it fresh"
    );
    let again = bin("clone", &PASSING);
    let o = xtask(
        &tree,
        &args(&again.join("buck2"), &clone.join("p"), &clone.join("s")),
    );
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), format!("{FARM}\n"));
    assert_eq!(calls_in(&again, "calls"), BUILT);
}

/// The stale line of the stand-in for the file `input`, from the text `was` to the text `now`.
fn changed(input: &str, was: &str, now: &str) -> String {
    format!(
        "{input} changed since `buildrules sync` (recorded sha256 {}, now {})",
        sha256(was),
        sha256(now)
    )
}

#[test]
fn a_changed_manifest_hands_over_a_patch_that_changes_the_table_and_adds_a_file() {
    // Catches: a stale table built (or failed without a fix), sync not run or run without the
    // job's buck2, the second check skipped, a patch that leaves out a file sync made (git diff
    // without the new file in the index), the fix's words left in the inputs, a summary that
    // replaces what the job wrote before, a patch that does not apply to the branch or leaves the
    // table stale, and a patch that takes in a change of the working tree that sync did not make
    // (a tracked file outside the paths sync writes, changed by an earlier step).
    let d = Rig::new("stale-add", false, &PASSING);
    let was = d.read("Cargo.toml");
    d.append_manifest(GIT_DEP);
    d.commit("a git dependency, the table not synced");
    d.write("README.md", "changed by an earlier step\n");
    let now = format!("{was}{GIT_DEP}");
    hands_over(&d, &[changed("Cargo.toml", &was, &now)], &[TABLE, ARCHIVES]);
}

#[test]
fn a_removed_dependency_hands_over_a_patch_that_deletes_a_file() {
    // Catches: a patch that keeps a file sync removed, and one that cannot be applied for it.
    let d = Rig::new("stale-remove", true, &PASSING);
    let was = d.read("Cargo.toml");
    let now = was.replace(GIT_DEP, "");
    d.write("Cargo.toml", &now);
    d.commit("the git dependency removed, the table not synced");
    hands_over(&d, &[changed("Cargo.toml", &was, &now)], &[TABLE, ARCHIVES]);
}

#[test]
fn a_generated_file_removed_by_hand_hands_over_a_patch_that_writes_it_again() {
    // Catches: a stale generated file (the inputs unchanged) not taken for a stale table, and a
    // patch that changes only files that were tracked.
    let d = Rig::new("stale-generated", true, &PASSING);
    git(&d.repo, &["rm", "--quiet", ARCHIVES]);
    d.commit("a generated file removed by hand");
    hands_over(
        &d,
        &[format!("{ARCHIVES} is not what sync writes")],
        &[ARCHIVES],
    );
}

#[test]
fn two_changed_inputs_are_both_named() {
    // Catches: only the first stale line of check taken as the stale inputs.
    let d = Rig::new("stale-two", false, &PASSING);
    let manifest = d.read("Cargo.toml");
    let lock = d.read("Cargo.lock");
    d.write("Cargo.toml", &format!("{manifest}# a\n"));
    d.write("Cargo.lock", &format!("{lock}# b\n"));
    d.commit("both changed");
    hands_over(
        &d,
        &[
            changed("Cargo.toml", &manifest, &format!("{manifest}# a\n")),
            changed("Cargo.lock", &lock, &format!("{lock}# b\n")),
        ],
        &[TABLE],
    );
}

#[test]
fn a_missing_generated_file_without_a_stale_mark_hands_over_a_patch_that_adds_it() {
    // Catches: a problem check prints without `STALE:` (a generated file deleted) taken for one
    // sync cannot fix, and a patch that leaves out a new file at the repository's root.
    let d = Rig::new("no-table", false, &PASSING);
    git(&d.repo, &["rm", "--quiet", TABLE]);
    d.commit("no table");
    hands_over(&d, &[format!("{TABLE} is missing")], &[TABLE]);
}

/// Makes the table of `d` stale: a comment in the root manifest, committed.
fn make_stale(d: &Rig) {
    d.append_manifest("# a comment\n");
    d.commit("the manifest changed, the table not synced");
}

/// Asserts that the run `o` of `d` failed with `words` after the log `log`, before the build,
/// leaving no patch and no summary.
fn refused(d: &Rig, o: &Output, log: &str, words: &str) {
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stdout(o), "");
    assert_eq!(stderr(o), format!("{log}xtask: {words}\n"));
    assert_eq!(d.calls(), STOPPED);
    assert!(!d.patch().exists());
    assert!(!d.summary().exists());
}

#[test]
fn a_failed_sync_fails_with_its_output_and_hands_over_nothing() {
    // Catches: a failed sync taken for a fix, its words hidden, or a patch of a half-written
    // table handed over.
    let d = Rig::new("sync-fails", false, &PASSING);
    make_stale(&d);
    d.defect("sync-fails");
    let o = d.farm_build();
    refused(
        &d,
        &o,
        "buildrules: sync: CargoFailed\n",
        "the generated build table is stale, and `buildrules sync` failed (exit status: 1); its \
         output is above",
    );
    assert_eq!(d.tool_calls().len(), 2);
}

#[test]
fn a_sync_that_leaves_the_table_stale_fails_with_what_check_still_says() {
    // Catches: a patch handed over without checking that it makes the table fresh.
    let d = Rig::new("sync-no-fix", false, &PASSING);
    let was = d.read("Cargo.toml");
    make_stale(&d);
    d.defect("sync-writes-nothing");
    let o = d.farm_build();
    let line = changed("Cargo.toml", &was, &format!("{was}# a comment\n"));
    refused(
        &d,
        &o,
        &format!("buildrules: wrote {TABLE}\n"),
        &format!(
            "`buildrules sync` ran, and `buildrules check` still fails (exit status: 1): \
             buildrules: STALE: {line}: {FIX}"
        ),
    );
    assert_eq!(d.tool_calls().len(), 3);
}

#[test]
fn a_check_that_cannot_judge_the_table_fails_with_its_words_and_runs_no_sync() {
    // Catches: a check that could not run (a usage error, status 2) taken for a fresh table or a
    // stale one, and check's words hidden.
    let d = Rig::new("check-usage", false, &PASSING);
    d.defect("check-usage");
    let o = d.farm_build();
    refused(
        &d,
        &o,
        "",
        "`buildrules check` could not judge the table (exit status: 2): buildrules: check: \
         unknown argument --nope",
    );
    assert_eq!(d.tool_calls(), ["check"]);
}

#[test]
fn a_command_line_that_cannot_be_built_named_or_run_fails_before_the_check() {
    // Catches: a failed build of the command line read as a fresh table, an output line of
    // another target or with no path taken for it, and a command line that cannot start taken
    // for a passing check.
    let mut answers = PASSING.to_vec();
    answers.extend([
        ("tool.status", "3"),
        ("tool.stderr", "no cell buildrules\n"),
    ]);
    let d = Rig::new("tool-fails", false, &answers);
    let o = d.farm_build();
    refused(
        &d,
        &o,
        "",
        "`buck2 build buildrules//cli:buildrules` failed (exit status: 3): no cell buildrules",
    );
    assert_eq!(d.tool_calls(), Vec::<String>::new());

    for (name, out) in [
        ("tool-none", ""),
        ("tool-other", "buildrules//cli:other /x/other\n"),
        ("tool-no-path", "buildrules//cli:buildrules \n"),
    ] {
        let mut answers = PASSING.to_vec();
        answers.push(("tool.stdout", out));
        let d = Rig::new(name, false, &answers);
        let o = d.farm_build();
        refused(
            &d,
            &o,
            "",
            &format!(
                "`buck2 build buildrules//cli:buildrules --show-full-output` named no output of \
                 buildrules//cli:buildrules: {out:?}"
            ),
        );
    }

    let mut answers = PASSING.to_vec();
    answers.push((
        "tool.stdout",
        "buildrules//cli:buildrules /nonexistent/buildrules\n",
    ));
    let d = Rig::new("tool-missing", false, &answers);
    let o = d.farm_build();
    assert_eq!(o.status.code(), Some(1));
    assert!(
        stderr(&o).starts_with("xtask: cannot run \"/nonexistent/buildrules\": "),
        "{}",
        stderr(&o)
    );
    assert_eq!(d.calls(), STOPPED);
}

#[test]
fn a_patch_that_git_cannot_make_fails_and_hands_over_nothing() {
    // Catches: a failed git taken for an empty patch, and its words hidden.
    let d = Rig::new("git-fails", false, &PASSING);
    make_stale(&d);
    let o = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .current_dir(&*d.repo)
        .args(args(&d.buck2(), &d.patch(), &d.summary()))
        .env("GIT_DIR", d.out.join("no-repository"))
        .output()
        .expect("run xtask");
    assert_eq!(o.status.code(), Some(1));
    let words = stderr(&o);
    let start = "buildrules: wrote buildrules.lock.bzl\nxtask: `git ls-files --others \
                 --exclude-standard -z -- BUCK Cargo.lock buildrules.bzl buildrules.lock.bzl \
                 third_party/buildrules toolchains` failed (exit status: 128): fatal: ";
    assert!(words.starts_with(start), "{words}");
    assert!(!d.patch().exists());
    assert!(!d.summary().exists());
}

#[test]
fn a_patch_or_summary_that_cannot_be_written_fails() {
    // Catches: a fix the job could not write reported as handed over.
    let d = Rig::new("patch-unwritable", false, &PASSING);
    make_stale(&d);
    std::fs::write(d.out.join("patch"), "a file, not a directory").expect("block");
    let o = d.farm_build();
    assert_eq!(o.status.code(), Some(1));
    let patch = d.patch();
    assert_eq!(
        stderr(&o),
        format!(
            "buildrules: wrote {TABLE}\nxtask: cannot write {patch:?}: File exists (os error 17)\n"
        )
    );
    assert!(!d.summary().exists());

    let d = Rig::new("summary-unwritable", false, &PASSING);
    make_stale(&d);
    let o = xtask(&d.repo, &args(&d.buck2(), &d.patch(), &d.out));
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o),
        format!(
            "buildrules: wrote {TABLE}\nxtask: cannot write {:?}: Is a directory (os error 21)\n",
            &*d.out
        )
    );
    assert!(d.patch().exists());
}

/// The answers of a build whose first run printed `first` and ended with `status`, and whose
/// second printed `second` and ended with `second_status`; a third run, if one came, would pass.
fn runs<'a>(
    first: &'a str,
    status: &'a str,
    second: &'a str,
    second_status: &'a str,
) -> Vec<(&'a str, &'a str)> {
    vec![
        ("audit.stdout", SET),
        ("build.1.stderr", first),
        ("build.1.status", status),
        ("build.2.stderr", second),
        ("build.2.status", second_status),
        ("build.3.stderr", BUILD_LOG),
    ]
}

#[test]
fn a_build_whose_only_failures_are_cuts_runs_once_more_and_passes_saying_so() {
    // Catches: a cut connection failing the job though the farm answered at once after it, the
    // second run's summary not the one read, or the rerun hidden from the log and the job
    // summary (written after what the summary held).
    let d = Rig::new("cut-once", false, &runs(CUT_LOG, "3", BUILD_LOG, "0"));
    std::fs::create_dir_all(&*d.out).expect("out");
    std::fs::write(d.summary(), "earlier\n").expect("summary");
    let o = d.farm_build();
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), format!("{FARM}\n"));
    assert_eq!(stderr(&o), format!("{CUT_LOG}{RERUN}{BUILD_LOG}"));
    assert_eq!(d.calls(), BUILT_TWICE);
    assert_eq!(
        std::fs::read_to_string(d.summary()).expect("summary"),
        format!("earlier\n{RERUN}")
    );
    assert!(!d.patch().exists());
}

#[test]
fn a_build_cut_twice_fails_after_two_runs_and_never_runs_a_third() {
    // Catches: a farm that is down retried without end (the fixture has a passing third run
    // ready), or a second cut reported as an ordinary failure, not by name.
    let d = Rig::new("cut-twice", false, &runs(CUT_LOG, "3", CUT_LOG, "3"));
    let o = d.farm_build();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stdout(&o), "");
    assert_eq!(
        stderr(&o),
        format!(
            "{CUT_LOG}{RERUN}{CUT_LOG}xtask: `buck2 build //:ci` failed (exit status: 3) on its \
             second run too, every failure again a cut connection to the farm: the farm or the \
             network to it is down; run the job again once the farm answers\n"
        )
    );
    assert_eq!(d.calls(), BUILT_TWICE);
}

#[test]
fn a_cut_then_a_real_failure_fails_with_the_second_runs_failure() {
    // Catches: the failure of the second run excused by the cut of the first.
    let d = Rig::new(
        "cut-then-test",
        false,
        &runs(CUT_LOG, "3", TEST_FAILED_LOG, "3"),
    );
    let o = d.farm_build();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o),
        format!(
            "{CUT_LOG}{RERUN}{TEST_FAILED_LOG}xtask: `buck2 build //:ci` failed (exit status: 3)\n"
        )
    );
    assert_eq!(d.calls(), BUILT_TWICE);
}

#[test]
fn a_cut_then_a_build_with_local_actions_fails_on_the_second_runs_summary() {
    // Catches: the summary line of the first run, not the last, held to `local: 0`.
    let line = "Commands: 5 (cached: 2, remote: 1, local: 2)";
    let local = format!("{line}\nBUILD SUCCEEDED\n");
    let d = Rig::new("cut-then-local", false, &runs(CUT_LOG, "3", &local, "0"));
    let o = d.farm_build();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o),
        format!(
            "{CUT_LOG}{RERUN}{local}xtask: `{line}`: actions ran on this machine, not on the farm \
             (local must be 0)\n"
        )
    );
    assert_eq!(d.calls(), BUILT_TWICE);
}

#[test]
fn a_real_failure_is_never_run_again_even_beside_cuts() {
    // Catches: a failing test, or a failed action beside cuts, run again as if the farm had
    // failed it, and a summary line written for a build that was not run again.
    for (name, log) in [
        ("test-failed", TEST_FAILED_LOG),
        ("cut-and-failed", CUT_AND_FAILED_LOG),
    ] {
        let d = Rig::new(name, false, &runs(log, "3", BUILD_LOG, "0"));
        let o = d.farm_build();
        assert_eq!(o.status.code(), Some(1), "{name}");
        assert_eq!(
            stderr(&o),
            format!("{log}xtask: `buck2 build //:ci` failed (exit status: 3)\n"),
            "{name}"
        );
        assert_eq!(d.calls(), BUILT, "{name}");
        assert!(!d.summary().exists(), "{name}");
    }
}

#[test]
fn a_buck2_ended_by_a_signal_is_not_run_again_though_its_output_reads_as_a_cut() {
    // Catches: a buck2 killed (by the runner, or out of memory) taken for a cut and run again.
    let d = Rig::new("cut-killed", false, &runs(CUT_LOG, "abort", BUILD_LOG, "0"));
    let o = d.farm_build();
    assert_eq!(o.status.code(), Some(1));
    let words = stderr(&o);
    assert!(
        words.starts_with(&format!(
            "{CUT_LOG}xtask: `buck2 build //:ci` failed (signal: 6 (SIGABRT)"
        )),
        "{words}"
    );
    assert_eq!(d.calls(), BUILT);
}

#[test]
fn a_cut_build_whose_job_summary_cannot_be_written_fails_before_its_second_run() {
    // Catches: a second run made without the job summary saying so.
    let d = Rig::new("cut-no-summary", false, &runs(CUT_LOG, "3", BUILD_LOG, "0"));
    let o = xtask(&d.repo, &args(&d.buck2(), &d.patch(), &d.out));
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o),
        format!(
            "{CUT_LOG}xtask: cannot write {:?}: Is a directory (os error 21)\n",
            &*d.out
        )
    );
    assert_eq!(d.calls(), BUILT);
}

#[test]
fn a_build_with_actions_on_this_machine_fails_with_status_1() {
    // Catches: `local: N` with N above 0 (no farm, or a fallback to the runner) passed as a farm
    // build because the build itself succeeded.
    let line = "Commands: 3 (cached: 0, remote: 1, local: 2)";
    let log = format!("{line}\n");
    let d = Rig::new(
        "local",
        false,
        &[("audit.stdout", SET), ("build.stderr", &log)],
    );
    let o = d.farm_build();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o),
        format!(
            "{log}xtask: `{line}`: actions ran on this machine, not on the farm (local must be 0)\n"
        )
    );
    assert_eq!(d.calls(), BUILT);
}

#[test]
fn a_build_with_no_summary_line_fails_with_status_1() {
    // Catches: a build whose summary is missing (a buck2 that printed nothing) read as local: 0.
    let d = Rig::new(
        "no-summary",
        false,
        &[("audit.stdout", SET), ("build.stderr", "BUILD SUCCEEDED\n")],
    );
    let o = d.farm_build();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o),
        "BUILD SUCCEEDED\nxtask: the build printed no summary line (`Commands: N (cached: N, \
         remote: N, local: N)`)\n"
    );
}

#[test]
fn a_failed_build_fails_with_status_1_shows_its_log_and_still_stops_buck2() {
    // Catches: a failed build passed because its summary says local: 0, its log hidden, or the
    // daemon left running after a failure.
    let log = format!("{FARM}\nBUILD FAILED\n");
    let d = Rig::new(
        "failed",
        false,
        &[
            ("audit.stdout", SET),
            ("build.stderr", &log),
            ("build.status", "3"),
        ],
    );
    let o = d.farm_build();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o),
        format!("{log}xtask: `buck2 build //:ci` failed (exit status: 3)\n")
    );
    assert_eq!(d.calls(), BUILT);
}

#[test]
fn no_worker_property_set_fails_before_the_build_with_status_1() {
    // Catches: a build started with no farm configured (buck2 would have no execution platform,
    // or run on the runner), for the key unset (`{}`) and for it set to nothing.
    for (name, json) in [
        ("unset", "{}"),
        ("empty", r#"{"buildrules_re.linux_x86_64_properties":""}"#),
    ] {
        let d = Rig::new(&format!("workers-{name}"), false, &[("audit.stdout", json)]);
        let o = d.farm_build();
        assert_eq!(o.status.code(), Some(1), "{name}");
        assert_eq!(
            stderr(&o),
            "xtask: buildrules_re.linux_x86_64_properties is empty: farm-connect wrote no \
             [komira_re] linux_x86_64_properties, so buck2 has no farm to build on\n",
            "{name}"
        );
        assert_eq!(d.calls(), [AUDIT, "kill"], "{name}");
        assert_eq!(d.tool_calls(), Vec::<String>::new(), "{name}");
    }
}

#[test]
fn a_failed_audit_fails_before_the_build_with_status_1() {
    // Catches: buck2 that cannot read its configuration (no .buckconfig) taken for one with no
    // workers, or ignored.
    let d = Rig::new(
        "audit",
        false,
        &[
            ("audit.stderr", "no .buckconfig here\n"),
            ("audit.status", "3"),
        ],
    );
    let o = d.farm_build();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o),
        "xtask: `buck2 audit config` failed (exit status: 3): no .buckconfig here\n"
    );
    assert_eq!(d.calls(), [AUDIT, "kill"]);
}

#[test]
fn a_buck2_that_cannot_start_fails_with_status_1_and_the_flags_are_required() {
    // Catches: a missing buck2 reported as something else, and a verb that runs without its
    // target, its buck2 or any of the handoff's flags.
    let d = Rig::new("no-buck2", false, &[]);
    let missing = d.bin.join("missing");
    let o = xtask(&d.repo, &args(&missing, &d.patch(), &d.summary()));
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(
        stderr(&o),
        format!("xtask: cannot run {missing:?}: No such file or directory (os error 2)\n")
    );
    let all = args(&d.buck2(), &d.patch(), &d.summary());
    // Each flag and its value, left out in turn.
    for flag in (1..all.len()).step_by(2) {
        let mut without = all.clone();
        without.drain(flag..flag + 2);
        let o = xtask(&d.repo, &without);
        assert_eq!(o.status.code(), Some(2), "{without:?}");
        assert!(stderr(&o).contains("required"), "{}", stderr(&o));
    }
    assert_eq!(d.calls(), Vec::<String>::new());
}
