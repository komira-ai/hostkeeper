//! The layers of the host updater crates, held on the dependency graph.
//!
//! - no package anywhere in the graph (any kind, any platform) is named `kbf`, `kbf-*` or
//!   `kbf_*`;
//! - `hostkeeper-hook` and `hostkeeper-config` depend on no other crate of this workspace (their
//!   dev-dependencies hold only themselves and `hostkeeper-testkit`);
//! - `hostkeeper-steps` and `hostkeeper-rights` depend on `hostkeeper-config` and
//!   `hostkeeper-hook`, and `hostkeeper-unpack` on `hostkeeper-config`,
//!   `hostkeeper-hook` and `hostkeeper-steps`, and on no other workspace crate
//!   (dev-dependencies: themselves and `hostkeeper-testkit`);
//! - `hostkeeper-wire`, `hostkeeper-oidc` and `hostkeeper-tls` depend on no workspace crate;
//!   `hostkeeper-coordinator` depends on exactly those three and `hostkeeper-proto` (the
//!   messages and refusals of the operator's calls and the orders it sends) (dev-dependencies:
//!   itself, `hostkeeper-testkit` and `hostkeeper-oidc`, whose `testkit` feature its tests use);
//! - `hostkeeper-enrollment` (the coordinator's CA and enrollment store) depends on no other
//!   crate of this workspace (dev-dependencies: itself and `hostkeeper-testkit`);
//! - `hostkeeper-testkit` depends on no workspace crate, and no crate has it but as a
//!   dev-dependency;
//! - no non-test source file under `crates/*/src` brings back a closed set of roles (the enum
//!   `HostRole`, an enum named `...Role`, a `const` or `static` of `RoleName`, a function
//!   returning an array of `RoleName`): roles are open strings.
//!
//! The rules are in `check.rs`. The fixture workspaces under `fixtures/` are copied to a
//! scratch directory, locked offline and checked; each failing one has an exact expected
//! message.

mod check;
#[path = "../../src/testdir.rs"]
mod testdir;

use std::path::{Path, PathBuf};

use check::{
    cargo, check, check_hook_harness, closed_role_enums, metadata_of, parse_lock, read_lock,
};
use testdir::TestDir;

const HOOK_DEP: &str = "hostkeeper-hook = { path = \"../hostkeeper-hook\" }\n";
const CONFIG_DEP: &str = "hostkeeper-config = { path = \"../hostkeeper-config\" }\n";
const RIGHTS_DEP: &str = "hostkeeper-rights = { path = \"../hostkeeper-rights\" }\n";
const TESTKIT_DEP: &str = "hostkeeper-testkit = { path = \"../hostkeeper-testkit\" }\n";
const WIRE_DEP: &str = "hostkeeper-wire = { path = \"../hostkeeper-wire\" }\n";
const OIDC_DEP: &str = "hostkeeper-oidc = { path = \"../hostkeeper-oidc\" }\n";
const TLS_DEP: &str = "hostkeeper-tls = { path = \"../hostkeeper-tls\" }\n";
const PROTO_DEP: &str = "hostkeeper-proto = { path = \"../hostkeeper-proto\" }\n";

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
             but must depend on exactly {hostkeeper-config, hostkeeper-hook}"
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
/// testkit as a dev-dependency, steps and rights on the config crate and the hook).
#[test]
fn a_clean_fixture_passes() {
    let dir = fixture("clean", vec![]);
    assert_eq!(check(&dir), Ok(()));
}

/// A crate `shim` under the fixture's `vendored/` (outside the members) that depends on the
/// workspace crate `target`, and `hostkeeper-steps` depending on the config crate, the hook and
/// `shim`.
fn steps_through_shim(target: &str) -> Vec<(String, String)> {
    vec![
        (
            "vendored/shim/Cargo.toml".to_string(),
            layered(
                "shim",
                &format!("{target} = {{ path = \"../../crates/{target}\" }}\n"),
                "",
                "",
            ),
        ),
        ("vendored/shim/src/lib.rs".to_string(), String::new()),
        manifest_of(
            "hostkeeper-steps",
            layered(
                "hostkeeper-steps",
                &format!("{CONFIG_DEP}{HOOK_DEP}{}", vendored_dep("shim")),
                "",
                TESTKIT_DEP,
            ),
        ),
    ]
}

