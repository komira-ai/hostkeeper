//! Each rule of the report check refusing its defect on a small text, and accepting the text
//! without it.

use std::collections::BTreeSet;
use std::path::Path;

use crate::testdir::TestDir;

use crate::cells::{
    Kind, cells, open_problems, required_problems, result_problems, source_problems,
};
use crate::html::numbers;
use crate::markdown::{anchors, blocks, link_targets, loose_numbers, mask_code, slug};
use crate::rust_value::{declared_text, evaluate};
use crate::{World, check, diagram_problems, link_problems, method_names, rpcs, step_words};

/// Whether a path names one of `files` or a directory holding one, as the file system answers
/// for a checkout of exactly those files.
fn exists(files: &BTreeSet<String>) -> impl Fn(&str) -> bool + '_ {
    move |path| {
        let below = format!("{path}/");
        files.contains(path) || files.iter().any(|f| f.starts_with(&below))
    }
}

fn scratch(name: &str) -> TestDir {
    TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name)
}

/// `cells` of `text` with its code masked, as the check reads them.
fn parse(text: &str) -> (Vec<crate::cells::Cell>, Vec<String>) {
    cells(&mask_code(text))
}

#[test]
fn a_cell_has_exactly_one_source() {
    // Catches: a cell with no source, with two, a data-source with no item or the reverse, a
    // cell attribute on an element with no cell id, an unclosed cell and an id used twice.
    let text = "<span data-cell=\"a\">1</span>\n\
        <span data-cell=\"b\" data-result=\"r.tsv\" data-open=\"later\">open</span>\n\
        <span data-cell=\"c\" data-source=\"x.rs\">1</span>\n\
        <span data-cell=\"d\" data-item=\"X\">1</span>\n\
        <span data-open=\"later\">open</span>\n\
        <span data-cell=\"e\" data-open=\"later\">open</span>\n\
        <span data-cell=\"e\" data-open=\"later\">open</span>\n\
        <span data-cell=\"f\" data-open=\"later\">open\n";
    let (found, problems) = parse(text);
    assert_eq!(
        problems,
        vec![
            "line 1: cell a has no data-result, data-source or data-open",
            "line 2: cell b has more than one source",
            "line 3: cell c has data-source but no data-item",
            "line 4: cell d has data-item but no data-source",
            "line 5: data-open on an element with no data-cell",
            "line 7: cell e is used again (first on line 6)",
            "line 8: cell f is never closed",
        ]
    );
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].kind, Kind::Open("later".to_owned()));
}

#[test]
fn attributes_not_in_double_quotes_are_refused() {
    // Catches: a cell id or source written in single quotes, bare, or with no value, which a
    // reader of double-quoted values would never see.
    let text = "<span data-cell='a' data-open='x'>open</span>\n\
        <span data-cell=b data-open=\"x\">open</span>\n\
        <span data-cell>open</span>\n\
        <span class=\"ok\" hidden>fine</span>\n";
    let (found, problems) = parse(text);
    assert_eq!(
        problems,
        vec![
            "line 1: attribute data-cell is not double-quoted",
            "line 1: attribute data-open is not double-quoted",
            "line 2: attribute data-cell is not double-quoted",
            "line 3: attribute data-cell has no value",
            "line 2: data-open on an element with no data-cell",
        ]
    );
    assert!(found.is_empty());
}

#[test]
fn a_measured_number_must_be_a_value_of_a_row_naming_the_cell() {
    // Catches: a number no result holds, a number taken from another cell's row, a row naming
    // no cell, and accepts a row shared by two cells and a thousands separator.
    let dir = scratch("report-result");
    std::fs::write(
        dir.join("r.tsv"),
        "# kind\tid\tvalue\tunit\thow\tcells\n\
         raw\tfirst\t12.8\ts\tm\ta\n\
         raw\tsecond\t40\ts\tm\tb\n\
         raw\tshared\t1754\tMiB\tm\ta, b\n\
         raw\tnone\t99\ts\tm\n",
    )
    .expect("write");
    let text = "<span data-cell=\"a\" data-result=\"r.tsv\">12.8 s of 1,754 MiB</span>\n\
        <span data-cell=\"b\" data-result=\"r.tsv\">12.8 s, 40 s, 1754 MiB</span>\n\
        <span data-cell=\"c\" data-result=\"r.tsv\">99 s and 13.8 s</span>\n";
    let (found, problems) = parse(text);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        result_problems(&found, &dir),
        vec![
            "line 2: b: the number 12.8 is not a value of r.tsv for this cell; the rows holding it (first) do not name b",
            "line 3: c: the number 99 is not a value of r.tsv for this cell; the rows holding it (none) do not name c",
            "line 3: c: the number 13.8 is not a value of r.tsv",
        ]
    );
}

