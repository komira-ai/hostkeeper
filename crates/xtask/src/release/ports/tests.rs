//! The side-by-side runner of the process runner: every run answered, in the order given, in
//! batches of the width asked for, and a run whose thread panicked answered as an error.

use std::sync::Mutex;

use super::*;

/// A runner that answers a run with its program and arguments on stdout, records the runs it
/// started, and panics for the program `panic`.
#[derive(Default)]
struct Echo {
    started: Mutex<Vec<String>>,
}

impl Exec for Echo {
    fn spawn(&self, program: &str, args: &[String]) -> std::io::Result<Ran> {
        assert_ne!(program, "panic", "a run that panics");
        let line = format!("{program} {}", args.join(" "));
        self.started
            .lock()
            .map_err(|_| std::io::Error::other("poisoned"))?
            .push(line.clone());
        Ok(Ran {
            success: true,
            stdout: line.into_bytes(),
            stderr: Vec::new(),
        })
    }
}

fn runs(count: usize) -> Vec<(String, Vec<String>)> {
    (0..count)
        .map(|i| ("gh".to_owned(), vec![format!("run-{i}")]))
        .collect()
}

fn stdouts(answers: Vec<std::io::Result<Ran>>) -> Vec<String> {
    answers
        .into_iter()
        .map(|a| String::from_utf8(a.expect("an answer").stdout).expect("text"))
        .collect()
}

#[test]
fn every_run_is_answered_in_the_order_given_whatever_the_width() {
    // Catches: answers matched to the wrong run (a file judged by another file's attestation),
    // a run of the last, short batch dropped, or a width of zero that runs nothing.
    let want: Vec<String> = (0..7).map(|i| format!("gh run-{i}")).collect();
    for width in [0, 1, 3, 7, SIDE_BY_SIDE, 20] {
        let exec = Echo::default();
        assert_eq!(
            stdouts(spawn_side_by_side(&exec, &runs(7), width)),
            want,
            "width {width}"
        );
        let mut started = exec.started.into_inner().expect("not poisoned");
        started.sort();
        assert_eq!(started, want, "width {width}");
    }
    assert!(spawn_side_by_side(&Echo::default(), &[], SIDE_BY_SIDE).is_empty());
}

#[test]
fn a_run_whose_thread_panicked_is_an_error_and_the_others_are_answered() {
    // Catches: one panicking run taking the whole batch down, or answered as a success.
    let mut list = runs(3);
    list[1].0 = "panic".to_owned();
    let answers = spawn_side_by_side(&Echo::default(), &list, 2);
    assert_eq!(answers.len(), 3);
    assert_eq!(
        answers[1].as_ref().map_err(ToString::to_string).err(),
        Some("the run's thread panicked".to_owned())
    );
    assert!(answers[0].is_ok() && answers[2].is_ok());
}

#[test]
fn the_default_runs_each_run_one_after_the_other_in_order() {
    // Catches: the default of a runner that is not the process runner skipping or reordering
    // runs.
    let exec = Echo::default();
    assert_eq!(
        stdouts(exec.spawn_each(&runs(3))),
        ["gh run-0", "gh run-1", "gh run-2"]
    );
}