/// Catches: a layer rule that looks only at direct dependencies, so that the step runner
/// reaches the rights crate through a crate outside the workspace and goes unseen; the refusal
/// must name the chain.
#[test]
fn layering_refuses_a_layer_reached_through_an_excluded_crate() {
    let dir = fixture("clean", steps_through_shim("hostkeeper-rights"));
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-steps depends (normal or build) on the workspace crates \
             {hostkeeper-config, hostkeeper-hook, hostkeeper-rights}, but must depend on exactly \
             {hostkeeper-config, hostkeeper-hook}\n\
             hostkeeper-steps reaches the workspace crate hostkeeper-rights through \
             hostkeeper-steps -> shim -> hostkeeper-rights"
        )
    );
}

/// Catches: an intermediary outside the workspace being refused although it reaches only the
/// hook contract, which the step runner may depend on.
#[test]
fn an_excluded_crate_reaching_only_the_hook_passes() {
    let dir = fixture("clean", steps_through_shim("hostkeeper-hook"));
    assert_eq!(check(&dir), Ok(()));
}

/// Catches: the hook harness's dependency on `rustix` becoming unconditional (every service
/// that depends on the contract would link it), the `conformance` feature no longer turning it
/// on, or another non-optional normal dependency joining the contract crate.
#[test]
fn hook_harness_dependency_is_behind_the_conformance_feature() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut metadata = metadata_of(&root);
    assert_eq!(check_hook_harness(&metadata), Ok(()));
    let package = metadata["packages"]
        .as_array_mut()
        .expect("metadata lists packages")
        .iter_mut()
        .find(|package| package["name"] == "hostkeeper-hook")
        .expect("the hook crate is a member");
    let rustix = package["dependencies"]
        .as_array_mut()
        .expect("a package lists its dependencies")
        .iter_mut()
        .find(|dep| dep["name"] == "rustix")
        .expect("the hook crate depends on rustix");
    rustix["optional"] = serde_json::Value::Bool(false);
    package["features"]["conformance"] = serde_json::json!([]);
    assert_eq!(
        check_hook_harness(&metadata),
        refused(
            "hostkeeper-hook must have rustix as an optional normal dependency\n\
             hostkeeper-hook must have the feature conformance = [\"dep:rustix\"]\n\
             hostkeeper-hook has non-optional normal dependencies {rustix} beyond \
             serde, serde_json, thiserror, toml"
        )
    );
}

/// Catches: the hook harness check passing when the hook package is absent from the metadata.
#[test]
fn the_hook_harness_check_refuses_metadata_without_the_hook() {
    assert_eq!(
        check_hook_harness(&serde_json::json!({ "packages": [] })),
        refused("hostkeeper-hook is not in the metadata")
    );
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
            &format!("{CONFIG_DEP}{HOOK_DEP}"),
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
            &format!("{CONFIG_DEP}{HOOK_DEP}"),
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
                &format!("{CONFIG_DEP}{HOOK_DEP}{TESTKIT_DEP}"),
                "",
                "",
            ),
        )],
    );
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-rights depends (normal or build) on the workspace crates \
             {hostkeeper-config, hostkeeper-hook, hostkeeper-testkit}, but must depend on exactly \
             {hostkeeper-config, hostkeeper-hook}\n\
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
            layered(
                "hostkeeper-steps",
                &format!("{CONFIG_DEP}{HOOK_DEP}"),
                TESTKIT_DEP,
                "",
            ),
        )],
    );
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-steps depends (normal or build) on the workspace crates \
             {hostkeeper-config, hostkeeper-hook, hostkeeper-testkit}, but must depend on exactly \
             {hostkeeper-config, hostkeeper-hook}\n\
             hostkeeper-steps has a normal or build dependency on hostkeeper-testkit, \
             which is a dev-dependency only"
        )
    );
}

