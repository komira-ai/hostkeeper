//! Test-only code and file selection: what the scan does not read, and where that ends.

use crate::{lint, refusal, st_now};

const LIB: &str = "crates/demo/src/lib.rs";

/// Asserts that `text`, as the decision module `LIB`, is refused at `lines` only.
fn refused_at(text: &str, lines: &[usize]) {
    let want: Vec<String> = lines
        .iter()
        .map(|&line| refusal(LIB, line, "SystemTime::now"))
        .collect();
    assert_eq!(lint(&[(LIB, text)], ""), Err(want), "in {text:?}");
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
        (LIB, module.as_str()),
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

    refused_at(&format!("{module}fn g() {{ {now}; }}\n"), &[7]);
    refused_at(
        &format!("#[cfg(test)]\nconst X: [u8; 2] = [0; 2];\nfn g() {{ {now}; }}\n"),
        &[3],
    );
    refused_at(
        &format!("struct S {{\n    #[cfg(test)]\n    a: u8\n}}\nfn g() {{ {now}; }}\n"),
        &[5],
    );
    // A field, an arm or an argument behind `#[cfg(test)]` ends at its comma; an item does not.
    refused_at(
        &format!("let s = S {{\n    #[cfg(test)]\n    pub a: 1,\n    b: {now},\n}};\n"),
        &[4],
    );
    refused_at(
        &format!("match x {{\n    #[cfg(test)]\n    A => 1,\n    B => {now},\n}}\n"),
        &[4],
    );
    refused_at(
        &format!("f(\n    #[cfg(test)]\n    pub(crate) a,\n    {now},\n);\n"),
        &[4],
    );
    refused_at(
        &format!("#[cfg(test)]\n#[inline]\npub(crate) fn f<A, B>() {{}}\nfn g() {{ {now}; }}\n"),
        &[4],
    );
}

/// Catches: a test-only item cut short by a braced block that is not its body (a const generic
/// argument, a block in a `let` initializer, an arm's block), so that its later test-only reads
/// are refused, and a skip that then runs past the item.
#[test]
fn a_test_only_item_ends_after_its_body() {
    let now = st_now();
    refused_at(
        &format!(
            "#[cfg(test)]\nfn f() -> Holder<{{ 1 }}> {{\n    {now};\n}}\nfn g() {{ {now}; }}\n"
        ),
        &[5],
    );
    refused_at(
        &format!("fn h() {{\n    #[cfg(test)]\n    let x = {{ 1 }}.max({now});\n    {now};\n}}\n"),
        &[4],
    );
    refused_at(
        &format!("match x {{\n    #[cfg(test)]\n    A => {{ {now} }}\n    B => {now},\n}}\n"),
        &[4],
    );
    refused_at(
        &format!(
            "#[cfg(test)]\nimpl<T: Fn() -> u8> X for Y<T> {{\n    fn f() {{ {now}; }}\n}}\n\
             fn g() {{ {now}; }}\n"
        ),
        &[5],
    );
}

/// Catches: a cfg spelling that implies `test` (a trailing comma, `all(..)` holding `test`, an
/// inner `#![cfg(test)]` for a file or a module) read as production code, and one that does not
/// imply it (`any(test, ..)`, `not(test)`, a feature named test) skipped.
#[test]
fn a_cfg_that_implies_test_is_skipped_and_no_other() {
    let now = st_now();
    for attr in [
        "#[cfg(test,)]",
        "#[cfg(all(test))]",
        "#[cfg(all(unix, test))]",
    ] {
        refused_at(
            &format!("{attr}\nfn f() {{ {now}; }}\nfn g() {{ {now}; }}\n"),
            &[3],
        );
    }
    let file = format!("//! A test module.\n#![cfg(test)]\nfn f() {{ {now}; }}\n");
    assert_eq!(lint(&[(LIB, file.as_str())], ""), Ok(()));
    refused_at(
        &format!("mod m {{\n    #![cfg(test)]\n    fn f() {{ {now}; }}\n}}\nfn g() {{ {now}; }}\n"),
        &[5],
    );
    for attr in [
        "#[cfg(any(test, feature = \"x\"))]",
        "#[cfg(not(test))]",
        "#[cfg(feature = \"test\")]",
        "#[cfg_attr(test, allow(dead_code))]",
    ] {
        refused_at(&format!("{attr}\nfn f() {{ {now}; }}\n"), &[2]);
    }
}

/// Catches: a file selection that reads a file that is not Rust source under `src/`, or that
/// skips a shipping crate or module whose name only resembles the testkit, `tests.rs` or a
/// `tests` directory.
#[test]
fn file_selection_keeps_its_boundaries() {
    let read: &str = &format!("fn f() {{ {}; }}\n", st_now());
    let files = [
        ("crates/demo/src/data.txt", read),
        ("crates/demo/src/notes.md", read),
        ("crates/demo/src/lib.rs.orig", read),
        ("crates/hostkeeper-testkit-extra/src/lib.rs", read),
        ("crates/demo/src/tests_util.rs", read),
        ("crates/demo/src/attests.rs", read),
        ("crates/demo/src/my_tests/x.rs", read),
        ("crates/demo/src/tests.rs.d/x.rs", read),
    ];
    let refused: Vec<String> = [
        "crates/demo/src/attests.rs",
        "crates/demo/src/my_tests/x.rs",
        "crates/demo/src/tests.rs.d/x.rs",
        "crates/demo/src/tests_util.rs",
        "crates/hostkeeper-testkit-extra/src/lib.rs",
    ]
    .iter()
    .map(|path| refusal(path, 1, "SystemTime::now"))
    .collect();
    assert_eq!(lint(&files, ""), Err(refused));
}
