use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, SystemTime};

use hostkeeper_testkit::Scratch;

use super::*;
use crate::testutil::scratch;

/// The Xcode `version` of `build`.
fn id(version: &str, build: &str) -> XcodeId {
    XcodeId {
        version: XcodeVersion::parse(version).expect("a version"),
        build: build.to_owned(),
    }
}

/// A SHA-256 of 64 copies of `digit`: a name the store takes, whatever the bytes under it.
fn sha(digit: char) -> String {
    digit.to_string().repeat(64)
}

/// The archive of `version` and `build` named by `sha(digit)`.
fn archive(version: &str, build: &str, digit: char) -> Archive {
    Archive {
        sha256: sha(digit),
        id: id(version, build),
        size: 5,
        by: "ci-ops".to_owned(),
        at_unix: 1_800_000_000,
    }
}

/// A store taking archives of at most 1024 bytes in a new directory of its own.
fn store(name: &str) -> (Scratch, XcodeStore) {
    let dir = scratch(name);
    let store = XcodeStore::open(dir.path(), 1024).expect("the store opens");
    (dir, store)
}

/// Stores `archive` with the bytes `hello`, as an upload does.
fn put(store: &XcodeStore, archive: &Archive) {
    let (path, mut file) = store.upload_file(&archive.sha256).expect("an upload file");
    file.write_all(b"hello").expect("the bytes");
    drop(file);
    let _held = store.changing();
    store.commit(&path, archive).expect("the commit");
}

/// The permission bits of the file at `path`.
fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("the file").permissions().mode() & 0o777
}

/// Proves: a directory that does not exist, and a file in place of one, are each refused with
/// the path first in the text.
///
/// Catches: a store that creates the directory it was given, or starts on a file.
#[test]
fn open_refuses_a_missing_directory_and_a_file() {
    let dir = scratch("xcode-store-open-refused");
    let missing = dir.path().join("missing");
    let why = XcodeStore::open(&missing, 1024).expect_err("no directory");
    assert!(
        matches!(&why, StoreError::NotADirectory { path, .. } if *path == missing),
        "{why}"
    );
    assert!(
        why.to_string()
            .starts_with(&format!("{}: not a directory", missing.display())),
        "{why}"
    );
    assert!(!missing.exists(), "the store made the directory");
    let file = dir.path().join("file");
    fs::write(&file, b"x").expect("a file");
    let why = XcodeStore::open(&file, 1024).expect_err("a file");
    assert!(
        matches!(&why, StoreError::NotADirectory { path, .. } if *path == file),
        "{why}"
    );
}

/// Makes the last write of the file or directory at `path` `age` ago.
fn written_ago(path: &Path, age: Duration) {
    fs::File::open(path)
        .expect("the path opens")
        .set_modified(SystemTime::now() - age)
        .expect("the time is set");
}

/// Proves: opening removes the upload files whose last write is older than the store's bound (a
/// name starting `.upload-`) and nothing else, a fresh one (another coordinator's upload in
/// progress) and a file that is not an upload stay, and the store reports the limit it was given.
///
/// Catches: a half-written upload that stays on the disk for good, a start that removes an upload
/// another coordinator of the directory is writing, and a start that removes an archive.
#[test]
fn open_removes_a_stale_upload_and_nothing_else() {
    let dir = scratch("xcode-store-open-left");
    let left = dir.path().join(format!(".upload-{}-aa-0", sha('a')));
    fs::write(&left, b"half").expect("a left upload");
    written_ago(&left, STALE_UPLOAD + Duration::from_secs(60));
    let fresh = dir.path().join(format!(".upload-{}-bb-0", sha('a')));
    fs::write(&fresh, b"going").expect("a fresh upload");
    let other = dir.path().join("notes.txt");
    fs::write(&other, b"keep").expect("another file");
    let store = XcodeStore::open(dir.path(), 4242).expect("the store opens");
    assert!(!left.exists(), "the left upload stands");
    assert!(fresh.exists(), "an upload in progress was removed");
    assert!(other.exists(), "an unrelated file was removed");
    assert_eq!(store.max_bytes(), 4242);
    assert_eq!(store.archives().expect("a listing"), vec![]);
}

