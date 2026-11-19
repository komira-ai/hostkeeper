//! The initial-admin file over a database's rows (`docs/design/coordinator-multi-instance.md`,
//! section 9, "Initial-admin file"): every coordinator on the database looks at the same file, and
//! the one whose conditional write on the user's row changed it is the one that audits, so a file
//! is adopted once and its expiry voids and is audited once, however many look.
//!
//! The audit's `seq` is numbered in each store's memory, so a store that has not read another's
//! entries would number its own the same: a test that has two coordinators audit one after the
//! other opens them afresh between (a start reads the audit), and only one coordinator of a pair
//! ever audits what they look at together. Nothing sleeps; the looks that race wait for one another
//! at a barrier.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

use hostkeeper_testkit::Scratch;

use super::{TestDatabase, off_runtime};
use crate::accounts::AccountStore;
use crate::initial_admin::{InitialAdmin, InitialAdminError, Outcome};
use crate::ops::OpsStore;
use crate::password::PasswordSettings;
use crate::store::Shared;
use crate::store::postgres::Postgres;
use crate::store::postgres::admin_rows::PgAdmin;
use crate::testutil::scratch;

const PASSWORD: &str = "k7Pq9xWm3Rt5Vb2NcH8dZs4F";
const NOW: u64 = 1_800_000_000;
const BOOTSTRAP: &str = "user_bootstrap";
const EXPIRED: &str = "initial_password_expired";

fn cheap() -> PasswordSettings {
    PasswordSettings {
        memory_kib: 8,
        iterations: 1,
        parallelism: 1,
        min_length: 12,
    }
}

/// One coordinator's side of the initial admin: its store, its accounts and ops over it, and its
/// look at the file at `path`, judged by the shared clock `now`.
struct Side {
    store: Postgres,
    accounts: AccountStore,
    ops: OpsStore,
    admin: InitialAdmin,
}

impl Side {
    /// A coordinator on `db` that has read what the database holds now, good for `max_secs`.
    fn start(db: &TestDatabase, path: &Path, now: &Arc<AtomicU64>, max_secs: u64) -> Side {
        let (store, loaded) = db.open("coordinator").expect("the store opens");
        let backend: Shared = Arc::new(store.clone());
        let clock = Arc::clone(now);
        let admin = InitialAdmin::new(
            path.to_owned(),
            max_secs,
            cheap(),
            Arc::new(move || clock.load(Ordering::SeqCst)),
        )
        .expect("an initial admin")
        .over(PgAdmin::new(store.clone()));
        Side {
            accounts: AccountStore::over(Arc::clone(&backend), loaded.accounts),
            ops: OpsStore::over(backend, loaded.ops).with_unix(Arc::new(|| NOW)),
            admin,
            store,
        }
    }

    fn look(&self) -> Result<Outcome, InitialAdminError> {
        self.admin.look(&self.accounts, &self.ops)
    }

    /// The look, on a thread of its own: the blocking client does not run on a runtime's thread.
    fn looked(&self) -> Result<Outcome, InitialAdminError> {
        std::thread::scope(|scope| scope.spawn(|| self.look()).join().expect("joined"))
    }
}

/// The file of `dir`, as the deploy step writes it.
fn file(dir: &Scratch) -> PathBuf {
    dir.path().join("initial-admin.toml")
}

/// Writes the file at `path` for `user` with `id`, good until `valid_until`, mode 0400.
fn put(path: &Path, id: &str, user: &str, valid_until: u64) {
    let _ = fs::remove_file(path);
    fs::write(
        path,
        format!(
            "id = \"{id}\"\nuser = \"{user}\"\npassword = \"{PASSWORD}\"\nvalid_until = {valid_until}\n"
        ),
    )
    .expect("written");
    fs::set_permissions(path, fs::Permissions::from_mode(0o400)).expect("mode");
}

/// The number of rows of the audit whose action is `action`.
fn audited(db: &TestDatabase, action: &str) -> usize {
    db.rows(&format!("SELECT 1 FROM audit WHERE action = '{action}'"))
        .len()
}

