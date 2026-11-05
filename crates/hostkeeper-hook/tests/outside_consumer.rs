//! A service repository outside this workspace depends on the harness by git tag
//! (`docs/conformance.md`). The crate in `tests/consumer` is that repository in small: it is
//! built here, offline, with the dependency line's source (`git` and `tag`) replaced by a path
//! to this checkout and its features kept as documented, and its test runs the harness against
//! the example hook.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use hostkeeper_testkit::Scratch;

const CONSUMER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/consumer");
const DOCS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/conformance.md");
const REPOSITORY: &str = "https://github.com/komira-ai/hostkeeper";

/// Everything a coverage or CI run puts in the environment that would change the child build.
const LEAKS: [&str; 14] = [
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "RUSTDOCFLAGS",
    "CARGO_ENCODED_RUSTDOCFLAGS",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "LLVM_PROFILE_FILE",
    "CARGO_LLVM_COV",
    "CARGO_LLVM_COV_TARGET_DIR",
    "CARGO_INCREMENTAL",
    "CARGO_MAKEFLAGS",
];

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name)
        .expect("create the scratch directory")
}

/// The line that declares the dependency, in `text`.
fn dependency_line(text: &str) -> String {
    text.lines()
        .find(|line| line.starts_with("hostkeeper-hook = "))
        .expect("a hostkeeper-hook dependency line")
        .to_owned()
}

/// The first toml block of a markdown text, without its fences.
fn first_toml_block(text: &str) -> String {
    let (_, after) = text.split_once("```toml\n").expect("a toml block");
    let (block, _) = after.split_once("```").expect("the end of the toml block");
    block.to_owned()
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create a directory of the copy");
    for entry in fs::read_dir(from).expect("read the fixture directory") {
        let entry = entry.expect("read a fixture entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("fixture entry type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy a fixture file");
        }
    }
}

/// A fresh copy of the fixture in a scratch directory; the source tree is never written (cargo
/// would leave a `Cargo.lock` in it).
fn fixture_copy(scratch: &Scratch) -> PathBuf {
    let copy = scratch.path().join("consumer");
    copy_dir(Path::new(CONSUMER), &copy);
    copy
}

/// A TOML basic string.
fn quoted(path: &Path) -> String {
    let text = path.to_str().expect("a path of valid UTF-8");
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn cargo(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO"));
    command.args(args);
    for name in LEAKS {
        command.env_remove(name);
    }
    command
}

fn report(output: &Output) -> String {
    format!(
        "stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    )
}

/// The dependency line a service writes: the harness by git tag, `v` and this crate's version.
fn documented_line() -> String {
    format!(
        "hostkeeper-hook = {{ git = \"{REPOSITORY}\", tag = \"v{}\", features = [\"conformance\"] }}",
        env!("CARGO_PKG_VERSION")
    )
}

/// `manifest` with its one documented dependency line pointed at the checkout in `checkout`:
/// the source (`git` and `tag`) becomes a `path`, and the rest of the line (the features) is
/// kept as written. `None` when the manifest does not hold the documented line exactly once.
fn pointed_at_checkout(manifest: &str, checkout: &Path) -> Option<String> {
    let line = documented_line();
    if manifest.lines().filter(|l| *l == line).count() != 1 {
        return None;
    }
    let source = format!(
        "git = \"{REPOSITORY}\", tag = \"v{}\"",
        env!("CARGO_PKG_VERSION")
    );
    let local = line.replacen(&source, &format!("path = {}", quoted(checkout)), 1);
    Some(manifest.replacen(&line, &local, 1))
}

/// Catches documentation, a fixture or a crate version that drift apart, and a harness a
/// service outside the workspace cannot use: the dependency line of `docs/conformance.md` is the
/// fixture's line and names this crate's version as its tag, and a crate with that one line,
/// its source pointed at this checkout and its features as documented, builds offline outside
/// the workspace, and its test, which runs the harness against the example hook, passes.
#[test]
fn a_consumer_crate_outside_the_workspace_compiles_against_the_harness() {
    let expected = documented_line();
    let docs = fs::read_to_string(DOCS).expect("read docs/conformance.md");
    let fixture = fs::read_to_string(Path::new(CONSUMER).join("Cargo.toml"))
        .expect("read the fixture manifest");
    assert_eq!(dependency_line(&first_toml_block(&docs)), expected);
    assert_eq!(dependency_line(&fixture), expected);

    let scratch = scratch("outside-consumer");
    let copy = fixture_copy(&scratch);
    let manifest = copy.join("Cargo.toml");
    let local = pointed_at_checkout(&fixture, Path::new(env!("CARGO_MANIFEST_DIR")))
        .expect("the fixture holds the documented line once");
    fs::write(&manifest, local).expect("write the copy's manifest");
    let hook = format!(
        "env.SAMPLE_HOOK = {}",
        quoted(Path::new(env!("CARGO_BIN_EXE_hostkeeper-hook-example")))
    );
    let output = cargo(&["test", "--offline", "--manifest-path"])
        .arg(&manifest)
        .arg("--target-dir")
        .arg(scratch.path().join("target"))
        .args(["--config", &hook])
        .output()
        .expect("run cargo");
    assert!(output.status.success(), "{}", report(&output));
}

/// Catches a check that cannot fail: a manifest whose line is not the documented one (another
/// repository, another tag, no features, or the line twice) is not pointed at the checkout, so
/// it is never built in its place.
#[test]
fn a_dependency_line_other_than_the_documented_one_is_not_built() {
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = fs::read_to_string(Path::new(CONSUMER).join("Cargo.toml"))
        .expect("read the fixture manifest");
    let line = documented_line();
    let local = pointed_at_checkout(&fixture, checkout).expect("the documented line");
    assert!(!local.contains(REPOSITORY), "{local}");
    assert!(local.contains("features = [\"conformance\"]"), "{local}");
    let tag = format!("tag = \"v{}\"", env!("CARGO_PKG_VERSION"));
    for changed in [
        line.replace(REPOSITORY, "https://github.com/komira-ai/not-hostkeeper"),
        line.replace(&tag, "tag = \"v0.0.0-other\""),
        line.replace(", features = [\"conformance\"]", ""),
        format!("{line}\n{line}"),
    ] {
        let manifest = fixture.replace(&line, &changed);
        assert_ne!(manifest, fixture);
        assert_eq!(pointed_at_checkout(&manifest, checkout), None, "{changed}");
    }
}
