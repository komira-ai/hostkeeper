//! The ways a number, a cell or a figure could slip past the rules, each shown refused.

use crate::cells::{
    Kind, cells, open_problems, result_problems, source_rule_problems, stray_open_problems,
};
use crate::html::{quantities, tags};
use crate::markdown::anchors;
use crate::page::{glyph_problems, loose_numbers, mask_code};
use crate::testdir::TestDir;

/// The numbers `loose_numbers` finds in `text`, read as the check reads it.
fn loose(text: &str) -> Vec<String> {
    let masked = mask_code(text);
    let (found, problems) = cells(&masked);
    assert!(problems.is_empty(), "{problems:?}");
    loose_numbers(&masked, &found)
}

#[test]
fn an_unclosed_comment_or_code_element_hides_nothing() {
    // Catches: a stray `<!--` blanking the rest of the report, and a `<code>` or `<pre>` never
    // closed, either of which would hide every number and cell after it; a comment and a code
    // element that close are still masked, and a `<!--` inside code opens no comment.
    assert_eq!(
        loose("a<!-- never closed 66\nmore 67\n"),
        vec![
            "line 1: the number 66 stands outside a cell",
            "line 2: the number 67 stands outside a cell",
        ]
    );
    assert_eq!(
        loose("a <code>stray\n\nhosts 44 here\n<pre>never 45\n"),
        vec![
            "line 3: the number 44 stands outside a cell",
            "line 4: the number 45 stands outside a cell",
        ]
    );
    assert_eq!(
        loose("a <code>code\nspan 9</code> b <!-- 8 -->\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        loose("<code><!--</code> then 77\n"),
        vec!["line 1: the number 77 stands outside a cell"]
    );
}

#[test]
fn code_is_masked_in_any_letter_case_and_when_nested() {
    // Catches: an upper-case `<CODE>` read as prose, a `<code>` inside `<pre>` ending the
    // masking early, and a nested element of the same name ending it at the inner end tag.
    assert_eq!(
        loose("<CODE>12</Code> and <Pre>13</PRE>\n"),
        Vec::<String>::new()
    );
    assert_eq!(
        loose("<pre><code>1</code> 2\n3</pre> then 4\n"),
        vec!["line 2: the number 4 stands outside a cell"]
    );
    assert_eq!(
        loose("<code><code>5</code> 6</code> 7\n"),
        vec!["line 1: the number 7 stands outside a cell"]
    );
}

#[test]
fn a_style_sheet_or_script_hides_no_cell_or_tag_after_it() {
    // Catches: a style sheet or script whose text is read as prose (every margin a number), and
    // one whose end is missed so that what follows it is hidden.
    assert_eq!(
        loose("<style>p{margin:0 16px}</style>\n<script>let a = 1;</script> 8\n"),
        vec!["line 2: the number 8 stands outside a cell"]
    );
    assert_eq!(
        loose("<style>p{margin:0}\n9\n"),
        vec![
            "line 1: the number 0 stands outside a cell",
            "line 2: the number 9 stands outside a cell",
        ]
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
fn a_number_in_a_link_label_or_a_cell_attribute_is_read_as_written() {
    // Catches: a number written as the text of a link, where a citation should name its
    // section in words; a cell's own attributes and a link's target are not prose.
    assert_eq!(
        loose("<a href=\"https://example.com/12\">12 hosts</a> <a href=\"#s3\">ok</a>\n"),
        vec!["line 1: the number 12 stands outside a cell"]
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
        glyph_problems(
            "½ and ² and ① and ５\n&frac12; &amp; &nbsp; &mdash; &dagger; &darr; a & b; c\n"
        ),
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
fn a_cell_inside_code_is_no_cell() {
    // Catches: a cell quoted inside a code example counted as a cell of the report, so a
    // promised measurement could be "present" only as an example.
    let (found, problems) = cells(&mask_code(
        "<pre><span data-cell=\"m-a\" data-open=\"x\">open</span></pre>\n",
    ));
    assert!(found.is_empty());
    assert!(problems.is_empty(), "{problems:?}");
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
