//! Pruning a package directory to `current` and `previous` (design section 4.7): the rule, a
//! pure function of what the directory holds, and the deletion, which renames a version out of
//! the way, removes its seal, then removes its tree, never following a link.

mod support;

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hostkeeper_steps::selfupdate::Layout;
use hostkeeper_steps::{
    PRUNE_PREFIX, Protected, PruneInputs, PrunePlan, PruneReport, PruneStep, Seal, prune_plan,
    tree_digest,
};
use support::Scratch;

const PACKAGE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// The package directory `<scratch>/<service>`, made.
fn package(s: &Scratch, service: &str) -> Layout {
    let layout = Layout::package(&s.path().join(service));
    fs::create_dir_all(layout.dir()).unwrap();
    layout
}

/// Version `version` of `layout` as an unpack leaves it: `bin/x` and `manifest.toml`, a
/// directory `bin/lib` of mode 0555, the tree 0555, sealed. Returns the tree's path.
fn version(layout: &Layout, version: &str) -> PathBuf {
    let dir = layout.version_path(version);
    fs::create_dir_all(dir.join("bin/lib")).unwrap();
    fs::write(dir.join("bin/x"), format!("the binary of {version}")).unwrap();
    fs::write(dir.join("bin/lib/y"), "a library").unwrap();
    fs::write(dir.join("manifest.toml"), "schema = 1\n").unwrap();
    for (path, mode) in [
        (dir.join("bin/x"), 0o555),
        (dir.join("bin/lib/y"), 0o444),
        (dir.join("manifest.toml"), 0o444),
        (dir.join("bin/lib"), 0o555),
        (dir.join("bin"), 0o555),
        (dir.clone(), 0o555),
    ] {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    let seal = Seal::new(&tree_digest(&dir).unwrap(), PACKAGE).unwrap();
    fs::write(layout.seal_path(version), seal.to_json()).unwrap();
    dir
}

/// `layout` holds `versions`, each sealed, `current` names `current` and `previous` names
/// `previous`.
fn holding(layout: &Layout, versions: &[&str], current: &str, previous: &str) {
    for name in versions {
        version(layout, name);
    }
    link(layout, "current", current);
    link(layout, "previous", previous);
}

fn link(layout: &Layout, name: &str, target: &str) {
    let path = layout.dir().join(name);
    fs::remove_file(&path).ok();
    symlink(target, &path).unwrap();
}

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Every entry below `dir` with its kind and, for a file, its bytes: what a test compares to show
/// that nothing changed.
fn snapshot(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let body = match meta.is_file() {
                true => String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned(),
                false => String::new(),
            };
            out.push((path.clone(), format!("{:?} {body}", meta.file_type())));
            if meta.is_dir() {
                stack.push(path);
            }
        }
    }
    out.sort();
    out
}

/// What the rule plans for `versions` with directories, `seals`, the links and `protected`.
fn plan(
    versions: &[&str],
    seals: &[&str],
    current: Option<&str>,
    previous: Option<&str>,
    protected: Protected,
) -> PrunePlan {
    prune_plan(&PruneInputs {
        versions: set(versions),
        seals: set(seals),
        leftovers: set(&[".stage-prune-0011223344556677"]),
        current: current.map(str::to_owned),
        previous: previous.map(str::to_owned),
        protected,
    })
}

