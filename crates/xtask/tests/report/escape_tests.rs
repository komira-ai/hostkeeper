//! The ways a number, a cell or a diagram could slip past the rules, each shown refused.

use std::collections::BTreeSet;

use crate::cells::{
    Kind, cells, open_problems, result_problems, source_rule_problems, stray_open_problems,
};
use crate::html::{quantities, tags};
use crate::markdown::{anchors, glyph_problems, loose_numbers, mask_code};
use crate::testdir::TestDir;
use crate::{diagram_problems, rpcs};

/// The numbers `loose_numbers` finds in `text`, read as the check reads it.
fn loose(text: &str) -> Vec<String> {
    let masked = mask_code(text);
    let (found, problems) = cells(&masked);
    assert!(problems.is_empty(), "{problems:?}");
    loose_numbers(&masked, &found)
}

#[test]
fn an_unclosed_comment_or_code_span_hides_nothing() {
    // Catches: a stray `<!--` (or one quoted in a code span) blanking the rest of the report, and
    // a lone backtick pairing with one paragraphs later, either of which would hide every
    // number and cell after it.
    assert_eq!(
        loose("see `<!--` here\n\nhosts 77 and"),
        vec!["line 3: the number 77 stands outside a cell"]
    );
    assert_eq!(
        loose("a<!-- never closed 66\nmore 67\n"),
        vec![
            "line 1: the number 66 stands outside a cell",
            "line 2: the number 67 stands outside a cell",
        ]
    );
    assert_eq!(
        loose("a ` stray\n\nhosts 44 here\n\nb ` close\n"),
        vec!["line 3: the number 44 stands outside a cell"]
    );
    assert_eq!(
        loose("a `code\nspan 9` b <!-- 8 -->\n"),
        Vec::<String>::new()
    );
}

#[test]
fn a_tag_never_spans_lines_so_prose_is_not_swallowed() {
    // Catches: `x<a` followed by an apostrophe taken for a tag whose quote runs on, which hid the
    // numbers of the following lines; a real tag on one line is still read.
    assert_eq!(
        loose("if n<a don't\nrun 88 hosts\n\nit's > ok 99\n"),
        vec![
            "line 2: the number 88 stands outside a cell",
            "line 4: the number 99 stands outside a cell",
        ]
    );
    assert_eq!(tags("a <b class=\"x\">c</b>").len(), 2);
    assert!(tags("a <b class=\"x\nd\">c").is_empty());
}

#[test]
fn a_number_in_an_attribute_or_a_link_label_is_prose() {
    // Catches: a number written in a `title` or `alt` attribute, or as the text of a link to a
    // file of the repository, where no reader of the cells looks; a cell's own attributes are
    // not prose.
    assert_eq!(
        loose("<span title=\"12 ms\">x</span> <img alt=\"13\"> [12 hosts](../a.md) [ok](#s3)\n"),
        vec![
            "line 1: the number 12 stands in the attribute title outside a cell",
            "line 1: the number 13 stands in the attribute alt outside a cell",
            "line 1: the number 12 stands outside a cell",
        ]
    );
    assert_eq!(
        loose("<span data-cell=\"c-x2\" data-open=\"run 2\">open</span>\n"),
        Vec::<String>::new()
    );
}

#[test]
fn a_number_in_other_than_ascii_digits_is_refused() {
    // Catches: a fraction, a superscript, a circled or a fullwidth digit, and a named entity such
    // as `&frac12;`, none of which the number rules read; an open cell printing one is refused
    // too. The entities the report uses pass.
    assert_eq!(
        glyph_problems("½ and ² and ① and ５\n&frac12; &amp; &nbsp; &mdash; a & b; c\n"),
        vec![
            "line 1: '½' is a number not written in ASCII digits",
            "line 1: '²' is a number not written in ASCII digits",
            "line 1: '①' is a number not written in ASCII digits",
            "line 1: '５' is a number not written in ASCII digits",
            "line 2: the character reference &frac12; is not one the report may use",
        ]
    );
    let (found, _) = cells("<span data-cell=\"m-a\" data-open=\"x\">open, ½</span>");
    assert_eq!(
        open_problems(&found),
        vec!["line 1: m-a: an open cell prints a digit: \"open, ½\""]
    );
}

