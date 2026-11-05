use super::*;
use crate::release::fakes::{FakeHistory, Fakes, nth_commit, scratch};

fn repo() -> Repo {
    "komira-ai/hostkeeper".parse().expect("a repository")
}

/// Fakes with `commit` on main.
fn fakes_on_main(commit: &Commit) -> Fakes {
    let fakes = Fakes::over(FakeHistory::default());
    fakes.runs.on_main.borrow_mut().push(commit.clone());
    fakes
}

fn locate_of(fakes: &Fakes, commit: &Commit, output: Option<&Path>) -> Result<Vec<String>, Error> {
    locate(&fakes.runs, &repo(), commit, output)
}

#[test]
fn a_commit_not_on_main_is_refused_whatever_its_runs_say() {
    // Catches: a release cut from a commit that is not on main because its runs are green.
    let commit = nth_commit(1);
    let fakes = Fakes::over(FakeHistory::default());
    for workflow in REQUIRED_WORKFLOWS {
        fakes.runs.push(workflow, &commit, "success");
    }
    assert_eq!(
        locate_of(&fakes, &commit, None),
        Err(Error::Failed(format!(
            "{commit} is not on main: a release is cut only from a commit on main"
        )))
    );
}

#[test]
fn a_commit_with_three_successful_runs_is_located_by_its_artifacts_run() {
    // Catches: the printed run id being that of another workflow than the artifacts run.
    let commit = nth_commit(1);
    let fakes = fakes_on_main(&commit);
    let ci = fakes.runs.push("ci.yml", &commit, "success");
    let privileged = fakes.runs.push("privileged.yml", &commit, "success");
    let artifacts = fakes.runs.push("artifacts.yml", &commit, "success");
    assert_eq!(
        locate_of(&fakes, &commit, None),
        Ok(vec![
            format!(
                "{commit} is on main; ci.yml run {ci}, privileged.yml run {privileged} and artifacts.yml run {artifacts} succeeded"
            ),
            format!("run-id={artifacts}"),
        ])
    );
}

#[test]
fn the_run_id_is_appended_to_the_output_file() {
    // Catches: the answer not written to the output file, or written over what it holds.
    let commit = nth_commit(1);
    let fakes = fakes_on_main(&commit);
    fakes.runs.push("ci.yml", &commit, "success");
    fakes.runs.push("privileged.yml", &commit, "success");
    let artifacts = fakes.runs.push("artifacts.yml", &commit, "success");
    let dir = scratch("locate-output");
    let file = dir.join("out");
    std::fs::write(&file, "kept=1\n").expect("write the output");
    locate_of(&fakes, &commit, Some(file.as_path())).expect("located");
    assert_eq!(
        std::fs::read_to_string(&file).expect("read the output"),
        format!("kept=1\nrun-id={artifacts}\n")
    );
}

#[test]
fn every_workflow_without_a_run_is_a_line_of_the_refusal() {
    // Catches: stopping at the first missing workflow, hiding the others.
    let commit = nth_commit(1);
    let fakes = fakes_on_main(&commit);
    let line = |workflow: &str| {
        format!("no successful push run of {workflow} on main for {commit}: there is no such run")
    };
    assert_eq!(
        locate_of(&fakes, &commit, None),
        Err(Error::Refused(vec![
            line("ci.yml"),
            line("privileged.yml"),
            line("artifacts.yml"),
        ]))
    );
}

#[test]
fn a_pending_a_failed_and_a_cancelled_run_are_each_refused_in_words() {
    // Catches: a run that has not succeeded being taken for a success, or its state lost.
    let commit = nth_commit(1);
    let fakes = fakes_on_main(&commit);
    let ci = fakes.runs.add("ci.yml", &commit, "push", "main", None);
    let privileged = fakes.runs.push("privileged.yml", &commit, "failure");
    let artifacts = fakes.runs.push("artifacts.yml", &commit, "cancelled");
    assert_eq!(
        locate_of(&fakes, &commit, None),
        Err(Error::Refused(vec![
            format!(
                "no successful push run of ci.yml on main for {commit}: run {ci} has not completed"
            ),
            format!(
                "no successful push run of privileged.yml on main for {commit}: run {privileged} failed"
            ),
            format!(
                "no successful push run of artifacts.yml on main for {commit}: run {artifacts} ended cancelled"
            ),
        ]))
    );
}

#[test]
fn a_pull_request_run_does_not_locate_a_commit() {
    // Catches: a green pull-request run standing in for the push run on main.
    let commit = nth_commit(1);
    let fakes = fakes_on_main(&commit);
    fakes.runs.add(
        "ci.yml",
        &commit,
        "pull_request",
        "feature",
        Some("success"),
    );
    fakes.runs.push("privileged.yml", &commit, "success");
    fakes.runs.push("artifacts.yml", &commit, "success");
    assert_eq!(
        locate_of(&fakes, &commit, None),
        Err(Error::Refused(vec![format!(
            "no successful push run of ci.yml on main for {commit}: there is no such run"
        )]))
    );
}

#[test]
fn nothing_is_written_when_a_commit_is_refused() {
    // Catches: the answer file written although the commit is refused.
    let commit = nth_commit(1);
    let fakes = fakes_on_main(&commit);
    fakes.runs.push("ci.yml", &commit, "success");
    let dir = scratch("locate-refused");
    let file = dir.join("out");
    assert!(locate_of(&fakes, &commit, Some(file.as_path())).is_err());
    assert!(!file.exists());
}
