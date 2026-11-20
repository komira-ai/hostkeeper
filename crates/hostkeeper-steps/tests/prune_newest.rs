//! The versions a prune keeps besides `current`, `previous` and what the records use (design
//! section 4.7 and Q14): the pinned version, whatever its age, and the newest
//! `Protected::newest` other whole versions, newest by the time each was sealed, then by the
//! greater name.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::symlink;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hostkeeper_steps::selfupdate::Layout;
use hostkeeper_steps::{PRUNE_PREFIX, Protected, PruneInputs, Seal, prune_plan, tree_digest};
use support::Scratch;

const PACKAGE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// The second `secs` after the epoch.
fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

/// The inputs of a directory whose `versions` have a directory, whose `seals` each have a seal
/// written at its second, `current` C and `previous` B.
fn inputs(versions: &[&str], seals: &[(&str, u64)], protected: Protected) -> PruneInputs {
    let seals: BTreeMap<String, SystemTime> = seals
        .iter()
        .map(|(name, secs)| ((*name).to_owned(), at(*secs)))
        .collect();
    PruneInputs {
        versions: set(versions),
        seals,
        leftovers: BTreeSet::new(),
        current: Some("C".to_owned()),
        previous: Some("B".to_owned()),
        protected,
    }
}

/// Keeps the newest `newest`.
fn newest(newest: usize) -> Protected {
    Protected {
        newest,
        ..Protected::default()
    }
}

/// Of A, D, E, F and G, whole and sealed at seconds 1, 4, 5, 6 and 7, besides `current` C and
/// `previous` B.
const WHOLE: [(&str, u64); 7] = [
    ("A", 1),
    ("B", 2),
    ("C", 3),
    ("D", 4),
    ("E", 5),
    ("F", 6),
    ("G", 7),
];

const NAMES: [&str; 7] = ["A", "B", "C", "D", "E", "F", "G"];

/// Catches a rule that keeps the wrong number of others, counts `current` or `previous` among
/// them, or keeps the oldest rather than the newest: with 2 to keep, F and G stay with B and C,
/// and A, D and E go; with 0, only B and C stay; with more than there are, every version stays.
#[test]
fn the_rule_keeps_the_newest_others_and_deletes_the_older() {
    let two = prune_plan(&inputs(&NAMES, &WHOLE, newest(2)));
    assert_eq!(two.keep, set(&["B", "C", "F", "G"]));
    assert_eq!(two.delete, set(&["A", "D", "E"]));
    assert!(!two.unresolved);

    let none = prune_plan(&inputs(&NAMES, &WHOLE, newest(0)));
    assert_eq!(none.keep, set(&["B", "C"]));
    assert_eq!(none.delete, set(&["A", "D", "E", "F", "G"]));

    let all = prune_plan(&inputs(&NAMES, &WHOLE, newest(32)));
    assert_eq!(all.keep, set(&NAMES));
    assert!(all.delete.is_empty());
}

/// Catches a rule that orders by name, or keeps either half of a version for being new: G is
/// the greatest name and sealed first, so with 2 to keep the two sealed last, E and A, stay;
/// a seal with no directory (Y, the newest seal) and a directory with no seal (Z) go, and are
/// not counted among the 2.
#[test]
fn the_newest_are_the_last_sealed_whole_versions_whatever_their_names() {
    let seals = [
        ("G", 1),
        ("D", 2),
        ("B", 3),
        ("C", 4),
        ("E", 5),
        ("A", 6),
        ("Y", 9),
    ];
    let planned = prune_plan(&inputs(
        &["A", "B", "C", "D", "E", "G", "Z"],
        &seals,
        newest(2),
    ));
    assert_eq!(planned.keep, set(&["A", "B", "C", "E"]));
    assert_eq!(planned.delete, set(&["D", "G", "Y", "Z"]));
}

/// Catches a tie broken by the order of the inputs, or not at all: D, E and F sealed in the
/// same second, with 2 to keep, keep the two greater names, E and F.
#[test]
fn seals_of_the_same_time_are_ordered_by_the_greater_name() {
    let seals = [("A", 1), ("B", 2), ("C", 3), ("D", 5), ("E", 5), ("F", 5)];
    let planned = prune_plan(&inputs(&["A", "B", "C", "D", "E", "F"], &seals, newest(2)));
    assert_eq!(planned.keep, set(&["B", "C", "E", "F"]));
    assert_eq!(planned.delete, set(&["A", "D"]));
}

