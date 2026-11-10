//! The path rule of `pr-device.yml` ([`super::pr_paths`]): the committed skip list and the
//! reason each entry has, what the rule answers for a change (by the lint's model and by git
//! itself, on the same pathspecs), and each defect of the job `changes` and of the list refused.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use super::lint::lint;
use super::pr_device::FILE;
use super::pr_paths::{
    DIFF_PREFIX, Pattern, Segment, device_tests, entry_problems, files_under, included, mac_dirs,
    needed, path_of, read_problems, repository, skip_list, skip_list_of, strings,
};
use super::testdir::TestDir;
use super::with_in;
use super::workflows;
use super::yaml::{self, Node};

/// The committed `pr-device.yml`.
fn device() -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == FILE)
        .map(|(_, text)| text)
        .expect("pr-device.yml is in the repository")
}

fn doc() -> Node {
    yaml::load(&device()).expect("pr-device.yml parses")
}

fn committed_skip_list() -> Vec<Pattern> {
    skip_list(&doc()).expect("the committed skip list parses")
}

/// The tracked files of the repository, from `git ls-files`.
fn tracked() -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repository())
        .args(["ls-files", "-z"])
        .output()
        .expect("run git ls-files");
    assert!(out.status.success(), "git ls-files failed");
    out.stdout
        .split(|&b| b == 0)
        .filter(|raw| !raw.is_empty())
        .map(|raw| String::from_utf8(raw.to_vec()).expect("a tracked path is UTF-8"))
        .collect()
}

/// The Rust sources and manifests among `files`.
fn sources(files: &[String]) -> Vec<String> {
    files
        .iter()
        .filter(|f| f.ends_with(".rs"))
        .filter(|f| f.starts_with("crates/") || f.starts_with("examples/"))
        .cloned()
        .collect()
}

/// A scratch directory of this test binary.
fn scratch(name: &str) -> TestDir {
    TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name)
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("create the directory");
    std::fs::write(path, text).expect("write a file");
}

#[test]
fn the_committed_skip_list_holds_only_entries_no_macos_job_builds_or_reads() {
    // Catches: an entry that matches no file (stale), a workflow with a macOS job or one the
    // macOS jobs depend on, a privileged test target that the macOS device job builds or another
    // file includes, an entry of another kind (a crate's source, a manifest), and an entry that a
    // source a macOS job compiles, or a file of a crate whose tests run on macOS, names
    // (README.md, MIGRATION.md, docs/macos.md and docs/conformance.md are such files).
    let doc = doc();
    let skip = committed_skip_list();
    let root = repository();
    let files = tracked();
    let device = device_tests(&doc);
    assert!(
        device.contains("device_mac") && device.contains("install_macos_pkg"),
        "{device:?}"
    );
    assert_eq!(
        entry_problems(&root, &skip, &files, &device),
        Vec::<String>::new()
    );
    let mac = mac_dirs(&root, &doc).expect("the crates whose tests run on macOS");
    for dir in [
        "crates/hostkeeper-daemon",
        "crates/hostkeeper-privileged",
        "examples/hello",
    ] {
        assert!(mac.contains(&dir.to_owned()), "{mac:?}");
    }
    assert_eq!(
        read_problems(&root, &skip, &sources(&files), &mac),
        Vec::<String>::new()
    );
    // The files the tests of the Mac's crates read stay off the list.
    for read in [
        "README.md",
        "MIGRATION.md",
        "docs/macos.md",
        "docs/conformance.md",
    ] {
        assert!(files.contains(&read.to_owned()), "{read}");
        assert!(needed(&skip, &[read]), "{read}");
    }
}

