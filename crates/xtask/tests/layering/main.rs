//! The layers of the host updater crates, held on the dependency graph.
//!
//! - no package anywhere in the graph (any kind, any platform) is named `kbf`, `kbf-*` or
//!   `kbf_*`;
//! - `hostkeeper-hook` depends on no other crate of this workspace (its dev-dependencies hold
//!   only itself and `hostkeeper-testkit`);
//! - `hostkeeper-steps` and `hostkeeper-rights` depend on `hostkeeper-hook` and on no other
//!   workspace crate (dev-dependencies: themselves and `hostkeeper-testkit`);
//! - `hostkeeper-testkit` depends on no workspace crate, and no crate has it but as a
//!   dev-dependency.
//!
//! The rules are in `check.rs`. The fixture workspaces under `fixtures/` are copied to a
//! scratch directory, locked offline and checked; each failing one has an exact expected
//! message.

mod check;
#[path = "../../src/testdir.rs"]
mod testdir;

use std::path::{Path, PathBuf};

use check::{cargo, check, parse_lock, read_lock};
use testdir::TestDir;

const HOOK_DEP: &str = "hostkeeper-hook = { path = \"../hostkeeper-hook\" }\n";
const RIGHTS_DEP: &str = "hostkeeper-rights = { path = \"../hostkeeper-rights\" }\n";
const TESTKIT_DEP: &str = "hostkeeper-testkit = { path = \"../hostkeeper-testkit\" }\n";

const KBF_CHAIN: &str = "kbf-types is a kbf package (kbf, kbf-* or kbf_*), reached through";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/layering/fixtures")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create the copy");
    for entry in std::fs::read_dir(from).expect("read the fixture") {
        let entry = entry.expect("read a fixture entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy a fixture file");
        }
    }
}

/// The text of a `Cargo.toml` for a layer crate.
fn layered(name: &str, deps: &str, build: &str, dev: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
         [dependencies]\n{deps}\n[build-dependencies]\n{build}\n[dev-dependencies]\n{dev}"
    )
}

/// A dependency on a crate under the fixture's `vendored/` (outside the workspace members).
fn vendored_dep(name: &str) -> String {
    format!("{name} = {{ path = \"../../vendored/{name}\" }}\n")
}

/// The two files of a crate with no dependencies, at `<dir>/<name>`.
fn bare_crate(dir: &str, name: &str) -> Vec<(String, String)> {
    vec![
        (
            format!("{dir}/{name}/Cargo.toml"),
            layered(name, "", "", ""),
        ),
        (format!("{dir}/{name}/src/lib.rs"), String::new()),
    ]
}

/// A manifest replaced in a fixture copy.
fn manifest_of(crate_name: &str, text: String) -> (String, String) {
    (format!("crates/{crate_name}/Cargo.toml"), text)
}

/// Copies fixture `name` to a scratch directory, applies `edits` (path under the copy, new
/// text), and writes its lockfile offline.
fn fixture(name: &str, edits: Vec<(String, String)>) -> TestDir {
    let dir = TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "layering");
    copy_dir(&fixtures().join(name), &dir);
    for (relative, text) in edits {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().expect("a file has a parent")).expect("mkdir");
        std::fs::write(path, text).expect("write an edit");
    }
    cargo(&["generate-lockfile", "--offline"], &dir.join("Cargo.toml"));
    dir
}

fn refused(message: &str) -> Result<(), String> {
    Err(message.to_string())
}

/// Catches: a workspace crate that reaches a kbf crate through an intermediate crate going
/// unseen, or the error not naming the chain from the member to the kbf package.
#[test]
fn layering_refuses_a_kbf_dependency() {
    let dir = fixture("kbf-dependency", vec![]);
    assert_eq!(
        check(&dir),
        refused(&format!("{KBF_CHAIN} hostkeeper-hook -> shim -> kbf-types"))
    );
}

/// Catches: the hook contract growing a normal dependency on the step runner (which would make
/// every service that depends on the contract link the runner), and a step runner that no
/// longer depends on the contract being accepted.
#[test]
fn layering_refuses_hook_depending_on_steps() {
    let dir = fixture("hook-depends-on-steps", vec![]);
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-hook depends (normal or build) on the workspace crates \
             {hostkeeper-steps}, but must depend on exactly {}\n\
             hostkeeper-steps depends (normal or build) on the workspace crates {}, \
             but must depend on exactly {hostkeeper-hook}"
        )
    );
}

/// Catches: any layering rule broken in this repository's own workspace, and a graph walk that
/// passes only because it saw nothing (the lockfile must hold the hook crate's dependencies).
#[test]
fn the_workspace_keeps_its_layers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(read_lock(&root)["hostkeeper-hook"].contains("serde_json"));
    assert_eq!(check(&root), Ok(()));
}

/// Catches: a check that refuses the shape the real workspace has (self dev-dependencies, the
/// testkit as a dev-dependency, steps and rights on the hook).
#[test]
fn a_clean_fixture_passes() {
    let dir = fixture("clean", vec![]);
    assert_eq!(check(&dir), Ok(()));
}

