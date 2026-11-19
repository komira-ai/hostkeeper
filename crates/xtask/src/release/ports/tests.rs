//! The side-by-side runner of the process runner: every run answered, in the order given, in
//! batches of the width asked for, each run with a new, empty cache home of its own that is
//! removed after the last run, and a run whose thread panicked answered as an error.

use std::sync::Mutex;

use super::*;
use crate::testdir::TestDir;

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

fn scratch(name: &str) -> TestDir {
    let exe = std::env::current_exe().expect("test binary");
    TestDir::under(&exe.parent().expect("deps").join("xtask-ports-unit"), name)
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

/// The entries of `dir`, by name.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read the directory")
        .map(|e| {
            e.expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn every_run_is_answered_in_the_order_given_whatever_the_width() {
    // Catches: answers matched to the wrong run (a file judged by another file's attestation),
    // a run of the last, short batch dropped, or a width of zero that runs nothing.
    let want: Vec<String> = (0..7).map(|i| format!("gh run-{i}")).collect();
    for width in [0, 1, 3, 7, SIDE_BY_SIDE, 20] {
        let parent = scratch("order");
        let exec = Echo::default();
        let answers = spawn_side_by_side(&runs(7), width, &parent, |program, args, _env| {
            exec.spawn(program, args)
        });
        assert_eq!(stdouts(answers), want, "width {width}");
        let mut started = exec.started.into_inner().expect("not poisoned");
        started.sort();
        assert_eq!(started, want, "width {width}");
    }
    let parent = scratch("none");
    let none = spawn_side_by_side(&[], SIDE_BY_SIDE, &parent, |_, _, _| {
        Err(std::io::Error::other("started"))
    });
    assert!(none.is_empty());
    assert_eq!(entries(&parent), Vec::<String>::new());
}

#[test]
fn every_run_has_a_new_empty_cache_home_of_its_own_removed_after_the_last_run() {
    // Catches: two `gh` sharing one Sigstore TUF cache (each rewriting the metadata the other
    // reads back, so that its verifier fails to start), a run started without `XDG_CACHE_HOME`
    // set to its home, a home that already holds another run's files, a run of a later batch
    // given a home of an earlier one, or the homes left behind.
    let parent = scratch("homes");
    std::fs::write(parent.join("kept"), b"x").expect("a file of the parent's own");
    let homes = Mutex::new(Vec::new());
    let answers = spawn_side_by_side(&runs(7), 3, &parent, |program, args, env| {
        // The whole environment the run is started with on top of this process's: its cache
        // home as `XDG_CACHE_HOME`, which is where `gh` keeps its TUF cache, and nothing else.
        let [("XDG_CACHE_HOME", home)] = env else {
            panic!("the environment of a run: {env:?}");
        };
        assert_eq!(entries(home), Vec::<String>::new(), "{}", home.display());
        // What `gh` leaves in its cache home: a later run must not see it.
        std::fs::write(home.join("root.json"), args.join(" ")).expect("write the cache");
        homes.lock().expect("not poisoned").push((
            args.join(" "),
            home.parent().expect("the batch's directory").to_path_buf(),
            home.file_name()
                .expect("a name")
                .to_string_lossy()
                .into_owned(),
        ));
        Ok(Ran {
            success: true,
            stdout: program.as_bytes().to_vec(),
            stderr: Vec::new(),
        })
    });
    assert_eq!(answers.len(), 7);
    assert!(answers.iter().all(Result::is_ok));
    let mut homes = homes.into_inner().expect("not poisoned");
    homes.sort();
    let batch = homes[0].1.clone();
    assert_eq!(batch.parent(), Some(&*parent));
    let stem = format!("xtask-cache-homes-{}-", std::process::id());
    let name = batch.file_name().expect("a name").to_string_lossy();
    assert!(name.starts_with(&stem), "{name}");
    let want: Vec<(String, std::path::PathBuf, String)> = (0..7)
        .map(|i| (format!("run-{i}"), batch.clone(), format!("{i}")))
        .collect();
    assert_eq!(homes, want);
    assert_eq!(entries(&parent), ["kept"]);
}

#[test]
fn a_parent_that_cannot_hold_the_homes_answers_every_run_with_that_error_and_starts_none() {
    // Catches: runs started with no cache home of their own (sharing the user's cache after
    // all), a failure answered for some runs only, or an error that does not say where.
    let scratch = scratch("parent");
    let parent = scratch.join("a-file");
    std::fs::write(&parent, b"not a directory").expect("write");
    let why = std::fs::create_dir(parent.join("x")).expect_err("a file holds no directory");
    let started = Mutex::new(0_usize);
    let answers = spawn_side_by_side(&runs(3), 2, &parent, |_, _, _| {
        *started.lock().expect("not poisoned") += 1;
        Err(std::io::Error::other("started"))
    });
    let said: Vec<String> = answers
        .iter()
        .map(|a| {
            a.as_ref()
                .map_err(ToString::to_string)
                .expect_err("refused")
        })
        .collect();
    let want = format!("make the cache homes under {}: {why}", parent.display());
    assert_eq!(said, [want.clone(), want.clone(), want]);
    assert!(
        answers
            .iter()
            .all(|a| a.as_ref().is_err_and(|e| e.kind() == why.kind()))
    );
    assert_eq!(*started.lock().expect("not poisoned"), 0);
}

#[test]
fn a_run_whose_thread_panicked_is_an_error_and_the_others_are_answered() {
    // Catches: one panicking run taking the whole batch down, or answered as a success.
    let mut list = runs(3);
    list[1].0 = "panic".to_owned();
    let parent = scratch("panic");
    let exec = Echo::default();
    let answers = spawn_side_by_side(&list, 2, &parent, |program, args, _env| {
        exec.spawn(program, args)
    });
    assert_eq!(answers.len(), 3);
    assert_eq!(
        answers[1].as_ref().map_err(ToString::to_string).err(),
        Some("the run's thread panicked".to_owned())
    );
    assert!(answers[0].is_ok() && answers[2].is_ok());
    assert_eq!(entries(&parent), Vec::<String>::new());
}

#[test]
fn the_directory_of_the_homes_passes_over_a_stale_one_and_leaves_it_alone() {
    // Catches: a directory left by an earlier batch of the same process id failing the new one,
    // or being shared or emptied.
    let parent = scratch("stale");
    for stale in ["left-0", "left-1"] {
        std::fs::create_dir(parent.join(stale)).expect("a stale directory");
        std::fs::write(parent.join(stale).join("kept"), stale).expect("write");
    }
    let next = AtomicU64::new(0);
    let made = new_dir(&parent, "left", &next, HOMES_ATTEMPTS).expect("a new directory");
    assert_eq!(made, parent.join("left-2"));
    assert!(made.is_dir());
    assert_eq!(next.load(Ordering::Relaxed), 3);
    for stale in ["left-0", "left-1"] {
        let kept = std::fs::read(parent.join(stale).join("kept")).expect("read");
        assert_eq!(kept, stale.as_bytes());
    }
}

#[test]
fn the_directory_of_the_homes_fails_after_its_attempts_when_every_name_is_taken() {
    // Catches: a search that runs without end when every number it tries is taken.
    let parent = scratch("full");
    for taken in ["full-0", "full-1", "full-2"] {
        std::fs::create_dir(parent.join(taken)).expect("a taken directory");
    }
    let next = AtomicU64::new(0);
    let failed = new_dir(&parent, "full", &next, 3).expect_err("every number is taken");
    assert_eq!(failed.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        failed.to_string(),
        "no free directory full-<n> in 3 numbers"
    );
    assert_eq!(next.load(Ordering::Relaxed), 3);
}

#[test]
fn the_directory_of_the_homes_returns_any_other_error_at_the_first_number() {
    // Catches: an error other than an existing directory retried with the next number (a
    // search that never ends) or reported as made.
    let parent = scratch("missing").join("not-there");
    let next = AtomicU64::new(0);
    let failed = new_dir(&parent, "gone", &next, HOMES_ATTEMPTS).expect_err("no parent");
    assert_eq!(failed.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(next.load(Ordering::Relaxed), 1);
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