#[test]
fn an_unusable_result_name_or_file_is_refused() {
    // Catches: a result name that climbs out of the results directory, a missing file, a row
    // whose value is no number, a row with too few columns, a dotted version and a filled cell
    // with no number.
    let dir = scratch("report-names");
    std::fs::write(dir.join("bad.tsv"), "raw\tx\tmany\ts\tm\n").expect("write");
    std::fs::write(dir.join("short.tsv"), "raw\tx\n").expect("write");
    std::fs::write(dir.join("r.tsv"), "raw\tx\t5\ts\tm\te,f\n").expect("write");
    let text = "<span data-cell=\"a\" data-result=\"../r.tsv\">5</span>\n\
        <span data-cell=\"b\" data-result=\"gone.tsv\">5</span>\n\
        <span data-cell=\"c\" data-result=\"bad.tsv\">5</span>\n\
        <span data-cell=\"d\" data-result=\"short.tsv\">5</span>\n\
        <span data-cell=\"e\" data-result=\"r.tsv\">release 1.2.3 in 5 s</span>\n\
        <span data-cell=\"f\" data-result=\"r.tsv\">fast</span>\n";
    let (found, _) = parse(text);
    assert_eq!(
        result_problems(&found, &dir),
        vec![
            "line 1: a: data-result \"../r.tsv\" is not a plain .tsv name",
            "line 2: b: result file gone.tsv does not exist",
            "line 3: c: bad.tsv: line 1: value \"many\" is not a number",
            "line 4: d: short.tsv: line 1: fewer than 3 tab-separated columns",
            "line 5: e: 1.2.3 is a dotted version, not a number",
            "line 6: f: a filled cell prints no number",
        ]
    );
}

#[test]
fn a_code_number_must_be_the_value_of_the_named_declaration() {
    // Catches: a number that is not the constant's value (the constant was changed, or the
    // report mistyped it), a factor of the value taken for it (`20 MiB` for `1 << 20`), a wrong
    // unit (`7 years` for a week), two numbers in one cell, a name the file declares only in a
    // comment, a value that cannot be evaluated, and a path that leaves the repository or does
    // not exist; accepts the value in any unit that names it.
    let dir = scratch("report-source");
    std::fs::create_dir_all(dir.join("src")).expect("dir");
    std::fs::write(
        dir.join("src/lib.rs"),
        "/// const NOT_THIS: u64 = 7;\npub const LIMIT: u64 = 64 * 1_024;\n\
         static WAIT: Duration = Duration::from_millis(30_000u64);\n\
         pub(crate) const WEEK: u64 = 7 * 24 * 60 * 60;\nconst BIG: u64 = 1 << 20;\n\
         const NAME: &str = \"a;b 5\";\n",
    )
    .expect("write");
    let text = "<span data-cell=\"a\" data-source=\"src/lib.rs\" data-item=\"LIMIT\">64 KiB</span>\n\
        <span data-cell=\"b\" data-source=\"src/lib.rs\" data-item=\"WAIT\">30 s</span>\n\
        <span data-cell=\"c\" data-source=\"src/lib.rs\" data-item=\"LIMIT\">65,536</span>\n\
        <span data-cell=\"d\" data-source=\"src/lib.rs\" data-item=\"WEEK\">7 days</span>\n\
        <span data-cell=\"e\" data-source=\"src/lib.rs\" data-item=\"BIG\">1 MiB</span>\n\
        <span data-cell=\"f\" data-source=\"src/lib.rs\" data-item=\"LIMIT\">65</span>\n\
        <span data-cell=\"g\" data-source=\"src/lib.rs\" data-item=\"BIG\">20 MiB</span>\n\
        <span data-cell=\"h\" data-source=\"src/lib.rs\" data-item=\"WEEK\">7 years</span>\n\
        <span data-cell=\"i\" data-source=\"src/lib.rs\" data-item=\"LIMIT\">64 or 1024</span>\n\
        <span data-cell=\"j\" data-source=\"src/lib.rs\" data-item=\"NOT_THIS\">7</span>\n\
        <span data-cell=\"k\" data-source=\"src/lib.rs\" data-item=\"NAME\">5</span>\n\
        <span data-cell=\"l\" data-source=\"../lib.rs\" data-item=\"LIMIT\">64</span>\n\
        <span data-cell=\"m\" data-source=\"src/gone.rs\" data-item=\"LIMIT\">64</span>\n";
    let (found, _) = parse(text);
    assert_eq!(
        source_problems(&found, &dir),
        vec![
            "line 6: f: 65 is not the value of LIMIT in src/lib.rs, 65536",
            "line 7: g: 20 MiB is not the value of BIG in src/lib.rs, 1048576",
            "line 8: h: 7 years is not the value of WEEK in src/lib.rs, 604800",
            "line 9: i: a cell of the code prints one number, not 2",
            "line 10: j: src/lib.rs declares no const or static NOT_THIS",
            "line 11: k: NAME in src/lib.rs: '\"' in \"\\\"a;b 5\\\"\" cannot be evaluated",
            "line 12: l: data-source \"../lib.rs\" is not a plain path in the repository",
            "line 13: m: source src/gone.rs cannot be read",
        ]
    );
}