/// The row of `user` in `users`: its `initial_id`, `initial_void` and whether it has a deadline.
fn row(db: &TestDatabase, user: &str) -> Vec<Option<String>> {
    db.rows(&format!(
        "SELECT record->>'initial_id', record->>'initial_void', \
         (record ? 'initial_expires_at')::text FROM users WHERE name = '{user}'"
    ))
    .into_iter()
    .next()
    .unwrap_or_default()
}

fn text(value: &str) -> Option<String> {
    Some(value.to_owned())
}

/// Proves: two coordinators on one database that look at the same new file at the same moment
/// (a barrier, ten rounds, each with a file `id` of its own, two stores started afresh each round)
/// apply it once: one answers `Adopted` and the other `AlreadyAdopted`, the user's row holds the
/// round's id, and the audit holds one `user_bootstrap` per round; looking again, in turn, changes
/// nothing; and a restart reads the user, with the file's id, back from the row.
///
/// Catches: a file adopted by every coordinator that looks at it (a hash and an audit entry each),
/// a loser that fails where it should answer that the file is applied, and a row of the user that
/// does not hold the file's id.
#[test]
fn initial_admin_is_applied_once_when_two_coordinators_look_at_once() {
    let db = TestDatabase::new();
    let dir = scratch("admin-rows-once");
    let now = Arc::new(AtomicU64::new(NOW));
    for round in 1..=10_usize {
        let id = format!("file-{round}");
        put(&file(&dir), &id, "admin", NOW + 3000);
        let (one, other) = (
            Side::start(&db, &file(&dir), &now, 3600),
            Side::start(&db, &file(&dir), &now, 3600),
        );
        let barrier = Barrier::new(2);
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                one.look()
            });
            let second = scope.spawn(|| {
                barrier.wait();
                other.look()
            });
            (
                first.join().expect("joined"),
                second.join().expect("joined"),
            )
        });
        let mut outcomes = [first.expect("looked"), second.expect("looked")];
        outcomes.sort_by_key(|outcome| matches!(outcome, Outcome::AlreadyAdopted));
        assert_eq!(
            outcomes,
            [Outcome::Adopted, Outcome::AlreadyAdopted],
            "round {round}"
        );
        assert_eq!(audited(&db, BOOTSTRAP), round);
        assert_eq!(row(&db, "admin"), [text(&id), text("false"), text("true")]);
        // In turn, after the race: nothing is applied again.
        assert_eq!(one.looked().expect("looked"), Outcome::AlreadyAdopted);
        assert_eq!(other.looked().expect("looked"), Outcome::AlreadyAdopted);
        assert_eq!(audited(&db, BOOTSTRAP), round);
        one.store.stop();
        other.store.stop();
    }
    let (store, loaded) = db.open("restart").expect("the store opens again");
    let admin = loaded
        .accounts
        .user("admin")
        .expect("the user is read back");
    assert_eq!(admin.initial_id.as_deref(), Some("file-10"));
    assert!(admin.enabled && admin.must_change && !admin.initial_void);
    assert_eq!(admin.initial_expires_at, Some(NOW + 3000));
    store.stop();
}

/// Proves: a coordinator that looks at a file another coordinator adopted after it started (its
/// memory does not hold the user) answers `AlreadyAdopted` from the row, before it judges the file
/// (its own limit of one second is not asked: the file is good for 3000), hashes nothing, makes no
/// user in its memory, leaves the row (its hash) as it was and audits nothing.
///
/// Catches: a coordinator that hashes and writes a file the database already holds (replacing the
/// password hash another coordinator's logins use), and one that refuses a file that is applied
/// already.
#[test]
fn a_file_adopted_by_another_coordinator_is_not_hashed_again() {
    let db = TestDatabase::new();
    let dir = scratch("admin-rows-adopted");
    let now = Arc::new(AtomicU64::new(NOW));
    put(&file(&dir), "file-1", "admin", NOW + 3000);
    let first = Side::start(&db, &file(&dir), &now, 3600);
    let second = Side::start(&db, &file(&dir), &now, 1);
    assert_eq!(first.looked().expect("looked"), Outcome::Adopted);
    let hash = db.rows("SELECT record->>'hash' FROM users WHERE name = 'admin'");
    assert_eq!(hash.len(), 1);
    assert_eq!(second.accounts.snapshot().user("admin"), None);

    assert_eq!(second.looked().expect("looked"), Outcome::AlreadyAdopted);
    assert_eq!(second.accounts.snapshot().user("admin"), None);
    assert_eq!(
        db.rows("SELECT record->>'hash' FROM users WHERE name = 'admin'"),
        hash
    );
    assert_eq!(audited(&db, BOOTSTRAP), 1);

    // A file that is not applied is judged by the limit of the one that looks.
    put(&file(&dir), "file-2", "admin", NOW + 3000);
    assert_eq!(
        second.looked().map(drop).map_err(|why| why.to_string()),
        Err(
            "--initial-admin-file: valid_until is more than --initial-password-secs (1) from now"
                .to_owned()
        )
    );
    assert_eq!(row(&db, "admin")[0], text("file-1"));
    first.store.stop();
    second.store.stop();
}

