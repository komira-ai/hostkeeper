//! Tests of the small XML reader.

use super::*;

fn one(text: &str) -> Element {
    let mut found = elements(text).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    found.remove(0)
}

#[test]
fn elements_come_in_document_order_with_their_text() {
    let found = elements("<a>one<b/> <c>two</c></a>").unwrap();
    let got: Vec<(&str, &str)> = found
        .iter()
        .map(|e| (e.name.as_str(), e.text.as_str()))
        .collect();
    assert_eq!(got, [("a", "one"), ("b", ""), ("c", "two")]);
}

#[test]
fn attributes_with_either_quote_are_read() {
    let e = one(r#"<pkg-info a="1" b='two' c = "3" />"#);
    assert_eq!(e.name, "pkg-info");
    assert_eq!(e.attribute("a"), Some("1"));
    assert_eq!(e.attribute("b"), Some("two"));
    assert_eq!(e.attribute("c"), Some("3"));
    assert_eq!(e.attribute("d"), None);
}

#[test]
fn a_value_may_hold_the_other_quote_kind() {
    let e = one(r#"<a x="it's" y='say "hi"'/>"#);
    assert_eq!(e.attribute("x"), Some("it's"));
    assert_eq!(e.attribute("y"), Some(r#"say "hi""#));
}

/// Catches: a `>` inside a quoted value ending the tag early.
#[test]
fn a_greater_than_inside_a_quoted_value_does_not_end_the_tag() {
    let e = one(r#"<a x="1 > 0" y='>'>text</a>"#);
    assert_eq!(e.attribute("x"), Some("1 > 0"));
    assert_eq!(e.attribute("y"), Some(">"));
    assert_eq!(e.text, "text");
}

#[test]
fn entities_and_character_references_are_resolved_in_values_and_text() {
    let e = one(r#"<a x="&amp;&lt;&gt;&quot;&apos;&#65;&#x42;">&lt;t&gt; &#x263A;</a>"#);
    assert_eq!(e.attribute("x"), Some("&<>\"'AB"));
    assert_eq!(e.text, "<t> \u{263A}");
}

#[test]
fn comments_processing_instructions_and_end_tags_are_skipped() {
    let found =
        elements("<?xml version=\"1.0\"?><!-- <not a=\"tag\"> --><a><!-- c --></a><?pi x?>")
            .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "a");
}

#[test]
fn cdata_is_text_of_the_element_before_it() {
    let e = one("<a>x<![CDATA[ <raw> & ]]></a>");
    assert_eq!(e.text, "x<raw> &");
}

#[test]
fn cdata_before_any_element_is_dropped() {
    assert!(elements("<![CDATA[x]]>").unwrap().is_empty());
}

#[test]
fn text_with_no_tag_has_no_element() {
    assert!(elements("just text").unwrap().is_empty());
}

/// Catches: a DOCTYPE (a place for entity tricks) read as if it were absent.
#[test]
fn a_doctype_is_refused() {
    assert_eq!(
        elements("<!DOCTYPE a [<!ENTITY e \"x\">]><a/>"),
        Err(XmlError::Doctype)
    );
}

#[test]
fn an_unclosed_tag_comment_cdata_or_instruction_is_refused() {
    for text in [
        "<a b=\"1\"",
        "<a><!-- x",
        "<a><![CDATA[x",
        "<?xml",
        "<a x=\">",
    ] {
        assert_eq!(elements(text), Err(XmlError::Unclosed), "{text}");
    }
}

#[test]
fn a_tag_with_no_name_is_refused() {
    assert_eq!(elements("<  />"), Err(XmlError::NoName));
    assert_eq!(elements("<>"), Err(XmlError::NoName));
}

/// Catches: a malformed attribute skipped, so the document reads as holding less.
#[test]
fn bad_attribute_forms_are_refused() {
    for text in ["<a x>", "<a x=1>", "<a ='1'>", "<a x y='1'>", "<a x=>"] {
        assert!(
            matches!(elements(text), Err(XmlError::BadAttribute(ref n)) if n == "a"),
            "{text}: {:?}",
            elements(text)
        );
    }
}

#[test]
fn an_attribute_named_twice_is_refused() {
    assert_eq!(
        elements("<a x='1' x='2'/>"),
        Err(XmlError::DoubleAttribute("a".into(), "x".into()))
    );
}

#[test]
fn an_unknown_entity_or_a_bad_reference_is_refused() {
    for (text, name) in [
        ("<a>&nbsp;</a>", "nbsp"),
        ("<a x='&foo;'/>", "foo"),
        ("<a>&#xZZ;</a>", "#xZZ"),
        ("<a>&#abc;</a>", "#abc"),
        ("<a>&#xD800;</a>", "#xD800"),
        ("<a>&#;</a>", "#"),
    ] {
        assert_eq!(elements(text), Err(XmlError::Entity(name.into())), "{text}");
    }
}

#[test]
fn an_ampersand_with_no_semicolon_is_refused() {
    assert_eq!(elements("<a>a & b</a>"), Err(XmlError::Entity(" b".into())));
}

#[test]
fn every_error_reads_as_a_sentence() {
    assert_eq!(XmlError::Unclosed.to_string(), "a tag is not closed");
    assert_eq!(
        XmlError::DoubleAttribute("a".into(), "x".into()).to_string(),
        "<a> names the attribute x twice"
    );
}

/// Catches: a value whose closing quote is missing read as running to the end of the tag.
#[test]
fn a_start_tag_whose_value_has_no_closing_quote_is_refused() {
    assert_eq!(
        start_tag("a x=\"1").unwrap_err(),
        XmlError::BadAttribute("a".into())
    );
}