#[test]
fn values_are_evaluated_as_the_compiler_reads_them() {
    // Catches: a hex, octal, binary or exponent literal misread, a type suffix or separator
    // read as part of the number, an operator applied in the wrong order, a Duration in
    // milliseconds read as seconds, and a declaration found inside a longer name or cut at a
    // `;` inside brackets or a string.
    assert_eq!(evaluate("0x1000"), Ok(4096.0));
    assert_eq!(evaluate("0o17 + 0b101"), Ok(20.0));
    assert_eq!(evaluate("1e3 + 2.5E-1f64"), Ok(1000.25));
    assert_eq!(evaluate("1 << 4 + 1"), Ok(32.0));
    assert_eq!(evaluate("2 + 3 * (4 - 1) / 3"), Ok(5.0));
    assert_eq!(evaluate("-5_000_u64"), Ok(-5000.0));
    assert_eq!(evaluate("Duration::from_millis(500)"), Ok(0.5));
    assert_eq!(
        evaluate("Duration::from_micros(2e6) + Duration::from_nanos(1e9)"),
        Ok(3.0)
    );
    assert_eq!(evaluate("NonZeroU64::new(9)"), Ok(9.0));
    assert!(evaluate("u64::MAX").is_err());
    assert!(evaluate("(1 + 2").is_err());
    assert!(evaluate("1 2").is_err());
    assert!(evaluate("0x").is_err());
    assert!(evaluate("*").is_err());
    assert_eq!(declared_text("const MAX_LIMIT: u8 = 9;", "LIMIT"), None);
    assert_eq!(
        declared_text("pub(crate) static LIMIT: [u8; 2] = [3; 2];\n", "LIMIT"),
        Some("[3; 2]")
    );
    assert_eq!(
        declared_text("  pub const S: &str = \"x;\\\"y\";", "S"),
        Some("\"x;\\\"y\"")
    );
}

#[test]
fn an_open_cell_prints_no_digit_and_says_open() {
    // Catches: an open cell carrying an estimate, and an open cell a reader cannot see is open.
    let text = "<span data-cell=\"m-a\" data-open=\"farm run\">open (farm run)</span>\n\
        <span data-cell=\"m-b\" data-open=\"farm run\">about 5 s</span>\n\
        <span data-cell=\"m-c\" data-open=\"farm run\">&#53; s, open</span>\n\
        <span data-cell=\"m-d\" data-open=\"farm run\">pending</span>\n";
    let (found, _) = parse(text);
    assert_eq!(
        open_problems(&found),
        vec![
            "line 2: m-b: an open cell prints a digit: \"about 5 s\"",
            "line 2: m-b: an open cell does not say open: \"about 5 s\"",
            "line 3: m-c: an open cell prints a digit: \"5 s, open\"",
            "line 4: m-d: an open cell does not say open: \"pending\"",
        ]
    );
}

#[test]
fn every_promised_measurement_is_a_measured_or_open_cell() {
    // Catches: a measurement dropped from the report, and one filled from a code constant.
    let text = "<span data-cell=\"m-a\" data-open=\"x\">open</span>\n\
        <span data-cell=\"m-b\" data-source=\"x.rs\" data-item=\"X\">1</span>\n\
        <span data-cell=\"m-c\" data-result=\"r.tsv\">1</span>\n";
    let (found, _) = parse(text);
    assert_eq!(
        required_problems(&found, &["m-a", "m-b", "m-c", "m-d"]),
        vec![
            "line 2: the measurement cell m-b takes its value from the code, not from a measurement",
            "the measurement cell m-d is missing",
        ]
    );
}