/// Proves: a new file (another `id`) for a user that exists replaces the credential once: the row
/// holds the new id, is enabled and made to change the password again, `user_bootstrap` is audited
/// once more, and another coordinator that looks at it answers `AlreadyAdopted`.
///
/// Catches: a break-glass file that a coordinator over a database ignores because the user exists,
/// and one whose second look audits again.
#[test]
fn a_new_file_for_an_existing_user_replaces_the_credential_once() {
    let db = TestDatabase::new();
    let dir = scratch("admin-rows-breakglass");
    let now = Arc::new(AtomicU64::new(NOW));
    put(&file(&dir), "file-1", "admin", NOW + 3000);
    let first = Side::start(&db, &file(&dir), &now, 3600);
    assert_eq!(first.looked().expect("looked"), Outcome::Adopted);
    // The user has signed in and changed the password: no deadline, not forced to change.
    db.run(
        "UPDATE users SET record = (record - 'initial_expires_at') || '{\"must_change\": false}' \
         WHERE name = 'admin'",
    )
    .expect("the password is changed");
    assert_eq!(row(&db, "admin")[2], text("false"));

    put(&file(&dir), "file-2", "admin", NOW + 3000);
    assert_eq!(first.looked().expect("looked"), Outcome::Adopted);
    assert_eq!(
        db.rows("SELECT record->>'must_change' FROM users WHERE name = 'admin'"),
        [[text("true")]]
    );
    assert_eq!(
        row(&db, "admin"),
        [text("file-2"), text("false"), text("true")]
    );
    assert_eq!(audited(&db, BOOTSTRAP), 2);
    assert_eq!(first.looked().expect("looked"), Outcome::AlreadyAdopted);
    first.store.stop();

    let second = Side::start(&db, &file(&dir), &now, 3600);
    assert_eq!(second.looked().expect("looked"), Outcome::AlreadyAdopted);
    assert_eq!(audited(&db, BOOTSTRAP), 2);
    second.store.stop();
}

/// Proves: when the file expires, two coordinators that look at once (a barrier, eight rounds, each
/// with a user and a file of its own, the stores started afresh after the adoption) between them
/// void the credential once, audit `initial_password_expired` once, and delete the file; whichever
/// one voids, the row says `initial_void`.
///
/// Catches: an expiry audited by every coordinator that looks at it, one audited by none because
/// each thought another would, and a credential left unvoided.
#[test]
fn an_expired_file_is_voided_and_audited_once() {
    let db = TestDatabase::new();
    let dir = scratch("admin-rows-expired");
    let now = Arc::new(AtomicU64::new(NOW));
    for round in 1..=8_usize {
        let (user, id) = (format!("admin-{round}"), format!("file-{round}"));
        now.store(NOW, Ordering::SeqCst);
        put(&file(&dir), &id, &user, NOW + 100);
        let adopter = Side::start(&db, &file(&dir), &now, 3600);
        assert_eq!(adopter.looked().expect("looked"), Outcome::Adopted);
        adopter.store.stop();
        let (one, other) = (
            Side::start(&db, &file(&dir), &now, 3600),
            Side::start(&db, &file(&dir), &now, 3600),
        );
        now.store(NOW + 100, Ordering::SeqCst);
        let barrier = Barrier::new(2);
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                one.look()
            });
            let second = scope.spawn(|| {
                barrier.wait();
                other.look()
            });
            (
                first.join().expect("joined"),
                second.join().expect("joined"),
            )
        });
        // The one that finds the file gone found nothing to do.
        for outcome in [first.expect("looked"), second.expect("looked")] {
            assert!(
                matches!(outcome, Outcome::Expired | Outcome::Absent),
                "round {round}: {outcome:?}"
            );
        }
        assert!(!file(&dir).exists(), "round {round}");
        assert_eq!(audited(&db, EXPIRED), round, "round {round}");
        assert_eq!(row(&db, &user), [text(&id), text("true"), text("true")]);
        one.store.stop();
        other.store.stop();
    }
}

