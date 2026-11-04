//! The wall-clock lint: nothing the updater decides reads the wall clock. Decisions read the
//! injected `hostkeeper_steps::Clock` (monotonic seconds and the boot id), so a host whose clock
//! is wrong keeps its release instead of acting on a bad timestamp. The wall clock may be read only
//! where a certificate, an OIDC token or an enrollment nonce has a validity window, in the paths
//! `allowed.txt` lists, each with its reason.
//!
//! The files come from `git ls-files` at the repository root, as for the hygiene lint, so the
//! scan sees exactly the tracked sources, including those of a crate that is not (yet) a
//! workspace member. The rule is in `scan.rs`; a source is read by `lex.rs` (tokens), `skip.rs`
//! (what is not read) and `code.rs` (paths and imports). The tests of the forms are in
//! `forms.rs`, of test-only code and file selection in `test_only.rs`, of the allow-list in
//! `allow_list.rs`. The fixtures are small trees written to a scratch
//! directory; a text holding a form the rule refuses is built from pieces at run time.

mod allow_list;
mod code;
mod forms;
mod lex;
#[path = "../hygiene/rust_text.rs"]
mod rust_text;
mod scan;
mod skip;
mod test_only;
#[path = "../../src/testdir.rs"]
mod testdir;

use std::path::{Path, PathBuf};
use std::process::Command;

use scan::{ALLOWED_FILE, Read, check, scanned};
use testdir::TestDir;

const ALLOWED_TEXT: &str = include_str!("allowed.txt");

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The tracked paths, from `git ls-files -z` at `root`. A git failure panics.
fn tracked_files(root: &Path) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .expect("run git ls-files");
    assert!(
        out.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .split(|&b| b == 0)
        .filter(|raw| !raw.is_empty())
        .map(|raw| String::from_utf8(raw.to_vec()).expect("a tracked path is UTF-8"))
        .collect()
}

/// `(path, text)` of each scanned path among `paths`, read under `root`.
fn sources(root: &Path, paths: &[String]) -> Vec<(String, String)> {
    paths
        .iter()
        .filter(|path| scanned(path))
        .map(|path| {
            let text = std::fs::read_to_string(root.join(path))
                .unwrap_or_else(|e| panic!("read {path}: {e}"));
            (path.clone(), text)
        })
        .collect()
}

/// The files under `dir`, as paths from `dir` with forward slashes, sorted.
fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read a fixture directory") {
        let entry = entry.expect("read a fixture entry");
        let name = entry.file_name().into_string().expect("a UTF-8 name");
        let relative = format!("{prefix}{name}");
        if entry.file_type().expect("file type").is_dir() {
            walk(&entry.path(), &format!("{relative}/"), out);
        } else {
            out.push(relative);
        }
    }
    out.sort();
}

/// A scratch tree holding `files` (path, text).
fn tree(files: &[(&str, &str)]) -> TestDir {
    let dir = TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), "wallclock");
    for (relative, text) in files {
        let path = dir.join(relative);
        std::fs::create_dir_all(path.parent().expect("a file has a parent")).expect("mkdir");
        std::fs::write(path, text).expect("write a fixture file");
    }
    dir
}

/// The lint over a scratch tree of `files`, with the allow-list `allowed`.
fn lint(files: &[(&str, &str)], allowed: &str) -> Result<(), Vec<String>> {
    let dir = tree(files);
    let mut paths = Vec::new();
    walk(&dir, "", &mut paths);
    check(&sources(&dir, &paths), allowed)
}

/// The refusal of a read of `form` at `path:line`.
fn refusal(path: &str, line: usize, form: &str) -> String {
    Read {
        path: path.to_owned(),
        line,
        form: form.to_owned(),
        renamed: false,
    }
    .to_string()
}

/// The refusal of the rename `spelling` at `path:line`.
fn renaming(path: &str, line: usize, spelling: &str) -> String {
    Read {
        path: path.to_owned(),
        line,
        form: spelling.to_owned(),
        renamed: true,
    }
    .to_string()
}

/// `SystemTime`, built from pieces so this file holds no form the rule refuses.
fn st() -> String {
    ["System", "Time"].concat()
}

/// A call of the `SystemTime` read: `SystemTime::now()`.
fn st_now() -> String {
    format!("{}::{}()", st(), "now")
}

