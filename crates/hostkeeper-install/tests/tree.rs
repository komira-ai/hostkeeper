//! A repository package's unit files and the enablement of its `run` units (packages-native.md
//! section 10.2, packages-repositories.md section 5), run directly over scratch trees.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use hostkeeper_install::repository::enable::{SYSTEMD_DIR, apply, remove_link, unlinked, wanted};
use hostkeeper_install::repository::tree::{install_links, unit_file};
use hostkeeper_testkit::Scratch;

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

/// A tree under a scratch directory, built entry by entry.
struct Tree {
    scratch: Scratch,
}

impl Tree {
    fn new(name: &str) -> Tree {
        let scratch = scratch(name);
        fs::create_dir(scratch.path().join("tree")).expect("the tree");
        Tree { scratch }
    }

    fn root(&self) -> PathBuf {
        self.scratch.path().join("tree")
    }

    fn file(&self, rel: &str, text: &str, mode: u32) -> &Tree {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().expect("a parent")).expect("the directories");
        fs::write(&path, text).expect("the file");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("the mode");
        self
    }

    fn dir(&self, rel: &str) -> &Tree {
        fs::create_dir_all(self.root().join(rel)).expect("the directory");
        self
    }

    fn link(&self, rel: &str, target: &str) -> &Tree {
        let path = self.root().join(rel);
        fs::create_dir_all(path.parent().expect("a parent")).expect("the directories");
        symlink(target, path).expect("the link");
        self
    }
}

/// Proves `unit_file` finds a regular file in `usr/lib/systemd/system` first and
/// `lib/systemd/system` next, and nothing for a link, a directory or a missing unit.
#[test]
fn unit_file_finds_a_regular_file_in_either_directory() {
    let tree = Tree::new("unit-file");
    tree.file("usr/lib/systemd/system/a.service", "x", 0o644);
    tree.file("lib/systemd/system/b.service", "x", 0o644);
    tree.file("usr/lib/systemd/system/both.service", "x", 0o644);
    tree.file("lib/systemd/system/both.service", "y", 0o644);
    tree.link("usr/lib/systemd/system/link.service", "a.service");
    tree.dir("usr/lib/systemd/system/dir.service");
    let root = tree.root();
    assert_eq!(
        unit_file(&root, "a.service").as_deref(),
        Some("usr/lib/systemd/system/a.service")
    );
    assert_eq!(
        unit_file(&root, "b.service").as_deref(),
        Some("lib/systemd/system/b.service")
    );
    assert_eq!(
        unit_file(&root, "both.service").as_deref(),
        Some("usr/lib/systemd/system/both.service")
    );
    for none in ["link.service", "dir.service", "missing.service"] {
        assert_eq!(unit_file(&root, none), None, "{none}");
    }
}

fn links(unit: &str, text: &str) -> Vec<String> {
    install_links(unit, text).into_iter().collect()
}

/// Proves `WantedBy=` gives a `<target>.wants/<unit>` link and `RequiredBy=` a
/// `<target>.requires/<unit>` link, several targets on one line or on several lines give one each,
/// and a unit with no `[Install]` gives none.
#[test]
fn install_links_follow_wanted_by_and_required_by() {
    let unit = "mdm.service";
    assert_eq!(
        links(
            unit,
            "[Unit]\nDescription=x\n[Service]\nExecStart=/x\n[Install]\nWantedBy=multi-user.target\n"
        ),
        ["multi-user.target.wants/mdm.service"]
    );
    assert_eq!(
        links(unit, "[Install]\nRequiredBy=multi-user.target\n"),
        ["multi-user.target.requires/mdm.service"]
    );
    assert_eq!(
        links(
            unit,
            "[Install]\nWantedBy=multi-user.target timers.target\nWantedBy=default.target\nRequiredBy=network-online.target\n"
        ),
        [
            "default.target.wants/mdm.service",
            "multi-user.target.wants/mdm.service",
            "network-online.target.requires/mdm.service",
            "timers.target.wants/mdm.service",
        ]
    );
    assert_eq!(
        links(
            unit,
            "[Install]\n  WantedBy = multi-user.target  \nAlias=x.service\nAlso=y.service\nnot an assignment\n"
        ),
        ["multi-user.target.wants/mdm.service"]
    );
    assert!(links(unit, "[Service]\nExecStart=/x\n").is_empty());
    assert!(links(unit, "").is_empty());
}

