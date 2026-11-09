//! Tests of the signers file: the checks on the file, and on its text.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

use hostkeeper_testkit::Scratch;

use super::*;

const A: &str = "3d2a0000000000000000000000000000000000000000000000000000000000aa";
const B: &str = "00000000000000000000000000000000000000000000000000000000000000bb";

/// A scratch directory under the target directory's `tmp` (a unit test has no
/// `CARGO_TARGET_TMPDIR`): the test binary lives in `<target>/<profile>/deps`.
fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().unwrap();
    let target = exe.ancestors().nth(3).unwrap();
    Scratch::under(&target.join("tmp"), name).unwrap()
}

fn owner(dir: &Scratch) -> u32 {
    fs::metadata(dir.path()).unwrap().uid()
}

/// Writes `bytes` to `signers.toml` of `dir` with `mode`, and loads it as its owner.
fn load_with(dir: &Scratch, bytes: &[u8], mode: u32) -> Result<Option<Signers>, SignersError> {
    let path = dir.path().join("signers.toml");
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    load(&path, owner(dir))
}

fn text_of(list: &[&str]) -> String {
    let quoted: Vec<String> = list.iter().map(|s| format!("\"{s}\"")).collect();
    format!("signers = [{}]\n", quoted.join(", "))
}

#[test]
fn a_missing_file_is_no_error_and_no_signers() {
    let dir = scratch("sigmissing");
    assert_eq!(load(&dir.path().join("none.toml"), owner(&dir)), Ok(None));
}

#[test]
fn a_file_of_one_signer_loads() {
    let dir = scratch("sigone");
    let signers = load_with(&dir, text_of(&[A]).as_bytes(), 0o644)
        .unwrap()
        .unwrap();
    assert_eq!(signers.fingerprints().len(), 1);
    assert_eq!(signers.fingerprints()[0].as_str(), A);
}

#[test]
fn four_signers_load_in_the_files_order() {
    let dir = scratch("sigfour");
    let c = "c".repeat(64);
    let d = "d".repeat(64);
    let signers = load_with(&dir, text_of(&[A, B, &c, &d]).as_bytes(), 0o644)
        .unwrap()
        .unwrap();
    let got: Vec<&str> = signers
        .fingerprints()
        .iter()
        .map(Fingerprint::as_str)
        .collect();
    assert_eq!(got, [A, B, c.as_str(), d.as_str()]);
}

/// Catches: a fifth signer admitted, widening who may sign for the host.
#[test]
fn five_signers_are_refused() {
    let dir = scratch("sigfive");
    let list = [A, B, A, B, A];
    assert_eq!(
        load_with(&dir, text_of(&list).as_bytes(), 0o644),
        Err(SignersError::TooMany(5))
    );
}

#[test]
fn a_file_with_no_signer_is_refused() {
    let dir = scratch("signone");
    assert_eq!(
        load_with(&dir, b"signers = []\n", 0o644),
        Err(SignersError::NoSigner)
    );
}

#[test]
fn an_upper_case_signer_is_accepted_and_lowered() {
    let dir = scratch("sigupper");
    let upper = A.to_ascii_uppercase();
    let signers = load_with(&dir, text_of(&[&upper]).as_bytes(), 0o644)
        .unwrap()
        .unwrap();
    assert_eq!(signers.fingerprints()[0].as_str(), A);
    assert!(signers.names(&Fingerprint::parse(A).unwrap()));
}

#[test]
fn names_is_false_for_a_fingerprint_that_is_not_listed() {
    let signers = parse(text_of(&[A]).as_bytes()).unwrap();
    assert!(!signers.names(&Fingerprint::parse(B).unwrap()));
}

/// Catches: a signer of 63 digits padded or accepted by prefix.
#[test]
fn a_signer_of_63_hex_digits_is_refused() {
    let short = &A[..63];
    assert_eq!(
        parse(text_of(&[short]).as_bytes()),
        Err(SignersError::BadSigner(short.to_owned()))
    );
}

#[test]
fn a_signer_of_65_hex_digits_is_refused() {
    let long = format!("{A}0");
    assert_eq!(
        parse(text_of(&[&long]).as_bytes()),
        Err(SignersError::BadSigner(long))
    );
}

/// Catches: a letter past `f` accepted as a hex digit.
#[test]
fn a_signer_with_a_letter_past_f_is_refused() {
    let bad = format!("g{}", &A[1..]);
    assert_eq!(
        parse(text_of(&[&bad]).as_bytes()),
        Err(SignersError::BadSigner(bad))
    );
}

