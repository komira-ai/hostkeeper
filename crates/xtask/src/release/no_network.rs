//! The test that no release test reaches git, GitHub or the network.
//!
//! The adapters that start a process are built in one module, `real.rs`, and called from one
//! place, `lib.rs`. This test reads every Rust file of the crate and refuses a name that breaks
//! that: the process-starting `Exec` named outside `real.rs`, `real::connect` or `real::exec`
//! named outside `lib.rs`, test code in `real.rs`, and a test that starts a process of its own, but for the
//! tests listed in [`PROCESS_TESTS`], which run a program on purpose. The names are built from
//! pieces, so this file passes its own rules.
//!
//! Only code is read: a line is cut at the first `//` that is not inside a string literal
//! ([`code_of`]). That is simple, and right for this crate, which writes no raw string or
//! character literal holding a quote or a double slash on a line with code.

use std::path::{Path, PathBuf};

/// The one module that builds the process-starting `Exec`.
const REAL: &str = "src/release/real.rs";

/// The one module that calls `real::connect` and `real::exec`.
const LIB: &str = "src/lib.rs";

/// The name of the process-starting `Exec`.
const SYSTEM_EXEC: &str = concat!("System", "Exec");

/// The path of the function that builds the real adapters.
const CONNECT: &str = concat!("real::", "connect");

/// The path of the function that returns the process-starting `Exec` itself.
const EXEC: &str = concat!("real::", "exec");

/// The call that starts a process.
const COMMAND: &str = concat!("Command::", "new");

/// The attribute that makes code test-only.
const CFG_TEST: &str = concat!("#[cfg(", "test)]");

/// The test files that start a process on purpose, and why.
const PROCESS_TESTS: [(&str, &str); 17] = [
    ("tests/cli.rs", "runs the xtask binary as a process"),
    (
        "tests/coordinator_update_unit.rs",
        "runs the coordinator updater's unit command with the fake gh as its gh and curl",
    ),
    (
        "tests/channels_cli.rs",
        "runs the xtask binary as a process on the channel verbs' flags",
    ),
    (
        "tests/channels_publish_cli.rs",
        "runs the xtask binary as a process on the apt, rpm and installer channel verbs",
    ),
    (
        "tests/channels_check_apt_cli.rs",
        "runs the xtask binary as a process on the apt install check, with a fake env",
    ),
    (
        "tests/release_image_cli.rs",
        "runs the xtask binary as a process on the image index and layout verbs",
    ),
    (
        "tests/release_linux.rs",
        "runs the xtask binary as a process on crafted packages",
    ),
    (
        "tests/workflows/targets_run_tests.rs",
        "runs the committed steps of release.yml's targets job with bash, with a fake gh and cargo",
    ),
    (
        "tests/workflows/set_desired_calls_tests.rs",
        "runs the committed set step of set-desired.yml with bash, with a fake hkctl",
    ),
    (
        "tests/workflows/coordinator_wait_run_tests.rs",
        "runs the committed wait and error steps of release.yml's coordinator job with bash, with a fake curl",
    ),
    (
        "tests/workflows/hello_run_tests.rs",
        "runs the committed steps of hello-desired.yml's release job with bash, with a fake curl and cargo",
    ),
    (
        "tests/bin/gh_fixture.rs",
        "the fake gh runs jq over its scratch release list, as gh --jq does; the fake cargo runs the xtask binary beside it",
    ),
    (
        "tests/coordinator_deploy.rs",
        "runs the committed database backup unit's command with a fake docker",
    ),
    ("tests/hygiene/main.rs", "lists the tracked files with git"),
    (
        "tests/workflows/pr_paths_tests.rs",
        "lists the tracked files with git, and runs pr-device.yml's diff with git in a scratch repository",
    ),
    (
        "tests/wallclock/main.rs",
        "lists the tracked files with git",
    ),
    (
        "tests/layering/check.rs",
        "runs cargo to read the workspace",
    ),
];

/// The files outside `tests/` and the `tests.rs` modules that hold only test code.
const TEST_SUPPORT: [&str; 3] = [
    "src/release/fakes.rs",
    "src/release/no_network.rs",
    "src/testdir.rs",
];

/// `line` without its trailing `//` comment: the text before the first `//` outside a string.
fn code_of(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
        } else if b == b'"' {
            in_string = true;
        } else if b == b'/' && bytes.get(i + 1) == Some(&b'/') {
            return &line[..i];
        }
    }
    line
}

/// Whether the file at `path` (from the crate root) holds only test code.
fn is_test_file(path: &str) -> bool {
    path.starts_with("tests/")
        | (path == "tests.rs")
        | path.ends_with("/tests.rs")
        | TEST_SUPPORT.contains(&path)
}

/// One line per rule broken, over `files` (each a path from the crate root and its text).
fn findings(files: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (path, text) in files {
        let path = path.as_str();
        let may_run = PROCESS_TESTS.iter().any(|(p, _)| *p == path);
        let test_only = is_test_file(path) & !may_run;
        for (n, line) in text.lines().enumerate() {
            let code = code_of(line);
            let at = n + 1;
            if code.contains(SYSTEM_EXEC) & (path != REAL) {
                out.push(format!(
                    "{path}:{at}: names {SYSTEM_EXEC}, which only {REAL} may build"
                ));
            }
            for name in [CONNECT, EXEC] {
                if code.contains(name) & (path != LIB) {
                    out.push(format!(
                        "{path}:{at}: names {name}, which only {LIB} may call"
                    ));
                }
            }
            if code.contains(CFG_TEST) & (path == REAL) {
                out.push(format!(
                    "{path}:{at}: holds test code ({CFG_TEST}), which {REAL} may not"
                ));
            }
            if code.contains(COMMAND) & test_only {
                out.push(format!(
                    "{path}:{at}: starts a process with {COMMAND}, which a test may not"
                ));
            }
        }
    }
    out
}