/// Proves an empty assignment drops the targets before it (as systemd reads it), later ones
/// still count, and a target named outside `[Install]` is ignored.
#[test]
fn install_links_follow_an_empty_assignment_and_ignore_other_sections() {
    let unit = "mdm.service";
    assert!(links(unit, "[Install]\nWantedBy=multi-user.target\nWantedBy=\n").is_empty());
    assert_eq!(
        links(
            unit,
            "[Install]\nWantedBy=multi-user.target\nWantedBy=\nWantedBy=timers.target\n"
        ),
        ["timers.target.wants/mdm.service"]
    );
    assert_eq!(
        links(
            unit,
            "[Install]\nWantedBy=multi-user.target\nRequiredBy=timers.target\nRequiredBy=\n"
        ),
        ["multi-user.target.wants/mdm.service"]
    );
    assert!(
        links(
            unit,
            "[Unit]\nWantedBy=multi-user.target\n[Service]\nRequiredBy=timers.target\n"
        )
        .is_empty()
    );
    assert_eq!(
        links(
            unit,
            "[Install]\nWantedBy=multi-user.target\n[Other]\nWantedBy=timers.target\n"
        ),
        ["multi-user.target.wants/mdm.service"]
    );
    assert_eq!(
        links(
            unit,
            "[Install]\nWantedBy=multi-user.target\n[Unit]\n[Install]\nRequiredBy=timers.target\n"
        ),
        [
            "multi-user.target.wants/mdm.service",
            "timers.target.requires/mdm.service"
        ]
    );
}

/// Proves a target that is not a unit name (no dot, a slash, an odd byte, 256 bytes) yields no
/// link and no refusal, the good targets beside it still link, and one of 255 bytes is accepted.
#[test]
fn install_links_skip_a_target_that_is_not_a_unit_name() {
    let long = format!("{}.target", "a".repeat(249));
    for (key, target) in [
        ("WantedBy", "nodot"),
        ("WantedBy", "a/timers.target"),
        ("RequiredBy", "bad name.timer,x"),
        ("WantedBy", "é.target"),
        ("RequiredBy", "../x.target"),
        ("WantedBy", long.as_str()),
    ] {
        let text = format!("[Install]\n{key}={target}\nWantedBy=ok.target\n");
        let found = links("mdm.service", &text);
        assert_eq!(found, ["ok.target.wants/mdm.service"], "{target}");
    }
    let at_bound = format!("{}.target", "a".repeat(248));
    assert_eq!(at_bound.len(), 255);
    assert_eq!(
        links("mdm.service", &format!("[Install]\nWantedBy={at_bound}\n")).len(),
        1
    );
}

const MDM_UNIT: &str = "[Unit]\nDescription=mdm\n[Service]\nExecStart=/usr/bin/mdm\n[Install]\nWantedBy=multi-user.target\nRequiredBy=graphical.target\n";

/// Proves `wanted` gives, for each run unit, the absolute link path under the service manager's
/// directory and the absolute path of the unit file where the package installs it (in either
/// unit directory).
#[test]
fn wanted_maps_each_link_to_its_unit_file() {
    assert_eq!(SYSTEMD_DIR, "etc/systemd/system");
    let tree = Tree::new("wanted");
    tree.file("usr/lib/systemd/system/mdm.service", MDM_UNIT, 0o644);
    tree.file(
        "lib/systemd/system/mdm.timer",
        "[Install]\nWantedBy=timers.target\n",
        0o644,
    );
    tree.file(
        "usr/lib/systemd/system/mdm-quiet.service",
        "[Service]\n",
        0o644,
    );
    let run = ["mdm.service", "mdm.timer", "mdm-quiet.service"].map(str::to_owned);
    let found = wanted(&tree.root(), &run).expect("the links");
    let want: BTreeMap<String, String> = [
        (
            "/etc/systemd/system/multi-user.target.wants/mdm.service",
            "/usr/lib/systemd/system/mdm.service",
        ),
        (
            "/etc/systemd/system/graphical.target.requires/mdm.service",
            "/usr/lib/systemd/system/mdm.service",
        ),
        (
            "/etc/systemd/system/timers.target.wants/mdm.timer",
            "/lib/systemd/system/mdm.timer",
        ),
    ]
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .into_iter()
    .collect();
    assert_eq!(found, want);
}