/// Proves: an expired file whose id the user's row does not hold (the user does not exist, or the
/// row holds another file's id) voids nothing, is audited once by the coordinator whose delete
/// removed it (the other looks at no file), and leaves the row of another file's credential as it
/// was.
///
/// Catches: the expiry of a file that was never adopted left unaudited, audited twice, or voiding
/// the credential of another file.
#[test]
fn an_expired_file_of_another_user_is_audited_by_the_coordinator_that_deleted_it() {
    let db = TestDatabase::new();
    let dir = scratch("admin-rows-other");
    let now = Arc::new(AtomicU64::new(NOW));
    // No such user.
    put(&file(&dir), "file-0", "nobody", NOW - 1);
    let one = Side::start(&db, &file(&dir), &now, 3600);
    let other = Side::start(&db, &file(&dir), &now, 3600);
    assert_eq!(one.looked().expect("looked"), Outcome::Expired);
    assert!(!file(&dir).exists());
    assert_eq!(other.looked().expect("looked"), Outcome::Absent);
    assert_eq!(audited(&db, EXPIRED), 1);
    assert_eq!(row(&db, "nobody"), Vec::<Option<String>>::new());
    one.store.stop();
    other.store.stop();

    // A user whose row holds another file's id.
    put(&file(&dir), "file-1", "admin", NOW + 100);
    let adopter = Side::start(&db, &file(&dir), &now, 3600);
    assert_eq!(adopter.looked().expect("looked"), Outcome::Adopted);
    adopter.store.stop();
    assert_eq!(audited(&db, BOOTSTRAP), 1);
    put(&file(&dir), "file-2", "admin", NOW - 1);
    let one = Side::start(&db, &file(&dir), &now, 3600);
    let other = Side::start(&db, &file(&dir), &now, 3600);
    assert_eq!(other.looked().expect("looked"), Outcome::Expired);
    assert_eq!(one.looked().expect("looked"), Outcome::Absent);
    assert_eq!(audited(&db, EXPIRED), 2);
    assert_eq!(
        row(&db, "admin"),
        [text("file-1"), text("false"), text("true")],
        "the credential of the other file is not voided"
    );
    one.store.stop();
    other.store.stop();
}

/// Proves: when the user changed the password before the file expired (the row holds the file's id
/// and no deadline), or another coordinator voided the credential already, the file is deleted and
/// nothing is audited and nothing is voided again.
///
/// Catches: an `initial_password_expired` entry for a password that was changed, and a void of a
/// credential the user has replaced.
#[test]
fn a_password_changed_before_expiry_is_not_audited() {
    let db = TestDatabase::new();
    let dir = scratch("admin-rows-changed");
    let now = Arc::new(AtomicU64::new(NOW));
    put(&file(&dir), "file-1", "admin", NOW + 100);
    let adopter = Side::start(&db, &file(&dir), &now, 3600);
    assert_eq!(adopter.looked().expect("looked"), Outcome::Adopted);
    db.run("UPDATE users SET record = record - 'initial_expires_at' WHERE name = 'admin'")
        .expect("the password is changed");
    now.store(NOW + 100, Ordering::SeqCst);
    assert_eq!(adopter.looked().expect("looked"), Outcome::Expired);
    assert!(!file(&dir).exists());
    assert_eq!(audited(&db, EXPIRED), 0);
    assert_eq!(
        row(&db, "admin"),
        [text("file-1"), text("false"), text("false")]
    );
    adopter.store.stop();

    // A credential that is void already: another coordinator audited its voiding.
    now.store(NOW, Ordering::SeqCst);
    put(&file(&dir), "file-2", "other", NOW + 100);
    let adopter = Side::start(&db, &file(&dir), &now, 3600);
    assert_eq!(adopter.looked().expect("looked"), Outcome::Adopted);
    db.run(
        "UPDATE users SET record = jsonb_set(record, '{initial_void}', 'true') \
         WHERE name = 'other'",
    )
    .expect("voided elsewhere");
    now.store(NOW + 100, Ordering::SeqCst);
    assert_eq!(adopter.looked().expect("looked"), Outcome::Expired);
    assert!(!file(&dir).exists());
    assert_eq!(audited(&db, EXPIRED), 0);
    adopter.store.stop();
}

