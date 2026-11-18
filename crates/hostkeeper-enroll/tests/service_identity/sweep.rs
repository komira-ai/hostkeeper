//! The directories of services that are not installed, removed after a pass.

use std::sync::{Arc, Mutex};

use hostkeeper_enroll::{FilesError, IdentityDir, IdentityFile, IdentityFiles, Owner};

use crate::support::{SERVICE, World, me, wanted};

/// The identity directory, counting what is removed and failing to remove the names a test lists.
struct Counted {
    dir: IdentityDir,
    removed: Arc<Mutex<Vec<String>>>,
    stuck: Vec<&'static str>,
}

impl IdentityFiles for Counted {
    fn prepare(&self, service: &str, owner: Owner) -> Result<(), FilesError> {
        self.dir.prepare(service, owner)
    }

    fn read(
        &self,
        service: &str,
        file: IdentityFile,
        owner: Owner,
    ) -> Result<Option<Vec<u8>>, FilesError> {
        self.dir.read(service, file, owner)
    }

    fn write(
        &self,
        service: &str,
        file: IdentityFile,
        owner: Owner,
        bytes: &[u8],
    ) -> Result<(), FilesError> {
        self.dir.write(service, file, owner, bytes)
    }

    fn remove(&self, service: &str) -> Result<(), FilesError> {
        self.removed
            .lock()
            .expect("removed")
            .push(service.to_owned());
        if self.stuck.contains(&service) {
            return Err(FilesError::NotRegular(service.into()));
        }
        self.dir.remove(service)
    }

    fn services(&self) -> Result<Vec<String>, FilesError> {
        self.dir.services()
    }
}

fn counted(world: &World, stuck: Vec<&'static str>) -> (Arc<Mutex<Vec<String>>>, Box<Counted>) {
    let removed = Arc::new(Mutex::new(Vec::new()));
    let files = Counted {
        dir: IdentityDir::owned_by(&world.root(), me()),
        removed: Arc::clone(&removed),
        stuck,
    };
    (removed, Box::new(files))
}

fn names(world: &World) -> Vec<String> {
    IdentityDir::owned_by(&world.root(), me())
        .services()
        .expect("list")
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// Proves a sweep removes the directory of every service that is not installed, a stray
/// directory included, and keeps those of installed services whether or not they are wanted
/// (an installed service whose entry dropped its identity keeps what it holds), removing each
/// once and no other.
///
/// Catches: a directory recreated by a pass that began before an uninstall, left for good.
#[test]
fn a_sweep_removes_the_directories_of_services_that_are_not_installed() {
    let mut world = World::new("sweep");
    world.keeper.step(&[
        wanted("alpha"),
        wanted("dropped"),
        wanted("gone"),
        wanted(SERVICE),
    ]);
    let (removed, files) = counted(&world, Vec::new());
    std::fs::create_dir(world.root().join("stray")).expect("a stray directory");
    let mut keeper = world.keeper_over(files);
    keeper
        .sweep(&strings(&["alpha", "dropped"]))
        .expect("sweep");
    assert_eq!(names(&world), ["alpha", "dropped"]);
    assert_eq!(
        *removed.lock().expect("removed"),
        ["gone", SERVICE, "stray"]
    );
    assert!(world.path("dropped", IdentityFile::Key).exists());
    keeper
        .sweep(&strings(&["alpha", "dropped"]))
        .expect("again");
    assert_eq!(removed.lock().expect("removed").len(), 3);
}

/// Proves the loop sweeps after the pass, with what `installed()` says after it: a service that
/// the pass made a directory for and that `installed()` does not name is removed in the same
/// round, and when `installed()` cannot tell nothing is removed.
#[test]
fn the_loop_sweeps_after_its_pass_and_not_when_it_cannot_tell_what_is_installed() {
    let world = World::new("sweep-loop");
    let mut told = vec![None, Some(strings(&["beta"]))].into_iter();
    let mut rounds = 0;
    let mut keeper = world.keeper_over(counted(&world, Vec::new()).1);
    keeper.run_loop(
        &mut || vec![wanted("alpha"), wanted("beta")],
        &mut || told.next().expect("told"),
        &mut |_| {},
        &mut || {
            rounds += 1;
            rounds <= 2
        },
    );
    assert_eq!(names(&world), ["beta"]);
}

/// Proves a directory that cannot be removed is handed to the loop's `failed` once a round and
/// does not stop the others, and that a root that cannot be listed is the failure instead.
#[test]
fn a_sweep_that_fails_is_reported_and_removes_the_rest() {
    let mut world = World::new("sweep-fails");
    world.keeper.step(&[wanted("aaa"), wanted("zzz")]);
    let (removed, files) = counted(&world, vec!["aaa"]);
    let mut keeper = world.keeper_over(files);
    let error = keeper.sweep(&[]).expect_err("one is stuck");
    assert!(matches!(error, FilesError::NotRegular(_)), "{error}");
    assert_eq!(names(&world), ["aaa"]);
    assert_eq!(*removed.lock().expect("removed"), ["aaa", "zzz"]);

    let mut failures = Vec::new();
    let mut rounds = 0;
    keeper.run_loop(
        &mut || Vec::new(),
        &mut || Some(Vec::new()),
        &mut |error| failures.push(error.to_string()),
        &mut || {
            rounds += 1;
            rounds <= 2
        },
    );
    assert_eq!(
        failures,
        ["aaa is not a regular file", "aaa is not a regular file"]
    );

    let file = world.scratch.path().join("a-file");
    std::fs::write(&file, b"x").expect("write");
    let mut under = world.keeper_over(Box::new(IdentityDir::owned_by(&file.join("root"), me())));
    let error = under.sweep(&[]).expect_err("the root is under a file");
    assert!(matches!(error, FilesError::NotDirectory(_)), "{error}");
}
