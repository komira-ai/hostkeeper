use super::*;
use crate::release::fakes::{FakeHistory, Fakes, nth_commit, scratch};
use crate::release::ports::{Adapters, GitHubRuns, PortError};

fn repo() -> Repo {
    "komira-ai/hostkeeper".parse().expect("a repository")
}

fn request(commit: &Commit) -> Request {
    Request {
        repo: repo(),
        commit: commit.clone(),
        version_file: "VERSION".to_owned(),
        freeze: String::new(),
        scheduled: false,
        output: None,
    }
}

fn scheduled(commit: &Commit) -> Request {
    Request {
        scheduled: true,
        ..request(commit)
    }
}

/// The root commit: version file `0.1` and a source file.
fn root(history: &mut FakeHistory) -> Commit {
    history.commit(
        None,
        &[
            ("VERSION", Some("0.1\n")),
            ("src/lib.rs", Some("fn a() {}")),
        ],
    )
}

/// A commit on `parent` that changes code.
fn code(history: &mut FakeHistory, parent: &Commit, text: &str) -> Commit {
    history.commit(Some(parent), &[("src/lib.rs", Some(text))])
}

/// A commit on `parent` that changes documentation only.
fn docs(history: &mut FakeHistory, parent: &Commit) -> Commit {
    history.commit(
        Some(parent),
        &[("docs/x.md", Some("x")), ("README.md", Some("r"))],
    )
}

/// A successful push run of each required workflow for `commit`.
fn green(fakes: &Fakes, commit: &Commit) {
    for workflow in REQUIRED_WORKFLOWS {
        fakes.runs.push(workflow, commit, "success");
    }
}

fn plan_of(fakes: &Fakes, request: &Request) -> Result<Vec<String>, Error> {
    plan(&fakes.ports(), request)
}

fn published(tag: &str, commit: &Commit) -> Release {
    Release {
        tag: tag.to_owned(),
        commit: commit.to_string(),
        draft: false,
        assets: Vec::new(),
    }
}

const EMPTY_MATRIX: &str = "matrix={\"include\":[]}";

fn matrix(entries: &[(&str, &Commit)]) -> String {
    let items: Vec<String> = entries
        .iter()
        .map(|(v, c)| format!("{{\"version\":\"{v}\",\"tag\":\"v{v}\",\"commit\":\"{c}\"}}"))
        .collect();
    format!("matrix={{\"include\":[{}]}}", items.join(","))
}

/// The lines of the push path for a commit that is released as `version`.
fn push_released(version: &str, commit: &Commit) -> Vec<String> {
    vec![
        format!("release v{version} of {commit}"),
        "release=true".to_owned(),
        matrix(&[(version, commit)]),
        format!("version={version}"),
        format!("tag=v{version}"),
        format!("commit={commit}"),
    ]
}

/// The lines of the push path for a commit that is not released.
fn push_declined(commit: &Commit, version: &str, reason: &str) -> Vec<String> {
    vec![
        format!("no release of {commit} ({version}): {reason}"),
        "release=false".to_owned(),
        EMPTY_MATRIX.to_owned(),
        format!("reason={reason}"),
    ]
}

#[test]
fn docs_only_commits_use_a_number_but_are_not_released() {
    // Catches: a documentation-only commit being released, or its number being skipped so the
    // next code commit counts one less.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let d = docs(&mut h, &r);
    let c = code(&mut h, &d, "fn b() {}");
    let fakes = Fakes::over(h);
    for commit in [&r, &d, &c] {
        green(&fakes, commit);
    }
    assert_eq!(
        plan_of(&fakes, &request(&d)),
        Ok(push_declined(&d, "0.1.2", "documentation only"))
    );
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(push_released("0.1.3", &c))
    );
}

#[test]
fn plan_declines_while_privileged_is_pending_and_a_later_success_releases() {
    // Catches: a release cut while the privileged run has not completed, or a later success of
    // that run not releasing.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let c = code(&mut h, &r, "fn b() {}");
    let fakes = Fakes::over(h);
    fakes.runs.push("ci.yml", &c, "success");
    fakes.runs.push("artifacts.yml", &c, "success");
    let pending = fakes.runs.add("privileged.yml", &c, "push", "main", None);
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(push_declined(
            &c,
            "0.1.2",
            &format!("waiting for privileged.yml: run {pending} has not completed")
        ))
    );
    fakes.runs.push("privileged.yml", &c, "success");
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(push_released("0.1.2", &c))
    );
    assert_eq!(*fakes.runs.writes.borrow(), Vec::<String>::new());
}

