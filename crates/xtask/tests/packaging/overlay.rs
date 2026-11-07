//! The boot-image overlay, `overlay/`: copies of the sources, the link that enables the unit,
//! and the console's issue link.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use hostkeeper_daemon::console::{ENROLLMENT_ISSUE, ISSUE_LINK};

use crate::{INSTALLED_UNIT, TEMPLATE, TMPFILES, UNIT, config, linux};

/// The overlay's files and the source each is a copy of.
const OVERLAY_FILES: [(&str, &str); 3] = [
    ("usr/lib/systemd/system/hostkeeper.service", UNIT),
    ("usr/lib/tmpfiles.d/hostkeeper.conf", TMPFILES),
    ("etc/hostkeeper/hostkeeper.toml.template", TEMPLATE),
];
const ENABLE_LINK: &str = "etc/systemd/system/multi-user.target.wants/hostkeeper.service";
/// The console's issue link, which points at the issue file the daemon writes.
const ISSUE: &str = "etc/issue.d/hostkeeper.issue";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    File(Vec<u8>),
    Link(PathBuf),
}

/// Every file and link under `dir`, by its path from `root`; directories are walked, not listed.
fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Item>) {
    for entry in fs::read_dir(dir).expect("list the overlay") {
        let path = entry.expect("an overlay entry").path();
        let meta = fs::symlink_metadata(&path).expect("an overlay entry's type");
        let relative = path
            .strip_prefix(root)
            .expect("under the overlay")
            .to_string_lossy()
            .into_owned();
        if meta.file_type().is_symlink() {
            out.insert(
                relative,
                Item::Link(fs::read_link(path).expect("read the link")),
            );
        } else if meta.is_dir() {
            walk(root, &path, out);
        } else {
            out.insert(relative, Item::File(fs::read(path).expect("read the file")));
        }
    }
}