/// Proves `wanted` never refuses a run unit: one the package does not hold, or asking for a
/// target that is not a unit name, adds no link; a unit file that cannot be read is an I/O error.
#[test]
fn wanted_adds_nothing_for_a_missing_or_bad_unit_and_reports_an_unreadable_one() {
    let tree = Tree::new("wanted-bad");
    tree.file(
        "usr/lib/systemd/system/mdm.service",
        "[Install]\nWantedBy=nodot\n",
        0o644,
    );
    tree.file("usr/lib/systemd/system/locked.service", "x", 0o000);
    let run = ["absent.service".to_owned(), "mdm.service".to_owned()];
    assert_eq!(
        wanted(&tree.root(), &run).expect("no refusal"),
        BTreeMap::new()
    );
    let run = ["locked.service".to_owned()];
    let error = wanted(&tree.root(), &run).expect_err("unreadable");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
}

/// Proves `unlinked` lists, in the run list's order, the units with no unit file in the tree, no
/// `[Install]` section, or only targets that are not unit names, and leaves out a unit with a
/// link in either unit directory; a unit file that cannot be read is listed too (a superuser reads it, so this holds for a
/// non-root run, as the suite's others do).
#[test]
fn unlinked_lists_the_run_units_the_tree_gives_no_link() {
    let tree = Tree::new("unlinked");
    tree.file(
        "usr/lib/systemd/system/a.service",
        "[Install]\nWantedBy=multi-user.target\n",
        0o644,
    );
    tree.file(
        "lib/systemd/system/b.service",
        "[Install]\nWantedBy=timers.target\n",
        0o644,
    );
    tree.file("usr/lib/systemd/system/bare.service", "[Service]\n", 0o644);
    tree.file(
        "usr/lib/systemd/system/bad.service",
        "[Install]\nWantedBy=nodot\n",
        0o644,
    );
    tree.dir("usr/lib/systemd/system/dir.service");
    tree.file(
        "usr/lib/systemd/system/locked.service",
        "[Install]\nWantedBy=x.target\n",
        0o000,
    );
    let run = [
        "bare.service",
        "a.service",
        "ghost.service",
        "b.service",
        "bad.service",
        "dir.service",
        "locked.service",
    ]
    .map(str::to_owned);
    assert_eq!(
        unlinked(&tree.root(), &run),
        [
            "bare.service",
            "ghost.service",
            "bad.service",
            "dir.service",
            "locked.service"
        ]
    );
    assert!(unlinked(&tree.root(), &[]).is_empty());
}

/// Proves a unit file that is not UTF-8 is read lossily: its `[Install]` section still counts.
#[test]
fn wanted_reads_a_unit_that_is_not_utf8() {
    let tree = Tree::new("wanted-binary");
    let path = tree.root().join("usr/lib/systemd/system/mdm.service");
    fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
    fs::write(&path, b"# \xff\n[Install]\nWantedBy=multi-user.target\n").expect("the unit");
    let found = wanted(&tree.root(), &["mdm.service".to_owned()]).expect("the links");
    assert_eq!(
        found.keys().collect::<Vec<_>>(),
        ["/etc/systemd/system/multi-user.target.wants/mdm.service"]
    );
}

const LINK: &str = "/etc/systemd/system/multi-user.target.wants/mdm.service";
const UNIT: &str = "/usr/lib/systemd/system/mdm.service";

