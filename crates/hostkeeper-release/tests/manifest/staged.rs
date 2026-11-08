//! The hook program and the `place` sources against the staged tree: a fake tree answers what
//! each path is, and records what it was asked.

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::release::v1::{Hook, Manifest};
use hostkeeper_release::Installer;
use hostkeeper_release::staged::{self, StagedError, TreeEntry, TreeProblem};

use crate::support::{hook, manifest, place};

/// A tree of the given entries; every other path is missing.
struct Tree {
    entries: BTreeMap<&'static str, TreeEntry>,
    asked: Vec<String>,
}

impl Tree {
    fn new(entries: &[(&'static str, TreeEntry)]) -> Tree {
        Tree {
            entries: entries.iter().copied().collect(),
            asked: Vec::new(),
        }
    }

    fn check(&mut self, manifest: &Manifest) -> Result<(), StagedError> {
        staged::check(manifest, |path| {
            self.asked.push(path.to_owned());
            self.entries
                .get(path)
                .copied()
                .unwrap_or(TreeEntry::Missing)
        })
    }
}

/// A tree holding the tarball fixture's program and sources.
fn tarball_tree() -> Tree {
    Tree::new(&[
        ("bin", TreeEntry::Directory),
        ("bin/web", TreeEntry::File),
        ("units", TreeEntry::Directory),
        ("units/web.service", TreeEntry::File),
        ("units/web@.service", TreeEntry::File),
        ("files", TreeEntry::Directory),
        ("files/web.conf", TreeEntry::File),
        ("files/share", TreeEntry::Directory),
    ])
}

fn with_program(program: &str) -> Manifest {
    Manifest {
        hook: Some(Hook {
            command: vec![program.to_owned()],
            ..hook()
        }),
        place: Vec::new(),
        ..manifest(Installer::Tarball)
    }
}

/// Catches: a well-staged tree refused, or a path asked before its leading parts.
#[test]
fn a_tree_holding_the_program_and_every_source_passes_each_part_asked_in_order() {
    let mut tree = tarball_tree();
    assert_eq!(tree.check(&manifest(Installer::Tarball)), Ok(()));
    assert_eq!(
        tree.asked,
        [
            "bin",
            "bin/web",
            "units",
            "units/web.service",
            "units",
            "units/web@.service",
            "files",
            "files/web.conf",
            "files",
            "files/share",
        ]
    );
}

/// Catches: a hook program reached through a link, a link itself, a directory, another kind,
/// or missing from the tree passing; the walk going on below a link or a file.
#[test]
fn a_hook_program_is_a_regular_file_of_the_tree_reached_through_no_link() {
    let cases = [
        (
            "bin/web",
            vec![("bin", TreeEntry::Link)],
            TreeProblem::Link("bin".to_owned()),
            vec!["bin"],
        ),
        (
            "bin/web",
            vec![("bin", TreeEntry::Directory), ("bin/web", TreeEntry::Link)],
            TreeProblem::Link("bin/web".to_owned()),
            vec!["bin", "bin/web"],
        ),
        (
            "bin/web",
            vec![
                ("bin", TreeEntry::Directory),
                ("bin/web", TreeEntry::Directory),
            ],
            TreeProblem::Kind(false),
            vec!["bin", "bin/web"],
        ),
        (
            "bin/web",
            vec![("bin", TreeEntry::Directory), ("bin/web", TreeEntry::Other)],
            TreeProblem::Kind(false),
            vec!["bin", "bin/web"],
        ),
        (
            "bin/web",
            vec![("bin", TreeEntry::Directory)],
            TreeProblem::Missing("bin/web".to_owned()),
            vec!["bin", "bin/web"],
        ),
        (
            "bin/web",
            vec![],
            TreeProblem::Missing("bin".to_owned()),
            vec!["bin"],
        ),
        (
            "bin/web/x",
            vec![("bin", TreeEntry::Directory), ("bin/web", TreeEntry::File)],
            TreeProblem::NotADirectory("bin/web".to_owned()),
            vec!["bin", "bin/web"],
        ),
        (
            "bin/web",
            vec![("bin", TreeEntry::Other)],
            TreeProblem::NotADirectory("bin".to_owned()),
            vec!["bin"],
        ),
    ];
    for (program, entries, why, asked) in cases {
        let mut tree = Tree::new(&entries);
        assert_eq!(
            tree.check(&with_program(program)),
            Err(StagedError::HookProgram {
                program: program.to_owned(),
                why: why.clone()
            }),
            "{program} {why}"
        );
        assert_eq!(tree.asked, asked, "{why}");
    }
    let mut top = Tree::new(&[("web", TreeEntry::File)]);
    assert_eq!(top.check(&with_program("web")), Ok(()));
    for (err, text) in [
        (
            TreeProblem::Link("bin".to_owned()),
            "hook.command names the program \"bin/web\", which passes through the link bin",
        ),
        (
            TreeProblem::Missing("bin".to_owned()),
            "hook.command names the program \"bin/web\", which is not in the tree: bin is missing",
        ),
        (
            TreeProblem::NotADirectory("bin".to_owned()),
            "hook.command names the program \"bin/web\", which passes through bin, which is not a directory",
        ),
        (
            TreeProblem::Kind(false),
            "hook.command names the program \"bin/web\", which is not a regular file",
        ),
    ] {
        let staged = StagedError::HookProgram {
            program: "bin/web".to_owned(),
            why: err,
        };
        assert_eq!(staged.to_string(), text);
    }
}

/// Catches: a unit source that is a directory passing, a file source that is a directory
/// refused, or a source through a link passing.
#[test]
fn a_place_source_is_a_file_or_for_a_file_entry_a_directory_reached_through_no_link() {
    let places = |kind: &str, source: &str| Manifest {
        hook: Some(Hook {
            command: Vec::new(),
            boot: None,
            ..hook()
        }),
        place: vec![place(
            kind,
            source,
            if kind == "unit" {
                "web.service"
            } else {
                "/opt/web/x"
            },
        )],
        ..manifest(Installer::Tarball)
    };
    let mut tree = Tree::new(&[("share", TreeEntry::Directory), ("link", TreeEntry::Link)]);
    assert_eq!(tree.check(&places("file", "share")), Ok(()));
    assert_eq!(
        tree.check(&places("unit", "share")),
        Err(StagedError::PlaceSource {
            entry: "share".to_owned(),
            why: TreeProblem::Kind(false)
        })
    );
    assert_eq!(
        tree.check(&places("file", "link")),
        Err(StagedError::PlaceSource {
            entry: "link".to_owned(),
            why: TreeProblem::Link("link".to_owned())
        })
    );
    let mut other = Tree::new(&[("fifo", TreeEntry::Other)]);
    let err = other.check(&places("file", "fifo")).expect_err("refused");
    assert_eq!(
        err,
        StagedError::PlaceSource {
            entry: "fifo".to_owned(),
            why: TreeProblem::Kind(true)
        }
    );
    assert_eq!(
        err.to_string(),
        "the place source \"fifo\" is not a regular file or a directory"
    );
}

/// Catches: a manifest with no hook program asking the tree for one.
#[test]
fn a_manifest_with_no_hook_program_asks_the_tree_nothing_for_it() {
    let mut tree = Tree::new(&[]);
    assert_eq!(tree.check(&manifest(Installer::HostkeeperArchive)), Ok(()));
    assert_eq!(tree.check(&manifest(Installer::OciImage)), Ok(()));
    assert!(tree.asked.is_empty());
}