/// Proves: the upload files of two stores opened on one directory never share a name, even for
/// one digest and one count, and a store that cannot draw random bytes does not open.
///
/// Catches: two coordinators that mount one directory (each its own process, possibly both pid 1)
/// writing one upload file.
#[test]
fn stores_of_one_directory_name_their_uploads_apart() {
    let dir = scratch("xcode-store-upload-token");
    let one = XcodeStore::open(dir.path(), 1024).expect("the first store");
    let two = XcodeStore::open(dir.path(), 1024).expect("the second store");
    let (first, _a) = one.upload_file(&sha('b')).expect("the first");
    let (second, _b) = two.upload_file(&sha('b')).expect("the second");
    assert_ne!(first, second);
    let broken: Random = Box::new(|_| false);
    let why = XcodeStore::open_with(dir.path(), 1024, &broken).expect_err("no random bytes");
    assert!(
        matches!(&why, StoreError::Io { path, .. } if path == dir.path()),
        "{why}"
    );
}

/// Proves: an upload file is made with mode 0600 in the store's directory under a name that
/// starts `.upload-` and holds the digest, two uploads of one digest get two files, and neither
/// is an archive.
///
/// Catches: an archive's bytes readable by other users while they are written, and two uploads
/// of one digest writing to one file.
#[test]
fn an_upload_file_is_private_and_each_one_is_its_own() {
    let (dir, store) = store("xcode-store-upload-file");
    let (first, _a) = store.upload_file(&sha('b')).expect("the first");
    let (second, _b) = store.upload_file(&sha('b')).expect("the second");
    assert_ne!(first, second);
    for path in [&first, &second] {
        assert_eq!(mode(path), 0o600, "{}", path.display());
        assert_eq!(path.parent(), Some(dir.path()));
        let name = path.file_name().expect("a name").to_string_lossy();
        assert!(
            name.starts_with(&format!(".upload-{}-", sha('b'))),
            "{name}"
        );
    }
    assert_eq!(store.archives().expect("a listing"), vec![]);
}

/// Proves: a committed upload is listed and read back as the archive that was committed, its
/// file holds the uploaded bytes, its file and its record have mode 0600, and the upload's own
/// file is gone.
///
/// Catches: a record that loses a field, a committed archive that is not held, and an archive
/// created readable by other users.
#[test]
fn a_committed_archive_is_held_and_read_back_with_private_files() {
    let (dir, store) = store("xcode-store-commit");
    let held = archive("27.2", "27B52", 'a');
    put(&store, &held);
    assert_eq!(store.archives().expect("a listing"), vec![held.clone()]);
    assert_eq!(store.archive(&sha('a')).expect("a read"), Some(held));
    let file = store.archive_path(&sha('a'));
    assert_eq!(file, dir.path().join(format!("{}.xip", sha('a'))));
    assert_eq!(fs::read(&file).expect("the bytes"), b"hello");
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&dir.path().join(format!("{}.json", sha('a')))), 0o600);
    let names: Vec<String> = fs::read_dir(dir.path())
        .expect("the directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert!(
        names.iter().all(|name| !name.starts_with(".upload-")),
        "{names:?}"
    );
    assert_eq!(
        store.held().expect("the held ids"),
        vec![id("27.2", "27B52")]
    );
}

/// Proves: the archives are listed newest release first, a version compared by its numbers
/// (`27.10` after `27.9`), and an archive the store does not hold, or one named by something
/// that is not a digest, is none.
///
/// Catches: a listing in the directory's order or in the text's order, and a lookup that
/// answers for a name that is not a digest.
#[test]
fn archives_are_listed_newest_release_first() {
    let (_dir, store) = store("xcode-store-order");
    put(&store, &archive("27.9", "27A1", 'a'));
    put(&store, &archive("27.10", "27B1", '2'));
    put(&store, &archive("26.4", "26A1", '3'));
    let versions: Vec<String> = store
        .archives()
        .expect("a listing")
        .iter()
        .map(|a| a.id.version.to_string())
        .collect();
    assert_eq!(versions, ["27.10", "27.9", "26.4"]);
    assert_eq!(store.archive(&sha('9')).expect("a read"), None);
    assert_eq!(store.archive("../etc/passwd").expect("a read"), None);
    assert_eq!(
        store.archive(&sha('a').to_uppercase()).expect("a read"),
        None
    );
}