#[test]
fn a_docs_only_change_skips_and_a_crate_change_or_an_unknown_path_runs() {
    // Catches: a skip list that lets through a crate's source or manifest, the lock file, the
    // toolchain, a workflow the macOS jobs copy or this one, a shared test module, a file the
    // Mac's tests read, or a path nobody listed (a new directory); one that runs the macOS jobs
    // for documentation, the contributor guides, a Linux-only workflow or a Linux device test
    // (the change of komira-ai/hostkeeper#222); and a rename out of a crate judged by its new
    // path alone.
    let skip = committed_skip_list();
    for (changed, want) in [
        (vec!["docs/design/packages.md"], false),
        (
            vec!["docs/operations/enrollment.md", "docs/cli.md", "AGENTS.md"],
            false,
        ),
        (vec!["CONTRIBUTING.md", "SECURITY.md", "CLAUDE.md"], false),
        (
            vec![
                ".github/workflows/ci.yml",
                ".github/workflows/net-install.yml",
            ],
            false,
        ),
        (
            vec!["crates/hostkeeper-privileged/tests/install_uninstall.rs"],
            false,
        ),
        (
            vec![
                "crates/hostkeeper-privileged/tests/linux_packages.rs",
                "docs/report/hostkeeper.html",
            ],
            false,
        ),
        (vec![], false),
        (vec!["crates/hostkeeper-daemon/src/lib.rs"], true),
        (vec!["crates/hostkeeper-coordinator/src/lib.rs"], true),
        (vec!["crates/xtask/src/lib.rs"], true),
        (
            vec![
                "docs/design/packages.md",
                "crates/hostkeeper-launchd/Cargo.toml",
            ],
            true,
        ),
        (vec!["Cargo.lock"], true),
        (vec!["rust-toolchain.toml"], true),
        (vec![".cargo/config.toml"], true),
        (vec!["packaging/linux/units/hostkeeper.service"], true),
        (
            vec!["crates/hostkeeper-privileged/tests/support/mod.rs"],
            true,
        ),
        (
            vec!["crates/hostkeeper-privileged/tests/device_mac.rs"],
            true,
        ),
        (vec!["crates/hostkeeper-privileged/Cargo.toml"], true),
        (vec![".github/workflows/pr-device.yml"], true),
        (vec![".github/workflows/privileged.yml"], true),
        (vec![".github/workflows/artifacts.yml"], true),
        (vec!["README.md"], true),
        (vec!["MIGRATION.md"], true),
        (vec!["docs/macos.md"], true),
        (vec!["docs/conformance.md"], true),
        (vec!["a-new-directory/file"], true),
        (vec!["docs"], true),
        (vec!["docs/new.md"], true),
        (vec!["crates/README.md"], true),
        // A rename from a crate into docs/: its old path, then its new one.
        (
            vec!["crates/hostkeeper-daemon/src/old.rs", "docs/design/old.md"],
            true,
        ),
    ] {
        assert_eq!(needed(&skip, &changed), want, "{changed:?}");
    }
}

/// Runs git in `dir` with no user or system configuration (an empty global file beside `dir`, so
/// no signing or hook of the host's) and a fixed identity, failing the test when it cannot start.
fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    let global = dir.with_extension("gitconfig");
    std::fs::write(&global, "").expect("an empty global config");
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &global)
        .env("GIT_AUTHOR_NAME", "pr-paths")
        .env("GIT_AUTHOR_EMAIL", "pr-paths@example.com")
        .env("GIT_COMMITTER_NAME", "pr-paths")
        .env("GIT_COMMITTER_EMAIL", "pr-paths@example.com")
        .output()
        .expect("run git")
}

fn git_ok(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("UTF-8")
        .trim()
        .to_owned()
}

/// Commits `paths` (each written with `content`) on the branch checked out in `dir`.
fn commit(dir: &Path, paths: &[&str], content: &str) -> String {
    for path in paths {
        write(dir, path, content);
    }
    git_ok(dir, &["add", "--all"]);
    git_ok(dir, &["commit", "--quiet", "--allow-empty", "-m", "change"]);
    git_ok(dir, &["rev-parse", "HEAD"])
}

/// The exit code of the committed diff command run by git in `dir` on `base` and `head`: the
/// arguments of the step, the SHAs in place of `"$BASE_SHA"` and `"$HEAD_SHA"`, each pathspec
/// without its quotes.
fn diff_exit(dir: &Path, run: &str, base: &str, head: &str) -> i32 {
    let rest = run.strip_prefix(DIFF_PREFIX).expect("the prefix");
    let mut args = vec![
        "diff",
        "--quiet",
        "--no-renames",
        "--merge-base",
        base,
        head,
        "--",
        ".",
    ];
    args.extend(
        rest.split_whitespace()
            .map(|w| w.trim_start_matches('\'').trim_end_matches('\'')),
    );
    git(dir, &args).status.code().expect("an exit code")
}