#[test]
fn a_number_outside_a_cell_is_refused_and_code_is_not_prose() {
    // Catches: a number in the prose, in a table, in the text of a link (a citation names its
    // section in words), after a bracket or as an encoded digit; does not read a cell, a code
    // span, a fenced block, a comment, a tag or a link target.
    let text = "Renewal at <span data-cell=\"a\" data-open=\"x\">open</span> life.\n\
        Takes `P-256` keys and ``a `5` b`` and [design 5.4](../design/x.md#54-gate), [see 6](https://example.com/6).\n\
        <!-- 9 -->\n\
        ```text\n\
        12 lines\n\
        ```\n\
        | 3 hosts | (4) | v1, arm64, x86_64, S3 |\n\
        It waits &#53; s.<br>\n";
    let masked = mask_code(text);
    let (found, problems) = cells(&masked);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        loose_numbers(&masked, &found),
        vec![
            "line 2: the number 5.4 stands outside a cell",
            "line 2: the number 6 stands outside a cell",
            "line 7: the number 3 stands outside a cell",
            "line 7: the number 4 stands outside a cell",
            "line 8: the number 5 stands outside a cell",
        ]
    );
}

#[test]
fn fences_are_found_as_markdown_writes_them() {
    // Catches: a fence closed by a shorter run or another character, an indented code line read
    // as a fence, and an unclosed fence that would let its body be read as prose.
    let text = "````mermaid\nA\n```\n~~~\n````\n    ```\nnot code\n~~~\nopen\n";
    let found = blocks(text);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].info, "mermaid");
    assert_eq!(found[0].body, "A\n```\n~~~\n");
    assert_eq!(found[1].info, "");
    assert_eq!(found[1].body, "open\n");
    assert_eq!(mask_code(text).matches("not code").count(), 1);
}