/// Catches: the step runner gaining a normal dependency on another workspace crate (here the
/// rights crate) beside the config crate and the hook contract.
#[test]
fn steps_with_an_extra_workspace_dependency_is_refused() {
    let dir = fixture(
        "clean",
        vec![manifest_of(
            "hostkeeper-steps",
            layered(
                "hostkeeper-steps",
                &format!("{CONFIG_DEP}{HOOK_DEP}{RIGHTS_DEP}"),
                "",
                TESTKIT_DEP,
            ),
        )],
    );
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-steps depends (normal or build) on the workspace crates \
             {hostkeeper-config, hostkeeper-hook, hostkeeper-rights}, but must depend on exactly \
             {hostkeeper-config, hostkeeper-hook}"
        )
    );
}

const STEPS_DEP: &str = "hostkeeper-steps = { path = \"../hostkeeper-steps\" }\n";

/// Catches: the unpack crate gaining a normal dependency on a workspace crate beside its three
/// (here the rights crate), or losing one (here the step runner, whose layout and seal it
/// uses); either is refused with the set it has and the set it must have.
#[test]
fn unpack_with_another_set_of_workspace_dependencies_is_refused() {
    let extra = fixture(
        "clean",
        vec![manifest_of(
            "hostkeeper-unpack",
            layered(
                "hostkeeper-unpack",
                &format!("{CONFIG_DEP}{HOOK_DEP}{STEPS_DEP}{RIGHTS_DEP}"),
                "",
                TESTKIT_DEP,
            ),
        )],
    );
    assert_eq!(
        check(&extra),
        refused(
            "hostkeeper-unpack depends (normal or build) on the workspace crates \
             {hostkeeper-config, hostkeeper-hook, hostkeeper-rights, hostkeeper-steps}, \
             but must depend on exactly {hostkeeper-config, hostkeeper-hook, hostkeeper-steps}"
        )
    );
    let missing = fixture(
        "clean",
        vec![manifest_of(
            "hostkeeper-unpack",
            layered(
                "hostkeeper-unpack",
                &format!("{CONFIG_DEP}{HOOK_DEP}"),
                "",
                TESTKIT_DEP,
            ),
        )],
    );
    assert_eq!(
        check(&missing),
        refused(
            "hostkeeper-unpack depends (normal or build) on the workspace crates \
             {hostkeeper-config, hostkeeper-hook}, \
             but must depend on exactly {hostkeeper-config, hostkeeper-hook, hostkeeper-steps}"
        )
    );
}

/// Catches: the rights crate losing its dependency on the hook contract or on the config crate
/// (the set must be exactly the two, not merely a subset of them).
#[test]
fn rights_without_the_hook_is_refused() {
    let dir = fixture(
        "clean",
        vec![manifest_of(
            "hostkeeper-rights",
            layered("hostkeeper-rights", CONFIG_DEP, "", TESTKIT_DEP),
        )],
    );
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-rights depends (normal or build) on the workspace crates \
             {hostkeeper-config}, but must depend on exactly {hostkeeper-config, hostkeeper-hook}"
        )
    );
}

/// Catches: the config crate depending on a workspace crate. It holds the loaders the rights
/// crate builds on, so a dependency on the rights crate would turn the layers around, and one on
/// the hook contract would tie the file formats to the hook's.
#[test]
fn config_depending_on_a_workspace_crate_is_refused() {
    let dir = fixture(
        "clean",
        vec![manifest_of(
            "hostkeeper-config",
            layered("hostkeeper-config", HOOK_DEP, "", TESTKIT_DEP),
        )],
    );
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-config depends (normal or build) on the workspace crates \
             {hostkeeper-hook}, but must depend on exactly {}"
        )
    );
}

