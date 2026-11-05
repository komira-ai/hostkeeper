use super::*;
use crate::release::fakes::FakeExec;
use crate::release::ports::Ran;

const OBJECT: &str = "ef56ab12cd34ef56ab12cd34ef56ab12cd34ef56";
const COMMIT: &str = "ab12cd34ef56ab12cd34ef56ab12cd34ef56ab12";
const PARENT: &str = "cd34ef56ab12cd34ef56ab12cd34ef56ab12cd34";

fn commit() -> Commit {
    COMMIT.parse().expect("a commit")
}

fn parent() -> Commit {
    PARENT.parse().expect("a commit")
}

fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn calls(cli: &GitCli<FakeExec>) -> Vec<Vec<String>> {
    cli.exec.calls.borrow().clone()
}

fn failure(line: &str, why: &str) -> PortError {
    PortError::new(line, why)
}

/// An exec that answers every command with the same raw process result.
struct Raw(Ran);

impl Exec for Raw {
    fn spawn(&self, _program: &str, _args: &[String]) -> std::io::Result<Ran> {
        Ok(self.0.clone())
    }
}

fn raw(success: bool, stdout: &[u8], stderr: &[u8]) -> GitCli<Raw> {
    GitCli::new(Raw(Ran {
        success,
        stdout: stdout.to_vec(),
        stderr: stderr.to_vec(),
    }))
}

#[test]
fn is_shallow_reads_true_and_false_from_git() {
    // Catches: a shallow clone read as complete history, or the other way round.
    let line = ["git", "rev-parse", "--is-shallow-repository"];
    let yes = GitCli::new(FakeExec::default().on(&line, true, "true\n", ""));
    let no = GitCli::new(FakeExec::default().on(&line, true, "false\n", ""));
    assert_eq!(yes.is_shallow(), Ok(true));
    assert_eq!(no.is_shallow(), Ok(false));
    assert_eq!(calls(&yes), vec![words(&line)]);
}

#[test]
fn is_shallow_refuses_text_that_is_neither_true_nor_false() {
    // Catches: an unknown answer taken as false, which would count a shallow history.
    let line = ["git", "rev-parse", "--is-shallow-repository"];
    let cli = GitCli::new(FakeExec::default().on(&line, true, "maybe\n", ""));
    assert_eq!(
        cli.is_shallow(),
        Err(failure(
            "git rev-parse --is-shallow-repository",
            "expected true or false, got \"maybe\""
        ))
    );
}

#[test]
fn a_failing_git_reports_its_trimmed_stderr() {
    // Catches: a failed command whose stdout is used anyway, or an error that hides git's text.
    let line = ["git", "rev-parse", "--is-shallow-repository"];
    let cli = GitCli::new(FakeExec::default().on(&line, false, "true\n", "fatal: not a repo\n"));
    assert_eq!(
        cli.is_shallow(),
        Err(failure(
            "git rev-parse --is-shallow-repository",
            "fatal: not a repo"
        ))
    );
}

#[test]
fn a_git_that_cannot_start_names_the_command_line() {
    // Catches: a spawn failure reported without the command that was asked for.
    let cli = GitCli::new(FakeExec::default());
    assert_eq!(
        cli.is_shallow(),
        Err(failure(
            "git rev-parse --is-shallow-repository",
            "not scripted: git rev-parse --is-shallow-repository"
        ))
    );
}

#[test]
fn stderr_that_is_not_utf8_is_read_lossily() {
    // Catches: a stderr with a stray byte that panics or is dropped instead of being reported.
    let cli = raw(false, b"", b"bad \xff byte\n");
    assert_eq!(
        cli.is_shallow(),
        Err(failure(
            "git rev-parse --is-shallow-repository",
            "bad \u{fffd} byte"
        ))
    );
}

#[test]
fn stdout_that_is_not_utf8_is_an_error() {
    // Catches: non-UTF-8 output decoded lossily into a wrong answer.
    let cli = raw(true, b"tr\xffe", b"");
    assert_eq!(
        cli.is_shallow(),
        Err(failure(
            "git rev-parse --is-shallow-repository",
            "the output is not UTF-8"
        ))
    );
}

