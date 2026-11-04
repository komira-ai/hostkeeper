//! The wall-clock lint: nothing the updater decides reads the wall clock. Decisions read the
//! injected `hostkeeper_steps::Clock` (monotonic seconds and the boot id), so a host whose clock
//! is wrong keeps its release instead of acting on a bad timestamp. The wall clock may be read only
//! where a certificate, an OIDC token or an enrollment nonce has a validity window, in the paths
//! `allowed.txt` lists, each with its reason.
//!
//! The files come from `git ls-files` at the repository root, as for the hygiene lint, so the
//! scan sees exactly the tracked sources, including those of a crate that is not (yet) a
//! workspace member. The rule is in `scan.rs`, the reading of a source in `code.rs`, and the
//! tests of the allow-list in `allow_list.rs`. The fixtures are small trees written to a scratch
//! directory; a text holding a form the rule refuses is built from pieces at run time.

mod allow_list;
mod code;
#[path = "../hygiene/rust_text.rs"]
mod rust_text;
mod scan;
#[path = "../../src/testdir.rs"]
mod testdir;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use scan::{ALLOWED_FILE, FORMS, Read, check, scanned};
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
fn refusal(path: &str, line: usize, form: &'static str) -> String {
    Read {
        path: path.to_owned(),
        line,
        form,
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
}

/// Each case: the form it reads, the source, and the line of the read.
fn form_cases() -> Vec<(&'static str, String, usize)> {
    let st = st();
    let n = "now";
    vec![
        (
            "SystemTime::now",
            format!("let t = std::time::{st}::{n}();"),
            1,
        ),
        (
            "SystemTime::now",
            format!("let t = ::std::time::{st}::{n}();"),
            1,
        ),
        (
            "SystemTime::now",
            format!("use std::time::{st} as T;\nlet t = T::{n}();"),
            2,
        ),
        (
            "SystemTime::now",
            format!("use std::time::{{Instant, {st} as Wall}};\nlet t =\n  Wall::{n}();"),
            3,
        ),
        (
            "SystemTime::now",
            format!("use std::{{time::{{self as t}}}};\nt::{st}::{n}();"),
            2,
        ),
        (
            "SystemTime::now",
            format!("use std::time;\ntime::{st}::{n}();"),
            2,
        ),
        (
            "SystemTime::now",
            format!("fn f() {{\n    W::{n}();\n}}\nuse std::time::{st} as W;"),
            2,
        ),
        ("Utc::now", format!("chrono::Utc::{n}()"), 1),
        ("Local::now", format!("use chrono::Local;\nLocal::{n}()"), 2),
        (
            "OffsetDateTime::now_utc",
            format!("time::OffsetDateTime::{n}_utc()"),
            1,
        ),
        (
            "OffsetDateTime::now_local",
            format!("use time::OffsetDateTime as O;\nO::{n}_local()"),
            2,
        ),
        ("Timestamp::now", format!("jiff::Timestamp::{n}()"), 1),
        ("Zoned::now", format!("jiff::Zoned::{n}()"), 1),
        (
            "libc::time",
            ["libc::", "time(std::ptr::null_mut())"].concat(),
            1,
        ),
        (
            "libc::gettimeofday",
            ["use libc::get", "timeofday;\nget", "timeofday(&mut tv, n)"].concat(),
            2,
        ),
        (
            "clock_gettime(CLOCK_REALTIME)",
            ["libc::clock_gettime(libc::CLOCK_", "REALTIME, &mut ts)"].concat(),
            1,
        ),
        (
            "clock_gettime(CLOCK_REALTIME_COARSE)",
            ["clock_gettime(CLOCK_", "REALTIME_COARSE, &mut ts)"].concat(),
            1,
        ),
        (
            "clock_gettime(ClockId::Realtime)",
            [
                "use rustix::time::{clock_gettime, ClockId};\nclock_gettime(ClockId::",
                "Realtime)",
            ]
            .concat(),
            2,
        ),
        (
            "clock_gettime(ClockId::RealtimeCoarse)",
            ["rustix::time::clock_gettime(ClockId::", "RealtimeCoarse)"].concat(),
            1,
        ),
        (
            "clock_gettime_dynamic(ClockId::Realtime)",
            [
                "clock_gettime_dynamic(DynamicClockId::Known(ClockId::",
                "Realtime))",
            ]
            .concat(),
            1,
        ),
        (
            "clock_gettime_dynamic(ClockId::RealtimeCoarse)",
            [
                "clock_gettime_dynamic(DynamicClockId::Known(ClockId::",
                "RealtimeCoarse))",
            ]
            .concat(),
            1,
        ),
    ]
}

/// Catches: a row of the form table that refuses nothing (or that is removed), a form missed
/// when it is spelled through a `use` (above or below the read), an alias, a group, `self` or a
/// leading `::`, and a row added to the table without a case here.
#[test]
fn each_wall_clock_form_is_refused() {
    let cases = form_cases();
    for &(form, ref text, line) in &cases {
        assert_eq!(
            lint(&[(DECIDE, text.as_str())], ""),
            Err(vec![refusal(DECIDE, line, form)]),
            "in {text:?}"
        );
    }
    let tested: BTreeSet<&str> = cases.iter().map(|(form, _, _)| *form).collect();
    let table: BTreeSet<&str> = FORMS.iter().map(|f| f.name).collect();
    assert_eq!(tested, table);
    assert_eq!(table.len(), FORMS.len(), "two rows share a name");
}

/// Catches: a table that over-matches (any `::now`, any `clock_gettime`), and a `use` of a
/// wall-clock type or function taken for a read.
#[test]
fn monotonic_reads_pass() {
    let st = st();
    let text = [
        "use std::time::{Instant, ",
        st.as_str(),
        "};\nuse libc::time;\nuse libc::{clock_gettime as cg};\nuse rustix::time::*;\n",
        "fn f(clock: &dyn Clock, id: ClockId) {\n",
        "    let a = Instant::now();\n    let b = std::time::Instant::now();\n",
        "    let c = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);\n",
        "    let d = clock_gettime(ClockId::Boottime);\n",
        "    let e = clock_gettime_dynamic(DynamicClockId::Known(ClockId::Monotonic));\n",
        "    let g = libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);\n",
        "    let h = cg(id);\n    let i = Stamp::now(clock);\n",
        "    let j = ",
        st.as_str(),
        "::UNIX_EPOCH;\n    let k = x.time();\n    let l = clock_gettime;\n}\n",
    ]
    .concat();
    assert_eq!(lint(&[(DECIDE, text.as_str())], ""), Ok(()));
}

/// Catches: a test-only read refused (inside `#[cfg(test)]`, a crate's `tests/`, `benches/` or
/// `examples/`, a `tests.rs` or a `tests` directory in `src/`, the testkit), and a skip that
/// runs past the end of the test item: a read just after it, or after a `;`-ended item, a
/// field, an arm or an argument behind `#[cfg(test)]`, is refused.
#[test]
fn a_test_only_read_passes() {
    let now = st_now();
    let module = format!(
        "#[cfg(test)]\n#[allow(dead_code)]\nmod tests {{\n    const C: char = '}}';\n    \
         fn f<A, B>() {{ {now}; }}\n}}\n"
    );
    let elsewhere: &str = &format!("fn f() {{ {now}; }}\n");
    let test_only = [
        ("crates/demo/src/lib.rs", module.as_str()),
        ("crates/demo/src/tests.rs", elsewhere),
        ("crates/demo/src/net/tests/helper.rs", elsewhere),
        ("crates/demo/tests/it.rs", elsewhere),
        ("crates/demo/benches/b.rs", elsewhere),
        ("crates/demo/examples/e.rs", elsewhere),
        ("crates/hostkeeper-testkit/src/lib.rs", elsewhere),
        ("crates/demo/build.rs", elsewhere),
        ("README.md", elsewhere),
    ];
    assert_eq!(lint(&test_only, ""), Ok(()));

    let after = format!("{module}fn g() {{ {now}; }}\n");
    let lib = "crates/demo/src/lib.rs";
    assert_eq!(
        lint(&[(lib, after.as_str())], ""),
        Err(vec![refusal(lib, 7, "SystemTime::now")])
    );
    let statement = format!("#[cfg(test)]\nconst X: [u8; 2] = [0; 2];\nfn g() {{ {now}; }}\n");
    assert_eq!(
        lint(&[(lib, statement.as_str())], ""),
        Err(vec![refusal(lib, 3, "SystemTime::now")])
    );
    let field = format!("struct S {{\n    #[cfg(test)]\n    a: u8\n}}\nfn g() {{ {now}; }}\n");
    assert_eq!(
        lint(&[(lib, field.as_str())], ""),
        Err(vec![refusal(lib, 5, "SystemTime::now")])
    );
    // A field, an arm or an argument behind `#[cfg(test)]` ends at its comma; an item does not.
    let lists = [
        format!("let s = S {{\n    #[cfg(test)]\n    pub a: 1,\n    b: {now},\n}};\n"),
        format!("match x {{\n    #[cfg(test)]\n    A => 1,\n    B => {now},\n}}\n"),
        format!("f(\n    #[cfg(test)]\n    pub(crate) a,\n    {now},\n);\n"),
        format!("#[cfg(test)]\n#[inline]\npub(crate) fn f<A, B>() {{}}\nfn g() {{ {now}; }}\n"),
    ];
    for text in &lists {
        assert_eq!(
            lint(&[(lib, text.as_str())], ""),
            Err(vec![refusal(lib, 4, "SystemTime::now")]),
            "in {text:?}"
        );
    }
}

/// Catches: a form named in a line, doc or block comment, a string, a raw string, a byte string
/// or after a character literal taken for a read, and a read missed on a line that also holds
/// one of those.
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
}
