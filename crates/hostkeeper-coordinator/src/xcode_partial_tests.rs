//! The partial uploads of the Xcode store ([`super`]): their files, their lock between two stores
//! over one directory, and the sweep that removes the ones that lapse. The clock of every lock is
//! a number the test sets, and every file's time is set, so nothing here waits.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Barrier;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hostkeeper_testkit::Scratch;

use super::*;
use crate::testutil::{Log, scratch};

/// The second the clock of the tests starts at.
const START: u64 = 1_800_000_000;

/// A SHA-256 of 64 copies of `digit`.
fn sha(digit: char) -> String {
    digit.to_string().repeat(64)
}

/// The record of an upload of Xcode 27.2 (27B52) of `size` bytes, started by `ci-ops`.
fn record(size: u64) -> UploadRecord {
    UploadRecord {
        version: "27.2".to_owned(),
        build: "27B52".to_owned(),
        size,
        by: "ci-ops".to_owned(),
    }
}

/// A store over the directory `dir`, whose locks are judged by `clock` added to [`START`].
fn store_over(dir: &Path, clock: &Arc<AtomicU64>) -> XcodeStore {
    let clock = Arc::clone(clock);
    XcodeStore::open(dir, 1024, SystemTime::UNIX_EPOCH)
        .expect("a store")
        .with_clock(Arc::new(move || START + clock.load(Ordering::SeqCst)))
}

/// A directory, the clock at 0 and a store over it.
fn rig(name: &str) -> (Scratch, Arc<AtomicU64>, XcodeStore) {
    let dir = scratch(name);
    let clock = Arc::new(AtomicU64::new(0));
    let store = store_over(dir.path(), &clock);
    (dir, clock, store)
}

/// The names in `dir`, sorted.
fn names(dir: &Scratch) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir.path())
        .expect("the directory")
        .map(|entry| {
            entry
                .expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// The time the sweeps of these tests are run at.
fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(START)
}

/// Makes the last write of `name` in `dir` `age` seconds before [`now`].
fn written_ago(dir: &Scratch, name: &str, age: u64) {
    fs::File::open(dir.path().join(name))
        .expect("the path opens")
        .set_modified(now() - Duration::from_secs(age))
        .expect("the time is set");
}

/// Proves: of two stores over one directory that ask for the lock of one SHA-256 at the same
/// moment, one gets it and the other is refused [`LockError::Held`], in every round of a hundred;
/// once the winner drops it the other can take it. This is the `O_CREAT|O_EXCL` of the lock file
/// seen from two coordinators of one deployment.
///
/// Catches: a lock that two coordinators can both hold, which is two writers appending to one
/// partial upload.
#[test]
fn of_two_stores_over_one_directory_exactly_one_takes_the_lock() {
    let (dir, clock, first) = rig("partial-race");
    let second = store_over(dir.path(), &clock);
    let name = sha('a');
    for round in 0..100 {
        let barrier = Barrier::new(2);
        let wins = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for (who, store) in [("first", &first), ("second", &second)] {
                let (barrier, wins, name) = (&barrier, &wins, &name);
                scope.spawn(move || {
                    barrier.wait();
                    let taken = store.lock_upload(name);
                    let held = taken.is_ok();
                    if held {
                        wins.lock().expect("the list").push(who);
                    }
                    // Both threads finish their attempt before either lock is dropped.
                    barrier.wait();
                    drop(taken);
                });
            }
        });
        assert_eq!(wins.lock().expect("the list").len(), 1, "round {round}");
        assert_eq!(names(&dir), Vec::<String>::new(), "round {round}");
    }

    let winner = first.lock_upload(&name).expect("the lock");
    assert!(matches!(second.lock_upload(&name), Err(LockError::Held)));
    drop(winner);
    second.lock_upload(&name).expect("free again");
}

