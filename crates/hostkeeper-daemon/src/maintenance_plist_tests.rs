//! The tests of `maintenance_plist`: the dictionary of an XML and of a binary property list, a
//! file that is not there, and each reason a file that is there gives none, over a scratch
//! directory, each asserted by equality.

use std::fs;

use hostkeeper_testkit::Scratch;
use plist::Value;

use super::*;

const XML: &[u8] = include_bytes!("../tests/fixtures/maintenance/plist/version.xml.plist");
const BINARY: &[u8] = include_bytes!("../tests/fixtures/maintenance/plist/version.binary.plist");
const ARRAY: &[u8] = include_bytes!("../tests/fixtures/maintenance/plist/array.xml.plist");

const PATH: &str = "/Library/Managed Preferences/example.plist";

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// A host whose file [`PATH`] holds `bytes`.
fn host_with(name: &str, bytes: &[u8]) -> (Scratch, HostPlists) {
    let scratch = scratch(name);
    let file = scratch.path().join(PATH.trim_start_matches('/'));
    fs::create_dir_all(file.parent().expect("a parent")).expect("the directory");
    fs::write(file, bytes).expect("the file");
    let plists = HostPlists::new(HostRoot::at(scratch.path()));
    (scratch, plists)
}

/// The dictionary the fixtures hold.
fn expected() -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert(
        "CFBundleShortVersionString".to_owned(),
        Value::String("27.2".to_owned()),
    );
    dictionary.insert("Enabled".to_owned(), Value::Boolean(true));
    dictionary.insert("Count".to_owned(), Value::Integer(3_i64.into()));
    dictionary.insert(
        "Names".to_owned(),
        Value::Array(vec![
            Value::String("Deferrals".to_owned()),
            Value::String("CatalogURL".to_owned()),
        ]),
    );
    dictionary
}

/// An XML property list of exactly `size` bytes: the fixture with spaces before its last tags.
fn xml_of_size(size: usize) -> Vec<u8> {
    let text = std::str::from_utf8(XML).expect("text");
    let (head, tail) = text
        .split_once("</dict>")
        .expect("the end of the dictionary");
    let pad = size - XML.len();
    format!("{head}{}</dict>{tail}", " ".repeat(pad)).into_bytes()
}

/// Proves an XML property list and a binary one with the same content give the same dictionary,
/// whole.
#[test]
fn an_xml_and_a_binary_property_list_give_the_same_dictionary() {
    let (_xml_scratch, xml) = host_with("plist-xml", XML);
    let (_binary_scratch, binary) = host_with("plist-binary", BINARY);
    assert_eq!(xml.dictionary(PATH), Ok(Some(expected())));
    assert_eq!(binary.dictionary(PATH), Ok(Some(expected())));
}

/// Proves a file that is not there is `None`, not an error.
#[test]
fn a_file_that_is_not_there_gives_none() {
    let scratch = scratch("plist-absent");
    let plists = HostPlists::new(HostRoot::at(scratch.path()));
    assert_eq!(plists.dictionary(PATH), Ok(None));
}

/// Proves bytes that are not a property list give the reason naming the path alone, with no byte
/// of the file in it.
#[test]
fn bytes_that_are_not_a_property_list_give_a_reason_without_a_byte_of_them() {
    let (_scratch, plists) = host_with("plist-garbage", b"<secret-bytes");
    assert_eq!(
        plists.dictionary(PATH),
        Err(format!("{PATH} is not a property list"))
    );
}

/// Proves a property list whose top is an array is not a dictionary.
#[test]
fn a_property_list_whose_top_is_an_array_is_not_a_dictionary() {
    let (_scratch, plists) = host_with("plist-array", ARRAY);
    assert_eq!(
        plists.dictionary(PATH),
        Err(format!("{PATH} is not a dictionary"))
    );
}

/// Proves a file one byte over the bound is refused as too large, and one of exactly the bound
/// that is a property list is read.
#[test]
fn a_file_over_the_bound_is_refused_and_one_of_exactly_the_bound_is_read() {
    let size = usize::try_from(MAX_PLIST_BYTES).expect("a size");
    let (_over_scratch, over) = host_with("plist-over", &vec![b' '; size + 1]);
    assert_eq!(
        over.dictionary(PATH),
        Err(format!("{PATH} is larger than 1048576 bytes"))
    );
    let (_exact_scratch, exact) = host_with("plist-exact", &xml_of_size(size));
    assert_eq!(exact.dictionary(PATH), Ok(Some(expected())));
}

/// Proves a directory at the path is a read failure naming the path and the operating system's
/// reason.
#[test]
fn a_directory_at_the_path_cannot_be_read() {
    let scratch = scratch("plist-directory");
    fs::create_dir_all(scratch.path().join(PATH.trim_start_matches('/'))).expect("the directory");
    let plists = HostPlists::new(HostRoot::at(scratch.path()));
    let reason = io::Error::from_raw_os_error(libc::EISDIR);
    assert_eq!(
        plists.dictionary(PATH),
        Err(format!("{PATH} could not be read: {reason}"))
    );
}

/// Proves a file whose parent is a file cannot be opened, and the reason names the path and the
/// operating system's reason.
#[test]
fn a_path_under_a_file_cannot_be_opened() {
    let (_scratch, plists) = host_with("plist-under-file", b"");
    let under = format!("{PATH}/inner.plist");
    let reason = io::Error::from_raw_os_error(libc::ENOTDIR);
    assert_eq!(
        plists.dictionary(&under),
        Err(format!("{under} could not be read: {reason}"))
    );
}