#[test]
fn first_parents_lists_the_commit_and_each_first_parent() {
    // Catches: a chain in the wrong order, or one that leaves out the starting commit.
    let line = ["git", "rev-list", "--first-parent", COMMIT];
    let out = format!("{}\n{}\n", commit(), parent());
    let cli = GitCli::new(FakeExec::default().on(&line, true, &out, ""));
    assert_eq!(cli.first_parents(&commit()), Ok(vec![commit(), parent()]));
    assert_eq!(calls(&cli), vec![words(&line)]);
}

#[test]
fn first_parents_refuses_a_line_that_is_not_a_commit() {
    // Catches: a malformed line skipped, which would shorten the count.
    let line = ["git", "rev-list", "--first-parent", COMMIT];
    let out = format!("{}\nnot-a-commit\n", commit());
    let cli = GitCli::new(FakeExec::default().on(&line, true, &out, ""));
    assert_eq!(
        cli.first_parents(&commit()),
        Err(failure(
            &format!("git rev-list --first-parent {}", commit()),
            "not a commit: \"not-a-commit\" (expected 40 lowercase hex digits)"
        ))
    );
}

#[test]
fn first_parents_refuses_an_empty_answer() {
    // Catches: an empty history taken as a chain of no commits.
    let line = ["git", "rev-list", "--first-parent", COMMIT];
    let cli = GitCli::new(FakeExec::default().on(&line, true, "", ""));
    assert_eq!(
        cli.first_parents(&commit()),
        Err(failure(
            &format!("git rev-list --first-parent {}", commit()),
            "no commits"
        ))
    );
}

#[test]
fn file_at_reads_the_blob_the_tree_names() {
    // Catches: the file read from the wrong object, or without the tree lookup at the commit.
    let tree = format!("100644 blob {OBJECT}\tVERSION\0");
    let list = ["git", "ls-tree", "-z", COMMIT, "--", "VERSION"];
    let cat = ["git", "cat-file", "blob", OBJECT];
    let cli = GitCli::new(
        FakeExec::default()
            .on(&list, true, &tree, "")
            .on(&cat, true, "0.1\n", ""),
    );
    assert_eq!(
        cli.file_at(&commit(), "VERSION"),
        Ok(Some("0.1\n".to_owned()))
    );
    assert_eq!(calls(&cli), vec![words(&list), words(&cat)]);
}

#[test]
fn file_at_is_none_when_the_commit_has_no_such_file() {
    // Catches: a missing file reported as an error or as an empty text.
    let list = ["git", "ls-tree", "-z", COMMIT, "--", "VERSION"];
    let cli = GitCli::new(FakeExec::default().on(&list, true, "", ""));
    assert_eq!(cli.file_at(&commit(), "VERSION"), Ok(None));
    assert_eq!(calls(&cli), vec![words(&list)]);
}

#[test]
fn file_at_refuses_what_is_not_a_single_blob() {
    // Catches: a directory, a submodule or a damaged listing read as file text.
    let list = ["git", "ls-tree", "-z", COMMIT, "--", "docs"];
    let what = format!("git ls-tree -z {} -- docs", commit());
    let cases = [
        (
            format!("040000 tree {OBJECT}\tdocs\0"),
            format!("not one file: \"040000 tree {OBJECT}\\tdocs\\0\""),
        ),
        (
            format!("160000 commit {OBJECT}\tdocs\0"),
            format!("not one file: \"160000 commit {OBJECT}\\tdocs\\0\""),
        ),
        (
            format!("100644 blob {OBJECT}\tdocs"),
            format!("not one file: \"100644 blob {OBJECT}\\tdocs\""),
        ),
        (
            format!("100644 blob {OBJECT} docs\0"),
            format!("not one file: \"100644 blob {OBJECT} docs\\0\""),
        ),
        (
            "100644 blob\tdocs\0".to_owned(),
            "not one file: \"100644 blob\\tdocs\\0\"".to_owned(),
        ),
    ];
    for (listing, why) in cases {
        let cli = GitCli::new(FakeExec::default().on(&list, true, &listing, ""));
        assert_eq!(cli.file_at(&commit(), "docs"), Err(failure(&what, &why)));
    }
}