#[test]
fn git_answers_as_the_lint_on_the_same_pathspecs_and_fails_on_a_diff_it_cannot_compute() {
    // Catches: the lint's model of git's exclude-glob pathspecs drifting from git (a `*` that
    // crosses a `/`, a `**` that matches the directory itself, a pattern not anchored at the top),
    // so a change the lint says skips runs, or the reverse; the base branch's own changes since
    // the pull request branched (a merge from main) counted as the pull request's; a rename
    // judged by its new path alone; and a diff that cannot be computed (an unknown or empty SHA)
    // answering anything but a failure.
    let run = super::pr_paths::diff_run(&doc())
        .expect("the diff step")
        .to_owned();
    let skip = committed_skip_list();
    let repo = scratch("pr-paths-git");
    let dir = repo.join("repo");
    std::fs::create_dir(&dir).expect("the repository directory");
    git_ok(&dir, &["init", "--quiet", "--initial-branch=main"]);
    let base = commit(
        &dir,
        &[
            "crates/hostkeeper-daemon/src/old.rs",
            "docs/design/packages.md",
            "README.md",
        ],
        "base\n",
    );
    let cases: Vec<Vec<&str>> = vec![
        vec!["docs/design/packages.md"],
        vec!["docs/design/sub/deep.md", "AGENTS.md"],
        vec!["docs/new.md"],
        vec!["docs/admin.md"],
        vec!["README.md"],
        vec!["x/AGENTS.md"],
        vec!["x/docs/design/a.md"],
        vec!["crates/hostkeeper-privileged/tests/install_uninstall.rs"],
        vec!["crates/hostkeeper-privileged/tests/x/install_uninstall.rs"],
        vec!["crates/hostkeeper-privileged/tests/install_uninstall.rs.orig"],
        vec![".github/workflows/ci.yml"],
        vec![".github/workflows/pr-device.yml"],
        vec!["crates/hostkeeper-daemon/src/lib.rs"],
        vec!["a-new-directory/file"],
    ];
    for (n, changed) in cases.iter().enumerate() {
        git_ok(
            &dir,
            &["checkout", "--quiet", "-B", &format!("case-{n}"), &base],
        );
        let head = commit(&dir, changed, &format!("case {n}\n"));
        let exit = diff_exit(&dir, &run, &base, &head);
        let want = if needed(&skip, changed) { 1 } else { 0 };
        assert_eq!(exit, want, "{changed:?}");
    }
    // The base moves on with a crate change after the branch point; the pull request changes
    // docs only. The merge base is the branch point, so the base's change is not counted.
    git_ok(&dir, &["checkout", "--quiet", "-B", "pull", &base]);
    let head = commit(&dir, &["docs/design/packages.md"], "pull\n");
    git_ok(&dir, &["checkout", "--quiet", "main"]);
    let moved = commit(&dir, &["crates/hostkeeper-daemon/src/lib.rs"], "main\n");
    assert_eq!(diff_exit(&dir, &run, &moved, &head), 0);
    // The same pull request after merging the moved base into it: still docs only.
    git_ok(&dir, &["checkout", "--quiet", "pull"]);
    git_ok(&dir, &["merge", "--quiet", "--no-edit", &moved]);
    let merged = git_ok(&dir, &["rev-parse", "HEAD"]);
    assert_eq!(diff_exit(&dir, &run, &moved, &merged), 0);
    // A rename out of a crate into docs/ shows its old path.
    git_ok(&dir, &["checkout", "--quiet", "-B", "rename", &base]);
    git_ok(
        &dir,
        &[
            "mv",
            "crates/hostkeeper-daemon/src/old.rs",
            "docs/design/old.md",
        ],
    );
    git_ok(&dir, &["commit", "--quiet", "-m", "move"]);
    let renamed = git_ok(&dir, &["rev-parse", "HEAD"]);
    assert_eq!(diff_exit(&dir, &run, &base, &renamed), 1);
    // No diff: an unknown SHA, an empty one (an event with no base), a head that is not there.
    let unknown = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(diff_exit(&dir, &run, unknown, &head), 128);
    assert_eq!(diff_exit(&dir, &run, "", &head), 128);
    assert_eq!(diff_exit(&dir, &run, &base, ""), 128);
    assert_eq!(diff_exit(&dir, &run, &base, unknown), 128);
}