const DECIDE: &str = "crates/demo/src/decide.rs";

/// Catches: a wall-clock read in any tracked source of a shipping crate without an allow-list
/// entry, an allow-list entry that went stale, and a scan that passes because it saw nothing
/// (it must see the injected clock's source, and must skip test-only files).
#[test]
fn no_decision_reads_the_wall_clock() {
    let root = repo_root();
    let files = sources(&root, &tracked_files(&root));
    let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
    assert!(paths.contains(&"crates/hostkeeper-steps/src/clock.rs"));
    assert!(paths.contains(&"crates/xtask/src/lib.rs"));
    assert!(!paths.contains(&"crates/xtask/src/install/tests.rs"));
    assert!(
        !paths
            .iter()
            .any(|p| p.starts_with("crates/hostkeeper-testkit/"))
    );
    assert_eq!(check(&files, ALLOWED_TEXT), Ok(()));
}

/// Catches: a scan that skips a crate it does not know, or that matches only the full
/// `std::time::` path and misses the name a `use` brought in.
#[test]
fn a_decision_module_reading_the_wall_clock_fails() {
    let text: &str = &format!(
        "use std::time::{st};\n\npub fn due() -> bool {{\n    {} > {st}::UNIX_EPOCH\n}}\n",
        st_now(),
        st = st()
    );
    assert_eq!(
        lint(&[(DECIDE, text)], ""),
        Err(vec![refusal(DECIDE, 4, "SystemTime::now")])
    );
    assert_eq!(
        refusal(DECIDE, 4, "SystemTime::now"),
        format!(
            "{DECIDE}:4: wall-clock read SystemTime::now: decisions read hostkeeper_steps::Clock; \
             if this is certificate, OIDC or nonce validity, add the path to {ALLOWED_FILE} \
             with a reason"
        )
    );
    assert_eq!(
        renaming(DECIDE, 1, "libc as c"),
        format!(
            "{DECIDE}:1: wall-clock name renamed, libc as c: the lint does not follow a rename, \
             so use the name itself; decisions read hostkeeper_steps::Clock; if this is \
             certificate, OIDC or nonce validity, add the path to {ALLOWED_FILE} with a reason"
        )
    );
}

/// Catches: a form named in a line, doc or block comment, a string, a raw string, a byte string
/// or after a character literal taken for a read, a read missed on a line that also holds one of
/// those, and a wrong line (or an item closed) after masked text that spans lines.
#[test]
fn a_read_in_a_comment_or_string_passes() {
    let now = st_now();
    let quoted = format!(
        "// {now}\n/// {now}\n/* {now} */\nlet s = \"{now}\";\nlet r = r#\"{now}\"#;\n\
         let b = b\"{now}\";\nlet q = '\"'; let e = '\\''; fn l<'a>(x: &'a str) {{}}\n"
    );
    assert_eq!(lint(&[(DECIDE, quoted.as_str())], ""), Ok(()));
    let read = format!("{quoted}let t = {now}; // {now}\n");
    assert_eq!(
        lint(&[(DECIDE, read.as_str())], ""),
        Err(vec![refusal(DECIDE, 8, "SystemTime::now")])
    );
    // Masked text over several lines, holding braces and attributes, keeps the line count and
    // closes no item.
    let multiline = format!(
        "fn f() {{\n/* a\n   /* nested }} */\n   }} #[cfg(test)] mod x {{\n*/\n\
         let s = \"one\n  }} #[cfg(test)] {{\n\";\nlet r = r#\"\n  }}\n\"#;\n\
         let t = {now};\n}}\nfn g() {{ {now}; }}\n"
    );
    assert_eq!(
        lint(&[(DECIDE, multiline.as_str())], ""),
        Err(vec![
            refusal(DECIDE, 12, "SystemTime::now"),
            refusal(DECIDE, 14, "SystemTime::now"),
        ])
    );
    let test_item = format!(
        "#[cfg(test)]\nmod t {{\n/* }} */\nlet s = \"}}\n}}\";\nfn f() {{ {now}; }}\n}}\n\
         fn g() {{ {now}; }}\n"
    );
    assert_eq!(
        lint(&[(DECIDE, test_item.as_str())], ""),
        Err(vec![refusal(DECIDE, 8, "SystemTime::now")])
    );
}