#[test]
fn a_red_privileged_run_blocks_the_next_release_and_opens_one_issue() {
    // Catches: a commit after a red privileged run being released on a green ci run alone, a
    // second issue opened by a second plan, or the red commit itself being released.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let a = code(&mut h, &r, "fn a2() {}");
    let b = code(&mut h, &a, "fn b() {}");
    let fakes = Fakes::over(h);
    green(&fakes, &r);
    fakes.runs.push("ci.yml", &a, "success");
    fakes.runs.push("artifacts.yml", &a, "success");
    let red = fakes.runs.push("privileged.yml", &a, "failure");
    fakes.runs.push("ci.yml", &b, "success");
    fakes.runs.push("artifacts.yml", &b, "success");
    let held = format!("held: privileged run {red} failed on {a}; tracking issue #1");

    let expected = push_declined(&b, "0.1.3", &held);
    assert_eq!(plan_of(&fakes, &request(&b)), Ok(expected.clone()));
    assert_eq!(plan_of(&fakes, &request(&b)), Ok(expected));
    assert_eq!(*fakes.runs.writes.borrow(), ["open #1".to_owned()]);
    let issues = fakes.runs.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].title, BLOCK_TITLE);
    assert_eq!(issues[0].body, hold_body(&a, red));

    fakes.runs.push("privileged.yml", &b, "success");
    assert_eq!(
        plan_of(&fakes, &request(&b)),
        Ok(push_released("0.1.3", &b))
    );
    assert_eq!(
        plan_of(&fakes, &request(&a)),
        Ok(push_declined(&a, "0.1.2", &held))
    );
    assert_eq!(*fakes.runs.writes.borrow(), ["open #1".to_owned()]);
}

#[test]
fn an_open_issue_with_another_body_is_edited_once_and_others_are_left() {
    // Catches: a second issue opened beside the open one, the body left stale, or an unrelated
    // issue taken for the hold's.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let a = code(&mut h, &r, "fn a2() {}");
    let b = code(&mut h, &a, "fn b() {}");
    let fakes = Fakes::over(h);
    let red = fakes.runs.push("privileged.yml", &a, "failure");
    fakes
        .runs
        .open_issue(&repo(), "Something else", "unrelated")
        .expect("an unrelated issue");
    fakes
        .runs
        .open_issue(&repo(), BLOCK_TITLE, "old")
        .expect("the hold's issue");
    let held = format!("held: privileged run {red} failed on {a}; tracking issue #2");
    assert_eq!(
        plan_of(&fakes, &request(&b)),
        Ok(push_declined(&b, "0.1.3", &held))
    );
    assert_eq!(
        *fakes.runs.writes.borrow(),
        [
            "open #1".to_owned(),
            "open #2".to_owned(),
            "edit #2".to_owned()
        ]
    );
    let issues = fakes.runs.issues();
    assert_eq!(issues[0].body, "unrelated");
    assert_eq!(issues[1].body, hold_body(&a, red));
}

#[test]
fn a_run_from_a_pull_request_or_another_sha_never_counts() {
    // Catches: a green pull-request run, or a green run of another commit, standing in for the
    // push run on main of the planned commit.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let c = code(&mut h, &r, "fn b() {}");
    let fakes = Fakes::over(h);
    fakes
        .runs
        .add("ci.yml", &c, "pull_request", "feature", Some("success"));
    fakes.runs.push("ci.yml", &r, "success");
    fakes.runs.push("privileged.yml", &c, "success");
    fakes.runs.push("artifacts.yml", &c, "success");
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(push_declined(
            &c,
            "0.1.2",
            "waiting for ci.yml: no push run on main for this commit"
        ))
    );
    fakes.runs.push("ci.yml", &c, "success");
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(push_released("0.1.2", &c))
    );
}

#[test]
fn a_failed_and_a_cancelled_run_are_declined_with_their_conclusion() {
    // Catches: a run that ended red or cancelled being reported as pending, or its conclusion
    // lost from the reason.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let c = code(&mut h, &r, "fn b() {}");
    let fakes = Fakes::over(h);
    let failed = fakes.runs.push("ci.yml", &c, "timed_out");
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(push_declined(
            &c,
            "0.1.2",
            &format!("ci.yml run {failed} ended timed_out")
        ))
    );
    fakes.runs.push("ci.yml", &c, "success");
    fakes.runs.push("privileged.yml", &c, "success");
    let cancelled = fakes.runs.push("artifacts.yml", &c, "cancelled");
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(push_declined(
            &c,
            "0.1.2",
            &format!("artifacts.yml run {cancelled} ended cancelled")
        ))
    );
}