#[test]
fn patterns_are_read_in_the_grammar_and_match_as_git_globs() {
    // Catches: a glob git reads otherwise than the lint (`*.md`, `?`, `[...]`, `**` before the
    // end), an empty, `.` or `..` segment, a pattern of wildcards alone (which would skip
    // everything), and a model that lets `*` cross a `/` or `**` match its own directory.
    let one = Pattern::parse("docs/*/**").expect("parses");
    assert_eq!(
        one.segments,
        [
            Segment::Literal("docs".to_owned()),
            Segment::One,
            Segment::Rest
        ]
    );
    for (path, want) in [
        ("docs/design/a.md", true),
        ("docs/design/sub/a.md", true),
        ("docs/a.md", false),
        ("docs/design", false),
        ("docs", false),
        ("x/docs/design/a.md", false),
        ("docsx/design/a.md", false),
        ("docs//a.md", false),
        ("docs/design/", false),
    ] {
        assert_eq!(one.matches(path), want, "{path}");
    }
    let file = Pattern::parse("AGENTS.md").expect("parses");
    for (path, want) in [
        ("AGENTS.md", true),
        ("x/AGENTS.md", false),
        ("AGENTS.md/x", false),
        ("AGENTS.mdx", false),
    ] {
        assert_eq!(file.matches(path), want, "{path}");
    }
    // A directory a source names may hold a matched path; a file never does.
    assert!(one.may_hold("docs"));
    assert!(one.may_hold("docs/design"));
    assert!(one.may_hold("docs/design/sub"));
    assert!(!one.may_hold("crates"));
    assert!(!file.may_hold("AGENTS.md"));
    let deep = Pattern::parse("a/b/c").expect("parses");
    assert!(deep.may_hold("a") && deep.may_hold("a/b") && !deep.may_hold("a/c"));
    for (text, why) in [
        (
            "*.md",
            "\"*.md\": \"*.md\" is neither a name, `*` nor a last `**`",
        ),
        (
            "docs/?.md",
            "\"docs/?.md\": \"?.md\" is neither a name, `*` nor a last `**`",
        ),
        (
            "docs/[a].md",
            "\"docs/[a].md\": \"[a].md\" is neither a name, `*` nor a last `**`",
        ),
        (
            "docs/**/a.md",
            "\"docs/**/a.md\": \"**\" is neither a name, `*` nor a last `**`",
        ),
        (
            "**",
            "\"**\": \"**\" is neither a name, `*` nor a last `**`",
        ),
        ("*", "\"*\": no segment names a path"),
        ("*/**", "\"*/**\": no segment names a path"),
        (
            "docs//a.md",
            "\"docs//a.md\": an empty, `.` or `..` segment",
        ),
        ("./docs", "\"./docs\": an empty, `.` or `..` segment"),
        (
            "docs/../Cargo.toml",
            "\"docs/../Cargo.toml\": an empty, `.` or `..` segment",
        ),
        ("", "\"\": an empty, `.` or `..` segment"),
    ] {
        assert_eq!(Pattern::parse(text), Err(why.to_owned()), "{text}");
    }
}