/// Proves: a record whose archive file is missing is not an archive: it is not listed, not read
/// by its digest and not held by `held`.
///
/// Catches: a release counted as stored while its bytes are not there.
#[test]
fn an_archive_whose_file_is_missing_is_not_held() {
    let (_dir, store) = store("xcode-store-missing-file");
    put(&store, &archive("27.2", "27B52", 'a'));
    fs::remove_file(store.archive_path(&sha('a'))).expect("the file goes");
    assert_eq!(store.archives().expect("a listing"), vec![]);
    assert_eq!(store.archive(&sha('a')).expect("a read"), None);
    assert_eq!(store.held().expect("the held ids"), vec![]);
}

/// Proves: a record that is not JSON, one whose version is not a version number and one that
/// is too large to be a record are each left out of the listing while the others are listed,
/// and each is an error when read by its digest.
///
/// Catches: one damaged record that hides every archive or is read as a release.
#[test]
fn a_damaged_record_is_left_out_of_the_listing_and_is_an_error_by_digest() {
    let (dir, store) = store("xcode-store-damaged");
    put(&store, &archive("27.2", "27B52", 'a'));
    let bad = [
        ('b', "{ not json".to_owned()),
        (
            'c',
            r#"{"version":"x.y","build":"1","size":1,"by":"a","at_unix":1}"#.to_owned(),
        ),
        ('d', " ".repeat(10_000)),
    ];
    for (digit, text) in &bad {
        fs::write(dir.path().join(format!("{}.json", sha(*digit))), text).expect("a record");
        fs::write(store.archive_path(&sha(*digit)), b"bytes").expect("an archive");
    }
    fs::write(dir.path().join("notes.json"), b"{}").expect("a file that is no record");
    let listed = store.archives().expect("a listing");
    assert_eq!(listed, vec![archive("27.2", "27B52", 'a')]);
    for (digit, _) in &bad {
        let why = store.archive(&sha(*digit)).expect_err("a damaged record");
        assert!(
            matches!(&why, StoreError::Io { path, .. } if *path == dir.path().join(format!("{}.json", sha(*digit)))),
            "{why}"
        );
    }
}

/// Proves: a directory that can no longer be read is an error of the listing, of `holds` and
/// of `held`, not an empty store.
///
/// Catches: a store that reads as empty while its directory is gone, so a release is stored
/// again over what was lost.
#[test]
fn a_directory_that_cannot_be_read_is_an_error_not_an_empty_store() {
    let dir = scratch("xcode-store-gone");
    let inner = dir.path().join("store");
    fs::create_dir(&inner).expect("the directory");
    let store = XcodeStore::open(&inner, 1024).expect("the store opens");
    fs::remove_dir_all(&inner).expect("the directory goes");
    assert!(matches!(store.archives(), Err(StoreError::Io { .. })));
    assert!(matches!(
        store.holds(&id("27.2", "27B52"), &sha('a')),
        Err(StoreError::Io { .. })
    ));
    assert!(store.held().is_err());
}