/// Catches a rule that keeps by name rather than by link, forgets one of the three protections,
/// or keeps half of a version: with `current` naming C and `previous` B, the release of an
/// unfinished record D, the target of a switch under way E and a staged version H are kept with
/// them; A (directory and seal), F (a seal with no directory) and G (a directory with no seal)
/// are deleted whole; the leftover of a crashed prune is removed.
#[test]
fn the_rule_keeps_current_previous_and_the_protected_and_deletes_the_rest() {
    let protected = Protected {
        previous_releases: set(&["D"]),
        switch_targets: set(&["E"]),
        staged: set(&["H"]),
    };
    let planned = plan(
        &["A", "B", "C", "D", "E", "G", "H"],
        &["A", "B", "C", "D", "E", "F", "H"],
        Some("C"),
        Some("B"),
        protected,
    );
    assert_eq!(
        planned,
        PrunePlan {
            keep: set(&["B", "C", "D", "E", "H"]),
            delete: set(&["A", "F", "G"]),
            leftovers: set(&[".stage-prune-0011223344556677"]),
            unresolved: false,
        }
    );
    let one_link = plan(
        &["A", "B"],
        &["A", "B"],
        Some("B"),
        Some("B"),
        Protected::default(),
    );
    assert_eq!(one_link.keep, set(&["B"]));
    assert_eq!(one_link.delete, set(&["A"]));
}

/// Catches a rule that takes a link it cannot resolve for "everything is surplus": `current` or
/// `previous` missing, or naming a version with no directory (only a seal, or nothing), plans
/// nothing deleted, not even a leftover, and says `unresolved`.
#[test]
fn the_rule_deletes_nothing_when_a_link_does_not_resolve() {
    let cases = [
        (None, Some("B")),
        (Some("C"), None),
        (Some("Z"), Some("B")),
        (Some("C"), Some("F")),
    ];
    for (current, previous) in cases {
        let planned = plan(
            &["A", "B", "C"],
            &["A", "B", "C", "F"],
            current,
            previous,
            Protected::default(),
        );
        assert_eq!(
            planned,
            PrunePlan {
                keep: set(&["A", "B", "C", "F"]),
                delete: BTreeSet::new(),
                leftovers: BTreeSet::new(),
                unresolved: true,
            },
            "{current:?} {previous:?}"
        );
    }
}

/// Catches a protection that holds for one kind only.
#[test]
fn protected_holds_each_of_its_three_kinds() {
    let protected = Protected {
        previous_releases: set(&["1"]),
        switch_targets: set(&["2"]),
        staged: set(&["3"]),
    };
    for version in ["1", "2", "3"] {
        assert!(protected.contains(version), "{version}");
    }
    assert!(!protected.contains("4"));
}

/// Catches a prune that leaves a seal or a tree of a doomed version, touches a name that is not
/// a version, or fails on a read-only tree: of A, B and C with `current` C and `previous` B, A
/// and its seal go, a seal with no directory X and a directory with no seal Y go, a leftover
/// directory of a crashed prune goes, and an unpack's staging directory, a temporary seal, the
/// links' temporary names, a file and a link named as versions, a directory named as a seal and a
/// name that is not UTF-8 stay as they were.
#[test]
fn a_prune_deletes_each_surplus_version_whole_and_nothing_else() {
    let s = Scratch::new("prune-surplus");
    let layout = package(&s, "web");
    holding(&layout, &["A", "B", "C"], "C", "B");
    fs::write(layout.seal_path("X"), "a seal with no tree").unwrap();
    fs::create_dir(layout.version_path("Y")).unwrap();
    fs::write(layout.version_path("Y").join("f"), "a tree with no seal").unwrap();
    let leftover = layout.dir().join(".stage-prune-0011223344556677");
    fs::create_dir_all(leftover.join("bin")).unwrap();
    fs::set_permissions(leftover.join("bin"), fs::Permissions::from_mode(0o555)).unwrap();
    let dir = layout.dir();
    fs::create_dir(dir.join(".stage-0123")).unwrap();
    fs::write(dir.join(".Q.seal.tmp"), "an unpack's temporary seal").unwrap();
    fs::write(dir.join(".stage-prune-file"), "not a directory").unwrap();
    symlink("C", dir.join("current.tmp")).unwrap();
    fs::write(dir.join("W"), "a file named as a version").unwrap();
    symlink("B", dir.join("V")).unwrap();
    fs::create_dir(dir.join("U.seal")).unwrap();
    fs::write(dir.join(OsStr::from_bytes(b"not-utf8-\xff")), "").unwrap();

    let report = layout.prune(&Protected::default()).unwrap();

    assert_eq!(
        report,
        PruneReport {
            unresolved: false,
            kept: set(&["B", "C"]),
            deleted: set(&["A", "X", "Y"]),
            failed: Default::default(),
        }
    );
    let mut left = vec![
        ".Q.seal.tmp",
        ".stage-0123",
        ".stage-prune-file",
        "B",
        "B.seal",
        "C",
        "C.seal",
        "U.seal",
        "V",
        "W",
        "current",
        "current.tmp",
        "not-utf8-\u{fffd}",
        "previous",
    ];
    left.sort();
    assert_eq!(names(dir), left);
    assert_eq!(layout.current().unwrap().as_deref(), Some("C"));
    assert_eq!(layout.previous().unwrap().as_deref(), Some("B"));
    layout.check_tree("B").unwrap();
    layout.check_tree("C").unwrap();
}