/// Proves: one store refuses a second lock of the same SHA-256 while the first is held, and a lock
/// of another SHA-256 is free; the lock file holds a name of the writer and the second of the
/// clock, in a text of one fixed width, and is gone when the lock is dropped.
///
/// Catches: two requests of one coordinator that write one partial upload, and a lock file that
/// outlives its request.
#[test]
fn a_store_locks_each_upload_once_and_removes_its_lock_file() {
    let (dir, _, store) = rig("partial-lock-once");
    let held = store.lock_upload(&sha('a')).expect("the lock");

    assert!(matches!(store.lock_upload(&sha('a')), Err(LockError::Held)));
    assert!(store.is_writing(&sha('a')));
    assert!(!store.is_writing(&sha('b')));
    let other = store.lock_upload(&sha('b')).expect("another upload's lock");
    let text = fs::read_to_string(dir.path().join(format!(".partial-{}.lock", sha('a'))))
        .expect("the lock file");
    assert!(text.ends_with(&format!(" {START:020}\n")), "{text:?}");
    assert_eq!(text.len(), 16 + 1 + 20 + 1 + 20 + 1, "{text:?}");
    assert_eq!(
        fs::metadata(dir.path().join(format!(".partial-{}.lock", sha('a'))))
            .expect("the lock file")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    drop(held);
    drop(other);
    assert_eq!(names(&dir), Vec::<String>::new());
    assert!(!store.is_writing(&sha('a')));
}

/// Proves: another store's lock is held up to the second before it is [`LOCK_STALE`] seconds old
/// and taken over at that second, with a log line; a lock whose text is not a lock's (empty, no
/// number, a number that is not one, no name) is taken over at once; the taken-over lock is the
/// new writer's.
///
/// Catches: a takeover a second early or late, a crashed writer's empty lock file that blocks an
/// upload for ever, and a takeover that leaves the old text in place.
#[test]
fn a_stale_lock_and_a_lock_that_is_not_one_are_taken_over() {
    let log = Log::default();
    let _guard = log.install();
    let (dir, clock, store) = rig("partial-stale");
    let other = store_over(dir.path(), &clock);
    let lock = dir.path().join(format!(".partial-{}.lock", sha('a')));
    let dead = other
        .lock_upload(&sha('a'))
        .expect("the first writer's lock");

    clock.store(LOCK_STALE - 1, Ordering::SeqCst);
    assert!(matches!(store.lock_upload(&sha('a')), Err(LockError::Held)));
    clock.store(LOCK_STALE, Ordering::SeqCst);
    let mine = store.lock_upload(&sha('a')).expect("taken over");
    assert!(
        log.text()
            .contains("a stale lock of a partial Xcode upload was taken over")
    );
    let text = fs::read_to_string(&lock).expect("the lock file");
    assert!(
        text.ends_with(&format!(" {:020}\n", START + LOCK_STALE)),
        "{text:?}"
    );
    // The old writer's drop leaves the new writer's lock alone.
    drop(dead);
    assert!(lock.exists());
    drop(mine);
    assert!(!lock.exists());

    for junk in ["", "no-number\n", "name abc\n", " 5\n", "name 5 6x\n"] {
        fs::write(&lock, junk).expect("a lock that is not one");
        let taken = store.lock_upload(&sha('a')).expect(junk);
        drop(taken);
    }
}

/// A store over `dir` whose clock runs `between` once, the second time it is read after `armed` is
/// set: the first read writes the writer's own lock's text, the second is the moment after it has
/// read a lock that is there and before it judges it by the clock.
fn store_with_hook(
    dir: &Path,
    clock: &Arc<AtomicU64>,
    armed: &Arc<AtomicBool>,
    between: impl Fn() + Send + Sync + 'static,
) -> XcodeStore {
    let (clock, armed) = (Arc::clone(clock), Arc::clone(armed));
    let reads = AtomicU64::new(0);
    XcodeStore::open(dir, 1024, SystemTime::UNIX_EPOCH)
        .expect("a store")
        .with_clock(Arc::new(move || {
            if armed.load(Ordering::SeqCst) && reads.fetch_add(1, Ordering::SeqCst) == 1 {
                between();
            }
            START + clock.load(Ordering::SeqCst)
        }))
}

/// Proves: a writer that read a lock as stale and finds, when it comes to take it, that another
/// writer has made a fresh lock since, does not remove it: it is refused (`Held`), the fresh
/// lock's text is as it was, no file of the takeover is left, and the writer that made it
/// refreshes and goes on.
///
/// Catches: a takeover that removes whatever is at the lock's name, which takes a live writer's
/// lock and lets two writers append to one partial upload.
#[test]
fn a_takeover_never_removes_a_lock_made_since_it_was_judged_stale() {
    let dir = scratch("partial-takeover-race");
    let clock = Arc::new(AtomicU64::new(0));
    let armed = Arc::new(AtomicBool::new(false));
    let lock = dir.path().join(format!(".partial-{}.lock", sha('a')));
    let fresh = lock_text("live-writer", START + LOCK_STALE + 5);
    let replace = {
        let (lock, fresh) = (lock.clone(), fresh.clone());
        move || {
            fs::remove_file(&lock).expect("the stale lock goes");
            fs::write(&lock, &fresh).expect("a fresh lock is made");
        }
    };
    let late = store_with_hook(dir.path(), &clock, &armed, replace);
    let first = store_over(dir.path(), &clock);
    let dead = first
        .lock_upload(&sha('a'))
        .expect("the first writer's lock");
    clock.store(LOCK_STALE + 5, Ordering::SeqCst);
    armed.store(true, Ordering::SeqCst);

    assert!(matches!(late.lock_upload(&sha('a')), Err(LockError::Held)));

    assert_eq!(fs::read_to_string(&lock).expect("the lock"), fresh);
    assert_eq!(
        names(&dir),
        vec![format!(".partial-{}.lock", sha('a'))],
        "nothing of the takeover is left"
    );
    assert!(!late.is_writing(&sha('a')));
    drop(dead);
}

/// Proves: a stale lock that is removed by another writer after this one read it is not an error:
/// the takeover finds nothing to move and the lock is made.
///
/// Catches: a takeover that fails the upload with a store failure because the stale lock was
/// taken away first.
#[test]
fn a_stale_lock_removed_meanwhile_is_made_again() {
    let dir = scratch("partial-takeover-gone");
    let clock = Arc::new(AtomicU64::new(0));
    let armed = Arc::new(AtomicBool::new(false));
    let lock = dir.path().join(format!(".partial-{}.lock", sha('a')));
    let gone = {
        let lock = lock.clone();
        move || fs::remove_file(&lock).expect("the stale lock goes")
    };
    let late = store_with_hook(dir.path(), &clock, &armed, gone);
    let first = store_over(dir.path(), &clock);
    let _dead = first
        .lock_upload(&sha('a'))
        .expect("the first writer's lock");
    clock.store(LOCK_STALE, Ordering::SeqCst);
    armed.store(true, Ordering::SeqCst);

    let mine = late.lock_upload(&sha('a')).expect("made again");

    assert!(lock.exists());
    drop(mine);
    assert!(!lock.exists());
}

/// Proves: a lock that cannot be examined is [`LockError::Store`], not a refusal: the lock path
/// is a directory (reading it fails), and the directory of the store is gone (making it fails).
///
/// Catches: a failure of the disk answered as another writer's lock, which a client retries for
/// ever.
#[test]
fn a_lock_that_cannot_be_examined_or_made_is_a_store_failure() {
    let (dir, _, store) = rig("partial-lock-io");
    fs::create_dir(dir.path().join(format!(".partial-{}.lock", sha('a')))).expect("a directory");
    assert!(matches!(
        store.lock_upload(&sha('a')),
        Err(LockError::Store(_))
    ));
    assert!(!store.is_writing(&sha('a')), "a failed lock is not held");

    fs::remove_dir_all(dir.path()).expect("the directory goes");
    assert!(matches!(
        store.lock_upload(&sha('b')),
        Err(LockError::Store(_))
    ));
}

/// Proves: a refresh writes the second of the clock into the lock (once for each second: the same
/// second is no second write), keeping the writer's name; and a refresh is [`LockError::Held`]
/// when the lock file is gone or another writer's, which is what a writer whose lock was taken as
/// stale finds, and a refresh of a lock whose file cannot be read is a store failure.
///
/// Catches: a lock that is never refreshed and so is taken from a live writer, and a writer that
/// goes on writing after another has taken its lock.
#[test]
fn a_refresh_keeps_the_lock_alive_and_finds_it_taken() {
    let (dir, clock, store) = rig("partial-refresh");
    let other = store_over(dir.path(), &clock);
    let path = dir.path().join(format!(".partial-{}.lock", sha('a')));
    let mut mine = store.lock_upload(&sha('a')).expect("the lock");
    let made = fs::read_to_string(&path).expect("the lock");

    mine.refresh().expect("a refresh in the second it was made");
    mine.refresh().expect("and a second refresh in it");
    assert_eq!(fs::read_to_string(&path).expect("the lock"), made);
    clock.store(20, Ordering::SeqCst);
    mine.refresh().expect("a refresh");
    let refreshed = fs::read_to_string(&path).expect("the lock");
    assert_eq!(
        refreshed,
        made.replace(&format!("{START:020}"), &format!("{:020}", START + 20))
    );
    // Refreshed at 20: not stale at 49, stale at 50.
    clock.store(20 + LOCK_STALE - 1, Ordering::SeqCst);
    assert!(matches!(other.lock_upload(&sha('a')), Err(LockError::Held)));
    clock.store(20 + LOCK_STALE, Ordering::SeqCst);
    let taken = other.lock_upload(&sha('a')).expect("taken over");

    clock.store(100, Ordering::SeqCst);
    assert!(matches!(mine.refresh(), Err(LockError::Held)));
    drop(taken);
    assert!(matches!(mine.refresh(), Err(LockError::Held)));

    fs::create_dir(&path).expect("a directory in the lock's place");
    clock.store(200, Ordering::SeqCst);
    assert!(matches!(mine.refresh(), Err(LockError::Store(_))));
}

/// Proves: a partial upload is begun with its record, appended to, read back with the bytes held,
/// the size and who started it, and removed with its record, twice over without an error; the
/// files are mode 0600; a record with no file is an upload of no bytes; a name that is not a
/// SHA-256 is no upload.
///
/// Catches: a partial upload whose bytes are not what the record says, files other users can
/// read, and a removal that fails on what is already gone.
#[test]
fn an_upload_is_begun_appended_read_and_removed() {
    let (dir, _, store) = rig("partial-files");
    let name = sha('a');
    assert_eq!(store.upload(&name).expect("none"), None);
    assert_eq!(store.upload("not a sha").expect("none"), None);

    // A file left with no record holds no bytes of a new upload.
    fs::write(store.upload_file_of(&name), b"left over").expect("a file with no record");
    store.begin_upload(&name, &record(100)).expect("begun");
    assert_eq!(
        store
            .upload(&name)
            .expect("an upload")
            .map(|upload| upload.received),
        Some(0),
        "a record with no file, or an empty one, holds no bytes"
    );
    let mut file = store.append_upload(&name).expect("opened");
    file.write_all(b"hello").expect("written");
    drop(file);
    let mut file = store.append_upload(&name).expect("opened again");
    file.write_all(b" world").expect("written");
    drop(file);

    let upload = store.upload(&name).expect("read").expect("an upload");
    assert_eq!(upload.sha256, name);
    assert_eq!(upload.record, record(100));
    assert_eq!(upload.received, 11);
    assert_eq!(
        fs::read(store.upload_file_of(&name)).expect("the bytes"),
        b"hello world"
    );
    for file in [format!(".partial-{name}"), format!(".partial-{name}.json")] {
        assert_eq!(
            fs::metadata(dir.path().join(&file))
                .expect("a file")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "{file}"
        );
    }

    store.remove_upload(&name).expect("removed");
    store.remove_upload(&name).expect("removed again");
    assert_eq!(names(&dir), Vec::<String>::new());
    store
        .begin_upload(&name, &record(100))
        .expect("begun again");
    assert_eq!(
        store
            .upload(&name)
            .expect("read")
            .map(|upload| upload.received),
        Some(0)
    );
}

/// Proves: a record that is not a record is an error naming its file, and a file that cannot be
/// removed is an error naming it (the partial upload's file is a non-empty directory).
///
/// Catches: a damaged record read as an upload of nothing, and a removal that says it removed
/// what it could not.
#[test]
fn a_damaged_record_and_a_file_that_stays_are_errors() {
    let (dir, _, store) = rig("partial-damaged");
    let record_path = dir.path().join(format!(".partial-{}.json", sha('a')));
    fs::write(&record_path, b"{").expect("a damaged record");
    let why = store.upload(&sha('a')).expect_err("a damaged record");
    assert!(
        why.to_string()
            .starts_with(&record_path.display().to_string()),
        "{why}"
    );

    let file = dir.path().join(format!(".partial-{}", sha('b')));
    fs::create_dir(&file).expect("a directory");
    fs::write(file.join("inside"), b"x").expect("a file inside");
    let why = store
        .remove_upload(&sha('b'))
        .expect_err("a file that stays");
    assert!(
        why.to_string().starts_with(&file.display().to_string()),
        "{why}"
    );
}

/// Proves: the partial uploads are listed the one written longest ago first, with a tie by SHA-256,
/// the time of the last write being that of its file (its record's for one with no file); a
/// record that is damaged is left out with a log line, and names that are not an upload's record
/// (the file, the lock, a record of a name that is not a SHA-256, an archive's record) are not
/// uploads.
///
/// Catches: an `Xcode.List` that fails for one bad record, one that lists an archive's record as
/// an upload, and an order that changes between calls.
#[test]
fn the_uploads_are_listed_oldest_first_and_a_bad_record_is_left_out() {
    let log = Log::default();
    let _guard = log.install();
    let (dir, _, store) = rig("partial-list");
    for (digit, written) in [('c', 300), ('a', 200), ('b', 200), ('d', 100)] {
        store.begin_upload(&sha(digit), &record(10)).expect("begun");
        let mut file = store.append_upload(&sha(digit)).expect("opened");
        file.write_all(b"xy").expect("written");
        drop(file);
        written_ago(&dir, &format!(".partial-{}", sha(digit)), 1000 - written);
    }
    // No file: the record's own time.
    store.begin_upload(&sha('e'), &record(10)).expect("begun");
    written_ago(&dir, &format!(".partial-{}.json", sha('e')), 1000 - 50);
    fs::write(dir.path().join(format!(".partial-{}.json", sha('f'))), b"{").expect("damaged");
    let _lock = store.lock_upload(&sha('a')).expect("a lock");
    fs::write(dir.path().join(".partial-short.json"), b"{}").expect("not a sha");
    fs::write(dir.path().join(format!("{}.json", sha('9'))), b"{}").expect("an archive's record");

    let listed: Vec<(String, u64, u64)> = store
        .uploads()
        .expect("listed")
        .into_iter()
        .map(|upload| (upload.sha256, upload.received, upload.last_write_unix))
        .collect();

    let at = |ago: u64| START - ago;
    assert_eq!(
        listed,
        vec![
            (sha('e'), 0, at(950)),
            (sha('d'), 2, at(900)),
            (sha('a'), 2, at(800)),
            (sha('b'), 2, at(800)),
            (sha('c'), 2, at(700)),
        ]
    );
    assert!(
        log.text()
            .contains("a partial Xcode upload's record could not be read")
    );

    fs::remove_dir_all(dir.path()).expect("the directory goes");
    assert!(store.uploads().is_err());
}

/// Proves: the sweep removes the three names of a partial upload together by the last write of
/// its file (a record or a lock older than the file does not lapse it, and a file older than a day
/// takes its newer record and lock with it), at a day and a second and not at a day; a record or a
/// lock with no file goes by its own age; `.upload-` files go by their own age; and nothing else
/// is touched. It answers the names removed, in order, and logs each.
///
/// Catches: a lapse that removes the record of an upload that is still being written (its record
/// is a day older than its last block), one that leaves a lock or record beside a removed file,
/// and one that removes an archive.
#[test]
fn the_sweep_lapses_an_upload_by_its_file_and_each_other_name_by_its_own_time() {
    let log = Log::default();
    let _guard = log.install();
    let (dir, _, store) = rig("partial-sweep");
    let day = 24 * 60 * 60;
    let put = |name: &str, age: u64| {
        fs::write(dir.path().join(name), b"x").expect("a file");
        written_ago(&dir, name, age);
    };
    let a = format!(".partial-{}", sha('a'));
    let b = format!(".partial-{}", sha('b'));
    let c = format!(".partial-{}", sha('c'));
    let d = format!(".partial-{}", sha('d'));
    // Still being written: the file is new, the record and the lock are old.
    put(&a, 5);
    put(&format!("{a}.json"), 3 * day);
    put(&format!("{a}.lock"), 3 * day);
    // Exactly a day: stays. A day and a second: goes, with its newer record and lock.
    put(&b, day);
    put(&format!("{b}.json"), day - 10);
    put(&c, day + 1);
    put(&format!("{c}.json"), 10);
    put(&format!("{c}.lock"), 10);
    // No file: by their own age.
    put(&format!("{d}.json"), day + 5);
    put(&format!("{d}.lock"), day - 5);
    put(".partial-odd", day + 5);
    put(".upload-old", day + 5);
    put(".upload-new", day - 5);
    put("keep.xip", 9 * day);
    put("keep.json", 9 * day);

    let removed = store.lapse(now()).expect("swept");

    assert_eq!(
        removed,
        vec![
            c.clone(),
            format!("{c}.json"),
            format!("{c}.lock"),
            format!("{d}.json"),
            ".partial-odd".to_owned(),
            ".upload-old".to_owned(),
        ]
    );
    let mut expected = vec![
        a.clone(),
        format!("{a}.json"),
        format!("{a}.lock"),
        b.clone(),
        format!("{b}.json"),
        format!("{d}.lock"),
        ".upload-new".to_owned(),
        "keep.json".to_owned(),
        "keep.xip".to_owned(),
    ];
    expected.sort();
    assert_eq!(names(&dir), expected);
    for name in &removed {
        assert!(
            log.text().contains(&format!(
                "a lapsed file of the Xcode store was removed name={name}"
            )),
            "{name}: {}",
            log.text()
        );
    }
}

/// Proves: opening a store runs the same sweep: a partial upload not written for more than a day
/// is removed with its record and lock, and one written within it stays, so a coordinator that
/// starts does not remove what another coordinator of the directory is writing.
///
/// Catches: partial uploads of a stopped coordinator that stay for ever, and a start that removes
/// another coordinator's upload in progress.
#[test]
fn opening_a_store_lapses_the_partial_uploads_a_day_old() {
    let dir = scratch("partial-open");
    let day = 24 * 60 * 60;
    let old = format!(".partial-{}", sha('a'));
    let new = format!(".partial-{}", sha('b'));
    for (name, age) in [
        (old.clone(), day + 1),
        (format!("{old}.json"), day + 1),
        (format!("{old}.lock"), day + 1),
        (new.clone(), 60),
        (format!("{new}.json"), 60),
    ] {
        fs::write(dir.path().join(&name), b"x").expect("a file");
        written_ago(&dir, &name, age);
    }

    XcodeStore::open(dir.path(), 1024, now()).expect("the store opens");

    assert_eq!(names(&dir), vec![new.clone(), format!("{new}.json")]);
}

/// Proves: a sweep that cannot examine a file is an error naming the file, and a directory that
/// cannot be listed is one naming the directory.
///
/// Catches: a lapse that skips a file it could not read, which then stays for ever unseen.
#[test]
fn a_sweep_that_cannot_examine_or_remove_is_an_error() {
    let (dir, _, store) = rig("partial-sweep-fails");
    let name = format!(".partial-{}", sha('a'));
    // A file that is removable only as a file: a non-empty directory of the name.
    fs::create_dir(dir.path().join(&name)).expect("a directory");
    fs::write(dir.path().join(&name).join("inside"), b"x").expect("a file inside");
    written_ago(&dir, &name, 2 * 24 * 60 * 60);

    let why = store.lapse(now()).expect_err("it cannot be removed");
    assert!(why.to_string().contains(&name), "{why}");

    let gone = dir.path().to_owned();
    fs::remove_dir_all(&gone).expect("the directory goes");
    let why = store.lapse(now()).expect_err("it cannot be listed");
    assert!(
        why.to_string().contains(&gone.display().to_string()),
        "{why}"
    );
}

/// Proves: an upload whose record cannot be read (the record's path is a directory) or whose file
/// cannot be examined (it is a link to itself) is an error naming the path, and a store formats
/// for a log without its clock.
///
/// Catches: a failure of the disk read as an upload of no bytes, which would then be started again
/// over the bytes it holds.
#[test]
fn an_upload_that_cannot_be_read_is_an_error() {
    let (dir, _, store) = rig("partial-unreadable");
    let record_path = dir.path().join(format!(".partial-{}.json", sha('a')));
    fs::create_dir(&record_path).expect("a directory in the record's place");
    let why = store.upload(&sha('a')).expect_err("an unreadable record");
    assert!(
        why.to_string()
            .starts_with(&record_path.display().to_string()),
        "{why}"
    );

    store.begin_upload(&sha('b'), &record(10)).expect("started");
    let data = dir.path().join(format!(".partial-{}", sha('b')));
    std::os::unix::fs::symlink(&data, &data).expect("a link to itself");
    let why = store
        .upload(&sha('b'))
        .expect_err("a file that cannot be examined");
    assert!(
        why.to_string().starts_with(&data.display().to_string()),
        "{why}"
    );

    assert!(format!("{store:?}").contains("Clock"));
}

/// Proves: starting an upload and opening its file to append are errors naming the path when the
/// store's directory is gone.
///
/// Catches: an upload that goes on, in memory, after the disk refused it.
#[test]
fn an_upload_that_cannot_be_started_or_opened_is_an_error() {
    let (dir, _, store) = rig("partial-start-fails");
    fs::remove_dir_all(dir.path()).expect("the directory goes");

    let why = store
        .begin_upload(&sha('a'), &record(10))
        .expect_err("not started");
    assert!(why.to_string().contains(&sha('a')), "{why}");
    let why = store.append_upload(&sha('a')).expect_err("not opened");
    assert!(why.to_string().contains(&sha('a')), "{why}");
}
