//! The Xcode release rows (`xcode_releases`, migration `0028`; `docs/design/maintenance-updates.md`,
//! section 1.1, job `xcode-feed`): the list the job keeps is the list a coordinator reads, and
//! each keep replaces the one before whole. Nothing here reads a clock.

use super::{TestDatabase, off_runtime, with_session};
use crate::store::postgres::Postgres;
use crate::store::postgres::migrate;
use crate::store::postgres::xcode_rows::PgReleases;
use crate::xcode_feed::{self, KnownReleases, XcodeRelease};

/// The message of a write refused at once because the store is lost.
const WRITE_LOST: &str = "the writing session stopped";

/// The message of a read refused at once because the store is lost.
const READ_LOST: &str = "the reading session stopped";

/// The releases of the recorded list of `xcodereleases.com`.
fn feed() -> Vec<XcodeRelease> {
    xcode_feed::parse(include_bytes!("../../../../tests/fixtures/xcode/data.json"))
        .expect("the recorded list parses")
        .releases
}

/// `releases` in the order the rows come back in: oldest first.
fn by_key(mut releases: Vec<XcodeRelease>) -> Vec<XcodeRelease> {
    releases.sort();
    releases
}

/// A database with a store open on it, and the rows over it.
fn rows() -> (TestDatabase, Postgres, PgReleases) {
    let db = TestDatabase::new();
    let (store, _) = db.open("xcode-rows").expect("the store opens");
    let releases = PgReleases::new(store.clone());
    (db, store, releases)
}

/// Proves: a store with nothing kept reads an empty list, and a list kept is read back with every
/// release of it, equal in version, build, channel and the macOS it requires.
///
/// Catches: a column read into another field, and a release lost or made up on the way through
/// the table.
#[test]
fn a_list_kept_is_read_back_equal_and_a_store_with_none_reads_empty() {
    let (db, store, releases) = rows();
    assert_eq!(releases.known(), Ok(Vec::new()));
    let list = feed();
    assert!(list.len() > 3, "the recorded list has releases to keep");
    releases.keep(&list).expect("kept");
    assert_eq!(releases.known(), Ok(by_key(list.clone())));
    assert_eq!(
        db.rows("SELECT count(*) FROM xcode_releases"),
        vec![vec![Some(list.len().to_string())]]
    );
    store.stop();
}

/// Proves: a second keep replaces the first whole: the rows of the first that the second does not
/// list are gone, and a release listed in both is one row; keeping an empty list empties the table.
///
/// Catches: a keep that adds to the rows before, so that a release the feed no longer lists stays
/// known, and a list listing a release twice that makes the keep fail.
#[test]
fn a_second_keep_replaces_the_first_whole() {
    let (_db, store, releases) = rows();
    let list = feed();
    releases.keep(&list).expect("kept");
    let fewer: Vec<XcodeRelease> = list[..2].to_vec();
    releases.keep(&fewer).expect("kept again");
    assert_eq!(releases.known(), Ok(by_key(fewer.clone())));
    let twice = [fewer[0].clone(), fewer[0].clone()];
    releases.keep(&twice).expect("kept with a release twice");
    assert_eq!(releases.known(), Ok(vec![fewer[0].clone()]));
    releases.keep(&[]).expect("kept empty");
    assert_eq!(releases.known(), Ok(Vec::new()));
    store.stop();
}

/// Proves: a row whose version is not a version number makes `known` an error that names the
/// version, and the rows that can be read are not returned in its place.
///
/// Catches: a row that cannot be read that is skipped or panics, so that a list read in part
/// is taken for the whole list.
#[test]
fn a_row_that_cannot_be_read_is_an_error_naming_it() {
    let (db, store, releases) = rows();
    releases.keep(&feed()).expect("kept");
    db.run(
        "INSERT INTO xcode_releases (version, build, channel, requires_macos) \
         VALUES ('x.y', '1A1', 'release', '26.0')",
    )
    .expect("a row the coordinator would not write");
    assert_eq!(
        releases.known(),
        Err("\"x.y\" is not a version number: dot-separated decimal numbers".to_owned())
    );
    store.stop();
}

/// Proves: the table refuses a channel the feed has no token for, so that a row the coordinator
/// reads always has a channel it knows.
///
/// Catches: a `0028` without its check on `channel`.
#[test]
fn the_table_refuses_a_channel_that_is_not_release_beta_or_rc() {
    let (db, store, _releases) = rows();
    assert!(
        db.run(
            "INSERT INTO xcode_releases (version, build, channel, requires_macos) \
             VALUES ('27.2', '27B52', 'nightly', '26.6')"
        )
        .is_err()
    );
    store.stop();
}

/// Proves: over a store that is lost, `keep` and `known` each return the store's error, and the
/// rows kept before stand.
///
/// Catches: a keep that reports a list it did not write, and a read that returns an empty list
/// for a store it could not read.
#[test]
fn a_lost_store_keeps_and_reads_nothing() {
    let (db, store, releases) = rows();
    let list = feed();
    releases.keep(&list[..1]).expect("kept");
    store.stop();
    off_runtime(move || {
        assert_eq!(releases.keep(&list), Err(WRITE_LOST.to_owned()));
        assert_eq!(releases.known(), Err(READ_LOST.to_owned()));
    });
    assert_eq!(
        db.rows("SELECT count(*) FROM xcode_releases"),
        vec![vec![Some("1".to_owned())]]
    );
}

/// Proves (B8, both ways, for `0028`): the release before (migrations `0001` to `0027`) runs on a
/// database this release migrated, running nothing, and the table it does not know holds a row
/// without the release before's statements touching it; this release, started on a database the
/// release before migrated, runs `0028` and the migration after it (`0029`) and makes the table
/// empty.
///
/// Catches: a `0028` that is not `may-run`, which would turn a rollback into a restore, and a
/// table the start does not make.
#[test]
fn the_release_before_runs_on_a_database_with_0028_and_this_one_on_its() {
    let db = TestDatabase::new();
    let (store, _) = db.open("this release").expect("this release starts");
    let releases = PgReleases::new(store.clone());
    releases.keep(&feed()).expect("kept");
    store.stop();
    let ran = with_session(&db, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..27]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before is accepted")
    });
    assert_eq!(ran, Vec::<i32>::new());

    let old = TestDatabase::new();
    with_session(&old, |session| {
        let before = migrate::checked(&migrate::SHIPPED[..27]).expect("the release before's set");
        migrate::apply(session, &before).expect("the release before migrates");
    });
    assert_eq!(
        old.rows(
            "SELECT count(*) FROM pg_class WHERE relkind = 'r' AND relname = 'xcode_releases'"
        ),
        vec![vec![Some("0".to_owned())]],
        "the release before has no such table"
    );
    let (store, _) = old.open("this release").expect("this release starts on it");
    store.stop();
    assert_eq!(
        old.rows("SELECT version FROM schema_version WHERE version > 27 ORDER BY version"),
        [
            vec![Some("28".to_owned())],
            vec![Some("29".to_owned())],
            vec![Some("30".to_owned())],
            vec![Some("31".to_owned())]
        ],
        "0028 to 0031 ran"
    );
    assert_eq!(
        old.rows("SELECT count(*) FROM xcode_releases"),
        vec![vec![Some("0".to_owned())]]
    );
}