/// Catches a pin that is counted among the newest, or kept only while it is new: the pin names
/// A, the oldest, and with 1 to keep A stays besides G, the newest; a pin of a version that is
/// not there keeps nothing else; and a version a record protects is not counted among the
/// newest either.
#[test]
fn the_pin_is_kept_whatever_its_age_and_is_not_one_of_the_newest() {
    let pinned = Protected {
        pinned: set(&["A"]),
        newest: 1,
        ..Protected::default()
    };
    let planned = prune_plan(&inputs(&NAMES, &WHOLE, pinned));
    assert_eq!(planned.keep, set(&["A", "B", "C", "G"]));
    assert_eq!(planned.delete, set(&["D", "E", "F"]));

    let elsewhere = Protected {
        pinned: set(&["Q"]),
        newest: 1,
        ..Protected::default()
    };
    let planned = prune_plan(&inputs(&NAMES, &WHOLE, elsewhere));
    assert_eq!(planned.keep, set(&["B", "C", "G"]));

    let recorded = Protected {
        switch_targets: set(&["G"]),
        newest: 1,
        ..Protected::default()
    };
    let planned = prune_plan(&inputs(&NAMES, &WHOLE, recorded));
    assert_eq!(planned.keep, set(&["B", "C", "F", "G"]));
}

/// Catches the newest kept when the links do not resolve being read as permission to delete
/// the rest: `current` naming no directory keeps everything, whatever is to be kept.
#[test]
fn an_unresolved_link_keeps_everything_whatever_the_count() {
    let mut unresolved = inputs(&NAMES, &WHOLE, newest(1));
    unresolved.current = Some("Q".to_owned());
    let planned = prune_plan(&unresolved);
    assert!(planned.unresolved);
    assert_eq!(planned.keep, set(&NAMES));
    assert!(planned.delete.is_empty());
}

/// Version `version` of `layout`, a one-file tree, sealed, its seal last written at second
/// `secs`.
fn sealed_version(layout: &Layout, version: &str, secs: u64) {
    let dir = layout.version_path(version);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("x"), version).unwrap();
    let seal = Seal::new(&tree_digest(&dir).unwrap(), PACKAGE).unwrap();
    fs::write(layout.seal_path(version), seal.to_json()).unwrap();
    fs::File::options()
        .write(true)
        .open(layout.seal_path(version))
        .unwrap()
        .set_modified(at(secs))
        .unwrap();
}

/// The names in the package directory of `layout`, sorted.
fn names(layout: &Layout) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(layout.dir())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with(PRUNE_PREFIX))
        .collect();
    names.sort();
    names
}

/// Catches a prune on disk that reads no time from the seals, or keeps by name: of 1.0.0 to
/// 6.0.0 with `current` 6.0.0 and `previous` 5.0.0, where 1.0.0 was staged last (a rollback
/// target fetched again) and 4.0.0 first, keeping 2 others leaves 1.0.0 and 3.0.0 with their
/// seals, and deletes 2.0.0 and 4.0.0 whole.
#[test]
fn a_prune_keeps_the_versions_staged_last() {
    let s = Scratch::new("prune-newest");
    let layout = Layout::package(&s.path().join("web"));
    fs::create_dir_all(layout.dir()).unwrap();
    for (version, secs) in [
        ("4.0.0", 10),
        ("2.0.0", 20),
        ("3.0.0", 30),
        ("5.0.0", 40),
        ("6.0.0", 50),
        ("1.0.0", 60),
    ] {
        sealed_version(&layout, version, secs);
    }
    symlink("6.0.0", layout.current_path()).unwrap();
    symlink("5.0.0", layout.previous_path()).unwrap();
    let report = layout.prune(&newest(2)).unwrap();
    assert_eq!(report.deleted, set(&["2.0.0", "4.0.0"]));
    assert_eq!(report.kept, set(&["1.0.0", "3.0.0", "5.0.0", "6.0.0"]));
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(
        names(&layout),
        [
            "1.0.0",
            "1.0.0.seal",
            "3.0.0",
            "3.0.0.seal",
            "5.0.0",
            "5.0.0.seal",
            "6.0.0",
            "6.0.0.seal",
            "current",
            "previous"
        ]
    );
}