/// Proves: `holds` says Free for a release the store has nothing of; Already for the same
/// version and build, and for the same digest under another release; and VersionTaken with the
/// held build for the same version under another build and another digest.
///
/// Catches: a second build of a version stored beside the first, and the same bytes stored
/// twice.
#[test]
fn holds_tells_free_already_and_a_version_taken() {
    let (_dir, store) = store("xcode-store-holds");
    let free = |version: &str, build: &str, digit: char| {
        store
            .holds(&id(version, build), &sha(digit))
            .expect("the store reads")
    };
    assert_eq!(free("27.2", "27B52", 'a'), Held::Free);
    put(&store, &archive("27.2", "27B52", 'a'));
    assert_eq!(free("27.2", "27B52", 'b'), Held::Already, "the same id");
    assert_eq!(free("27.3", "27C1", 'a'), Held::Already, "the same digest");
    assert_eq!(free("27.2", "27B52", 'a'), Held::Already, "both");
    assert_eq!(
        free("27.2", "27B99", 'b'),
        Held::VersionTaken("27B52".to_owned())
    );
    assert_eq!(free("27.3", "27C1", 'b'), Held::Free, "another version");
}

/// Proves: a refused second build is noted once per version (a later note for the version
/// replaces the earlier), `taken` lists the notes in version order, and removing the held
/// archive of a version clears that version's note and no other.
///
/// Catches: an alert for a taken version that stays after its archive is removed, and one that
/// clears another version's.
#[test]
fn a_taken_version_stands_until_its_archive_is_removed() {
    let (_dir, store) = store("xcode-store-taken");
    assert_eq!(store.taken(), vec![]);
    let first = archive("27.2", "27B52", 'a');
    let second = archive("28.0", "28A1", 'b');
    put(&store, &first);
    put(&store, &second);
    store.note_taken("28.0", "28A1", "28A2");
    store.note_taken("27.2", "27B52", "27B99");
    store.note_taken("27.2", "27B52", "27B77");
    let taken = |version: &str, held: &str, refused: &str| Taken {
        version: version.to_owned(),
        held_build: held.to_owned(),
        refused_build: refused.to_owned(),
    };
    assert_eq!(
        store.taken(),
        vec![
            taken("27.2", "27B52", "27B77"),
            taken("28.0", "28A1", "28A2")
        ]
    );
    let _held = store.changing();
    store.remove(&first).expect("the removal");
    assert_eq!(store.taken(), vec![taken("28.0", "28A1", "28A2")]);
}

/// Proves: removing an archive deletes its file and its record, so it is no longer held, and
/// removing it again is not an error.
///
/// Catches: a removal that leaves the bytes behind, and one that fails on what is already gone.
#[test]
fn removing_an_archive_deletes_both_files_and_can_be_repeated() {
    let (dir, store) = store("xcode-store-remove");
    let held = archive("27.2", "27B52", 'a');
    put(&store, &held);
    let _changing = store.changing();
    store.remove(&held).expect("the removal");
    assert!(!store.archive_path(&sha('a')).exists());
    assert!(!dir.path().join(format!("{}.json", sha('a'))).exists());
    assert_eq!(store.archives().expect("a listing"), vec![]);
    store.remove(&held).expect("a second removal");
}

/// Proves: a commit whose upload file is missing fails and leaves neither a record nor an
/// archive, so nothing is held.
///
/// Catches: a record written for an archive that was never moved into place, which would list
/// a release the store cannot serve.
#[test]
fn a_commit_that_fails_keeps_nothing() {
    let (dir, store) = store("xcode-store-commit-fails");
    let _changing = store.changing();
    let missing = dir.path().join(".upload-gone");
    let why = store
        .commit(&missing, &archive("27.2", "27B52", 'a'))
        .expect_err("no upload to commit");
    assert!(matches!(why, StoreError::Io { .. }), "{why}");
    assert!(!dir.path().join(format!("{}.json", sha('a'))).exists());
    assert!(!store.archive_path(&sha('a')).exists());
}

/// Proves: a second reservation of a version is refused while the first stands, another version
/// is reserved at once, and the version is free again when the first reservation is dropped.
///
/// Catches: two uploads of one version at the same time, and a reservation that outlives its
/// upload.
#[test]
fn a_version_is_reserved_by_one_upload_at_a_time() {
    let (_dir, store) = store("xcode-store-reserve");
    let first = store.reserve("27.2").expect("the first reservation");
    assert!(store.reserve("27.2").is_none(), "a second upload of 27.2");
    let other = store.reserve("27.3").expect("another version");
    drop(first);
    let again = store.reserve("27.2").expect("free after the drop");
    assert!(store.reserve("27.2").is_none());
    drop((other, again));
    assert!(store.reserve("27.3").is_some());
}

