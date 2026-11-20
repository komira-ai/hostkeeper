//! The tests of `maintenance_own`: the names read from a `placed.json` written into a scratch
//! state directory.

use hostkeeper_testkit::Scratch;

use super::*;

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// Proves the names are hostkeeper's own package and the package of each installed entry that
/// names one: an entry with no package adds nothing, and two entries of one package add it once.
#[test]
fn the_names_are_the_own_package_and_the_recorded_packages() {
    let scratch = scratch("own-recorded");
    std::fs::write(
        scratch.path().join("placed.json"),
        r#"{"schema":1,"installed":{
            "web":{"installer":"apt","package":"nginx"},
            "web2":{"installer":"apt","package":"nginx"},
            "db":{"installer":"apt","package":"postgresql-16"},
            "tool":{"installer":"archive"}}}"#,
    )
    .expect("placed.json");
    assert_eq!(
        PlacedPackages::new(scratch.path()).names(),
        Ok(set(&["hostkeeper", "nginx", "postgresql-16"]))
    );
}

/// Proves a state directory with no `placed.json` holds hostkeeper's own package alone.
#[test]
fn no_record_holds_the_own_package_alone() {
    let scratch = scratch("own-no-record");
    assert_eq!(
        PlacedPackages {
            dir: scratch.path().to_owned()
        }
        .names(),
        Ok(set(&[OWN_PACKAGE]))
    );
}

/// Proves a `placed.json` that cannot be read fails the names with the record's own words, which
/// name the file.
#[test]
fn a_corrupt_record_fails_the_names() {
    let scratch = scratch("own-corrupt");
    let file = scratch.path().join("placed.json");
    std::fs::write(&file, "not json").expect("placed.json");
    let error = PlacedPackages::new(scratch.path())
        .names()
        .expect_err("a corrupt record");
    assert!(
        error.starts_with(&format!("{} is not a placed-path record: ", file.display())),
        "{error}"
    );
}