#[test]
fn links_lead_to_files_and_headings() {
    // Catches: a link to a file that does not exist, out of the repository, to a heading that
    // does not exist (in another file or this one), and a line anchor into source code;
    // accepts a directory, a heading with code in it, an anchor of this file and a URL.
    let tracked: BTreeSet<String> = [
        "docs/report/hostkeeper.md",
        "docs/design/d.md",
        "crates/a/src/lib.rs",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    let read = |p: &str| (p == "docs/design/d.md").then(|| "## 5.4 The gate: `hosts`\n".to_owned());
    let own = "## Problem\n\
        [a](../design/d.md#54-the-gate-hosts) [b](../../crates/a) [c](https://example.com/x)\n\
        [d](#problem) [e](#nowhere) [f](../design/gone.md) [g](../../../x.md)\n\
        [h](../design/d.md#55-other) [i](../../crates/a/src/lib.rs#L10) `[j](gone.md)`\n";
    let masked = mask_code(own);
    assert_eq!(
        link_problems(
            "docs/report/hostkeeper.md",
            &masked,
            own,
            &exists(&tracked),
            &read
        ),
        vec![
            "the link #nowhere names no heading of docs/report/hostkeeper.md",
            "the link ../design/gone.md leads to no file",
            "the link ../../../x.md climbs out of the repository",
            "the link ../design/d.md#55-other names no heading of docs/design/d.md",
            "the link ../../crates/a/src/lib.rs#L10 points at a line of a source file; name the item instead",
        ]
    );
    assert_eq!(link_targets("[a](x.md \"title\")"), vec!["x.md"]);
}

#[test]
fn anchors_follow_github() {
    // Catches: an anchor computed with the punctuation kept, the case kept, or a heading inside
    // a fenced block taken as one.
    assert_eq!(
        slug(" 5.4 The rollout gate: which hosts? "),
        "54-the-rollout-gate-which-hosts"
    );
    assert_eq!(
        anchors("# A `b_c`-d\n```\n## not\n```\n####### seven\n#none\n"),
        vec!["a-b_c-d"]
    );
}

#[test]
fn diagrams_are_at_most_three_and_name_only_methods_of_the_protos() {
    // Catches: a fourth diagram (mermaid or svg), and a diagram naming a method no service of
    // the protos has; does not take a dotted file name or a lower-case word for a method.
    let protos = "service Hosts {\n  rpc Approve(A) returns (B);\n  rpc List (A) returns (B);\n}\n\
        service Releases {\n rpc Force(A) returns (B);\n}\n";
    let known = rpcs([protos]);
    assert_eq!(
        known.iter().cloned().collect::<Vec<_>>(),
        vec!["Hosts.Approve", "Hosts.List", "Releases.Force"]
    );
    let one = "```mermaid\nA->>B: Hosts.Approve and Releases.Force\nB->>A: Hosts.Reject, manifest.toml, a.B, X.Y.Z\n```\n";
    let text = format!("{one}{one}{one}<svg></svg>\n");
    let masked = mask_code(&text);
    assert_eq!(
        diagram_problems(&text, &masked, &known, &[]),
        vec![
            "4 diagrams, more than 3",
            "the diagram on line 1: Hosts.Reject is no method of the protos",
            "the diagram on line 5: Hosts.Reject is no method of the protos",
            "the diagram on line 9: Hosts.Reject is no method of the protos",
            "no mermaid diagram has the line \"%% update\"",
        ]
    );
    assert_eq!(
        method_names("(Hosts.List) Hosts.Approve."),
        vec!["Hosts.List", "Hosts.Approve"]
    );
}

#[test]
fn the_update_diagram_names_every_step_of_the_contract() {
    // Catches: an update diagram that dropped a step's message while the word stays in a note,
    // an alt label or a longer word, and a step list read from the wrong place; reads the steps
    // as the contract's source spells them.
    let source = "pub const ALL: [Step; 2] = [\n Step::Prepare,\n Step::CutOver,\n];\n\
        Step::Prepare => \"prepare\",\n Step::CutOver => \"cutover\",\n Step::Gone => \"gone\",\n";
    let steps = step_words(source);
    assert_eq!(steps, vec!["prepare", "cutover"]);
    assert!(step_words("no steps").is_empty());
    let text = "```mermaid\n%% update\nD->>H: prepare\nD->>H: precutover\nNote over D: cutover here\nalt cutover passes\nend\n```\n";
    assert_eq!(
        diagram_problems(text, &mask_code(text), &BTreeSet::new(), &steps),
        vec!["the update diagram on line 1 does not name the step cutover"]
    );
    let good = "```mermaid\nsequenceDiagram\n  %% update\nD->>H: prepare\nD->>H: cutover\n```\n";
    assert!(diagram_problems(good, &mask_code(good), &BTreeSet::new(), &steps).is_empty());
}

#[test]
fn numbers_are_read_as_the_report_prints_them() {
    // Catches: a scanner that reads a digit inside a word, splits a thousands separator, or
    // takes a dotted version for a number.
    assert_eq!(
        numbers("p50 of 447 ms (64 hosts), S3, x86_64, arm64, 1,754 MiB, .5, 19%. v0.1.0 1.2.3"),
        vec![447.0, 64.0, 1754.0, 0.5, 19.0]
    );
    assert_eq!(
        numbers("SHA-256 and P-256 keys, ids of 1-63 bytes, -5"),
        vec![1.0, 63.0, 5.0]
    );
}

#[test]
fn a_clean_report_passes_every_rule() {
    // Catches: a check that refuses a correct report, so it could never be green.
    let dir = scratch("report-clean");
    std::fs::create_dir_all(dir.join("results")).expect("dir");
    std::fs::create_dir_all(dir.join("src")).expect("dir");
    std::fs::write(
        dir.join("results/r.tsv"),
        "raw\tenroll\t4.2\ts\tm\tm-enroll\n",
    )
    .expect("write");
    std::fs::write(dir.join("src/lib.rs"), "const HALF: u32 = 2;\n").expect("write");
    let mut text = String::from(
        "# Report\n\nRenewed at one over <span data-cell=\"c-half\" data-source=\"src/lib.rs\" data-item=\"HALF\">2</span> of life ([code](../src/lib.rs)).\n\n",
    );
    for id in crate::MEASUREMENTS {
        if *id == "m-enroll" {
            text.push_str("<span data-cell=\"m-enroll\" data-result=\"r.tsv\">4.2 s</span>\n");
        } else {
            text.push_str(&format!(
                "<span data-cell=\"{id}\" data-open=\"farm run\">open</span>\n"
            ));
        }
    }
    text.push_str("```mermaid\n%% update\nA->>B: Hosts.Approve\nA->>B: prepare\n```\n");
    let tracked: BTreeSet<String> = ["doc/r.md", "src/lib.rs"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let known = rpcs(["service Hosts {\n rpc Approve(A) returns (B);\n}\n"]);
    let read = |_: &str| None;
    let steps = vec!["prepare".to_owned()];
    let world = World {
        root: &dir,
        results: &dir.join("results"),
        exists: &exists(&tracked),
        rpcs: &known,
        steps: &steps,
        read: &read,
    };
    let (found, problems) = check("doc/r.md", &text, &world);
    assert_eq!(problems, Vec::<String>::new());
    assert_eq!(found.len(), crate::MEASUREMENTS.len() + 1);
}