#[test]
fn a_diff_command_off_its_prefix_or_its_grammar_is_refused() {
    // Catches: a diff of other commits (three dots, the last commit, no merge base), one that
    // follows renames, one without --quiet (which exits 0 whatever it finds), a pathspec that is
    // not an exclude glob (an include, or git's default fnmatch where `*` crosses `/`), one not
    // quoted for the shell, and a command with no pathspec.
    let ok = format!("{DIFF_PREFIX} ':(exclude,glob)docs/*/**'");
    assert_eq!(skip_list_of(&ok).map(|p| p.len()), Ok(1));
    let prefix = format!("the diff step must run `{DIFF_PREFIX}` and its pathspecs");
    for run in [
        "git diff --quiet --no-renames \"$BASE_SHA...$HEAD_SHA\" -- . ':(exclude,glob)docs/*/**'",
        "git diff --quiet --no-renames HEAD^1 HEAD -- . ':(exclude,glob)docs/*/**'",
        "git diff --quiet --merge-base \"$BASE_SHA\" \"$HEAD_SHA\" -- . ':(exclude,glob)docs/*/**'",
        "git diff --no-renames --merge-base \"$BASE_SHA\" \"$HEAD_SHA\" -- . ':(exclude,glob)docs/*/**'",
        "git diff --quiet --no-renames --merge-base \"$BASE_SHA\" \"$HEAD_SHA\" -- .':(exclude,glob)docs/*/**'",
    ] {
        assert_eq!(skip_list_of(run), Err(prefix.clone()), "{run}");
    }
    for (rest, why) in [
        ("", "the diff step names no pathspec to skip"),
        (
            " ':(glob)docs/*/**'",
            "the pathspec ':(glob)docs/*/**' does not start with :(exclude,glob)",
        ),
        (
            " ':(exclude)docs/*'",
            "the pathspec ':(exclude)docs/*' does not start with :(exclude,glob)",
        ),
        (
            " :(exclude,glob)docs/*/**",
            "the pathspec :(exclude,glob)docs/*/** is not one word in single quotes",
        ),
        (
            " \":(exclude,glob)docs/*/**\"",
            "the pathspec \":(exclude,glob)docs/*/**\" is not one word in single quotes",
        ),
        (
            " ':(exclude,glob)a'b'",
            "the pathspec ':(exclude,glob)a'b' is not one word in single quotes",
        ),
        (
            " ':(exclude,glob)*.md'",
            "\"*.md\": \"*.md\" is neither a name, `*` nor a last `**`",
        ),
    ] {
        assert_eq!(
            skip_list_of(&format!("{DIFF_PREFIX}{rest}")),
            Err(why.to_owned()),
            "{rest}"
        );
    }
}

#[test]
fn a_changes_job_off_its_template_is_refused() {
    // Catches: a shallow checkout (the merge base is then missing and every diff fails, so the
    // rule never skips, or worse with a moved base), a kept token, a diff step that fails the
    // job or is not the one the output reads, the SHAs of another event or of the merge commit,
    // an output that does not fall back to running, and a skip list off the grammar; each by the
    // lint of the committed file with the defect put in.
    let text = device();
    let template = "job changes: not pr_paths::CHANGES_TEMPLATE, key for key".to_owned();
    for (from, to) in [
        ("          fetch-depth: 0\n", "          fetch-depth: 1\n"),
        ("          fetch-depth: 0\n", ""),
        (
            "          persist-credentials: false\n          fetch-depth: 0\n",
            "          fetch-depth: 0\n",
        ),
        ("        continue-on-error: true\n", ""),
        ("        id: diff\n", "        id: changed\n"),
        (
            "HEAD_SHA: ${{ github.event.pull_request.head.sha }}",
            "HEAD_SHA: ${{ github.sha }}",
        ),
        (
            "      needed: ${{ github.event_name != 'pull_request' || steps.diff.outcome != 'success' }}",
            "      needed: ${{ steps.diff.outcome != 'success' }}",
        ),
        (
            "    timeout-minutes: 10\n",
            "    timeout-minutes: 10\n    continue-on-error: true\n",
        ),
    ] {
        let found = lint(FILE, &with_in(&text, from, to));
        assert!(found.contains(&template), "{to:?}: {found:?}");
    }
    let found = lint(
        FILE,
        &with_in(
            &text,
            "':(exclude,glob)docs/*/**'",
            "':(exclude,glob)docs/**/x'",
        ),
    );
    assert_eq!(
        found,
        ["job changes: \"docs/**/x\": \"**\" is neither a name, `*` nor a last `**`"]
    );
    let found = lint(
        FILE,
        &with_in(
            &text,
            "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1\n        with:\n          persist-credentials: false\n          fetch-depth: 0\n",
            "      - uses: actions/cache/restore@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0\n        with:\n          persist-credentials: false\n          fetch-depth: 0\n",
        ),
    );
    assert_eq!(
        found,
        ["job changes: its first step must be actions/checkout"]
    );
    let found = lint(FILE, &with_in(&text, "        id: diff\n", ""));
    assert!(
        found.contains(&"job changes: no step `diff` with a `run` in the job changes".to_owned()),
        "{found:?}"
    );
}

