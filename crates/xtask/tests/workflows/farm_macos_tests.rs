//! The tests the farm leaves to the Mac ([`super::farm_macos`]): the committed build files say
//! which members those are, and each rule is shown to refuse its defect on members made up here.

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::farm_macos::{MARK, Member, mac_only, macos_tested, members, problems};
use super::workflows;

/// The repository's root.
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository")
}

/// The committed release.yml.
fn release() -> String {
    workflows()
        .into_iter()
        .find(|(name, _)| name == "release.yml")
        .expect("release.yml is in the repository")
        .1
}

/// A member made up for a rule's test.
fn member(farm_tested: bool, mac_only: bool, marked: bool) -> Member {
    Member {
        name: "hostkeeper-x".to_owned(),
        buck_path: "crates/hostkeeper-x/BUCK".to_owned(),
        farm_tested,
        mac_only,
        buck: if marked {
            format!(
                "load(\"//:buildrules.bzl\", \"cargo_package\")\n\n{MARK}\ncargo_package(path = \".\")\n"
            )
        } else {
            "cargo_package(path = \".\")\n".to_owned()
        },
    }
}

/// The packages `macos-test` runs, in a test.
fn tested(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

#[test]
fn the_committed_build_files_say_which_members_leave_tests_to_the_mac() {
    // Catches: a farm-tested member gaining code under cfg(target_os = "macos") without its BUCK
    // file saying the farm compiles it out, a member whose Mac tests run nowhere, and a stale
    // line in a BUCK file. The list is the answer to "which tests does the farm leave to the
    // macOS check": those of these five members' macOS code.
    let all = members(&root());
    assert_eq!(
        problems(&all, &macos_tested(&release())),
        Vec::<String>::new()
    );
    let marked: Vec<&str> = all
        .iter()
        .filter(|m| m.farm_tested && m.mac_only)
        .map(|m| m.name.as_str())
        .collect();
    assert_eq!(
        marked,
        [
            "hostkeeper-bootstrap",
            "hostkeeper-config",
            "hostkeeper-daemon",
            "hostkeeper-install",
            "hostkeeper-launchd",
        ]
    );
}

#[test]
fn code_only_a_mac_compiles_is_a_cfg_that_requires_macos() {
    // Catches: a negated cfg (code a Mac does not compile), an `any(...)` that Linux compiles
    // too, a `cfg!` expression (compiled on every platform) or a mention in a comment taken for
    // macOS-only code, and an attribute in another form (inner, nested in `all`, indented,
    // without spaces) missed.
    for text in [
        "#[cfg(target_os = \"macos\")]\nfn f() {}\n",
        "    #[cfg(target_os = \"macos\")]\n    {\n    }\n",
        "#![cfg(target_os = \"macos\")]\n",
        "#[cfg(all(target_os = \"macos\", target_arch = \"aarch64\"))]\n",
        "#[cfg(target_os=\"macos\")]\n",
    ] {
        assert!(mac_only(text), "{text}");
    }
    for text in [
        "#[cfg(not(target_os = \"macos\"))]\nfn f() {}\n",
        "let x = cfg!(target_os = \"macos\");\n",
        "// only under #[cfg(target_os = \"macos\")]\n",
        "#[cfg(target_os = \"linux\")]\n",
        "#[cfg(any(target_os = \"macos\", target_os = \"linux\"))]\n",
        "#[cfg(not(any(target_os = \"linux\", target_os = \"macos\")))]\n",
        "",
    ] {
        assert!(!mac_only(text), "{text}");
    }
}

#[test]
fn a_member_with_mac_code_and_no_line_in_its_build_file_is_refused() {
    // Catches: macOS-only code the farm compiles out that the build files do not mention.
    assert_eq!(
        problems(&[member(true, true, false)], &tested(&["hostkeeper-x"])),
        [format!(
            "crates/hostkeeper-x/BUCK: hostkeeper-x has code under cfg(target_os = \"macos\") \
             that the Linux farm compiles out, and its BUCK file does not say so: add the line \
             {MARK}"
        )]
    );
}

#[test]
fn a_member_whose_mac_tests_the_mac_does_not_run_is_refused() {
    // Catches: a member whose macOS-only tests are compiled out on the farm and not run by the
    // Mac's job either.
    assert_eq!(
        problems(&[member(true, true, true)], &tested(&["hostkeeper-y"])),
        [
            "hostkeeper-x: the Linux farm compiles out its macOS tests and release.yml's job \
          macos-test does not test it, so they run nowhere"
        ]
    );
}

#[test]
fn a_line_in_the_build_file_of_a_member_that_owes_none_is_refused() {
    // Catches: a stale line left after the macOS code went, or one in a member the farm does not
    // test (its tests are all in ci.yml).
    let stale = "crates/hostkeeper-x/BUCK: says the farm leaves macOS tests of hostkeeper-x to \
                 the Mac, but the farm does not test it or it has no code under cfg(target_os = \
                 \"macos\")";
    let all = tested(&["hostkeeper-x"]);
    assert_eq!(problems(&[member(true, false, true)], &all), [stale]);
    assert_eq!(problems(&[member(false, true, true)], &all), [stale]);
    assert_eq!(
        problems(&[member(false, true, false)], &all),
        Vec::<String>::new()
    );
    assert_eq!(
        problems(&[member(true, false, false)], &all),
        Vec::<String>::new()
    );
}

#[test]
fn the_packages_the_mac_tests_are_those_its_test_run_names() {
    // Catches: the packages read from the step that only builds the tests, or from another job,
    // and a release.yml that cannot be read taken for one that tests everything.
    let text = "jobs:\n  macos-test:\n    steps:\n      - run: cargo test --locked --no-run -p a -p b\n      - run: cargo test --locked --no-fail-fast -p a\n  other:\n    steps:\n      - run: cargo test --locked --no-fail-fast -p c\n";
    assert_eq!(macos_tested(text), tested(&["a"]));
    assert_eq!(macos_tested("jobs: ["), BTreeSet::new());
    assert_eq!(
        macos_tested("jobs:\n  macos-test:\n    steps: none\n"),
        BTreeSet::new()
    );
    assert_eq!(
        macos_tested("jobs:\n  macos-test:\n    steps:\n      - run: echo\n"),
        BTreeSet::new()
    );
}