/// Catches: a file another account owns trusted, so that account chooses the signers.
#[test]
fn a_file_not_owned_by_the_owner_uid_is_refused() {
    let dir = scratch("sigowner");
    let path = dir.path().join("signers.toml");
    fs::write(&path, text_of(&[A])).unwrap();
    let mine = owner(&dir);
    assert_eq!(
        load(&path, mine + 1),
        Err(SignersError::WrongOwner {
            found: mine,
            expected: mine + 1
        })
    );
}

/// Catches: a group-writable file trusted.
#[test]
fn a_file_writable_by_its_group_is_refused() {
    let dir = scratch("siggroup");
    assert_eq!(
        load_with(&dir, text_of(&[A]).as_bytes(), 0o664),
        Err(SignersError::WritableByOthers { found: 0o664 })
    );
}

/// Catches: a world-writable file trusted.
#[test]
fn a_file_writable_by_others_is_refused() {
    let dir = scratch("sigothers");
    assert_eq!(
        load_with(&dir, text_of(&[A]).as_bytes(), 0o646),
        Err(SignersError::WritableByOthers { found: 0o646 })
    );
}

/// Catches: a link followed to a file elsewhere.
#[test]
fn a_link_is_refused() {
    let dir = scratch("siglink");
    let real = dir.path().join("real.toml");
    fs::write(&real, text_of(&[A])).unwrap();
    let link = dir.path().join("signers.toml");
    symlink(&real, &link).unwrap();
    assert_eq!(load(&link, owner(&dir)), Err(SignersError::Link));
}

#[test]
fn a_directory_is_refused_as_no_regular_file() {
    let dir = scratch("sigdir");
    let path = dir.path().join("signers.toml");
    fs::create_dir(&path).unwrap();
    assert_eq!(load(&path, owner(&dir)), Err(SignersError::NotRegular));
}

/// Catches: a file larger than the cap read whole.
#[test]
fn a_file_over_the_size_cap_is_refused_and_one_at_the_cap_is_read() {
    let dir = scratch("sigbig");
    let mut big = text_of(&[A]).into_bytes();
    big.resize(MAX_SIGNERS_BYTES as usize + 1, b' ');
    assert_eq!(load_with(&dir, &big, 0o644), Err(SignersError::TooLarge));
    big.pop();
    assert!(load_with(&dir, &big, 0o644).unwrap().is_some());
}

#[test]
fn a_file_that_is_not_utf8_is_refused() {
    let dir = scratch("sigutf");
    assert_eq!(
        load_with(&dir, b"signers = [\"\xff\"]", 0o644),
        Err(SignersError::Syntax("not UTF-8 text".into()))
    );
}

/// Catches: an unknown key ignored, so a misspelt `signers` starts a host with none.
#[test]
fn an_unknown_key_is_refused() {
    let text = format!("{}extra = 1\n", text_of(&[A]));
    assert!(matches!(
        parse(text.as_bytes()),
        Err(SignersError::Syntax(_))
    ));
}

#[test]
fn text_that_is_not_toml_is_refused() {
    assert!(matches!(
        parse(b"signers = ["),
        Err(SignersError::Syntax(_))
    ));
}

#[test]
fn a_file_without_the_signers_key_is_refused() {
    assert!(matches!(parse(b"\n"), Err(SignersError::Syntax(_))));
}

#[test]
fn a_file_that_cannot_be_opened_is_unreadable() {
    let dir = scratch("sigunread");
    let path = dir.path().join("file.toml/below");
    fs::write(dir.path().join("file.toml"), "x").unwrap();
    assert!(matches!(
        load(&path, owner(&dir)),
        Err(SignersError::Unreadable(_))
    ));
}

#[test]
fn every_refusal_reads_as_a_sentence_about_the_file() {
    let texts = [
        (SignersError::Link, "is a link"),
        (SignersError::NotRegular, "is not a regular file"),
        (
            SignersError::WrongOwner {
                found: 5,
                expected: 0,
            },
            "is owned by uid 5, not 0",
        ),
        (
            SignersError::WritableByOthers { found: 0o666 },
            "has mode 666, which a group or others can write",
        ),
        (SignersError::NoSigner, "names no signer"),
        (SignersError::TooMany(5), "names 5 signers, more than 4"),
    ];
    for (error, text) in texts {
        assert_eq!(error.to_string(), text);
    }
}

/// Catches: an I/O error on the open file (its metadata or its read) reported as anything
/// other than `Unreadable` with the error's text.
#[test]
fn an_io_error_on_the_open_file_is_unreadable_with_its_text() {
    let error = SignersError::from(std::io::Error::other("the disk went away"));
    assert_eq!(error, SignersError::Unreadable("the disk went away".into()));
    assert_eq!(error.to_string(), "cannot be read: the disk went away");
}