/// Every Rust file under `dir`, in path order, as (path from `root`, text).
fn collect(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("read a source directory")
        .map(|e| e.expect("read a directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect(&path, root, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let rel = path.strip_prefix(root).expect("a path under the root");
            let text = std::fs::read_to_string(&path).expect("read a source file");
            out.push((rel.to_string_lossy().into_owned(), text));
        }
    }
}

/// Every Rust file of `src` and `tests` of this crate.
fn sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in ["src", "tests"] {
        collect(&root.join(dir), root, &mut files);
    }
    files
}

fn file(path: &str, text: &str) -> (String, String) {
    (path.to_owned(), text.to_owned())
}

#[test]
fn release_commands_never_call_the_network_in_tests() {
    // Catches: a test (or an adapter test) building the process-starting Exec, calling
    // `real::connect` or `real::exec`, or starting git, gh or any program, so that a release test reaches the
    // network or the host.
    let files = sources();
    let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    assert!(paths.contains(&REAL), "{REAL} was not scanned");
    assert!(paths.contains(&LIB), "{LIB} was not scanned");
    assert_eq!(findings(&files), Vec::<String>::new());

    // The tests that run a process on purpose still do, so the list stays honest.
    for (path, why) in PROCESS_TESTS {
        let text = &files
            .iter()
            .find(|(p, _)| p == path)
            .expect("a listed process test is scanned")
            .1;
        assert!(text.contains(COMMAND), "{path} ({why}) starts no process");
    }

    // Each rule fails on a file that breaks it, and only on that.
    let exec = format!("let e = {SYSTEM_EXEC};\n");
    assert_eq!(
        findings(&[file("src/release/git.rs", &exec)]),
        vec![format!(
            "src/release/git.rs:1: names {SYSTEM_EXEC}, which only {REAL} may build"
        )]
    );
    assert_eq!(
        findings(&[file("tests/x.rs", &exec), file(REAL, &exec)]),
        vec![format!(
            "tests/x.rs:1: names {SYSTEM_EXEC}, which only {REAL} may build"
        )]
    );
    let real_exec = format!("let e = {EXEC}();\n");
    assert_eq!(
        findings(&[
            file("src/mdm_signing_test/tests.rs", &real_exec),
            file(LIB, &real_exec)
        ]),
        vec![format!(
            "src/mdm_signing_test/tests.rs:1: names {EXEC}, which only {LIB} may call"
        )]
    );
    let connect = format!("x();\nlet r = {CONNECT}();\n");
    assert_eq!(
        findings(&[file("src/release.rs", &connect), file(LIB, &connect)]),
        vec![format!(
            "src/release.rs:2: names {CONNECT}, which only {LIB} may call"
        )]
    );
    assert_eq!(
        findings(&[file(
            REAL,
            &format!("struct {SYSTEM_EXEC};\n{CFG_TEST}\nmod t {{}}\n")
        )]),
        vec![format!(
            "{REAL}:2: holds test code ({CFG_TEST}), which {REAL} may not"
        )]
    );
    let spawn = format!("let o = {COMMAND}(\"git\");\n");
    let broken = |path: &str| {
        findings(&[file(path, &spawn)])
            == vec![format!(
                "{path}:1: starts a process with {COMMAND}, which a test may not"
            )]
    };
    assert!(broken("tests/new.rs"));
    assert!(broken("src/release/tests.rs"));
    assert!(broken("tests.rs"));
    assert!(broken("src/release/fakes.rs"));
    assert!(broken("src/testdir.rs"));
    assert_eq!(
        findings(&[file("tests/cli.rs", &spawn)]),
        Vec::<String>::new()
    );
    assert_eq!(findings(&[file(REAL, &spawn)]), Vec::<String>::new());
    assert_eq!(
        findings(&[file("src/lib.rs", &spawn)]),
        Vec::<String>::new()
    );
    assert_eq!(
        findings(&[file("src/release/attests.rs", &spawn)]),
        Vec::<String>::new()
    );

    // A comment is not code, a name in a string is.
    let quiet = format!("// {SYSTEM_EXEC}\n/// {CONNECT}\nlet a = 1; // {COMMAND}\n");
    assert_eq!(
        findings(&[file("tests/x.rs", &quiet)]),
        Vec::<String>::new()
    );
    let quoted = format!("let s = \"{SYSTEM_EXEC} // x\";\n");
    assert_eq!(
        findings(&[file("tests/x.rs", &quoted)]),
        vec![format!(
            "tests/x.rs:1: names {SYSTEM_EXEC}, which only {REAL} may build"
        )]
    );
}

#[test]
fn code_of_cuts_a_line_comment_outside_a_string_only() {
    // Catches: a name in a comment refused, a comment hidden behind a string that holds a double
    // slash or an escaped quote, or a single slash taken for a comment.
    for (line, code) in [
        ("let a = 1; // c", "let a = 1; "),
        ("// c", ""),
        ("  /// d", "  "),
        (r#"let s = "a // b"; // c"#, r#"let s = "a // b"; "#),
        (r#"let s = "q\"// b"; // c"#, r#"let s = "q\"// b"; "#),
        (r#"let s = "q\\"; // c"#, r#"let s = "q\\"; "#),
        ("a / b", "a / b"),
        ("a /", "a /"),
        ("plain", "plain"),
        ("", ""),
    ] {
        assert_eq!(code_of(line), code, "{line}");
    }
}
