//! Each rule of the report check refusing its defect on a small text, and accepting the text
//! without it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::testdir::TestDir;

use crate::cells::{
    Kind, cells, cite_problems, open_problems, required_problems, result_problems, source_problems,
};
use crate::figures::{
    Literal, Ui, html_literals, js_literals, method_names, method_problems,
    problem_figure_problems, rpcs, step_words, ui_problems, update_problems,
};
use crate::html::numbers;
use crate::markdown::{anchors, blocks, slug};
use crate::page::{id_problems, link_problems, loose_numbers, mask_code, mask_raw, sources};
use crate::rust_value::{declared_text, evaluate};
use crate::{World, check};

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

/// A UI of the given pages, each with the literals of its source, and the shared source.
fn ui(pages: &[(&str, &str)], shared: &str) -> Ui {
    Ui {
        pages: pages
            .iter()
            .map(|(p, src)| ((*p).to_owned(), js_literals(src)))
            .collect::<BTreeMap<_, _>>(),
        shared: js_literals(shared),
    }
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
        <span data-cell=\"f\" data-open=\"later\">open\n\
        <span data-cell=\"g\" data-cite=\"https://a.example/\" data-open=\"x\">open</span>\n\
        <span data-cite=\"https://a.example/\">5</span>\n";
    let (found, problems) = parse(text);
    assert_eq!(
        problems,
        vec![
            "line 1: cell a has no data-result, data-source, data-open or data-cite",
            "line 2: cell b has more than one source",
            "line 3: cell c has data-source but no data-item",
            "line 4: cell d has data-item but no data-source",
            "line 5: data-open on an element with no data-cell",
            "line 7: cell e is used again (first on line 6)",
            "line 8: cell f is never closed",
            "line 9: cell g has more than one source",
            "line 10: data-cite on an element with no data-cell",
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
        <span class=\"ok\" hidden>fine</span>\n\
        <span data-cell=\"c\" data-cite>open</span>\n";
    let (found, problems) = parse(text);
    assert_eq!(
        problems,
        vec![
            "line 1: attribute data-cell is not double-quoted",
            "line 1: attribute data-open is not double-quoted",
            "line 2: attribute data-cell is not double-quoted",
            "line 3: attribute data-cell has no value",
            "line 5: attribute data-cite has no value",
            "line 2: data-open on an element with no data-cell",
            "line 5: cell c has no data-result, data-source, data-open or data-cite",
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
    // Catches: a measurement dropped from the report, one filled from a code constant, and one
    // filled with a figure quoted from another tool's documentation.
    let text = "<span data-cell=\"m-a\" data-open=\"x\">open</span>\n\
        <span data-cell=\"m-b\" data-source=\"x.rs\" data-item=\"X\">1</span>\n\
        <span data-cell=\"m-c\" data-result=\"r.tsv\">1</span>\n\
        <span data-cell=\"m-e\" data-cite=\"https://a.example/\">1 s</span>\n";
    let (found, _) = parse(text);
    assert_eq!(
        required_problems(&found, &["m-a", "m-b", "m-c", "m-d", "m-e"]),
        vec![
            "line 2: the measurement cell m-b takes its value from the code, not from a measurement",
            "the measurement cell m-d is missing",
            "line 4: the measurement cell m-e quotes another tool's documentation, not a measurement",
        ]
    );
}

#[test]
fn a_cite_cell_names_a_listed_https_page_and_prints_a_number() {
    // Catches: a quotation of a page the sources do not list, of a URL that is not https, a
    // quotation with no number and one printing a dotted version; reads the sources only from
    // the element with id sources, so a link elsewhere in the page lists nothing.
    let text = "<p><a href=\"https://b.example/elsewhere\">b</a></p>\n\
        <span data-cell=\"a\" data-cite=\"https://a.example/doc\">a 60 s timeout</span>\n\
        <span data-cell=\"b\" data-cite=\"https://b.example/elsewhere\">5 hosts</span>\n\
        <span data-cell=\"c\" data-cite=\"http://a.example/doc\">5 hosts</span>\n\
        <span data-cell=\"d\" data-cite=\"https://a.example/doc\">many hosts</span>\n\
        <span data-cell=\"e\" data-cite=\"https://a.example/doc\">release 1.2.3</span>\n\
        <section id=\"sources\"><ul><li><a href=\"https://a.example/doc\">a</a></li></ul></section>\n\
        <a href=\"https://c.example/after\">c</a>\n";
    let masked = mask_code(text);
    let listed = sources(&masked);
    assert_eq!(listed, vec!["https://a.example/doc"]);
    let (found, problems) = cells(&masked);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        cite_problems(&found, &listed),
        vec![
            "line 3: b: data-cite https://b.example/elsewhere is not a page of the sources",
            "line 4: c: data-cite \"http://a.example/doc\" is not an https URL",
            "line 5: d: a filled cell prints no number",
            "line 6: e: 1.2.3 is a dotted version, not a number",
            "line 6: e: a filled cell prints no number",
        ]
    );
    assert!(sources("<p>no sources</p>").is_empty());
    assert!(sources("<section id=\"sources\"><a href=\"https://a.example/\">x</a>").is_empty());
}

#[test]
fn a_number_outside_a_cell_is_refused_and_code_is_not_prose() {
    // Catches: a number in the prose, in a table cell, in the text of a link, after a bracket or
    // as an encoded digit; does not read a cell, a code element, a preformatted block, a style
    // sheet, a script, a comment, a tag or a link target.
    let text = "Renewal at <span data-cell=\"a\" data-open=\"x\">open</span> life.\n\
        Takes <code>P-256</code> keys and <code class=\"x\">a 5 b</code>, <a href=\"https://example.com/6\">see 6</a>.\n\
        <!-- 9 -->\n\
        <pre>\n\
        12 lines <code>13</code>\n\
        </pre>\n\
        <style>p{margin:14px}</style><script>let a = 15;</script>\n\
        <table><tr><td>3 hosts</td><td>(4)</td><td>v1, arm64, x86_64, S3</td></tr></table>\n\
        It waits &#53; s.<br>\n";
    let masked = mask_code(text);
    let (found, problems) = cells(&masked);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        loose_numbers(&masked, &found),
        vec![
            "line 2: the number 6 stands outside a cell",
            "line 8: the number 3 stands outside a cell",
            "line 8: the number 4 stands outside a cell",
            "line 9: the number 5 stands outside a cell",
        ]
    );
}

#[test]
fn attributes_are_prose_unless_structure_or_geometry() {
    // Catches: a number written in a title, an aria-label, an alt text, a meta content or an
    // attribute the check does not know, where no reader of the cells looks; does not read
    // geometry, ids, classes, link targets or a cell's own attributes.
    let text = "<svg viewBox=\"0 0 960 400\" aria-label=\"6 hosts\"><rect x=\"10\" y=\"20\" width=\"30\" height=\"4\" rx=\"2\"></rect>\n\
        <text x=\"5\" font-size=\"12\" text-anchor=\"middle\">node-1</text></svg>\n\
        <span title=\"12 ms\" id=\"s2\" class=\"c3\">x</span> <img alt=\"13\" src=\"a1.png\">\n\
        <meta name=\"viewport\" content=\"initial-scale=1\"><a href=\"#s2\" data-x=\"7\">ok</a>\n\
        <span data-cell=\"c-x2\" data-open=\"run 2\" title=\"8\">open</span>\n";
    let masked = mask_code(text);
    let (found, problems) = cells(&masked);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        loose_numbers(&masked, &found),
        vec![
            "line 1: the number 6 stands in the attribute aria-label outside a cell",
            "line 3: the number 12 stands in the attribute title outside a cell",
            "line 3: the number 13 stands in the attribute alt outside a cell",
            "line 4: the number 1 stands in the attribute content outside a cell",
            "line 4: the number 7 stands in the attribute data-x outside a cell",
        ]
    );
}

#[test]
fn fences_are_found_as_markdown_writes_them() {
    // Catches: a fence closed by a shorter run or another character, an indented code line read
    // as a fence, and an unclosed fence, any of which would make a heading of a linked Markdown
    // file appear or vanish.
    let text = "````mermaid\nA\n```\n~~~\n````\n    ```\nnot code\n~~~\nopen\n";
    let found = blocks(text);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].info, "mermaid");
    assert_eq!(found[0].body, "A\n```\n~~~\n");
    assert_eq!(found[1].info, "");
    assert_eq!(found[1].body, "open\n");
}

