//! The layout's file of the file store (`<state-file>.layout`): written whole when a commit
//! changes the layout and only then, read at load, and refused by name when it is bad. No clock,
//! no timing.

use std::fs;
use std::path::Path;

use super::conformance_tests::{desired_at, rich_layout};
use super::{Backend, BackendError, Commit, Files};
use crate::layout::{Layout, LayoutError, layout_path};
use crate::testutil::{LEGACY, scratch};

fn files_over(dir: &Path) -> Files {
    let state = dir.join("desired.json");
    Files::default()
        .with_desired(&state, Some(LEGACY))
        .with_layout(&layout_path(&state))
}

/// Proves: a commit of the layout alone writes `<state-file>.layout` and no other file, a commit
/// of another part leaves it alone, and a load gives the layout back.
///
/// Catches: the layout written into the state file, which the coordinator before would refuse, or
/// a commit of something else that rewrites the layout.
#[test]
fn a_commit_of_the_layout_writes_its_file_alone_and_a_load_reads_it() {
    let dir = scratch("files-layout-written");
    let files = files_over(dir.path());
    let held = rich_layout();
    files
        .commit(&Commit {
            layout: Some((&Layout::default(), &held)),
            ..Commit::default()
        })
        .expect("committed");
    let names: Vec<_> = fs::read_dir(dir.path())
        .expect("a directory")
        .map(|e| e.expect("an entry").file_name())
        .collect();
    assert_eq!(names, ["desired.json.layout"]);
    let before = fs::read(dir.path().join("desired.json.layout")).expect("the file");

    let (none, some) = (crate::desired::Held::default(), desired_at("2.0", 'a'));
    files
        .commit(&Commit {
            desired: Some((&none, &some)),
            ..Commit::default()
        })
        .expect("a desired state committed");
    assert_eq!(
        fs::read(dir.path().join("desired.json.layout")).expect("the file"),
        before
    );
    let loaded = files.load().expect("loads");
    assert_eq!(loaded.layout, held);
    assert_eq!(loaded.desired, some);
}

/// Proves: a commit of the layout whose temporary file cannot be made is refused as a layout
/// error and leaves the file as it was.
///
/// Catches: a layout commit taken as done when its file was not written.
#[test]
fn a_failed_write_of_the_layout_is_refused_and_leaves_the_file() {
    let dir = scratch("files-layout-fails");
    let files = files_over(dir.path());
    let held = rich_layout();
    files
        .commit(&Commit {
            layout: Some((&Layout::default(), &held)),
            ..Commit::default()
        })
        .expect("committed");
    fs::create_dir(dir.path().join("desired.json.layout.new")).expect("in the way");
    let refused = files.commit(&Commit {
        layout: Some((&held, &Layout::default())),
        ..Commit::default()
    });
    assert!(matches!(
        refused,
        Err(BackendError::Layout(LayoutError::Io(_)))
    ));
    assert_eq!(files.load_layout().expect("loads"), held);
}

/// Proves: with no layout file a load holds an empty layout, a file store given no layout file
/// takes a commit of the layout as done and loads none, and a bad file is refused at load by its
/// path.
///
/// Catches: a start that needs the new file, which the coordinator before never wrote, or a bad
/// file loaded as an empty lab.
#[test]
fn a_load_without_the_layout_file_holds_none_and_a_bad_one_is_refused() {
    let dir = scratch("files-layout-absent");
    let files = files_over(dir.path());
    assert!(files.load().expect("loads").layout.is_empty());
    assert!(!dir.path().join("desired.json.layout").exists());

    let held = rich_layout();
    let unplaced = Files::default();
    unplaced
        .commit(&Commit {
            layout: Some((&Layout::default(), &held)),
            ..Commit::default()
        })
        .expect("taken as done");
    assert!(unplaced.load().expect("loads").layout.is_empty());

    fs::write(dir.path().join("desired.json.layout"), b"7").expect("a bad file");
    let refused = files.load().expect_err("refused");
    assert!(
        refused.to_string().starts_with(&format!(
            "{}: ",
            dir.path().join("desired.json.layout").display()
        )),
        "{refused}"
    );
}
