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
        output: None,
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

/// Successful push runs of privileged and artifacts for `commit`, and a ci run that has not
/// completed; the id of the ci run.
fn ci_pending(fakes: &Fakes, commit: &Commit) -> u64 {
    fakes.runs.push("privileged.yml", commit, "success");
    fakes.runs.push("artifacts.yml", commit, "success");
    fakes.runs.add("ci.yml", commit, "push", "main", None)
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

/// Fakes over `history` in which the root `root` is released as v0.1.1, so a plan walks only
/// the commits after it.
fn after_root(history: FakeHistory, root: &Commit) -> Fakes {
    let fakes = Fakes::over(history);
    fakes.store.insert(published("v0.1.1", root));
    fakes
}

const EMPTY_MATRIX: &str = "matrix={\"include\":[]}";

fn matrix(entries: &[(&str, &Commit)]) -> String {
    let items: Vec<String> = entries
        .iter()
        .map(|(v, c)| format!("{{\"version\":\"{v}\",\"tag\":\"v{v}\",\"commit\":\"{c}\"}}"))
        .collect();
    format!("matrix={{\"include\":[{}]}}", items.join(","))
}

/// The printed line of a commit released as `version`.
fn release_line(version: &str, commit: &Commit) -> String {
    format!("release v{version} of {commit}")
}

/// The printed line of a commit not released.
fn decline_line(commit: &Commit, version: &str, reason: &str) -> String {
    format!("no release of {commit} ({version}): {reason}")
}

/// The lines of a plan: the printed `judged` lines, then the answers for the `released`
/// commits.
fn lines(judged: &[String], released: &[(&str, &Commit)]) -> Vec<String> {
    let mut out = judged.to_vec();
    out.push(format!("release={}", !released.is_empty()));
    out.push(matrix(released));
    out
}

#[test]
fn a_plan_for_a_commit_whose_ci_is_pending_releases_the_earlier_commits_whose_runs_are_done() {
    // Catches: a run started by one commit's completion planning that commit alone, so that
    // when a newer commit's run replaces the pending release run, the earlier commits whose
    // ci, privileged and artifacts runs all succeeded are never released.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let a = code(&mut h, &r, "fn a2() {}");
    let b = code(&mut h, &a, "fn b() {}");
    let c = code(&mut h, &b, "fn c() {}");
    let fakes = after_root(h, &r);
    green(&fakes, &a);
    green(&fakes, &b);
    let pending = ci_pending(&fakes, &c);
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(lines(
            &[
                release_line("0.1.2", &a),
                release_line("0.1.3", &b),
                decline_line(
                    &c,
                    "0.1.4",
                    &format!("waiting for ci.yml: run {pending} has not completed")
                ),
            ],
            &[("0.1.2", &a), ("0.1.3", &b)],
        ))
    );
    assert_eq!(*fakes.runs.writes.borrow(), Vec::<String>::new());
}

#[test]
fn a_missing_or_failed_run_does_not_stop_the_walk() {
    // Catches: a commit whose run never came or failed (which may never change) holding back
    // the newer commits whose runs all succeeded; only a failed privileged run holds them.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let a = code(&mut h, &r, "fn a2() {}");
    let m = code(&mut h, &a, "fn m() {}");
    let f = code(&mut h, &m, "fn f() {}");
    let b = code(&mut h, &f, "fn b() {}");
    let fakes = after_root(h, &r);
    green(&fakes, &a);
    fakes.runs.push("ci.yml", &f, "success");
    fakes.runs.push("privileged.yml", &f, "success");
    let failed = fakes.runs.push("artifacts.yml", &f, "failure");
    green(&fakes, &b);
    assert_eq!(
        plan_of(&fakes, &request(&b)),
        Ok(lines(
            &[
                release_line("0.1.2", &a),
                decline_line(
                    &m,
                    "0.1.3",
                    "waiting for ci.yml: no push run on main for this commit"
                ),
                decline_line(
                    &f,
                    "0.1.4",
                    &format!("artifacts.yml run {failed} ended failure")
                ),
                release_line("0.1.5", &b),
            ],
            &[("0.1.2", &a), ("0.1.5", &b)],
        ))
    );
}