/// The archives of the ids `ids`, one digit each from `1`.
fn many(ids: &[(&str, &str)]) -> Vec<Archive> {
    ids.iter()
        .zip('1'..)
        .map(|((version, build), digit)| archive(version, build, digit))
        .collect()
}

/// The ids of `archives`, as `version/build`.
fn named(archives: &[Archive]) -> Vec<String> {
    archives
        .iter()
        .map(|a| format!("{}/{}", a.id.version, a.id.build))
        .collect()
}

/// Proves: with `keep` 0 the newest release stays and every other goes, and both lists are
/// newest first whatever the order given.
///
/// Catches: a policy that removes the release just pushed, and one that keeps nothing.
#[test]
fn keeping_none_still_keeps_the_newest_release() {
    let all = many(&[("26.4", "26A1"), ("27.2", "27B52"), ("27.1", "27A9")]);
    let (stays, goes) = kept(&all, 0, &BTreeSet::new());
    assert_eq!(named(&stays), ["27.2/27B52"]);
    assert_eq!(named(&goes), ["27.1/27A9", "26.4/26A1"]);
}

/// Proves: `keep` newest stay, `keep` equal to the count and above it (up to `u32::MAX`) keep
/// all, and an empty store keeps and removes nothing.
///
/// Catches: an off-by-one at the boundary of the count, and an overflow on a large `keep`.
#[test]
fn keeping_as_many_as_there_are_or_more_keeps_all() {
    let all = many(&[("27.1", "27A9"), ("27.2", "27B52"), ("26.4", "26A1")]);
    let (stays, goes) = kept(&all, 2, &BTreeSet::new());
    assert_eq!(named(&stays), ["27.2/27B52", "27.1/27A9"]);
    assert_eq!(named(&goes), ["26.4/26A1"]);
    for keep in [3, 4, u32::MAX] {
        let (stays, goes) = kept(&all, keep, &BTreeSet::new());
        assert_eq!(
            named(&stays),
            ["27.2/27B52", "27.1/27A9", "26.4/26A1"],
            "{keep}"
        );
        assert!(goes.is_empty(), "{keep}");
    }
    assert_eq!(kept(&[], 5, &BTreeSet::new()), (vec![], vec![]));
}

/// Proves: a release a Mac reports installed stays though it is older than the newest `keep`,
/// in its place among the kept; an installed id that no archive has (another build of a held
/// version) keeps nothing.
///
/// Catches: a prune that removes the Xcode a Mac is running, and one that matches an installed
/// release by its version alone.
#[test]
fn an_installed_release_stays_beyond_the_newest() {
    let all = many(&[("27.2", "27B52"), ("27.1", "27A9"), ("26.4", "26A1")]);
    let installed = BTreeSet::from([id("26.4", "26A1"), id("27.1", "27A0")]);
    let (stays, goes) = kept(&all, 1, &installed);
    assert_eq!(named(&stays), ["27.2/27B52", "26.4/26A1"]);
    assert_eq!(named(&goes), ["27.1/27A9"]);
}

/// Proves: two archives of one release (same version and build, two digests) are ordered by
/// digest whatever the order given, so which of them the newest `keep` keeps does not depend on
/// the order the directory listed them in.
///
/// Catches: a prune whose result changes with the order of the directory.
#[test]
fn archives_of_one_release_are_ordered_by_digest() {
    let low = archive("27.2", "27B52", '1');
    let high = archive("27.2", "27B52", '2');
    for given in [
        vec![high.clone(), low.clone()],
        vec![low.clone(), high.clone()],
    ] {
        let (stays, goes) = kept(&given, 1, &BTreeSet::new());
        assert_eq!(stays, vec![low.clone()]);
        assert_eq!(goes, vec![high.clone()]);
    }
}