/// Catches a prune that reads `current` or `previous` it cannot resolve as "everything is
/// surplus": `current` missing, `current` naming a version with no tree, `previous` with a
/// directory part, and `previous` not a link each leave every tree and seal as it was, with the
/// report `unresolved`.
#[test]
fn a_dangling_current_or_previous_deletes_nothing() {
    type Breaks = fn(&Layout);
    let breaks: [(&str, Breaks); 4] = [
        ("current missing", |layout| {
            fs::remove_file(layout.current_path()).unwrap()
        }),
        ("current dangling", |layout| link(layout, "current", "Z")),
        ("previous out of the directory", |layout| {
            link(layout, "previous", "../other/B")
        }),
        ("previous not a link", |layout| {
            fs::remove_file(layout.previous_path()).unwrap();
            fs::write(layout.previous_path(), "B").unwrap();
        }),
    ];
    for (case, broken) in breaks {
        let s = Scratch::new("prune-dangling");
        let layout = package(&s, "web");
        holding(&layout, &["A", "B", "C"], "C", "B");
        broken(&layout);
        let before = snapshot(layout.dir());
        let report = layout.prune(&Protected::default()).unwrap();
        assert!(report.unresolved, "{case}");
        assert_eq!(report.kept, set(&["A", "B", "C"]), "{case}");
        assert!(report.deleted.is_empty(), "{case}");
        assert_eq!(snapshot(layout.dir()), before, "{case}");
    }
}