#[test]
fn an_entry_without_its_reason_is_refused() {
    // Catches: an entry that matches no file; a workflow the macOS jobs depend on or one with a
    // macOS job; a glob over workflows; a privileged test target the device job builds, one
    // another file includes (as a module, by `#[path]`, as a target of the manifest), one in a
    // subdirectory, a glob of them; and an entry of a kind the list may not hold (a crate's
    // source, a manifest).
    let root = scratch("pr-paths-entries");
    let workflows = ".github/workflows";
    write(
        &root,
        &format!("{workflows}/linux.yml"),
        "jobs:\n  a:\n    runs-on: ubuntu-24.04\n",
    );
    write(
        &root,
        &format!("{workflows}/mac.yml"),
        "jobs:\n  a:\n    runs-on: [MacOS-15]\n",
    );
    write(
        &root,
        &format!("{workflows}/broken.yml"),
        "jobs: [unclosed\n",
    );
    write(&root, &format!("{workflows}/privileged.yml"), "jobs: {}\n");
    let tests = "crates/hostkeeper-privileged/tests";
    for target in [
        "linux_only",
        "device_mac",
        "as_module",
        "by_path",
        "as_target",
    ] {
        write(&root, &format!("{tests}/{target}.rs"), "\n");
    }
    write(&root, &format!("{tests}/sub/deep.rs"), "\n");
    write(
        &root,
        &format!("{tests}/device_mac_e2e.rs"),
        "mod as_module;\n",
    );
    write(
        &root,
        &format!("{tests}/support/mod.rs"),
        "#[path = \"../by_path.rs\"]\nmod x;\n",
    );
    write(
        &root,
        "crates/hostkeeper-privileged/Cargo.toml",
        "[[bin]]\nname = \"x\"\npath = \"tests/as_target.rs\"\n",
    );
    write(&root, "crates/a/src/lib.rs", "\n");
    // A module of the same name elsewhere is another file: src/linux_only.rs, support/linux_only.rs.
    write(
        &root,
        "crates/hostkeeper-privileged/src/lib.rs",
        "pub mod linux_only;\n",
    );
    write(
        &root,
        &format!("{tests}/support/inner.rs"),
        "pub mod linux_only;\n",
    );
    write(&root, "docs/design/a.md", "\n");
    write(&root, "AGENTS.md", "\n");
    let files = files_under(&root, &root);
    let device: BTreeSet<String> = ["device_mac".to_owned()].into();
    let check = |pattern: &str| {
        let skip = [Pattern::parse(pattern).expect("parses")];
        entry_problems(&root, &skip, &files, &device)
    };
    for pattern in [
        "docs/*/**",
        "AGENTS.md",
        ".github/workflows/linux.yml",
        "crates/hostkeeper-privileged/tests/linux_only.rs",
    ] {
        assert_eq!(check(pattern), Vec::<String>::new(), "{pattern}");
    }
    for (pattern, want) in [
        (
            "docs/gone.md",
            "the skip entry docs/gone.md matches no file",
        ),
        (
            ".github/workflows/mac.yml",
            "the skip entry .github/workflows/mac.yml has a job on macOS",
        ),
        (
            ".github/workflows/broken.yml",
            "the skip entry .github/workflows/broken.yml has a job on macOS",
        ),
        (
            ".github/workflows/privileged.yml",
            "the skip entry .github/workflows/privileged.yml is a workflow the macOS jobs depend on",
        ),
        (
            ".github/workflows/*",
            "the skip entry .github/workflows/* is not one workflow file",
        ),
        (
            "crates/hostkeeper-privileged/tests/device_mac.rs",
            "the skip entry crates/hostkeeper-privileged/tests/device_mac.rs is a test target macos-device builds",
        ),
        (
            "crates/hostkeeper-privileged/tests/as_module.rs",
            "the skip entry crates/hostkeeper-privileged/tests/as_module.rs is included by crates/hostkeeper-privileged/tests/device_mac_e2e.rs",
        ),
        (
            "crates/hostkeeper-privileged/tests/by_path.rs",
            "the skip entry crates/hostkeeper-privileged/tests/by_path.rs is included by crates/hostkeeper-privileged/tests/support/mod.rs",
        ),
        (
            "crates/hostkeeper-privileged/tests/as_target.rs",
            "the skip entry crates/hostkeeper-privileged/tests/as_target.rs is included by crates/hostkeeper-privileged/Cargo.toml",
        ),
        (
            "crates/hostkeeper-privileged/tests/sub/deep.rs",
            "the skip entry crates/hostkeeper-privileged/tests/sub/deep.rs is not one test target of crates/hostkeeper-privileged/tests/",
        ),
        (
            "crates/hostkeeper-privileged/tests/*",
            "the skip entry crates/hostkeeper-privileged/tests/* is not one test target of crates/hostkeeper-privileged/tests/",
        ),
        (
            "crates/a/src/lib.rs",
            "the skip entry crates/a/src/lib.rs is neither under docs/, a top-level *.md, a workflow nor a test target of crates/hostkeeper-privileged/tests/",
        ),
        (
            "crates/hostkeeper-privileged/Cargo.toml",
            "the skip entry crates/hostkeeper-privileged/Cargo.toml is neither under docs/, a top-level *.md, a workflow nor a test target of crates/hostkeeper-privileged/tests/",
        ),
    ] {
        assert_eq!(check(pattern), [want], "{pattern}");
    }
}