/// Catches: the config crate as a dev-dependency of the hook contract or the step runner, which
/// may hold only themselves and the testkit.
#[test]
fn steps_with_a_dev_dependency_on_config_is_refused() {
    let dir = fixture(
        "clean",
        vec![manifest_of(
            "hostkeeper-steps",
            layered(
                "hostkeeper-steps",
                &format!("{CONFIG_DEP}{HOOK_DEP}"),
                "",
                &format!("{TESTKIT_DEP}{CONFIG_DEP}"),
            ),
        )],
    );
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-steps has a dev-dependency on the workspace crate hostkeeper-config, \
             which is neither itself nor hostkeeper-testkit"
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
                &format!("{CONFIG_DEP}{HOOK_DEP}"),
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
         \"crates/hostkeeper-config\",\n    \"crates/hostkeeper-enrollment\",\n    \
         \"crates/hostkeeper-hook\",\n    \"crates/hostkeeper-steps\",\n    \
         \"crates/hostkeeper-wire\",\n    \"crates/hostkeeper-oidc\",\n    \
         \"crates/hostkeeper-tls\",\n    \"crates/hostkeeper-coordinator\",\n    \
         \"crates/hostkeeper-testkit\",\n    \"crates/hostkeeper-unpack\",\n    \
         \"crates/hostkeeper-proto\",\n]\n"
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

/// The manifest of the coordinator with `deps` as its normal dependencies.
fn coordinator(deps: &str) -> (String, String) {
    manifest_of(
        "hostkeeper-coordinator",
        layered("hostkeeper-coordinator", deps, "", TESTKIT_DEP),
    )
}

/// Catches: the coordinator reaching a kbf crate (here a path crate outside the members) going
/// unseen; the refusal must name the chain from the coordinator.
#[test]
fn layering_refuses_a_coordinator_with_a_kbf_dependency() {
    let mut edits = bare_crate("vendored", "kbf-types");
    edits.push(coordinator(&format!(
        "{WIRE_DEP}{OIDC_DEP}{TLS_DEP}{PROTO_DEP}{}",
        vendored_dep("kbf-types")
    )));
    let dir = fixture("clean", edits);
    assert_eq!(
        check(&dir),
        refused(&format!("{KBF_CHAIN} hostkeeper-coordinator -> kbf-types"))
    );
}

/// Catches: the coordinator losing one of its four workspace dependencies (the set must be
/// exactly wire, oidc, tls and proto, not a subset of them).
#[test]
fn a_coordinator_missing_a_workspace_dependency_is_refused() {
    let dir = fixture(
        "clean",
        vec![coordinator(&format!("{OIDC_DEP}{TLS_DEP}{PROTO_DEP}"))],
    );
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-coordinator depends (normal or build) on the workspace crates \
             {hostkeeper-oidc, hostkeeper-proto, hostkeeper-tls}, but must depend on exactly \
             {hostkeeper-oidc, hostkeeper-proto, hostkeeper-tls, hostkeeper-wire}"
        )
    );
}

/// Catches: the coordinator gaining a fifth workspace dependency (here the hook contract)
/// beside wire, oidc, tls and proto.
#[test]
fn a_coordinator_with_an_extra_workspace_dependency_is_refused() {
    let dir = fixture(
        "clean",
        vec![coordinator(&format!(
            "{WIRE_DEP}{OIDC_DEP}{TLS_DEP}{PROTO_DEP}{HOOK_DEP}"
        ))],
    );
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-coordinator depends (normal or build) on the workspace crates \
             {hostkeeper-hook, hostkeeper-oidc, hostkeeper-proto, hostkeeper-tls, \
             hostkeeper-wire}, but must depend on exactly {hostkeeper-oidc, hostkeeper-proto, \
             hostkeeper-tls, hostkeeper-wire}"
        )
    );
}

/// Catches: the coordinator's dev-dependency on oidc (for its `testkit` feature) being refused,
/// and a dev-dependency on any other layer crate (here the hook contract) being let through.
#[test]
fn a_coordinator_may_have_oidc_as_a_dev_dependency_and_no_other_layer_crate() {
    let deps = format!("{WIRE_DEP}{OIDC_DEP}{TLS_DEP}{PROTO_DEP}");
    let with_dev = |dev: &str| {
        manifest_of(
            "hostkeeper-coordinator",
            layered("hostkeeper-coordinator", &deps, "", dev),
        )
    };
    let dir = fixture("clean", vec![with_dev(&format!("{TESTKIT_DEP}{OIDC_DEP}"))]);
    assert_eq!(check(&dir), Ok(()));
    let dir = fixture("clean", vec![with_dev(&format!("{TESTKIT_DEP}{HOOK_DEP}"))]);
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-coordinator has a dev-dependency on the workspace crate hostkeeper-hook, \
             which is neither itself nor hostkeeper-testkit"
        )
    );
}