/// Catches a deletion that follows a link out of the version directory: a surplus tree holding a
/// link to a sibling service's directory, a link to `..` and a link to a file of a kept version
/// is removed, and the sibling's tree, the package directory and the kept version are intact.
#[test]
fn prune_stays_inside_the_version_directory() {
    let s = Scratch::new("prune-links");
    let layout = package(&s, "web");
    let sibling = package(&s, "db");
    holding(&sibling, &["1"], "1", "1");
    holding(&layout, &["B", "C"], "C", "B");
    let doomed = layout.version_path("A");
    fs::create_dir_all(doomed.join("bin")).unwrap();
    symlink(sibling.dir(), doomed.join("bin/sibling")).unwrap();
    symlink("..", doomed.join("bin/up")).unwrap();
    symlink(layout.version_path("C").join("bin/x"), doomed.join("x")).unwrap();
    fs::set_permissions(doomed.join("bin"), fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(&doomed, fs::Permissions::from_mode(0o555)).unwrap();
    let sibling_before = snapshot(sibling.dir());

    let report = layout.prune(&Protected::default()).unwrap();

    assert_eq!(report.deleted, set(&["A"]));
    assert_eq!(snapshot(sibling.dir()), sibling_before);
    assert_eq!(
        names(layout.dir()),
        ["B", "B.seal", "C", "C.seal", "current", "previous"]
    );
    layout.check_tree("C").unwrap();
    let mode = fs::metadata(layout.version_path("C"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o555);
}

/// Catches a prune that shares one scan between services: two services with three versions each,
/// and the prune of one leaves the other exactly as it was.
#[test]
fn prune_is_per_service() {
    let s = Scratch::new("prune-per-service");
    let web = package(&s, "web");
    let db = package(&s, "db");
    holding(&web, &["1", "2", "3"], "3", "2");
    holding(&db, &["1", "2", "3"], "3", "2");
    let db_before = snapshot(db.dir());

    let report = web.prune(&Protected::default()).unwrap();

    assert_eq!(report.deleted, set(&["1"]));
    assert_eq!(
        names(web.dir()),
        ["2", "2.seal", "3", "3.seal", "current", "previous"]
    );
    assert_eq!(snapshot(db.dir()), db_before);
}

/// The doomed version of the crash tests: `web` holding 1, 2 and 3, `current` 3, `previous` 2.
fn crash_rig(name: &str) -> (Scratch, Layout) {
    let s = Scratch::new(name);
    let layout = package(&s, "web");
    holding(&layout, &["1", "2", "3"], "3", "2");
    (s, layout)
}

/// Catches a crash that leaves a half-deleted version, a seal naming a tree that is half gone, or
/// a state the next prune cannot finish: a prune stopped after the rename of the doomed version,
/// after its seal is removed, and after half its tree is removed leaves `1/` absent (never
/// partial), and the next prune ends where a prune that did not crash ends.
#[test]
fn a_crash_mid_prune_is_finished_by_the_next_prune() {
    let (_clean, uncrashed) = crash_rig("prune-uncrashed");
    let reference = uncrashed.prune(&Protected::default()).unwrap();
    let finished = names(uncrashed.dir());
    assert_eq!(
        finished,
        ["2", "2.seal", "3", "3.seal", "current", "previous"]
    );

    type Stop = fn(&PruneStep<'_>, &mut u32) -> bool;
    let stops: [(&str, Stop); 3] = [
        ("after the rename", |step, _| {
            matches!(step, PruneStep::Moved { version: "1" })
        }),
        ("after the seal", |step, _| {
            matches!(step, PruneStep::SealRemoved { version: "1" })
        }),
        ("after half the tree", |step, seen| {
            if let PruneStep::Removing { .. } = step {
                *seen += 1;
            }
            *seen == 3
        }),
    ];
    for (case, stop) in stops {
        let (_s, layout) = crash_rig("prune-crash");
        let mut seen = 0;
        let crashed = layout
            .prune_with(
                &Protected::default(),
                &mut |step| match stop(&step, &mut seen) {
                    true => Err(io::Error::other("crash")),
                    false => Ok(()),
                },
            )
            .unwrap();
        assert_eq!(crashed.failed.keys().collect::<Vec<_>>(), ["1"], "{case}");
        assert!(!layout.version_path("1").exists(), "{case}");
        let leftovers: Vec<String> = names(layout.dir())
            .into_iter()
            .filter(|name| name.starts_with(PRUNE_PREFIX))
            .collect();
        assert_eq!(leftovers.len(), 1, "{case}");
        layout.check_tree("2").unwrap();
        layout.check_tree("3").unwrap();

        let again = layout.prune(&Protected::default()).unwrap();
        assert!(again.failed.is_empty(), "{case}: {again:?}");
        assert_eq!(names(layout.dir()), finished, "{case}");
        assert_eq!(again.kept, reference.kept, "{case}");
    }
}

/// Catches a failed removal that stops the others, goes unreported, or is not tried again: a
/// version whose seal is a directory cannot have it removed and is reported with the error,
/// another surplus version is still deleted, a leftover that cannot be removed is reported by
/// its name, and the next prune, once the cause is gone, deletes what is left.
#[test]
fn a_removal_that_fails_is_reported_and_the_others_are_deleted() {
    let s = Scratch::new("prune-fails");
    let layout = package(&s, "web");
    holding(&layout, &["1", "2", "3", "4"], "4", "3");
    fs::remove_file(layout.seal_path("1")).unwrap();
    fs::create_dir(layout.seal_path("1")).unwrap();
    let leftover = layout.dir().join(".stage-prune-ffffffffffffffff");
    fs::create_dir(&leftover).unwrap();

    let report = layout
        .prune_with(&Protected::default(), &mut |step| match step {
            PruneStep::Removing { path } if path.ends_with(".stage-prune-ffffffffffffffff") => {
                Err(io::Error::other("refused"))
            }
            _ => Ok(()),
        })
        .unwrap();

    assert_eq!(report.deleted, set(&["2"]));
    assert_eq!(
        report.failed.keys().collect::<Vec<_>>(),
        [".stage-prune-ffffffffffffffff", "1"]
    );
    assert_eq!(report.failed[".stage-prune-ffffffffffffffff"], "refused");
    assert!(!report.failed["1"].is_empty());
    assert!(!layout.version_path("1").exists());

    fs::remove_dir(layout.seal_path("1")).unwrap();
    let again = layout.prune(&Protected::default()).unwrap();
    assert!(again.failed.is_empty(), "{again:?}");
    assert_eq!(
        names(layout.dir()),
        ["3", "3.seal", "4", "4.seal", "current", "previous"]
    );
}

/// Catches a prune that a long version name defeats: a version of 250 bytes, the longest whose
/// seal `<version>.seal` still fits a file name of 255 bytes, is renamed under a short name (the
/// prefix and the version would not fit) and deleted.
#[test]
fn a_version_with_the_longest_name_is_pruned() {
    let s = Scratch::new("prune-long");
    let layout = package(&s, "web");
    let long = "a".repeat(250);
    holding(&layout, &[long.as_str(), "2", "3"], "3", "2");
    let report = layout.prune(&Protected::default()).unwrap();
    assert_eq!(report.deleted, set(&[long.as_str()]));
    assert_eq!(
        names(layout.dir()),
        ["2", "2.seal", "3", "3.seal", "current", "previous"]
    );
}

/// Catches a prune of the wrong kind of directory, or of one that is not there: the updater's
/// binary directory is refused, and a package directory that cannot be listed is an error with
/// nothing done.
#[test]
fn a_prune_is_refused_for_a_binary_directory_and_fails_on_a_missing_one() {
    let s = Scratch::new("prune-kinds");
    let binary = Layout::new(&s.path().join("bin"));
    let refused = binary.prune(&Protected::default()).unwrap_err();
    assert_eq!(refused.kind(), io::ErrorKind::InvalidInput);
    let missing = Layout::package(&s.path().join("missing"));
    let failed = missing.prune(&Protected::default()).unwrap_err();
    assert_eq!(failed.kind(), io::ErrorKind::NotFound);
}

/// Catches the inputs of a prune read from names instead of what the entries are: a version is a
/// real directory, a seal is anything but a directory named `<version>.seal`, and the links are
/// read as their plain targets.
#[test]
fn the_inputs_are_read_from_what_the_entries_are() {
    let s = Scratch::new("prune-inputs");
    let layout = package(&s, "web");
    holding(&layout, &["1", "2"], "2", "1");
    fs::write(layout.seal_path("3"), "").unwrap();
    symlink("1", layout.dir().join("4")).unwrap();
    let protected = Protected {
        staged: set(&["9"]),
        ..Protected::default()
    };
    let inputs = layout.prune_inputs(&protected).unwrap();
    assert_eq!(
        inputs,
        PruneInputs {
            versions: set(&["1", "2"]),
            seals: set(&["1", "2", "3"]),
            leftovers: BTreeSet::new(),
            current: Some("2".to_owned()),
            previous: Some("1".to_owned()),
            protected,
        }
    );
}