/// Catches: the underscore spelling of a kbf package name (`kbf_types`) slipping past a check
/// that looks only for `kbf-`.
#[test]
fn a_kbf_package_with_an_underscore_is_refused() {
    let mut edits = bare_crate("vendored", "kbf_types");
    edits.push(manifest_of(
        "hostkeeper-hook",
        layered(
            "hostkeeper-hook",
            &vendored_dep("kbf_types"),
            "",
            &format!("hostkeeper-hook = {{ path = \".\" }}\n{TESTKIT_DEP}"),
        ),
    ));
    let dir = fixture("clean", edits);
    assert_eq!(
        check(&dir),
        refused(
            "kbf_types is a kbf package (kbf, kbf-* or kbf_*), \
             reached through hostkeeper-hook -> kbf_types"
        )
    );
}

/// Catches: a kbf crate hidden among a member's dev-dependencies being ignored because only
/// normal dependencies were walked.
#[test]
fn a_dev_only_kbf_dependency_is_refused() {
    let mut edits = bare_crate("vendored", "kbf-types");
    edits.push(manifest_of(
        "hostkeeper-steps",
        layered(
            "hostkeeper-steps",
            HOOK_DEP,
            "",
            &format!("{TESTKIT_DEP}{}", vendored_dep("kbf-types")),
        ),
    ));
    let dir = fixture("clean", edits);
    assert_eq!(
        check(&dir),
        refused(&format!("{KBF_CHAIN} hostkeeper-steps -> kbf-types"))
    );
}

/// Catches: a kbf crate that is only a build-dependency being ignored.
#[test]
fn a_build_only_kbf_dependency_is_refused() {
    let mut edits = bare_crate("vendored", "kbf-types");
    edits.push(manifest_of(
        "hostkeeper-rights",
        layered(
            "hostkeeper-rights",
            HOOK_DEP,
            &vendored_dep("kbf-types"),
            &format!("hostkeeper-rights = {{ path = \".\" }}\n{TESTKIT_DEP}"),
        ),
    ));
    let dir = fixture("clean", edits);
    assert_eq!(
        check(&dir),
        refused(&format!("{KBF_CHAIN} hostkeeper-rights -> kbf-types"))
    );
}

/// Catches: the kbf rule matching any name that merely contains the letters (`kbfoo`,
/// `my-kbf`), which would refuse unrelated crates.
#[test]
fn a_name_that_only_contains_kbf_is_allowed() {
    let mut edits = bare_crate("vendored", "kbfoo");
    edits.extend(bare_crate("vendored", "my-kbf"));
    edits.push(manifest_of(
        "hostkeeper-hook",
        layered(
            "hostkeeper-hook",
            &format!("{}{}", vendored_dep("kbfoo"), vendored_dep("my-kbf")),
            "",
            &format!("hostkeeper-hook = {{ path = \".\" }}\n{TESTKIT_DEP}"),
        ),
    ));
    let dir = fixture("clean", edits);
    assert_eq!(check(&dir), Ok(()));
}

/// Catches: the testkit becoming a normal dependency of a crate that ships (its scratch helpers
/// would then be linked into the product), reported both on the crate's own dependency set and
/// on the testkit rule.
#[test]
fn the_testkit_as_a_normal_dependency_is_refused() {
    let dir = fixture(
        "clean",
        vec![manifest_of(
            "hostkeeper-rights",
            layered(
                "hostkeeper-rights",
                &format!("{HOOK_DEP}{TESTKIT_DEP}"),
                "",
                "",
            ),
        )],
    );
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-rights depends (normal or build) on the workspace crates \
             {hostkeeper-hook, hostkeeper-testkit}, but must depend on exactly {hostkeeper-hook}\n\
             hostkeeper-rights has a normal or build dependency on hostkeeper-testkit, \
             which is a dev-dependency only"
        )
    );
}

/// Catches: the testkit as a build-dependency being taken for a dev-dependency.
#[test]
fn the_testkit_as_a_build_dependency_is_refused() {
    let dir = fixture(
        "clean",
        vec![manifest_of(
            "hostkeeper-steps",
            layered("hostkeeper-steps", HOOK_DEP, TESTKIT_DEP, ""),
        )],
    );
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-steps depends (normal or build) on the workspace crates \
             {hostkeeper-hook, hostkeeper-testkit}, but must depend on exactly {hostkeeper-hook}\n\
             hostkeeper-steps has a normal or build dependency on hostkeeper-testkit, \
             which is a dev-dependency only"
        )
    );
}

/// Catches: the step runner gaining a normal dependency on another workspace crate (here the
/// rights crate) beside the hook contract.
#[test]
fn steps_with_an_extra_workspace_dependency_is_refused() {
    let dir = fixture(
        "clean",
        vec![manifest_of(
            "hostkeeper-steps",
            layered(
                "hostkeeper-steps",
                &format!("{HOOK_DEP}{RIGHTS_DEP}"),
                "",
                TESTKIT_DEP,
            ),
        )],
    );
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-steps depends (normal or build) on the workspace crates \
             {hostkeeper-hook, hostkeeper-rights}, but must depend on exactly {hostkeeper-hook}"
        )
    );
}