#[test]
fn a_pending_run_holds_the_newer_commits_until_it_completes_and_then_all_release_in_order() {
    // Catches: a newer commit released past an older one whose run has not completed, which
    // ends every later walk above the older one so it is never released; or the held commits
    // not released, oldest first, once that run completes.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let a = code(&mut h, &r, "fn a2() {}");
    let p = code(&mut h, &a, "fn p() {}");
    let d = docs(&mut h, &p);
    let b = code(&mut h, &d, "fn b() {}");
    let c = code(&mut h, &b, "fn c() {}");
    let fakes = after_root(h, &r);
    green(&fakes, &a);
    let pending = ci_pending(&fakes, &p);
    for commit in [&d, &b, &c] {
        green(&fakes, commit);
    }
    let behind = format!("waiting for {p} (0.1.3), an older commit whose runs have not completed");
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(lines(
            &[
                release_line("0.1.2", &a),
                decline_line(
                    &p,
                    "0.1.3",
                    &format!("waiting for ci.yml: run {pending} has not completed")
                ),
                decline_line(&d, "0.1.4", "documentation only"),
                decline_line(&b, "0.1.5", &behind),
                decline_line(&c, "0.1.6", &behind),
            ],
            &[("0.1.2", &a)],
        ))
    );
    fakes.store.insert(published("v0.1.2", &a));
    fakes.runs.push("ci.yml", &p, "success");
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(lines(
            &[
                release_line("0.1.3", &p),
                decline_line(&d, "0.1.4", "documentation only"),
                release_line("0.1.5", &b),
                release_line("0.1.6", &c),
            ],
            &[("0.1.3", &p), ("0.1.5", &b), ("0.1.6", &c)],
        ))
    );
}

#[test]
fn the_walk_starts_after_the_newest_release_on_the_history() {
    // Catches: a walk that starts after an older release, so a released commit is planned again,
    // or that takes a release whose tag is not a version, or one off the history, for the last.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let a = code(&mut h, &r, "fn a2() {}");
    let b = code(&mut h, &a, "fn b() {}");
    let c = code(&mut h, &b, "fn c() {}");
    let off = code(&mut h, &r, "fn off() {}");
    let fakes = after_root(h, &r);
    fakes.store.insert(published("v0.1.2", &a));
    fakes.store.insert(published("nightly", &b));
    fakes.store.insert(published("v0.1.9", &off));
    for commit in [&a, &b, &c] {
        green(&fakes, commit);
    }
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(lines(
            &[
                decline_line(&b, "0.1.3", "already released as nightly"),
                release_line("0.1.4", &c),
            ],
            &[("0.1.4", &c)],
        ))
    );
}

#[test]
fn with_no_release_the_walk_starts_at_the_commit_that_set_the_version() {
    // Catches: a repository with no release yet planning commits from before the version file's
    // last change, or skipping the commit that changed it.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let a = code(&mut h, &r, "fn a2() {}");
    let v = h.commit(Some(&a), &[("VERSION", Some("0.2\n"))]);
    let b = code(&mut h, &v, "fn b() {}");
    let fakes = Fakes::over(h);
    for commit in [&r, &a, &v, &b] {
        green(&fakes, commit);
    }
    assert_eq!(
        plan_of(&fakes, &request(&b)),
        Ok(lines(
            &[release_line("0.2.1", &v), release_line("0.2.2", &b)],
            &[("0.2.1", &v), ("0.2.2", &b)],
        ))
    );
}

#[test]
fn docs_only_commits_use_a_number_but_are_not_released() {
    // Catches: a documentation-only commit being released, or its number being skipped so the
    // next code commit counts one less.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let d = docs(&mut h, &r);
    let c = code(&mut h, &d, "fn b() {}");
    let fakes = after_root(h, &r);
    for commit in [&d, &c] {
        green(&fakes, commit);
    }
    let docs_only = decline_line(&d, "0.1.2", "documentation only");
    assert_eq!(
        plan_of(&fakes, &request(&d)),
        Ok(lines(std::slice::from_ref(&docs_only), &[]))
    );
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(lines(
            &[docs_only, release_line("0.1.3", &c)],
            &[("0.1.3", &c)]
        ))
    );
}