/// Proves: the digest of `abc` fed in two chunks is the known SHA-256 of `abc` as lowercase hex,
/// the count is the bytes fed, and nothing fed is the digest of the empty input with a count of
/// 0.
///
/// Catches: a digest that is not over every chunk, a count of chunks in place of bytes, and hex
/// in capitals or with a digit lost.
#[test]
fn digesting_gives_the_sha256_of_what_was_fed_and_its_length() {
    let mut digesting = Digesting::default();
    digesting.feed(b"a");
    digesting.feed(b"bc");
    assert_eq!(digesting.bytes(), 3);
    assert_eq!(
        digesting.finish(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let empty = Digesting::default();
    assert_eq!(empty.bytes(), 0);
    assert_eq!(
        empty.finish(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(hex(&[0x00, 0xff, 0x0a, 0xa0]), "00ff0aa0");
}

/// Proves: a left upload that cannot be removed stops the open with an error naming it, and an
/// upload file that cannot be made (the directory is gone) is an error, not a file elsewhere.
///
/// Catches: an open that starts over an upload it could not clear, and an upload that is written
/// somewhere that is not the store.
#[test]
fn an_upload_that_cannot_be_cleared_or_made_is_an_error() {
    let dir = scratch("xcode-store-upload-errors");
    let stuck = dir.path().join(format!(".upload-{}-aa-0", sha('a')));
    fs::create_dir(&stuck).expect("a directory where an upload was");
    written_ago(&stuck, STALE_UPLOAD + Duration::from_secs(60));
    let why = XcodeStore::open(dir.path(), 1024).expect_err("the upload stands");
    assert!(
        matches!(&why, StoreError::Io { path, .. } if *path == stuck),
        "{why}"
    );
    fs::remove_dir(&stuck).expect("the directory goes");
    let inner = dir.path().join("store");
    fs::create_dir(&inner).expect("the directory");
    let store = XcodeStore::open(&inner, 1024).expect("the store opens");
    fs::remove_dir(&inner).expect("the directory goes");
    let why = store.upload_file(&sha('b')).expect_err("no directory");
    assert!(matches!(why, StoreError::Io { .. }), "{why}");
}

/// Proves: a record that cannot be read (a directory stands where the file should be) is an
/// error naming it, and a file of an archive that cannot be removed makes the removal an error
/// naming that file and keeps the record.
///
/// Catches: a read error taken for an archive that is not there, and a removal that reports
/// success over a file it left.
#[test]
fn a_record_or_file_that_cannot_be_read_or_removed_is_an_error() {
    let (dir, store) = store("xcode-store-io-errors");
    let record = dir.path().join(format!("{}.json", sha('a')));
    fs::create_dir(&record).expect("a directory where a record was");
    let why = store
        .archive(&sha('a'))
        .expect_err("a record that is a directory");
    assert!(
        matches!(&why, StoreError::Io { path, .. } if *path == record),
        "{why}"
    );
    let held = archive("27.2", "27B52", 'b');
    put(&store, &held);
    fs::remove_file(store.archive_path(&sha('b'))).expect("the file goes");
    fs::create_dir(store.archive_path(&sha('b'))).expect("a directory where the file was");
    let _changing = store.changing();
    let why = store.remove(&held).expect_err("a file that is a directory");
    assert!(
        matches!(&why, StoreError::Io { path, .. } if *path == store.archive_path(&sha('b'))),
        "{why}"
    );
    assert!(dir.path().join(format!("{}.json", sha('b'))).exists());
}

/// Proves: removing a file that is not there, or one that cannot be removed, is not an error of
/// the caller: the file that stands stays.
///
/// Catches: a failed cleanup of a refused upload that fails the refusal's answer too.
#[test]
fn removing_quietly_never_fails_the_caller() {
    let dir = scratch("xcode-store-quiet");
    remove_quietly(&dir.path().join("missing"));
    let stuck = dir.path().join("stuck");
    fs::create_dir(&stuck).expect("a directory");
    remove_quietly(&stuck);
    assert!(stuck.exists());
    let file = dir.path().join("file");
    fs::write(&file, b"x").expect("a file");
    remove_quietly(&file);
    assert!(!file.exists());
}