/// Catches: the rights crate losing its dependency on the hook contract (the set must be
/// exactly the hook, not merely a subset of it).
#[test]
fn rights_without_the_hook_is_refused() {
    let dir = fixture(
        "clean",
        vec![manifest_of(
            "hostkeeper-rights",
            layered("hostkeeper-rights", "", "", TESTKIT_DEP),
        )],
    );
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-rights depends (normal or build) on the workspace crates {}, \
             but must depend on exactly {hostkeeper-hook}"
        )
    );
}

/// Catches: a dev-dependency of the step runner on a workspace crate other than the testkit.
#[test]
fn steps_with_a_dev_dependency_on_rights_is_refused() {
    let dir = fixture(
        "clean",
        vec![manifest_of(
            "hostkeeper-steps",
            layered(
                "hostkeeper-steps",
                HOOK_DEP,
                "",
                &format!("{TESTKIT_DEP}{RIGHTS_DEP}"),
            ),
        )],
    );
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-steps has a dev-dependency on the workspace crate hostkeeper-rights, \
             which is neither itself nor hostkeeper-testkit"
        )
    );
}

/// Catches: a dev-dependency of the hook contract on the rights crate (which depends on the
/// hook in turn: a cycle through a dev-dependency, which Cargo allows), and a graph walk that
/// loops on the cycle.
#[test]
fn the_hook_with_a_dev_dependency_on_rights_is_refused() {
    let dir = fixture(
        "clean",
        vec![manifest_of(
            "hostkeeper-hook",
            layered(
                "hostkeeper-hook",
                "",
                "",
                &format!("hostkeeper-hook = {{ path = \".\" }}\n{TESTKIT_DEP}{RIGHTS_DEP}"),
            ),
        )],
    );
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-hook has a dev-dependency on the workspace crate hostkeeper-rights, \
             which is neither itself nor hostkeeper-testkit"
        )
    );
}

/// Catches: the testkit depending on a workspace crate, even only as a dev-dependency.
#[test]
fn the_testkit_depending_on_a_workspace_crate_is_refused() {
    let dir = fixture(
        "clean",
        vec![manifest_of(
            "hostkeeper-testkit",
            layered("hostkeeper-testkit", "", "", HOOK_DEP),
        )],
    );
    assert_eq!(
        check(&dir),
        refused("hostkeeper-testkit depends on the workspace crate hostkeeper-hook")
    );
}

/// Catches: a layer crate that is missing from the workspace passing for lack of anything to
/// check.
#[test]
fn a_missing_layer_crate_is_refused() {
    let root = (
        "Cargo.toml".to_string(),
        "[workspace]\nresolver = \"3\"\nexclude = [\"vendored\"]\nmembers = [\n    \
         \"crates/hostkeeper-hook\",\n    \"crates/hostkeeper-steps\",\n    \
         \"crates/hostkeeper-testkit\",\n]\n"
            .to_string(),
    );
    let dir = fixture("clean", vec![root]);
    assert_eq!(
        check(&dir),
        refused("hostkeeper-rights is not a workspace member")
    );
}

/// Catches: a failing `cargo` command being read as an empty answer; the check must stop with
/// the command and its stderr.
#[test]
#[should_panic(expected = "cargo metadata --format-version 1 --no-deps --offline failed")]
fn a_failed_cargo_command_fails_loudly() {
    let dir = fixture("clean", vec![]);
    std::fs::remove_file(dir.join("crates/hostkeeper-hook/Cargo.toml")).expect("remove");
    let _ = check(&dir);
}

/// Catches: a workspace with no `Cargo.lock` passing because the graph was empty.
#[test]
#[should_panic(expected = "Cargo.lock: ")]
fn a_missing_lockfile_fails_loudly() {
    let dir = TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "layering");
    copy_dir(&fixtures().join("clean"), &dir);
    let _ = check(&dir);
}

/// Catches: a lockfile reader that drops the edges of a block, keeps the version or source of
/// an entry in the name, or reads a table that is not a package.
#[test]
fn the_lock_parser_reads_names_and_edges() {
    let lock = r#"# This file is automatically @generated by Cargo.
version = 4

[[package]]
name = "alpha"
version = "0.1.0"
dependencies = [
 "beta",
 "gamma 1.2.3",
 "delta 0.4.0 (registry+https://example.com/index)",
]

[[package]]
name = "beta"
version = "0.1.0"

[[package]]
name = "gamma"
version = "1.2.3"
source = "registry+https://example.com/index"
checksum = "00"
dependencies = [
 "beta",
]

[[patch.unused]]
name = "ghost"
version = "9.9.9"
"#;
    let edges = parse_lock(lock);
    let expected = |names: &[&str]| -> std::collections::BTreeSet<String> {
        names.iter().map(|name| name.to_string()).collect()
    };
    assert_eq!(edges.len(), 3);
    assert_eq!(edges["alpha"], expected(&["beta", "delta", "gamma"]));
    assert_eq!(edges["beta"], expected(&[]));
    assert_eq!(edges["gamma"], expected(&["beta"]));
}