#[test]
fn plan_declines_while_privileged_is_pending_and_a_later_success_releases() {
    // Catches: a release cut while the privileged run has not completed, or a later success of
    // that run not releasing.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let c = code(&mut h, &r, "fn b() {}");
    let fakes = after_root(h, &r);
    fakes.runs.push("ci.yml", &c, "success");
    fakes.runs.push("artifacts.yml", &c, "success");
    let pending = fakes.runs.add("privileged.yml", &c, "push", "main", None);
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(lines(
            &[decline_line(
                &c,
                "0.1.2",
                &format!("waiting for privileged.yml: run {pending} has not completed")
            )],
            &[]
        ))
    );
    fakes.runs.push("privileged.yml", &c, "success");
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(lines(&[release_line("0.1.2", &c)], &[("0.1.2", &c)]))
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
    let fakes = after_root(h, &r);
    green(&fakes, &r);
    fakes.runs.push("ci.yml", &a, "success");
    fakes.runs.push("artifacts.yml", &a, "success");
    let red = fakes.runs.push("privileged.yml", &a, "failure");
    fakes.runs.push("ci.yml", &b, "success");
    fakes.runs.push("artifacts.yml", &b, "success");
    let held = format!("held: privileged run {red} failed on {a}; tracking issue #1");

    let expected = lines(
        &[
            decline_line(&a, "0.1.2", &held),
            decline_line(&b, "0.1.3", &held),
        ],
        &[],
    );
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
        Ok(lines(
            &[
                decline_line(
                    &a,
                    "0.1.2",
                    &format!("privileged.yml run {red} ended failure")
                ),
                release_line("0.1.3", &b),
            ],
            &[("0.1.3", &b)],
        ))
    );
    assert_eq!(
        plan_of(&fakes, &request(&a)),
        Ok(lines(&[decline_line(&a, "0.1.2", &held)], &[]))
    );
    assert_eq!(*fakes.runs.writes.borrow(), ["open #1".to_owned()]);
}

#[test]
fn a_red_privileged_run_holds_later_commits_while_the_tip_waits() {
    // Catches: the walk of a run started by one commit's completion releasing a commit after a
    // red privileged run, or holding the commits before it.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let older = code(&mut h, &r, "fn older() {}");
    let a = code(&mut h, &older, "fn a2() {}");
    let b = code(&mut h, &a, "fn b() {}");
    let tip = code(&mut h, &b, "fn tip() {}");
    let fakes = after_root(h, &r);
    green(&fakes, &older);
    fakes.runs.push("ci.yml", &a, "success");
    fakes.runs.push("artifacts.yml", &a, "success");
    let red = fakes.runs.push("privileged.yml", &a, "failure");
    fakes.runs.push("ci.yml", &b, "success");
    fakes.runs.push("artifacts.yml", &b, "success");
    fakes.runs.add("privileged.yml", &b, "push", "main", None);
    fakes.runs.push("artifacts.yml", &tip, "success");
    fakes.runs.add("ci.yml", &tip, "push", "main", None);
    fakes.runs.add("privileged.yml", &tip, "push", "main", None);
    let held = format!("held: privileged run {red} failed on {a}; tracking issue #1");
    assert_eq!(
        plan_of(&fakes, &request(&tip)),
        Ok(lines(
            &[
                release_line("0.1.2", &older),
                decline_line(&a, "0.1.3", &held),
                decline_line(&b, "0.1.4", &held),
                decline_line(&tip, "0.1.5", &held),
            ],
            &[("0.1.2", &older)],
        ))
    );
}

#[test]
fn an_open_issue_with_another_body_is_edited_once_and_others_are_left() {
    // Catches: a second issue opened beside the open one, the body left stale, or an unrelated
    // issue taken for the hold's.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let a = code(&mut h, &r, "fn a2() {}");
    let b = code(&mut h, &a, "fn b() {}");
    let fakes = after_root(h, &r);
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
        Ok(lines(
            &[
                decline_line(&a, "0.1.2", &held),
                decline_line(&b, "0.1.3", &held),
            ],
            &[]
        ))
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
    let fakes = after_root(h, &r);
    fakes
        .runs
        .add("ci.yml", &c, "pull_request", "feature", Some("success"));
    fakes.runs.push("ci.yml", &r, "success");
    fakes.runs.push("privileged.yml", &c, "success");
    fakes.runs.push("artifacts.yml", &c, "success");
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(lines(
            &[decline_line(
                &c,
                "0.1.2",
                "waiting for ci.yml: no push run on main for this commit"
            )],
            &[]
        ))
    );
    fakes.runs.push("ci.yml", &c, "success");
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(lines(&[release_line("0.1.2", &c)], &[("0.1.2", &c)]))
    );
}

