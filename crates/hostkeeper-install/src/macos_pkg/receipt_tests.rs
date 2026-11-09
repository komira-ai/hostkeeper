//! Tests of the reading of receipts and of `pkgutil`'s answers.

use plist::{Dictionary, Value};

use super::*;

fn dict(entries: &[(&str, Value)]) -> Dictionary {
    let mut d = Dictionary::new();
    for (key, value) in entries {
        d.insert((*key).into(), value.clone());
    }
    d
}

fn text(s: &str) -> Value {
    Value::String(s.into())
}

fn xml(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    value.to_writer_xml(&mut out).unwrap();
    out
}

fn binary(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    value.to_writer_binary(&mut out).unwrap();
    out
}

fn info(entries: &[(&str, Value)]) -> Vec<u8> {
    xml(&Value::Dictionary(dict(entries)))
}

#[test]
fn the_pkg_info_of_a_receipt_with_a_volume_and_a_location_joins_them() {
    let out = info(&[
        ("pkgid", text("com.example.hello")),
        ("pkg-version", text("1.2.0")),
        ("volume", text("/")),
        ("install-location", text("Library/Application Support")),
    ]);
    assert_eq!(
        parse_pkg_info(&out),
        Ok(Receipt {
            identifier: "com.example.hello".into(),
            version: "1.2.0".into(),
            prefix: "/Library/Application Support".into(),
        })
    );
}

#[test]
fn the_pkg_info_without_a_volume_is_relative_to_slash() {
    let out = info(&[
        ("pkgid", text("a.b")),
        ("pkg-version", text("1")),
        ("install-location", text("/")),
    ]);
    assert_eq!(parse_pkg_info(&out).unwrap().prefix, "/");
}

#[test]
fn the_pkg_info_on_another_volume_has_that_volume_in_its_prefix() {
    let out = info(&[
        ("pkgid", text("a.b")),
        ("pkg-version", text("1")),
        ("volume", text("/Volumes/Data")),
        ("install-location", text("opt")),
    ]);
    let receipt = parse_pkg_info(&out).unwrap();
    assert_eq!(receipt.prefix, "/Volumes/Data/opt");
    assert_eq!(receipt.path_of("x/./y"), "/Volumes/Data/opt/x/y");
}

#[test]
fn the_pkg_info_missing_a_key_names_the_key() {
    let full = [
        ("pkgid", text("a.b")),
        ("pkg-version", text("1")),
        ("install-location", text("/")),
    ];
    for skip in 0..full.len() {
        let kept: Vec<(&str, Value)> = full
            .iter()
            .enumerate()
            .filter(|(at, _)| *at != skip)
            .map(|(_, e)| e.clone())
            .collect();
        assert_eq!(
            parse_pkg_info(&info(&kept)),
            Err(ReceiptError::Missing(full[skip].0))
        );
    }
}

#[test]
fn the_pkg_info_with_a_key_that_is_not_text_is_missing_it() {
    let out = info(&[
        ("pkgid", Value::Integer(5.into())),
        ("pkg-version", text("1")),
        ("install-location", text("/")),
    ]);
    assert_eq!(parse_pkg_info(&out), Err(ReceiptError::Missing("pkgid")));
}

#[test]
fn an_answer_that_is_not_a_plist_or_not_a_dictionary_is_refused() {
    assert!(matches!(
        parse_pkg_info(b"No receipt"),
        Err(ReceiptError::Plist(_))
    ));
    assert_eq!(
        parse_pkg_info(&xml(&Value::Array(vec![]))),
        Err(ReceiptError::Plist("not a dictionary".into()))
    );
    assert!(matches!(
        parse_receipt_file(b""),
        Err(ReceiptError::Plist(_))
    ));
    assert!(matches!(parse_file_info(b"x"), Err(ReceiptError::Plist(_))));
}

#[test]
fn a_receipt_file_is_read_in_binary_and_in_xml() {
    let value = Value::Dictionary(dict(&[
        ("PackageIdentifier", text("com.example.hello")),
        ("PackageVersion", text("1.2.0")),
        ("InstallPrefixPath", text("Library")),
    ]));
    let want = Receipt {
        identifier: "com.example.hello".into(),
        version: "1.2.0".into(),
        prefix: "/Library".into(),
    };
    assert_eq!(parse_receipt_file(&binary(&value)), Ok(want.clone()));
    assert_eq!(parse_receipt_file(&xml(&value)), Ok(want));
}