#[test]
fn file_at_reports_a_blob_that_cannot_be_read() {
    // Catches: a failing cat-file taken as an empty file.
    let tree = format!("100644 blob {OBJECT}\tVERSION\0");
    let list = ["git", "ls-tree", "-z", COMMIT, "--", "VERSION"];
    let cat = ["git", "cat-file", "blob", OBJECT];
    let cli = GitCli::new(FakeExec::default().on(&list, true, &tree, "").on(
        &cat,
        false,
        "",
        "fatal: bad object\n",
    ));
    assert_eq!(
        cli.file_at(&commit(), "VERSION"),
        Err(failure(
            &format!("git cat-file blob {OBJECT}"),
            "fatal: bad object"
        ))
    );
}

#[test]
fn changed_paths_against_a_parent_splits_the_nul_separated_names() {
    // Catches: names split on newlines, or the diff taken in the wrong direction.
    let line = [
        "git",
        "diff",
        "--name-only",
        "-z",
        "--no-renames",
        PARENT,
        COMMIT,
    ];
    let cli = GitCli::new(FakeExec::default().on(&line, true, "docs/a b.md\0src/lib.rs\0", ""));
    assert_eq!(
        cli.changed_paths(&commit(), Some(&parent())),
        Ok(vec!["docs/a b.md".to_owned(), "src/lib.rs".to_owned()])
    );
    assert_eq!(calls(&cli), vec![words(&line)]);
}

#[test]
fn changed_paths_of_a_root_commit_lists_every_path() {
    // Catches: a root commit with no parent reported as changing nothing.
    let line = ["git", "ls-tree", "-r", "-z", "--name-only", COMMIT];
    let cli = GitCli::new(FakeExec::default().on(&line, true, "VERSION\0src/lib.rs\0", ""));
    assert_eq!(
        cli.changed_paths(&commit(), None),
        Ok(vec!["VERSION".to_owned(), "src/lib.rs".to_owned()])
    );
    assert_eq!(calls(&cli), vec![words(&line)]);
}

#[test]
fn changed_paths_is_empty_when_nothing_changed() {
    // Catches: an empty diff reported as one path with an empty name.
    let line = [
        "git",
        "diff",
        "--name-only",
        "-z",
        "--no-renames",
        PARENT,
        COMMIT,
    ];
    let cli = GitCli::new(FakeExec::default().on(&line, true, "", ""));
    assert_eq!(
        cli.changed_paths(&commit(), Some(&parent())),
        Ok(Vec::new())
    );
}

#[test]
fn changed_paths_refuses_names_that_are_not_utf8() {
    // Catches: a path decoded lossily, so a docs-only check could see a different name.
    let cli = raw(true, b"a\xffb\0", b"");
    assert_eq!(
        cli.changed_paths(&commit(), None),
        Err(failure(
            &format!("git ls-tree -r -z --name-only {}", commit()),
            "the output is not UTF-8"
        ))
    );
}

#[test]
fn commit_time_reads_the_committer_time_in_seconds() {
    // Catches: the author time or a formatted date read instead of the seconds since the epoch.
    let line = ["git", "show", "-s", "--format=%ct", COMMIT];
    let cli = GitCli::new(FakeExec::default().on(&line, true, "1800000060\n", ""));
    assert_eq!(cli.commit_time(&commit()), Ok(1_800_000_060));
    assert_eq!(calls(&cli), vec![words(&line)]);
}

#[test]
fn commit_time_refuses_text_that_is_not_a_number() {
    // Catches: an unparsable time taken as zero.
    let line = ["git", "show", "-s", "--format=%ct", COMMIT];
    let cli = GitCli::new(FakeExec::default().on(&line, true, "soon\n", ""));
    assert_eq!(
        cli.commit_time(&commit()),
        Err(failure(
            &format!("git show -s --format=%ct {}", commit()),
            "not a time: \"soon\""
        ))
    );
}