#[test]
fn a_measured_number_is_printed_in_its_rows_unit() {
    // Catches: a value printed in another unit than its row's (`12.8 ms` for a row in seconds)
    // or with none; a row with no unit takes the number alone.
    let dir = TestDir::under(
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
        "report-unit",
    );
    std::fs::write(
        dir.join("r.tsv"),
        "raw\tt\t12.8\ts\tm\ta,b,c\nraw\tn\t6\t\tm\ta\n",
    )
    .expect("write");
    let text = "<span data-cell=\"a\" data-result=\"r.tsv\">12.8 s on 6 hosts</span>\n\
        <span data-cell=\"b\" data-result=\"r.tsv\">12.8 ms</span>\n\
        <span data-cell=\"c\" data-result=\"r.tsv\">took 12.8</span>\n";
    let (found, _) = cells(text);
    assert_eq!(
        result_problems(&found, &dir),
        vec![
            "line 2: b: the number 12.8 is printed in ms, its row t is in s",
            "line 3: c: the number 12.8 is printed in no unit, its row t is in s",
        ]
    );
    assert_eq!(
        quantities("45 s, 16\u{a0}KiB and 3"),
        vec![
            (45.0, Some("s".to_owned())),
            (16.0, Some("KiB".to_owned())),
            (3.0, None)
        ]
    );
}

#[test]
fn only_a_measurement_may_be_open_and_some_cell_reads_the_code() {
    // Catches: a claim other than a promised measurement left open, and a report whose cells all
    // left the code, so the rule of code cells would check nothing.
    let text = "<span data-cell=\"m-a\" data-open=\"x\">open</span>\n\
        <span data-cell=\"c-b\" data-open=\"x\">open</span>\n";
    let (found, _) = cells(text);
    assert_eq!(
        stray_open_problems(&found, &["m-a"]),
        vec!["line 2: c-b is open but is not a measurement"]
    );
    assert_eq!(
        source_rule_problems(&found),
        vec!["no cell reads its value from the code"]
    );
    let (with_code, _) =
        cells("<span data-cell=\"c\" data-source=\"a.rs\" data-item=\"A\">1</span>");
    assert!(matches!(with_code[0].kind, Kind::Source { .. }));
    assert!(source_rule_problems(&with_code).is_empty());
}

#[test]
fn every_kind_of_diagram_counts_toward_the_limit() {
    // Catches: a fourth diagram drawn in another fence language, as a Markdown image, an `<img>`
    // or a `<picture>`, which a count of mermaid blocks alone would miss.
    let steps = vec!["prepare".to_owned()];
    let update = "```mermaid\n%% update\nA->>B: prepare\n```\n";
    let text = format!("{update}```dot\ndigraph {{}}\n```\n![a](x.svg)\n<img src=\"y.png\">\n");
    assert_eq!(
        diagram_problems(&text, &mask_code(&text), &BTreeSet::new(), &steps),
        vec!["4 diagrams, more than 3"]
    );
    let code = format!("{update}`![a](x.svg)` and `<img>`\n```text\n<svg>\n```\n");
    assert!(diagram_problems(&code, &mask_code(&code), &rpcs([""]), &steps).is_empty());
}

#[test]
fn anchors_of_repeated_headings_and_linked_headings_follow_github() {
    // Catches: a second heading of the same text given no `-1` suffix (so its anchor is refused
    // and the first one's is taken for it), and a heading holding a link slugged with its target.
    assert_eq!(
        anchors("## Limits\n## Limits\n## Limits\n## See [the code](a/b.rs)\n"),
        vec!["limits", "limits-1", "limits-2", "see-the-code"]
    );
}