/// Proves: an expired file that cannot be deleted (its directory refuses the delete) is refused
/// with the delete's error and stays; the credential is void; and once the file can be deleted the
/// expiry has been audited exactly once in all.
///
/// Catches: an expiry that is never audited because the void was written before a delete that
/// failed, and an expiry audited at every look while the file cannot be deleted.
#[test]
fn an_expired_file_that_cannot_be_deleted_is_audited_once_when_it_is_deleted() {
    let db = TestDatabase::new();
    let dir = scratch("admin-rows-undeletable");
    let now = Arc::new(AtomicU64::new(NOW));
    let sub = dir.path().join("volume");
    fs::create_dir(&sub).expect("a directory");
    let path = sub.join("initial-admin.toml");
    put(&path, "file-1", "admin", NOW + 100);
    let side = Side::start(&db, &path, &now, 3600);
    assert_eq!(side.looked().expect("looked"), Outcome::Adopted);
    now.store(NOW + 100, Ordering::SeqCst);
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o500)).expect("read-only");

    assert_eq!(
        side.looked().map(drop).map_err(|why| why.to_string()),
        Err("--initial-admin-file: the expired file was not deleted: permission denied".to_owned())
    );
    assert!(path.exists());
    assert_eq!(row(&db, "admin")[1], text("true"));
    assert!(audited(&db, EXPIRED) <= 1);

    fs::set_permissions(&sub, fs::Permissions::from_mode(0o700)).expect("writable");
    assert_eq!(side.looked().expect("looked"), Outcome::Expired);
    assert!(!path.exists());
    assert_eq!(audited(&db, EXPIRED), 1);
    side.store.stop();
}

/// Proves: over a store that is gone the look answers `InitialAdminError::Rows` and changes
/// nothing: for a new file (the row cannot be read), and for an expired one (the void cannot be
/// written, the file stays); the text names the flag and says why.
///
/// Catches: a database failure taken for "not adopted" (a user made in memory only, or an expired
/// file deleted and never audited), and a failure whose text hides what failed.
#[test]
fn rows_over_a_lost_store_answer_an_error_and_change_nothing() {
    let db = TestDatabase::new();
    let dir = scratch("admin-rows-lost");
    let now = Arc::new(AtomicU64::new(NOW));
    put(&file(&dir), "file-1", "admin", NOW + 3000);
    let side = Side::start(&db, &file(&dir), &now, 3600);
    side.store.stop();
    let why = |side: &Side| side.look().map(drop).map_err(|why| why.to_string());

    let refused = why(&side).expect_err("the rows are gone");
    assert_eq!(
        refused,
        "--initial-admin-file: the user's row could not be read or written: \
         the reading session stopped"
    );
    assert_eq!(side.accounts.snapshot().user("admin"), None);
    assert_eq!(audited(&db, BOOTSTRAP), 0);

    now.store(NOW + 3000, Ordering::SeqCst);
    let refused = why(&side).expect_err("the rows are gone");
    assert_eq!(
        refused,
        "--initial-admin-file: the user's row could not be read or written: \
         the writing session stopped"
    );
    assert!(file(&dir).exists());
    assert_eq!(audited(&db, EXPIRED), 0);
}