#[test]
fn an_entry_a_macos_job_reads_is_refused() {
    // Catches: a skipped path that a test of a crate whose tests run on macOS reads (by a path
    // from its crate, from the root, or a directory holding it), or that any source includes at
    // compile time; while another crate's tests (under `tests/`, `tests.rs`, `*_tests.rs`), a
    // string that is not an include outside those crates, a comment, a skipped source itself and
    // a listed string that reads nothing are not reads.
    let root = scratch("pr-paths-reads");
    write(&root, "docs/design/a.md", "\n");
    write(&root, "AGENTS.md", "\n");
    write(
        &root,
        "crates/mac/tests/reads.rs",
        "const A: &str = concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../../docs/design/a.md\");\n\
         // \"../../AGENTS.md\" in a comment\n\
         fn f() { let _ = std::fs::read_dir(\"../../docs\"); let _ = 'x'; }\n",
    );
    write(
        &root,
        "crates/linux/src/lib.rs",
        "const B: &str = include_str!(\n    \"../../../AGENTS.md\"\n);\nconst C: &str = \"docs/design/a.md\";\n",
    );
    write(
        &root,
        "crates/linux/tests/t.rs",
        "const D: &str = include_str!(\"../../AGENTS.md\");\n",
    );
    write(
        &root,
        "crates/linux/src/x_tests.rs",
        "const F: &str = include_str!(\"../../../AGENTS.md\");\n",
    );
    write(
        &root,
        "crates/linux/src/y/tests.rs",
        "const G: &str = include_str!(\"../../../../AGENTS.md\");\n",
    );
    write(
        &root,
        "crates/mac/src/z_tests.rs",
        "const H: &str = \"../../../AGENTS.md\";\n",
    );
    write(
        &root,
        "crates/skipped/x.rs",
        "const E: &str = include_str!(\"../../AGENTS.md\");\n",
    );
    write(
        &root,
        "crates/hostkeeper-package/src/crate_tests.rs",
        "const W: &str = \".github/workflows\";\n",
    );
    write(&root, ".github/workflows/ci.yml", "\n");
    let sources: Vec<String> = files_under(&root, &root)
        .into_iter()
        .filter(|f| f.ends_with(".rs"))
        .collect();
    let mac = vec![
        "crates/mac".to_owned(),
        "crates/hostkeeper-package".to_owned(),
    ];
    let skip: Vec<Pattern> = [
        "docs/*/**",
        "AGENTS.md",
        "crates/skipped/x.rs",
        ".github/workflows/ci.yml",
    ]
    .iter()
    .map(|p| Pattern::parse(p).expect("parses"))
    .collect();
    assert_eq!(
        read_problems(&root, &skip, &sources, &mac),
        [
            "crates/linux/src/lib.rs names \"../../../AGENTS.md\", which the skip entry AGENTS.md holds",
            "crates/mac/src/z_tests.rs names \"../../../AGENTS.md\", which the skip entry AGENTS.md holds",
            "crates/mac/tests/reads.rs names \"../../docs\", which the skip entry docs/*/** holds",
            "crates/mac/tests/reads.rs names \"/../../docs/design/a.md\", which the skip entry docs/*/** holds",
        ]
    );
    // The listed string is a read anywhere else.
    let elsewhere = vec!["crates/mac".to_owned(), "crates/other".to_owned()];
    write(
        &root,
        "crates/other/t.rs",
        "const W: &str = \".github/workflows\";\n",
    );
    let sources = vec!["crates/other/t.rs".to_owned()];
    assert_eq!(
        read_problems(&root, &skip, &sources, &elsewhere),
        [
            "crates/other/t.rs names \".github/workflows\", which the skip entry .github/workflows/ci.yml holds"
        ]
    );
}