#[test]
fn a_failed_and_a_cancelled_run_are_declined_with_their_conclusion() {
    // Catches: a run that ended red or cancelled being reported as pending, or its conclusion
    // lost from the reason.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let c = code(&mut h, &r, "fn b() {}");
    let fakes = after_root(h, &r);
    let failed = fakes.runs.push("ci.yml", &c, "timed_out");
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(lines(
            &[decline_line(
                &c,
                "0.1.2",
                &format!("ci.yml run {failed} ended timed_out")
            )],
            &[]
        ))
    );
    fakes.runs.push("ci.yml", &c, "success");
    fakes.runs.push("privileged.yml", &c, "success");
    let cancelled = fakes.runs.push("artifacts.yml", &c, "cancelled");
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(lines(
            &[decline_line(
                &c,
                "0.1.2",
                &format!("artifacts.yml run {cancelled} ended cancelled")
            )],
            &[]
        ))
    );
}

#[test]
fn missed_commits_are_planned_oldest_first_under_their_own_numbers() {
    // Catches: missed commits planned newest first, under the tip's number, or with the
    // documentation-only commit released.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let a = code(&mut h, &r, "fn a2() {}");
    let d = docs(&mut h, &a);
    let b = code(&mut h, &d, "fn b() {}");
    let fakes = after_root(h, &r);
    for commit in [&a, &d, &b] {
        green(&fakes, commit);
    }
    assert_eq!(
        plan_of(&fakes, &request(&b)),
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
fn a_plan_with_nothing_after_the_last_release_releases_nothing() {
    // Catches: the last release being planned again when nothing came after it.
    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let fakes = after_root(h, &r);
    assert_eq!(
        plan_of(&fakes, &request(&r)),
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
    let fakes = after_root(h, &r);
    green(&fakes, &c);
    fakes.store.insert(Release {
        draft: true,
        ..published("v0.1.2", &c)
    });
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(lines(&[release_line("0.1.2", &c)], &[("0.1.2", &c)]))
    );
    fakes.store.insert(published("nightly", &c));
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(lines(
            &[decline_line(&c, "0.1.2", "already released as nightly")],
            &[]
        ))
    );

    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let c = code(&mut h, &r, "fn b() {}");
    let fakes = Fakes::over(h);
    green(&fakes, &c);
    fakes.store.insert(published("v0.1.2", &r));
    assert_eq!(
        plan_of(&fakes, &request(&c)),
        Ok(lines(
            &[decline_line(&c, "0.1.2", "already released as v0.1.2")],
            &[]
        ))
    );

    let mut h = FakeHistory::default();
    let r = root(&mut h);
    let c = code(&mut h, &r, "fn b() {}");
    let fakes = Fakes::over(h);
    green(&fakes, &c);
    fakes.store.insert(published("v0.1.2", &c));
    assert_eq!(plan_of(&fakes, &request(&c)), Ok(lines(&[], &[])));
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
    for value in ["true", "1"] {
        let frozen = Request {
            freeze: value.to_owned(),
            ..request(&commit)
        };
        assert_eq!(
            plan_of(&fakes, &frozen),
            Ok(vec![
                "no release: RELEASE_FREEZE is set".to_owned(),
                "release=false".to_owned(),
                EMPTY_MATRIX.to_owned(),
            ])
        );
    }
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
    let fakes = after_root(h, &r);
    green(&fakes, &c);
    let dir = scratch("plan-output");
    let file = dir.join("out");
    std::fs::write(&file, "kept=1\n").expect("write the output");
    let asked = Request {
        output: Some(file.clone()),
        ..request(&c)
    };
    let printed = plan_of(&fakes, &asked).expect("planned");
    assert_eq!(
        printed,
        lines(&[release_line("0.1.2", &c)], &[("0.1.2", &c)])
    );
    assert_eq!(
        std::fs::read_to_string(&file).expect("read the output"),
        format!("kept=1\nrelease=true\n{}\n", matrix(&[("0.1.2", &c)]))
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