fn want_of(links: &[(&str, &str)]) -> BTreeMap<String, String> {
    links
        .iter()
        .map(|(l, t)| ((*l).to_owned(), (*t).to_owned()))
        .collect()
}

fn recorded(links: &[&str]) -> BTreeSet<String> {
    links.iter().map(|l| (*l).to_owned()).collect()
}

fn real(root: &Path, link: &str) -> PathBuf {
    root.join(link.trim_start_matches('/'))
}

/// Proves `apply` writes a missing link (making the directories, mode 0755) to the unit file,
/// answers it as owned, and keeps a link it recorded and still finds.
#[test]
fn apply_creates_a_missing_link_and_keeps_a_recorded_one() {
    let root = scratch("apply-create");
    let want = want_of(&[(LINK, UNIT)]);
    let owned = apply(root.path(), &want, &recorded(&[])).expect("the links");
    assert_eq!(owned, recorded(&[LINK]));
    let path = real(root.path(), LINK);
    assert_eq!(fs::read_link(&path).expect("a link"), Path::new(UNIT));
    let dir = path.parent().expect("a parent");
    assert_eq!(
        fs::metadata(dir)
            .expect("the directory")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    let again = apply(root.path(), &want, &recorded(&[LINK])).expect("the links");
    assert_eq!(again, recorded(&[LINK]));
    assert_eq!(fs::read_link(&path).expect("a link"), Path::new(UNIT));
}

/// Proves a link already there that hostkeeper did not record (an operator's, the package's own
/// scripts', a file) is left as it is and is not claimed, and a recorded link that now points
/// elsewhere is not claimed either.
#[test]
fn apply_leaves_a_foreign_link_unclaimed() {
    let root = scratch("apply-foreign");
    let path = real(root.path(), LINK);
    fs::create_dir_all(path.parent().expect("a parent")).expect("the directory");
    symlink("/somewhere/else", &path).expect("the link");
    let want = want_of(&[(LINK, UNIT)]);
    assert_eq!(
        apply(root.path(), &want, &recorded(&[])).expect("ok"),
        recorded(&[])
    );
    assert_eq!(
        fs::read_link(&path).expect("a link"),
        Path::new("/somewhere/else")
    );
    // Recorded, but it points elsewhere now: not ours.
    assert_eq!(
        apply(root.path(), &want, &recorded(&[LINK])).expect("ok"),
        recorded(&[])
    );
    assert_eq!(
        fs::read_link(&path).expect("a link"),
        Path::new("/somewhere/else")
    );
    // The same for a regular file.
    fs::remove_file(&path).expect("the link");
    fs::write(&path, "x").expect("a file");
    assert_eq!(
        apply(root.path(), &want, &recorded(&[LINK])).expect("ok"),
        recorded(&[])
    );
    assert_eq!(fs::read_to_string(&path).expect("the file"), "x");
}

/// Proves a recorded link that is no longer wanted is taken away, one that is now a regular file
/// or is missing is left without error, and the other wanted links are still written.
#[test]
fn apply_removes_a_recorded_link_no_longer_wanted() {
    let root = scratch("apply-remove");
    let old = "/etc/systemd/system/multi-user.target.wants/old.service";
    let file = "/etc/systemd/system/multi-user.target.wants/file.service";
    let gone = "/etc/systemd/system/multi-user.target.wants/gone.service";
    let dir = real(root.path(), old)
        .parent()
        .expect("a parent")
        .to_path_buf();
    fs::create_dir_all(&dir).expect("the directory");
    symlink(
        "/usr/lib/systemd/system/old.service",
        real(root.path(), old),
    )
    .expect("the link");
    fs::write(real(root.path(), file), "x").expect("a file");
    let want = want_of(&[(LINK, UNIT)]);
    let owned = apply(root.path(), &want, &recorded(&[old, file, gone])).expect("the links");
    assert_eq!(owned, recorded(&[LINK]));
    assert!(
        fs::symlink_metadata(real(root.path(), old)).is_err(),
        "the old link is gone"
    );
    assert!(
        real(root.path(), file).is_file(),
        "a file is not ours to remove"
    );
    assert!(fs::symlink_metadata(real(root.path(), LINK)).is_ok());
}

/// Proves a path that cannot be looked at (a file where a directory should be) is an error, for
/// both a wanted link and a recorded one.
#[test]
fn apply_and_remove_link_report_a_path_that_cannot_be_read() {
    let root = scratch("apply-error");
    fs::create_dir_all(root.path().join("etc")).expect("the directory");
    fs::write(root.path().join("etc/systemd"), "x").expect("a file");
    let want = want_of(&[(LINK, UNIT)]);
    assert!(apply(root.path(), &want, &recorded(&[])).is_err());
    assert!(apply(root.path(), &BTreeMap::new(), &recorded(&[LINK])).is_err());
    assert!(remove_link(root.path(), LINK).is_err());
}

/// Proves `remove_link` removes a link to the unit file it is named for, in either unit
/// directory, and leaves a regular file, a directory, a missing path, a link an operator pointed
/// elsewhere and a link to another unit alone without error.
///
/// Catches: a remove that deletes a link at a recorded path that is no longer hostkeeper's.
#[test]
fn remove_link_takes_away_a_link_and_nothing_else() {
    let root = scratch("remove-link");
    let dir = real(root.path(), "/etc/systemd/system");
    fs::create_dir_all(&dir).expect("the directory");
    symlink("/usr/lib/systemd/system/a.service", dir.join("a.service")).expect("the link");
    symlink("/lib/systemd/system/f.service", dir.join("f.service")).expect("the link");
    fs::write(dir.join("b.service"), "x").expect("a file");
    fs::create_dir(dir.join("c.service")).expect("a directory");
    symlink("/etc/elsewhere/e.service", dir.join("e.service")).expect("a repointed link");
    symlink(
        "/usr/lib/systemd/system/other.service",
        dir.join("g.service"),
    )
    .expect("another unit");
    for name in ["a.service", "f.service"] {
        remove_link(root.path(), &format!("/etc/systemd/system/{name}")).expect("the link goes");
        assert!(fs::symlink_metadata(dir.join(name)).is_err(), "{name}");
    }
    for name in [
        "b.service",
        "c.service",
        "d.service",
        "e.service",
        "g.service",
    ] {
        remove_link(root.path(), &format!("/etc/systemd/system/{name}")).expect("left alone");
    }
    assert!(dir.join("b.service").is_file());
    assert!(dir.join("c.service").is_dir());
    assert!(
        dir.join("e.service").is_symlink(),
        "a link pointed elsewhere stays"
    );
    assert!(
        dir.join("g.service").is_symlink(),
        "a link to another unit stays"
    );
}

/// Proves `apply` reports a link that cannot be written (a directory it cannot write into, or one
/// it cannot make) as an error, so the step fails instead of recording a link it did not make.
#[test]
fn apply_reports_a_link_it_cannot_write() {
    let root = scratch("apply-readonly");
    let units = real(root.path(), "/etc/systemd/system/multi-user.target.wants");
    fs::create_dir_all(&units).expect("the directory");
    fs::set_permissions(&units, fs::Permissions::from_mode(0o555)).expect("the mode");
    let want = want_of(&[(LINK, UNIT)]);
    let no_link = apply(root.path(), &want, &recorded(&[]));
    fs::set_permissions(&units, fs::Permissions::from_mode(0o755)).expect("the mode");
    let other = scratch("apply-readonly-parent");
    let etc = other.path().join("etc");
    fs::create_dir(&etc).expect("the directory");
    fs::set_permissions(&etc, fs::Permissions::from_mode(0o555)).expect("the mode");
    let no_directory = apply(other.path(), &want, &recorded(&[]));
    fs::set_permissions(&etc, fs::Permissions::from_mode(0o755)).expect("the mode");
    assert_eq!(
        no_link.expect_err("no link").to_string(),
        "Permission denied (os error 13)"
    );
    assert_eq!(
        no_directory.expect_err("no directory").to_string(),
        "Permission denied (os error 13)"
    );
    assert!(fs::symlink_metadata(real(root.path(), LINK)).is_err());
}