#[test]
fn scheduled_path_plans_missed_commits_oldest_first_under_their_own_numbers() {
    // Catches: missed commits planned newest first, under the tip's number, or with the
    // documentation-only commit released.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let a = code(&mut h, &r, "fn a2() {}");
    let d = docs(&mut h, &a);
    let b = code(&mut h, &d, "fn b() {}");
    let fakes = Fakes::over(h);
    fakes.store.insert(published("v0.1.1", &r));
    for commit in [&a, &d, &b] {
        green(&fakes, commit);
    }
    assert_eq!(
        plan_of(&fakes, &scheduled(&b)),
        Ok(vec![
            format!("release v0.1.2 of {a}"),
            format!("no release of {d} (0.1.3): documentation only"),
            format!("release v0.1.4 of {b}"),
            "release=true".to_owned(),
            format!(
                "matrix={{\"include\":[{{\"version\":\"0.1.2\",\"tag\":\"v0.1.2\",\"commit\":\"{a}\"}},{{\"version\":\"0.1.4\",\"tag\":\"v0.1.4\",\"commit\":\"{b}\"}}]}}"
            ),
        ])
    );
}

#[test]
fn the_scheduled_path_releases_commits_before_a_red_run_and_holds_the_rest() {
    // Catches: a hold declining the commits older than the red run, or lifting for the newer
    // ones.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let older = code(&mut h, &r, "fn older() {}");
    let a = code(&mut h, &older, "fn a2() {}");
    let b = code(&mut h, &a, "fn b() {}");
    let fakes = Fakes::over(h);
    fakes.store.insert(published("v0.1.1", &r));
    green(&fakes, &older);
    fakes.runs.push("ci.yml", &a, "success");
    fakes.runs.push("artifacts.yml", &a, "success");
    let red = fakes.runs.push("privileged.yml", &a, "failure");
    fakes.runs.push("ci.yml", &b, "success");
    fakes.runs.push("artifacts.yml", &b, "success");
    assert_eq!(
        plan_of(&fakes, &scheduled(&b)),
        Ok(vec![
            format!("release v0.1.2 of {older}"),
            format!(
                "no release of {a} (0.1.3): held: privileged run {red} failed on {a}; tracking issue #1"
            ),
            format!(
                "no release of {b} (0.1.4): held: privileged run {red} failed on {a}; tracking issue #1"
            ),
            "release=true".to_owned(),
            matrix(&[("0.1.2", &older)]),
        ])
    );
}

#[test]
fn a_scheduled_plan_with_nothing_missed_releases_nothing() {
    // Catches: the last release being planned again when nothing came after it.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let fakes = Fakes::over(h);
    fakes.store.insert(published("v0.1.1", &r));
    assert_eq!(
        plan_of(&fakes, &scheduled(&r)),
        Ok(vec!["release=false".to_owned(), EMPTY_MATRIX.to_owned()])
    );
}

#[test]
fn a_commit_released_under_its_tag_or_its_sha_is_not_released_again() {
    // Catches: a published release planned again, a draft taken for a release, or a release
    // matched by its tag only.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let c = code(&mut h, &r, "fn b() {}");
    let fakes = Fakes::over(h);
    green(&fakes, &c);
    fakes.store.insert(Release {
        draft: true,
        ..published("v0.1.2", &c)
    });
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(push_released("0.1.2", &c))
    );
    fakes.store.insert(published("nightly", &c));
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(push_declined(&c, "0.1.2", "already released as nightly"))
    );

    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let c = code(&mut h, &r, "fn b() {}");
    let fakes = Fakes::over(h);
    green(&fakes, &c);
    fakes.store.insert(published("v0.1.2", &r));
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(push_declined(&c, "0.1.2", "already released as v0.1.2"))
    );

    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let c = code(&mut h, &r, "fn b() {}");
    let fakes = Fakes::over(h);
    green(&fakes, &c);
    fakes.store.insert(published("v0.1.2", &c));
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(push_declined(&c, "0.1.2", "already released as v0.1.2"))
    );
}

