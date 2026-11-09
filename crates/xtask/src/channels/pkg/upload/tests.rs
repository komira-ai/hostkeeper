use super::*;
use crate::channels::pkg::testing::*;
use crate::channels::sums;

const PREFIX: &str = "pkg/hostkeeper/1.2.3/";

fn keys() -> Vec<String> {
    let mut keys: Vec<String> = names().iter().map(|n| format!("{PREFIX}{n}")).collect();
    keys.push(format!("{PREFIX}SHA256SUMS"));
    keys
}

#[test]
fn the_first_upload_puts_each_installer_its_bundle_and_the_sums_once_each_and_no_pointer() {
    // An upload signs nothing: what it writes is the testing directory of a version and nothing
    // that a user following the stable pointer would see.
    let f = Fixture::new("first");
    let aws = FakeAws::new();
    let said = upload(&aws, &f.request("")).expect("uploaded");
    assert_eq!(aws.keys_put(), keys());
    assert_eq!(
        said,
        keys()
            .iter()
            .map(|k| format!("{k}: written"))
            .collect::<Vec<_>>()
    );
    assert_eq!(aws.get("pkg/hostkeeper/stable.json"), None);
    for call in aws.calls_of("put-object") {
        let key = flag(&call, "--key").expect("key");
        assert_eq!(
            flag(&call, "--if-none-match").as_deref(),
            Some("*"),
            "{key}: every write is conditional"
        );
        assert_eq!(flag(&call, "--if-match"), None, "{key}");
        assert_eq!(flag(&call, "--bucket").as_deref(), Some(BUCKET));
        assert_eq!(
            flag(&call, "--cache-control").as_deref(),
            Some("public, max-age=31536000, immutable"),
            "{key}"
        );
    }
    let types: Vec<String> = aws
        .calls_of("put-object")
        .iter()
        .map(|c| flag(c, "--content-type").expect("type"))
        .collect();
    assert_eq!(
        types,
        [
            "application/octet-stream",
            "application/json",
            "application/octet-stream",
            "application/json",
            "text/plain; charset=utf-8"
        ]
    );
}

#[test]
fn the_sums_of_the_directory_list_exactly_the_installers_and_their_bundles() {
    let f = Fixture::new("contents");
    let aws = FakeAws::new();
    upload(&aws, &f.request("")).expect("uploaded");
    let text =
        String::from_utf8(aws.get(&format!("{PREFIX}SHA256SUMS")).expect("sums")).expect("text");
    let lines = sums::parse(&text).expect("parsed");
    let listed: Vec<&str> = lines.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(listed, names());
    assert_eq!(
        lines[0].sha256,
        Sha256::of(b"bytes of hello-1.2.3-macos-arm64.pkg")
    );
    assert_eq!(
        lines[1].sha256,
        Sha256::of(b"bundle of hello-1.2.3-macos-arm64.pkg")
    );
    assert_eq!(
        lines[2].sha256,
        Sha256::of(b"bytes of hostkeeper-1.2.3-macos-arm64.pkg")
    );
    assert_eq!(
        lines[3].sha256,
        Sha256::of(b"bundle of hostkeeper-1.2.3-macos-arm64.pkg")
    );
}

#[test]
fn a_rerun_with_the_same_bytes_passes_and_writes_nothing_new() {
    let f = Fixture::new("rerun");
    let aws = FakeAws::new();
    upload(&aws, &f.request("")).expect("first");
    let before = aws.store.borrow().clone();
    let said = upload(&aws, &f.request("")).expect("second");
    assert_eq!(*aws.store.borrow(), before);
    assert_eq!(
        said,
        keys()
            .iter()
            .map(|k| format!("{k}: already there with the same bytes"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_different_file_at_a_versions_key_fails_and_is_never_written_over() {
    let f = Fixture::new("different");
    let aws = FakeAws::new();
    let key = format!("{PREFIX}hostkeeper-1.2.3-macos-arm64.pkg");
    aws.put(&key, b"other bytes");
    let Error::Failed(why) = upload(&aws, &f.request("")).unwrap_err() else {
        panic!("failed")
    };
    assert!(
        why.starts_with(&format!("{key} exists with other bytes (sha256 ")),
        "{why}"
    );
    assert!(
        why.ends_with("a file of a version is never replaced"),
        "{why}"
    );
    assert_eq!(aws.get(&key).expect("kept"), b"other bytes");
    // The sums of the directory are not put after a file that does not hold.
    assert_eq!(aws.get(&format!("{PREFIX}SHA256SUMS")), None);
}

#[test]
fn a_412_whose_file_cannot_be_read_back_fails() {
    let f = Fixture::new("unreadable");
    let aws = FakeAws::new();
    let key = format!("{PREFIX}hostkeeper-1.2.3-macos-arm64.pkg.sigstore.json");
    aws.put(&key, b"x");
    aws.refuse_when(&format!("get-object --bucket {BUCKET} --key {key}"), DENIED);
    assert_eq!(
        upload(&aws, &f.request("")).unwrap_err(),
        Error::Failed(format!(
            "{key} exists, and reading it back failed: {DENIED}"
        ))
    );
}

#[test]
fn a_refusal_that_is_not_a_412_fails_with_the_answer_and_stops() {
    let f = Fixture::new("denied");
    let aws = FakeAws::new();
    aws.refuse_when("macos-arm64.pkg --body", "Access Denied\n");
    let e = upload(&aws, &f.request("")).unwrap_err();
    assert_eq!(
        e,
        Error::Failed(format!(
            "put {PREFIX}hello-1.2.3-macos-arm64.pkg: Access Denied"
        ))
    );
    // The first put refused, nothing after it is tried.
    assert_eq!(
        aws.keys_put(),
        [format!("{PREFIX}hello-1.2.3-macos-arm64.pkg")]
    );
}

#[test]
fn a_release_whose_files_are_not_the_sums_is_refused_before_anything_is_put() {
    let f = Fixture::new("sums");
    std::fs::write(
        f.release.join("hostkeeper-1.2.3-macos-arm64.pkg"),
        "changed",
    )
    .expect("change");
    let aws = FakeAws::new();
    assert!(matches!(
        upload(&aws, &f.request("")),
        Err(Error::Refused(_))
    ));
    assert!(aws.calls.borrow().is_empty());
}

#[test]
fn flags_out_of_their_form_are_usage_errors_before_anything_is_read() {
    let f = Fixture::new("usage");
    let aws = FakeAws::new();
    let mut request = f.request("");
    request.tag = "1.2.3";
    assert!(matches!(upload(&aws, &request), Err(Error::Usage(_))));
    assert!(aws.calls.borrow().is_empty());
}