/// Catches: wire, oidc or tls gaining a dependency on another workspace crate (wire on oidc,
/// oidc on tls, tls on wire), each of which must stand alone.
#[test]
fn wire_oidc_and_tls_depending_on_a_workspace_crate_are_refused() {
    for (name, dep, target) in [
        ("hostkeeper-wire", OIDC_DEP, "hostkeeper-oidc"),
        ("hostkeeper-oidc", TLS_DEP, "hostkeeper-tls"),
        ("hostkeeper-tls", WIRE_DEP, "hostkeeper-wire"),
    ] {
        let dir = fixture(
            "clean",
            vec![manifest_of(name, layered(name, dep, "", TESTKIT_DEP))],
        );
        assert_eq!(
            check(&dir),
            refused(&format!(
                "{name} depends (normal or build) on the workspace crates {{{target}}}, \
                 but must depend on exactly {{}}"
            )),
            "{name}"
        );
    }
}

/// Catches: the closed set of roles coming back in non-test source: the enum `HostRole`, an
/// enum named `...Role`, a `const` or `static` of `RoleName`, a function returning an array of
/// `RoleName` (its name possibly path-qualified, its declaration possibly split over lines);
/// each is named by file and the line the declaration starts on. Test files, a `#[cfg(test)]`
/// module, and line and block comments, string literals (plain and raw) and character literals
/// holding the same words are left alone, as are `const fn` and a const generic parameter.
#[test]
fn layering_refuses_a_closed_role_enum() {
    let (scanned, found) = closed_role_enums(&fixtures().join("closed-role-enum"));
    assert_eq!(scanned, 4);
    assert_eq!(
        found,
        vec![
            "crates/hostkeeper-coordinator/src/lib.rs:9: \
             a const or static of type RoleName"
                .to_string(),
            "crates/hostkeeper-oidc/src/lib.rs:3: a const or static of type RoleName".to_string(),
            "crates/hostkeeper-oidc/src/lib.rs:5: a const or static of type RoleName".to_string(),
            "crates/hostkeeper-tls/src/lib.rs:1: the enum NodeRole, a closed set of roles"
                .to_string(),
            "crates/hostkeeper-tls/src/lib.rs:5: a function returning an array of RoleName"
                .to_string(),
            "crates/hostkeeper-tls/src/lib.rs:9: a function returning an array of RoleName"
                .to_string(),
            "crates/hostkeeper-tls/src/lib.rs:13: a function returning an array of RoleName"
                .to_string(),
            "crates/hostkeeper-wire/src/lib.rs:2: the identifier HostRole".to_string(),
        ]
    );
}

/// Catches: a closed role enum in this repository's own source, and a scan that passes only
/// because it read no file.
#[test]
fn the_workspace_has_no_closed_role_enum() {
    let (scanned, found) = closed_role_enums(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    assert!(scanned > 0, "the scan read no source file");
    assert_eq!(found, Vec::<String>::new());
}

/// Catches: the enrollment crate (the coordinator's CA and enrollment store) gaining a normal
/// or build dependency on another workspace crate, or a dev-dependency on one other than the
/// testkit; and the crate missing from the workspace going unseen.
#[test]
fn enrollment_depending_on_a_workspace_crate_is_refused() {
    let dir = fixture(
        "clean",
        vec![manifest_of(
            "hostkeeper-enrollment",
            layered("hostkeeper-enrollment", HOOK_DEP, CONFIG_DEP, RIGHTS_DEP),
        )],
    );
    assert_eq!(
        check(&dir),
        refused(
            "hostkeeper-enrollment depends (normal or build) on the workspace crates \
             {hostkeeper-config, hostkeeper-hook}, but must depend on exactly {}\n\
             hostkeeper-enrollment has a dev-dependency on the workspace crate \
             hostkeeper-rights, which is neither itself nor hostkeeper-testkit"
        )
    );
    let dir = fixture("clean", vec![]);
    std::fs::remove_dir_all(dir.join("crates/hostkeeper-enrollment")).expect("remove the crate");
    cargo(&["generate-lockfile", "--offline"], &dir.join("Cargo.toml"));
    assert_eq!(
        check(&dir),
        refused("hostkeeper-enrollment is not a workspace member")
    );
}