#[test]
fn frozen_is_true_for_anything_but_empty_or_false() {
    // Catches: a freeze value of another spelling not freezing, or "false" freezing.
    for value in ["", "false", "FALSE", "False", " false ", "  "] {
        assert!(!frozen(value), "{value:?}");
    }
    for value in ["true", "1", "yes", "falsey", "no"] {
        assert!(frozen(value), "{value:?}");
    }
}

#[test]
fn a_frozen_repository_releases_nothing_and_reads_nothing() {
    // Catches: a freeze that still reads git or GitHub, or that answers without the reason.
    let commit = nth_commit(1);
    let fakes = Fakes::over(FakeHistory::default());
    let frozen_push = Request {
        freeze: "true".to_owned(),
        ..request(&commit)
    };
    assert_eq!(
        plan_of(&fakes, &frozen_push),
        Ok(vec![
            "no release: RELEASE_FREEZE is set".to_owned(),
            "release=false".to_owned(),
            EMPTY_MATRIX.to_owned(),
            "reason=RELEASE_FREEZE is set".to_owned(),
        ])
    );
    let frozen_scheduled = Request {
        freeze: "1".to_owned(),
        ..scheduled(&commit)
    };
    assert_eq!(
        plan_of(&fakes, &frozen_scheduled),
        Ok(vec![
            "no release: RELEASE_FREEZE is set".to_owned(),
            "release=false".to_owned(),
            EMPTY_MATRIX.to_owned(),
        ])
    );
}

#[test]
fn a_freeze_of_false_plans_as_usual() {
    // Catches: the value "false" freezing releases.
    let commit = nth_commit(1);
    let fakes = Fakes::over(FakeHistory::default());
    let open = Request {
        freeze: " False ".to_owned(),
        ..request(&commit)
    };
    assert_eq!(
        plan_of(&fakes, &open),
        Err(Error::Port(PortError::new(
            format!("git: {commit}"),
            "unknown commit"
        )))
    );
}

#[test]
fn a_shallow_clone_is_an_error() {
    // Catches: a plan numbering commits from a history cut short.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    h.shallow = true;
    let fakes = Fakes::over(h);
    assert_eq!(
        plan_of(&fakes, &request(&r)),
        Err(Error::Failed(version::SHALLOW.to_owned()))
    );
}

#[test]
fn a_missing_version_file_is_an_error() {
    // Catches: a repository with no version file being planned under a made-up base.
    let mut h = FakeHistory::default();
    let r = h.commit(None, &[("src/lib.rs", Some("fn a() {}"))]);
    let fakes = Fakes::over(h);
    assert_eq!(
        plan_of(&fakes, &request(&r)),
        Err(Error::Failed(format!("VERSION does not exist at {r}")))
    );
}

#[test]
fn the_answers_are_appended_to_the_output_file() {
    // Catches: the answers printed but not written, or written over what the file holds.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let c = code(&mut h, &r, "fn b() {}");
    let fakes = Fakes::over(h);
    green(&fakes, &c);
    let dir = scratch("plan-output");
    let file = dir.join("out");
    std::fs::write(&file, "kept=1\n").expect("write the output");
    let asked = Request {
        output: Some(file.clone()),
        ..request(&c)
    };
    let lines = plan_of(&fakes, &asked).expect("planned");
    assert_eq!(lines, push_released("0.1.2", &c));
    assert_eq!(
        std::fs::read_to_string(&file).expect("read the output"),
        format!(
            "kept=1\nrelease=true\n{}\nversion=0.1.2\ntag=v0.1.2\ncommit={c}\n",
            matrix(&[("0.1.2", &c)])
        )
    );
}

#[test]
fn an_unwritable_output_is_an_error() {
    // Catches: a failed write of the answers being reported as a plan.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let c = code(&mut h, &r, "fn b() {}");
    let fakes = Fakes::over(h);
    green(&fakes, &c);
    let dir = scratch("plan-output-dir");
    let asked = Request {
        output: Some(dir.to_path_buf()),
        ..request(&c)
    };
    assert!(matches!(plan_of(&fakes, &asked), Err(Error::Io { .. })));
}

#[test]
fn documentation_paths_are_docs_and_markdown_at_any_depth() {
    // Catches: a path being taken for documentation by a prefix or suffix it does not have.
    for path in ["docs/a.md", "docs/sub/x.txt", "a/b/c.md", "README.md"] {
        assert!(is_docs_path(path), "{path}");
    }
    for path in ["docsx/a", "src/docs/x.rs", "crates/x/src/lib.rs", "md"] {
        assert!(!is_docs_path(path), "{path}");
    }
}