#[test]
fn a_receipt_file_without_a_prefix_installs_at_slash() {
    let value = Value::Dictionary(dict(&[
        ("PackageIdentifier", text("a.b")),
        ("PackageVersion", text("1")),
    ]));
    assert_eq!(parse_receipt_file(&binary(&value)).unwrap().prefix, "/");
}

#[test]
fn a_receipt_file_missing_its_identifier_or_version_names_it() {
    let no_id = Value::Dictionary(dict(&[("PackageVersion", text("1"))]));
    assert_eq!(
        parse_receipt_file(&xml(&no_id)),
        Err(ReceiptError::Missing("PackageIdentifier"))
    );
    let no_version = Value::Dictionary(dict(&[("PackageIdentifier", text("a"))]));
    assert_eq!(
        parse_receipt_file(&xml(&no_version)),
        Err(ReceiptError::Missing("PackageVersion"))
    );
}

fn path_info(pkgids: &[&str]) -> Value {
    Value::Array(
        pkgids
            .iter()
            .map(|id| Value::Dictionary(dict(&[("pkgid", text(id))])))
            .collect(),
    )
}

#[test]
fn file_info_lists_the_pkgid_of_each_receipt() {
    let out = info(&[
        ("path", text("/Library/x")),
        ("path-info", path_info(&["a.b", "c.d"])),
    ]);
    assert_eq!(parse_file_info(&out), Ok(vec!["a.b".into(), "c.d".into()]));
}

#[test]
fn file_info_without_path_info_has_no_receipt() {
    let out = info(&[("path", text("/Library/x"))]);
    assert_eq!(parse_file_info(&out), Ok(Vec::new()));
    assert_eq!(
        parse_file_info(&info(&[("path-info", path_info(&[]))])),
        Ok(Vec::new())
    );
}

/// Catches: an unreadable `path-info` read as "no receipt lists the file", so a conflict passes.
#[test]
fn file_info_with_a_path_info_that_is_not_an_array_is_refused() {
    let out = info(&[("path-info", text("x"))]);
    assert_eq!(
        parse_file_info(&out),
        Err(ReceiptError::Missing("path-info array"))
    );
}

#[test]
fn file_info_with_an_entry_lacking_a_pkgid_is_refused() {
    let entries = Value::Array(vec![
        Value::Dictionary(dict(&[("pkgid", text("a.b"))])),
        Value::Dictionary(dict(&[("other", text("x"))])),
    ]);
    assert_eq!(
        parse_file_info(&info(&[("path-info", entries)])),
        Err(ReceiptError::Missing("pkgid"))
    );
    let not_dict = Value::Array(vec![text("x")]);
    assert_eq!(
        parse_file_info(&info(&[("path-info", not_dict)])),
        Err(ReceiptError::Missing("pkgid"))
    );
}

#[test]
fn parse_files_gives_one_path_a_line_and_skips_blank_lines() {
    assert_eq!(
        parse_files(b"a/b\n\nc\n", false),
        Ok(vec!["a/b".into(), "c".into()])
    );
    assert_eq!(parse_files(b"", false), Ok(Vec::new()));
}

/// Catches: a listing cut at the output cap read as the whole list, so files are left behind.
#[test]
fn parse_files_of_a_cut_listing_is_refused() {
    assert_eq!(parse_files(b"a\n", true), Err(ReceiptError::Cut));
}

#[test]
fn join_makes_one_absolute_path_and_drops_empty_and_dot_parts() {
    assert_eq!(join("/", ""), "/");
    assert_eq!(join("/", "a/b"), "/a/b");
    assert_eq!(join("/Library//", "./x//y"), "/Library/x/y");
    assert_eq!(join("", "x"), "/x");
}

#[test]
fn the_pkgutil_arguments_and_the_receipt_file_path() {
    assert_eq!(pkg_info_args("a.b"), ["--pkg-info-plist", "a.b"]);
    assert_eq!(file_info_args("/x"), ["--file-info-plist", "/x"]);
    assert_eq!(files_args("a.b"), ["--only-files", "--files", "a.b"]);
    assert_eq!(forget_args("a.b"), ["--forget", "a.b"]);
    assert_eq!(receipt_file("a.b"), "/var/db/receipts/a.b.plist");
}