#[test]
fn strings_includes_and_paths_are_read_from_rust_sources() {
    // Catches: a string missed (raw, byte, across lines, after a character literal holding a
    // quote), a comment read as a string, an include's string missed or another call's taken,
    // and a path read from a string that names none.
    let text = "// \"comment\"\n/* \"block\" */\nlet a = '\"'; let b = \"x/y\";\nlet c = r#\"raw \"q\" \"#;\nlet d = b\"bytes\";\nlet e = include_bytes!(concat!(env!(\"D\"), \"/f.bin\")); let f = g(\"h\");\n";
    let found: Vec<String> = strings(text).into_iter().map(|(_, s)| s).collect();
    assert_eq!(found, ["x/y", "raw \"q\" ", "bytes", "D", "/f.bin", "h"]);
    assert_eq!(included(text), ["D", "/f.bin"]);
    assert_eq!(included("include_str!(\"a\""), ["a"]);
    for (string, want) in [
        ("../../docs/a.md", Some("docs/a.md")),
        ("/../../README.md", Some("README.md")),
        ("./docs/", Some("docs")),
        ("docs", Some("docs")),
        ("/", None),
        ("../..", None),
        ("", None),
        (".", None),
        ("a b", None),
        ("a\\nb", None),
    ] {
        assert_eq!(path_of(string).as_deref(), want, "{string:?}");
    }
}

#[test]
fn the_crates_whose_tests_run_on_macos_are_found_by_package_name() {
    // Catches: a crate the macOS test job names that is not found (its files would then go
    // unread by the read rule), and a device job whose test targets are not all read.
    let root = scratch("pr-paths-crates");
    write(
        &root,
        "crates/a/Cargo.toml",
        "[package]\nname = \"alpha\"\n",
    );
    write(
        &root,
        "crates/hostkeeper-privileged/Cargo.toml",
        "[package]\nname = \"hostkeeper-privileged\"\n",
    );
    write(
        &root,
        "examples/h/Cargo.toml",
        "[package]\nname = \"hello\"\n",
    );
    write(&root, "crates/no-package/Cargo.toml", "[workspace]\n");
    let doc = |packages: &str| {
        yaml::load(&format!(
            "jobs:\n  macos-test:\n    steps:\n      - run: cargo test --locked {packages}\n  macos-device:\n    steps:\n      - run: cargo test --test a --test b\n      - run: cargo test --test c -- --test x\n"
        ))
        .expect("parses")
    };
    assert_eq!(
        mac_dirs(&root, &doc("-p alpha -p hello")),
        Ok(vec![
            "crates/a".to_owned(),
            "examples/h".to_owned(),
            "crates/hostkeeper-privileged".to_owned()
        ])
    );
    assert_eq!(
        mac_dirs(&root, &doc("-p alpha -p gone")),
        Err("no crate named gone under crates/ or examples/".to_owned())
    );
    write(&root, "crates/bad/Cargo.toml", "[package\n");
    assert!(mac_dirs(&root, &doc("-p alpha")).is_err());
    let names: Vec<String> = device_tests(&doc("")).into_iter().collect();
    assert_eq!(names, ["a", "b", "c", "x"]);
}