/// Proves: the rows' conditional writes answer as the look reads them: `adopt` is true for a new
/// user and for a row holding another id and false for one holding the id; `adopted` is true only
/// for the user and the id the row holds; `void` is true once, and false for another id, another
/// user, a row without a deadline and a row that is void.
///
/// Catches: a condition that is too wide (a void of another file's credential, an adoption
/// reported for a row that did not change) or too narrow (a credential that cannot be voided).
#[test]
fn the_rows_write_only_when_their_condition_holds() {
    let db = TestDatabase::new();
    let (store, _) = db.open("rows").expect("the store opens");
    let rows = PgAdmin::new(store.clone());
    let record = |id: &str, deadline: bool| {
        let mut record = serde_json::json!({
            "hash": "h", "enabled": true, "must_change": true, "initial_id": id,
            "created_by": "coordinator", "created_at": 1
        });
        if deadline {
            record["initial_expires_at"] = serde_json::json!(5);
        }
        record
    };
    off_runtime(move || {
        assert_eq!(rows.adopted("admin", "a"), Ok(false));
        assert_eq!(rows.void("admin", "a"), Ok(false));
        assert_eq!(rows.adopt("admin", record("a", true), "a"), Ok(true));
        assert_eq!(rows.adopt("admin", record("a", true), "a"), Ok(false));
        assert_eq!(rows.adopted("admin", "a"), Ok(true));
        assert_eq!(rows.adopted("admin", "b"), Ok(false));
        assert_eq!(rows.adopted("other", "a"), Ok(false));
        assert_eq!(rows.void("admin", "b"), Ok(false));
        assert_eq!(rows.void("other", "a"), Ok(false));
        assert_eq!(rows.void("admin", "a"), Ok(true));
        assert_eq!(rows.void("admin", "a"), Ok(false));
        assert_eq!(rows.adopt("admin", record("b", true), "b"), Ok(true));
        assert_eq!(rows.void("admin", "b"), Ok(true));
        assert_eq!(rows.adopt("admin", record("c", false), "c"), Ok(true));
        assert_eq!(
            rows.void("admin", "c"),
            Ok(false),
            "the password was changed"
        );
        store.stop();
        assert_eq!(
            rows.adopted("admin", "c").map_err(|why| why.is_empty()),
            Err(false)
        );
        assert_eq!(
            rows.adopt("admin", record("d", true), "d")
                .map_err(|why| why.is_empty()),
            Err(false)
        );
        assert_eq!(
            rows.void("admin", "c").map_err(|why| why.is_empty()),
            Err(false)
        );
    });
}

/// Proves: a coordinator whose memory does not hold the user (another coordinator adopted the file
/// after it started) that finds the file expired voids the row, audits `initial_password_expired`
/// once and deletes the file, and leaves its own memory without the user.
///
/// Catches: a void applied to an account this coordinator never held (a user made out of
/// nothing), and a void of the row that goes unaudited because the memory had no user.
#[test]
fn an_expiry_voided_for_a_user_this_coordinator_does_not_hold_leaves_its_memory_alone() {
    let db = TestDatabase::new();
    let dir = scratch("admin-rows-memory");
    let now = Arc::new(AtomicU64::new(NOW));
    put(&file(&dir), "file-1", "admin", NOW + 100);
    let first = Side::start(&db, &file(&dir), &now, 3600);
    assert_eq!(first.looked().expect("looked"), Outcome::Adopted);
    // Started after the adoption, so that its audit numbering follows the first's, but holding
    // no account: what a coordinator that read the users before the adoption holds.
    let mut second = Side::start(&db, &file(&dir), &now, 3600);
    let backend: Shared = Arc::new(second.store.clone());
    second.accounts = AccountStore::over(backend, crate::accounts::Accounts::default());
    assert_eq!(second.accounts.snapshot().user("admin"), None);

    now.store(NOW + 100, Ordering::SeqCst);
    assert_eq!(second.looked().expect("looked"), Outcome::Expired);
    assert!(!file(&dir).exists());
    assert_eq!(audited(&db, EXPIRED), 1);
    assert_eq!(row(&db, "admin")[1], text("true"));
    assert_eq!(second.accounts.snapshot().user("admin"), None);
    first.store.stop();
    second.store.stop();
}