/// Where a link at `link` (a path from the image root) to `target` points, from the image
/// root: an absolute target is from the image root, a relative one from the link's directory.
fn resolve(link: &str, target: &Path) -> Result<String, String> {
    let mut parts: Vec<String> = Vec::new();
    if !target.is_absolute() {
        parts = link.split('/').map(str::to_owned).collect();
        parts.pop();
    }
    for component in target.components() {
        match component {
            Component::ParentDir => {
                parts
                    .pop()
                    .ok_or_else(|| format!("{link} climbs above the image root"))?;
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    Ok(parts.join("/"))
}

fn check_overlay(
    tree: &BTreeMap<String, Item>,
    source: impl Fn(&str) -> Vec<u8>,
) -> Result<(), String> {
    for path in tree.keys() {
        if path.rsplit('/').next() == Some("hostkeeper.toml") {
            return Err(format!("the overlay holds a live config, {path}"));
        }
        if path.starts_with("usr/bin/") || path.starts_with("usr/sbin/") {
            return Err(format!("the overlay holds a program, {path}"));
        }
    }
    let wanted: BTreeSet<&str> = OVERLAY_FILES
        .iter()
        .map(|(path, _)| *path)
        .chain([ENABLE_LINK, ISSUE])
        .collect();
    let found: BTreeSet<&str> = tree.keys().map(String::as_str).collect();
    if found != wanted {
        return Err(format!("the overlay holds {found:?}, not {wanted:?}"));
    }
    for (path, src) in OVERLAY_FILES {
        if tree.get(path) != Some(&Item::File(source(src))) {
            return Err(format!("{path} is not a copy of packaging/linux/{src}"));
        }
    }
    points(tree, ENABLE_LINK, INSTALLED_UNIT)?;
    points(tree, ISSUE, ISSUE_TARGET)
}

/// What the issue link points at: the daemon's issue file, outside the image (`/run` is made at
/// boot).
const ISSUE_TARGET: &str = ENROLLMENT_ISSUE;

/// `link` of `tree` is a link that points at `wanted`, a path from the image root.
fn points(tree: &BTreeMap<String, Item>, link: &str, wanted: &str) -> Result<(), String> {
    match tree.get(link) {
        Some(Item::Link(target)) => {
            let points = format!("/{}", resolve(link, target)?);
            if points != wanted {
                return Err(format!("{link} points at {points}, not {wanted}"));
            }
            Ok(())
        }
        _ => Err(format!("{link} is not a link")),
    }
}

/// Catches an overlay file that drifted from its source, an enable link that does not point
/// at the unit's installed path, an issue link that is not a link or points at another file
/// than the daemon's issue file or is not where the packages put it, a live `hostkeeper.toml`
/// or a program in the overlay, and a file too many or too few.
#[test]
fn overlay_files_equal_their_sources_and_the_enable_link_points_at_the_unit() {
    let root = linux().join("overlay");
    let mut tree = BTreeMap::new();
    walk(&root, &root, &mut tree);
    let source = |src: &str| fs::read(linux().join(src)).expect("read a source");
    assert_eq!(check_overlay(&tree, source), Ok(()));
    // The link's path is where the packages install the unit.
    let installed: Vec<String> = config()
        .files("deb")
        .expect("files")
        .into_iter()
        .map(|f| f.dst)
        .collect();
    assert!(
        installed.iter().any(|dst| dst.as_str() == INSTALLED_UNIT),
        "{installed:?}"
    );
    // The issue link is where the packages put theirs.
    let linked: Vec<String> = config()
        .links("deb")
        .expect("links")
        .into_iter()
        .map(|link| link.dst)
        .collect();
    assert_eq!(linked, [format!("/{ISSUE}")]);
    assert_eq!(ISSUE_LINK, format!("/{ISSUE}"));
    // An absolute link to the unit is as good as the relative one.
    let mut absolute = tree.clone();
    absolute.insert(
        ENABLE_LINK.to_owned(),
        Item::Link(PathBuf::from(INSTALLED_UNIT)),
    );
    assert_eq!(check_overlay(&absolute, source), Ok(()));
    let with = |path: &str, item: Item| {
        let mut changed = tree.clone();
        changed.insert(path.to_owned(), item);
        changed
    };
    let mut drifted = fs::read(linux().join(UNIT)).expect("the unit");
    drifted.extend_from_slice(b"# drifted\n");
    // A relative issue link to the same file is as good as the absolute one.
    let mut relative = tree.clone();
    relative.insert(
        ISSUE.to_owned(),
        Item::Link(PathBuf::from("../../run/hostkeeper/enrollment.issue")),
    );
    assert_eq!(check_overlay(&relative, source), Ok(()));
    let mut missing = tree.clone();
    missing.remove(ISSUE);
    let refused = check_overlay(&missing, source).expect_err("no issue link");
    assert!(refused.starts_with("the overlay holds"), "{refused:?}");
    let cases = [
        (
            with(OVERLAY_FILES[0].0, Item::File(drifted)),
            "is not a copy of packaging/linux/hostkeeper.service",
        ),
        (
            with(
                ENABLE_LINK,
                Item::Link(PathBuf::from(
                    "../../../../usr/lib/systemd/system/other.service",
                )),
            ),
            "points at /usr/lib/systemd/system/other.service",
        ),
        (
            with(
                ENABLE_LINK,
                Item::Link(PathBuf::from(
                    "../../../../../../usr/lib/systemd/system/hostkeeper.service",
                )),
            ),
            "climbs above the image root",
        ),
        (with(ENABLE_LINK, Item::File(source(UNIT))), "is not a link"),
        (
            with(
                ISSUE,
                Item::Link(PathBuf::from("/run/hostkeeper/admin.sock")),
            ),
            "etc/issue.d/hostkeeper.issue points at /run/hostkeeper/admin.sock",
        ),
        (
            with(ISSUE, Item::File(b"hostkeeper enrollment\n".to_vec())),
            "etc/issue.d/hostkeeper.issue is not a link",
        ),
        (
            with("etc/hostkeeper/hostkeeper.toml", Item::File(b"x".to_vec())),
            "a live config",
        ),
        (
            with("usr/bin/hostkeeper", Item::File(b"x".to_vec())),
            "a program",
        ),
        (
            with("README.md", Item::File(b"x".to_vec())),
            "the overlay holds",
        ),
    ];
    for (changed, words) in cases {
        let refused = check_overlay(&changed, source).expect_err(words);
        assert!(refused.contains(words), "{refused:?} lacks {words:?}");
    }
}