#[test]
fn links_lead_to_ids_files_and_headings() {
    // Catches: an anchor to no id of the page, a repository link to a file that does not exist,
    // with a `..` or an empty part, to a heading that does not exist, a line anchor into source
    // code, a link at a ref other than main, a relative link, an http link and a javascript
    // link; accepts an id of the page, a directory, a Markdown heading, and other https and
    // mailto URLs; reads no link inside code.
    let tracked: BTreeSet<String> = ["docs/design/d.md", "crates/a/src/lib.rs"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let read = |p: &str| (p == "docs/design/d.md").then(|| "## 5.4 The gate: `hosts`\n".to_owned());
    let blob = "https://github.com/komira-ai/hostkeeper/blob/main/";
    let tree = "https://github.com/komira-ai/hostkeeper/tree/main/";
    let text = format!(
        "<h2 id=\"problem\">Problem</h2>\n\
        <a href=\"{blob}docs/design/d.md#54-the-gate-hosts\">a</a> <a href=\"{tree}crates/a/\">b</a> <a href=\"https://example.com/x\">c</a>\n\
        <a href=\"#problem\">d</a> <a href=\"#nowhere\">e</a> <a href=\"{blob}docs/design/gone.md\">f</a>\n\
        <a href=\"{blob}docs/../x.md\">g</a> <a href=\"{blob}docs//d.md\">h</a>\n\
        <a href=\"{blob}docs/design/d.md#55-other\">i</a> <a href=\"{blob}crates/a/src/lib.rs#L10\">j</a>\n\
        <a href=\"https://github.com/komira-ai/hostkeeper/blob/v1/crates/a/src/lib.rs\">k</a> <a href=\"../design/d.md\">l</a>\n\
        <a href=\"http://example.com/\">m</a> <a href=\"javascript:x()\">n</a> <a href=\"mailto:a@example.com\">o</a>\n\
        <code><a href=\"gone.md\">p</a></code> <a href=\"https://github.com/komira-ai/hostkeeper/pull/64\">q</a>\n"
    );
    assert_eq!(
        link_problems(&mask_code(&text), &exists(&tracked), &read),
        vec![
            "line 3: the link #nowhere names no id of the page".to_owned(),
            format!("line 3: the link {blob}docs/design/gone.md leads to no file"),
            format!("line 4: the link {blob}docs/../x.md is not a plain path in the repository"),
            format!("line 4: the link {blob}docs//d.md is not a plain path in the repository"),
            format!("line 5: the link {blob}docs/design/d.md#55-other names no heading of docs/design/d.md"),
            format!("line 5: the link {blob}crates/a/src/lib.rs#L10 points at a line of a source file; name the item instead"),
            "line 6: the link https://github.com/komira-ai/hostkeeper/blob/v1/crates/a/src/lib.rs names a ref other than main".to_owned(),
            "line 6: the link ../design/d.md is not an https URL; a relative link breaks where the page is served, so link the repository URL".to_owned(),
            "line 7: the link http://example.com/ is not an https URL; only https and mailto links are allowed".to_owned(),
            "line 7: the link javascript:x() is not an https URL; only https and mailto links are allowed".to_owned(),
        ]
    );
}

#[test]
fn an_id_is_used_once() {
    // Catches: two elements with one id, so an anchor to it may land on either; an id inside
    // code is not read.
    let text = "<h2 id=\"a\">A</h2>\n<p id=\"b\">b</p>\n<svg id=\"a\"></svg>\n<code><i id=\"b\"></i></code>\n";
    assert_eq!(
        id_problems(&mask_code(text)),
        vec!["line 3: the id a is used again (first on line 1)"]
    );
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
fn the_problem_figures_are_two_to_four() {
    // Catches: a report with one problem figure or five; counts only figures marked as problem
    // figures, and none inside code.
    let fig = "<figure data-figure=\"problem\"><svg></svg></figure>\n";
    let other = "<figure data-figure=\"lab\"></figure><figure></figure>\n";
    let code = format!("<pre>{}</pre>\n", fig.trim_end());
    assert_eq!(
        problem_figure_problems(&mask_code(&format!("{fig}{other}{code}"))),
        vec!["1 problem figures, not between 2 and 4"]
    );
    assert!(problem_figure_problems(&mask_code(&fig.repeat(2))).is_empty());
    assert!(problem_figure_problems(&mask_code(&fig.repeat(4))).is_empty());
    assert_eq!(
        problem_figure_problems(&mask_code(&fig.repeat(5))),
        vec!["5 problem figures, not between 2 and 4"]
    );
}

#[test]
fn every_named_method_is_a_method_of_the_protos() {
    // Catches: a method no service of the protos has, named in prose, in code, in a diagram's
    // text or in an aria-label; does not take a dotted file name, a lower-case word or a longer
    // dotted name for a method, and does not read style sheets or comments.
    let protos = "service Hosts {\n  rpc Approve(A) returns (B);\n  rpc List (A) returns (B);\n}\n\
        service Releases {\n rpc Force(A) returns (B);\n}\n";
    let known = rpcs([protos]);
    assert_eq!(
        known.iter().cloned().collect::<Vec<_>>(),
        vec!["Hosts.Approve", "Hosts.List", "Releases.Force"]
    );
    let text = "<p>Hosts.Approve and <code>Hosts.Reject</code>, manifest.toml, a.B, X.Y.Z</p>\n\
        <svg aria-label=\"Releases.Undo\"><text>Releases.Force</text><text>Hosts.Gone</text></svg>\n\
        <style>.Hosts.Styled{}</style><!-- Hosts.Comment -->\n";
    assert_eq!(
        method_problems(&mask_raw(text), &known),
        vec![
            "line 1: Hosts.Reject is no method of the protos",
            "line 2: Hosts.Gone is no method of the protos",
            "line 2: Releases.Undo is no method of the protos",
        ]
    );
    assert_eq!(
        method_names("(Hosts.List) Hosts.Approve."),
        vec!["Hosts.List", "Hosts.Approve"]
    );
}

#[test]
fn the_update_diagram_names_every_step_of_the_contract() {
    // Catches: no update diagram, two of them, one never closed, and an update diagram that
    // dropped a step's label while the word stays inside a longer label or a note, and a step
    // list read from the wrong place; reads the steps as the contract's source spells them, and
    // a label split over tspans as one.
    let source = "pub const ALL: [Step; 2] = [\n Step::Prepare,\n Step::CutOver,\n];\n\
        Step::Prepare => \"prepare\",\n Step::CutOver => \"cutover\",\n Step::Gone => \"gone\",\n";
    let steps = step_words(source);
    assert_eq!(steps, vec!["prepare", "cutover"]);
    assert!(step_words("no steps").is_empty());
    let bad = "<p>x</p>\n<svg data-diagram=\"update\"><text>prepare</text><text>precutover</text>\n\
        <text>cutover here</text></svg><text>cutover</text>\n";
    assert_eq!(
        update_problems(&mask_code(bad), &steps),
        vec!["line 2: the update diagram does not name the step cutover"]
    );
    let good = "<svg data-diagram=\"update\"><text> prepare </text><text><tspan>cut</tspan>over</text></svg>\n";
    assert!(update_problems(&mask_code(good), &steps).is_empty());
    assert_eq!(
        update_problems("<svg><text>prepare</text></svg>", &steps),
        vec!["no svg has data-diagram=\"update\""]
    );
    assert_eq!(
        update_problems(&format!("{good}{good}"), &steps),
        vec!["2 svgs have data-diagram=\"update\", not one"]
    );
    assert_eq!(
        update_problems("<svg data-diagram=\"update\"><text>prepare</text>", &steps),
        vec!["line 1: the update diagram is never closed"]
    );
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
        numbers("SHA-256 and P-256 keys, ids of 1-63 bytes, -5, node-1"),
        vec![1.0, 63.0, 5.0]
    );
}

#[test]
fn every_ui_page_has_one_mock_up_of_its_own_strings() {
    // Catches: a page of the UI with no mock-up, a mock-up of a page that does not exist, two of
    // one page, a mock-up showing a label no literal of the UI holds, a label of another page,
    // a UI string outside every mock-up and one never closed; accepts the page's literals, a
    // shared module's, a template literal with its holes filled, and sample data not marked as
    // UI text.
    let fleet = "h(\"h2\", {}, \"Fleet\"); h(\"th\", {}, 'Host'); `Hosts (${n} shown)`;";
    let holds = "h(\"button\", {}, \"Release\");";
    let shared = "nav(\"Sign out\")";
    let ui = ui(&[("fleet", fleet), ("holds", holds), ("audit", "")], shared);
    let text = "<div data-ui-page=\"fleet\"><p class=\"ui-t\">Fleet</p><b class=\"x ui-t\"> Host </b>\n\
        <i class=\"ui-t\">Hosts (all shown)</i><i class=\"ui-t\">Sign out</i><i class=\"ui-t\">Release</i>\n\
        <i class=\"ui-t\">Delete everything</i><i>node-1</i></div>\n\
        <div data-ui-page=\"holds\"><i class=\"ui-t\">Release</i></div><div data-ui-page=\"holds\"></div>\n\
        <div data-ui-page=\"gone\"></div><p class=\"ui-t\">Fleet</p>\n\
        <div data-ui-page=\"fleet\"><i class=\"ui-t\">open</div>\n";
    assert_eq!(
        ui_problems(&mask_code(text), &ui),
        vec![
            "line 2: the mock-up of fleet shows \"Release\", which no literal of the UI holds",
            "line 3: the mock-up of fleet shows \"Delete everything\", which no literal of the UI holds",
            "line 4: a second mock-up of holds (first on line 4)",
            "line 5: a mock-up of gone, which is no page of ui/pages",
            "line 6: a second mock-up of fleet (first on line 1)",
            "line 6: a ui-t element is never closed",
            "line 5: a ui-t element outside every mock-up",
            "the page audit of ui/pages has no mock-up",
        ]
    );
    assert_eq!(
        ui_problems("<div data-ui-page=\"audit\">", &ui),
        vec![
            "line 1: the mock-up of audit is never closed",
            "the page audit of ui/pages has no mock-up",
            "the page fleet of ui/pages has no mock-up",
            "the page holds of ui/pages has no mock-up",
        ]
    );
}

#[test]
fn ui_literals_are_read_as_javascript_writes_them() {
    // Catches: an apostrophe in a comment opening a string, an escaped quote ending one, a
    // template literal's holes read as text or its nested literals missed, and a template of
    // holes alone matching any label; reads an HTML file's texts between its tags.
    let source = "// the operator's page\n/* it's */ const a = 'it\\'s';\n\
        const b = `Hold ${x ? \"a\" : `b ${y}`} at ${t}`; const c = \"x\\ny\";\n\
        const d = `${p}${q}`; const e = `plain`; const f = { g: 1 };\n";
    let found = js_literals(source);
    assert_eq!(
        found,
        vec![
            Literal::Plain("it's".to_owned()),
            Literal::Plain("a".to_owned()),
            Literal::Template(vec!["b ".to_owned(), String::new()]),
            Literal::Template(vec!["Hold ".to_owned(), " at ".to_owned(), String::new()]),
            Literal::Plain("x y".to_owned()),
            Literal::Template(vec![String::new(), String::new(), String::new()]),
            Literal::Plain("plain".to_owned()),
        ]
    );
    let ui = Ui {
        pages: [("p".to_owned(), found)].into_iter().collect(),
        shared: html_literals("<title> hostkeeper  app </title><p>Sign in</p>"),
    };
    let text = "<div data-ui-page=\"p\"><i class=\"ui-t\">Hold a at b</i><i class=\"ui-t\">hostkeeper app</i>\n\
        <i class=\"ui-t\">Sign in</i><i class=\"ui-t\">anything</i><i class=\"ui-t\">Hold a</i></div>\n";
    assert_eq!(
        ui_problems(text, &ui),
        vec![
            "line 2: the mock-up of p shows \"anything\", which no literal of the UI holds",
            "line 2: the mock-up of p shows \"Hold a\", which no literal of the UI holds",
        ]
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
        "<!doctype html>\n<html lang=\"en\">\n<head><meta charset=\"utf-8\"><title>Report</title>\n\
         <style>body{margin:0 16px}</style></head><body>\n\
         <h2 id=\"top\">Report</h2>\n\
         <p>Renewed at one over <span data-cell=\"c-half\" data-source=\"src/lib.rs\" data-item=\"HALF\">2</span> of life (<a href=\"https://github.com/komira-ai/hostkeeper/blob/main/src/lib.rs\">code</a>, <a href=\"#top\">top</a>). Hosts.Approve. A <span data-cell=\"x-t\" data-cite=\"https://a.example/doc\">60 s</span> timeout.</p>\n",
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
    text.push_str("<figure data-figure=\"problem\"><svg viewBox=\"0 0 10 10\" aria-label=\"drift\"><text x=\"1\" y=\"2\">node-1</text></svg></figure>\n");
    text.push_str("<figure data-figure=\"problem\"><svg viewBox=\"0 0 10 10\"></svg></figure>\n");
    text.push_str("<figure><svg data-diagram=\"update\" viewBox=\"0 0 10 10\"><text x=\"1\">prepare</text></svg></figure>\n");
    text.push_str("<div data-ui-page=\"fleet\"><h3 class=\"ui-t\">Fleet</h3></div>\n");
    text.push_str("<section id=\"sources\"><a href=\"https://a.example/doc\">a doc</a></section>\n</body></html>\n");
    let tracked: BTreeSet<String> = ["src/lib.rs"].iter().map(|s| (*s).to_owned()).collect();
    let known = rpcs(["service Hosts {\n rpc Approve(A) returns (B);\n}\n"]);
    let read = |_: &str| None;
    let steps = vec!["prepare".to_owned()];
    let ui = ui(&[("fleet", "h(\"h2\", {}, \"Fleet\")")], "");
    let world = World {
        root: &dir,
        results: &dir.join("results"),
        exists: &exists(&tracked),
        rpcs: &known,
        steps: &steps,
        read: &read,
        ui: &ui,
    };
    let (found, problems) = check(&text, &world);
    assert_eq!(problems, Vec::<String>::new());
    assert_eq!(found.len(), crate::MEASUREMENTS.len() + 2);
}
