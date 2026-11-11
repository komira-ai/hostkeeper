//! The initial-admin file: what a look does to the users and the audit, which files it refuses
//! and with what text, and the tick that looks again. The wall clock is a number the test sets.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use hostkeeper_testkit::Scratch;
use hostkeeper_wire::secret::SecretError;

use crate::accounts::{AccountStore, Accounts, AccountsError};
use crate::audit_actions::about;
use crate::initial_admin::{FLAG, InitialAdmin, InitialAdminError, MAX_VALID_SECS, Outcome, watch};
use crate::ops::{AuditEntry, OpsState, OpsStore};
use crate::password::{PasswordSettings, Passwords, PolicyError, Verdict};
use crate::store::{Fake, audit_failure, store_failure};
use crate::testutil::{Rig, scratch};

const PASSWORD: &str = "k7Pq9xWm3Rt5Vb2NcH8dZs4F";
const NOW: u64 = 1_800_000_000;

fn cheap() -> PasswordSettings {
    PasswordSettings {
        memory_kib: 8,
        iterations: 1,
        parallelism: 1,
        min_length: 12,
    }
}

/// The text of a file with the given keys.
fn text(id: &str, user: &str, password: &str, valid_until: u64) -> String {
    format!(
        "id = \"{id}\"\nuser = \"{user}\"\npassword = \"{password}\"\nvalid_until = {valid_until}\n"
    )
}

/// A look at a file in a scratch directory, by a clock the test sets, over a fake store.
struct Look {
    dir: Scratch,
    now: Arc<AtomicU64>,
    fake: Fake,
    accounts: AccountStore,
    ops: OpsStore,
    admin: InitialAdmin,
}

impl Look {
    fn new(name: &str) -> Self {
        Self::with_max(name, 3600)
    }

    fn with_max(name: &str, max_secs: u64) -> Self {
        let dir = scratch(name);
        let now = Arc::new(AtomicU64::new(NOW));
        let fake = Fake::new();
        let accounts = AccountStore::over(Arc::new(fake.clone()), Accounts::default());
        let ops = OpsStore::over(Arc::new(fake.clone()), OpsState::default());
        let clock = Arc::clone(&now);
        let admin = InitialAdmin::new(
            dir.path().join("initial-admin.toml"),
            max_secs,
            cheap(),
            Arc::new(move || clock.load(Ordering::SeqCst)),
        )
        .expect("an initial admin");
        Self {
            dir,
            now,
            fake,
            accounts,
            ops,
            admin,
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.path().join("initial-admin.toml")
    }

    fn put(&self, text: &str, mode: u32) {
        // A file of mode 0400 is not written over: it is replaced, as the deploy step does.
        let _ = fs::remove_file(self.path());
        fs::write(self.path(), text).expect("written");
        fs::set_permissions(self.path(), fs::Permissions::from_mode(mode)).expect("mode");
    }

    /// A good file for `user`, good for `secs` from now.
    fn put_for(&self, id: &str, user: &str, secs: u64) {
        self.put(&text(id, user, PASSWORD, NOW + secs), 0o400);
    }

    fn at(&self, now: u64) {
        self.now.store(now, Ordering::SeqCst);
    }

    fn look(&self) -> Result<Outcome, InitialAdminError> {
        self.admin.look(&self.accounts, &self.ops)
    }

    fn audit(&self) -> Vec<AuditEntry> {
        self.ops.audit_after(0).expect("the audit")
    }
}

/// The audit entries `(action, subject)` of `look`, by `coordinator` and numbered from 1.
fn entries(actions: &[(&str, &str)]) -> Vec<AuditEntry> {
    actions
        .iter()
        .zip(1u64..)
        .map(|((action, user), seq)| AuditEntry {
            seq,
            ..about("coordinator", action, user)
        })
        .collect()
}

#[test]
fn a_fresh_file_makes_the_user_who_must_change_the_password_and_audits_the_bootstrap() {
    let look = Look::new("ia-fresh");
    look.put_for("id-1", "admin", 3000);
    assert_eq!(look.look().unwrap(), Outcome::Adopted);
    let held = look.accounts.snapshot();
    let admin = held.user("admin").expect("the user");
    assert!(admin.enabled && admin.must_change && !admin.initial_void);
    assert_eq!(admin.initial_expires_at, Some(NOW + 3000));
    assert_eq!(admin.initial_id.as_deref(), Some("id-1"));
    assert_eq!(admin.created_by, "coordinator");
    assert_eq!(admin.created_at, NOW);
    assert_eq!(admin.last_login_at, None);
    let passwords = Passwords::new(cheap()).unwrap();
    assert_eq!(
        passwords.verify(PASSWORD, &admin.hash.clone().into()),
        Verdict::Right { rehash: None }
    );
    assert_eq!(look.audit(), entries(&[("user_bootstrap", "admin")]));
    assert!(
        look.path().exists(),
        "the file stays until the password is changed or expires"
    );
}

#[test]
fn the_same_file_looked_at_again_changes_nothing() {
    let look = Look::new("ia-again");
    look.put_for("id-1", "admin", 3000);
    look.look().unwrap();
    let before = look.accounts.snapshot();
    let commits = look.fake.commits();
    assert_eq!(look.look().unwrap(), Outcome::AlreadyAdopted);
    assert_eq!(look.accounts.snapshot(), before);
    assert_eq!(look.fake.commits(), commits);
    assert_eq!(look.audit(), entries(&[("user_bootstrap", "admin")]));
}

#[test]
fn a_new_id_for_an_existing_user_gives_it_the_new_password_and_enables_it() {
    let look = Look::new("ia-breakglass");
    look.put(
        &text("id-1", "admin", "first password 1234", NOW + 100),
        0o400,
    );
    look.look().unwrap();
    // The user chose a password, and was then disabled in error.
    let passwords = Passwords::new(cheap()).unwrap();
    let chosen = passwords
        .hash("chosen by the admin")
        .unwrap()
        .as_str()
        .to_owned();
    look.accounts
        .change(|held| {
            held.with_user_edited("admin", |user| {
                user.hash.clone_from(&chosen);
                user.must_change = false;
                user.initial_expires_at = None;
                user.initial_void = true;
                user.last_login_at = Some(NOW + 5);
            })
            .and_then(|held| {
                held.with_user(crate::accounts::User {
                    name: "bob".to_owned(),
                    hash: chosen.clone(),
                    enabled: true,
                    created_by: "admin".to_owned(),
                    created_at: NOW,
                    ..crate::accounts::User::default()
                })
            })
            .and_then(|held| held.with_enabled("admin", false))
        })
        .unwrap();
    look.put(&text("id-2", "admin", PASSWORD, NOW + 500), 0o400);
    assert_eq!(look.look().unwrap(), Outcome::Adopted);
    let held = look.accounts.snapshot();
    let admin = held.user("admin").unwrap();
    assert!(admin.enabled && admin.must_change && !admin.initial_void);
    assert_eq!(admin.initial_expires_at, Some(NOW + 500));
    assert_eq!(admin.initial_id.as_deref(), Some("id-2"));
    assert_eq!(admin.last_login_at, Some(NOW + 5));
    assert_eq!(admin.created_by, "coordinator");
    assert_eq!(
        passwords.verify("chosen by the admin", &admin.hash.clone().into()),
        Verdict::Wrong
    );
    assert_eq!(
        passwords.verify(PASSWORD, &admin.hash.clone().into()),
        Verdict::Right { rehash: None }
    );
    assert!(held.user("bob").is_some(), "another user is as it was");
    assert_eq!(
        look.audit(),
        entries(&[("user_bootstrap", "admin"), ("user_bootstrap", "admin")])
    );
}

#[test]
fn an_expired_file_is_deleted_voids_the_initial_password_and_audits_the_expiry() {
    let look = Look::new("ia-expired");
    look.put_for("id-1", "admin", 3000);
    look.look().unwrap();
    look.at(NOW + 3000);
    assert_eq!(look.look().unwrap(), Outcome::Expired);
    assert!(!look.path().exists());
    let held = look.accounts.snapshot();
    let admin = held.user("admin").expect("the record stays");
    assert!(admin.initial_void);
    assert_eq!(admin.initial_expires_at, Some(NOW + 3000));
    assert_eq!(
        look.audit(),
        entries(&[
            ("user_bootstrap", "admin"),
            ("initial_password_expired", "admin")
        ])
    );
    assert_eq!(look.look().unwrap(), Outcome::Absent);
}

#[test]
fn a_file_one_second_before_its_deadline_is_adopted_and_at_the_deadline_it_is_not() {
    let look = Look::new("ia-boundary");
    look.put_for("id-1", "admin", 10);
    look.at(NOW + 9);
    assert_eq!(look.look().unwrap(), Outcome::Adopted);
    let other = Look::new("ia-boundary-at");
    other.put_for("id-1", "admin", 10);
    other.at(NOW + 10);
    assert_eq!(other.look().unwrap(), Outcome::Expired);
    assert!(other.accounts.snapshot().user("admin").is_none());
}

#[test]
fn an_expired_file_nobody_adopted_is_deleted_makes_no_user_and_is_audited() {
    let look = Look::new("ia-unadopted");
    look.put_for("id-1", "admin", 100);
    look.at(NOW + 5000);
    assert_eq!(look.look().unwrap(), Outcome::Expired);
    assert!(!look.path().exists());
    assert!(look.accounts.snapshot().user("admin").is_none());
    assert_eq!(
        look.audit(),
        entries(&[("initial_password_expired", "admin")])
    );
}

#[test]
fn an_expired_file_of_a_password_already_changed_is_deleted_and_touches_nothing() {
    let look = Look::new("ia-changed");
    look.put_for("id-1", "admin", 100);
    look.look().unwrap();
    look.accounts
        .change(|held| {
            held.with_user_edited("admin", |user| {
                user.must_change = false;
                user.initial_expires_at = None;
            })
        })
        .unwrap();
    let before = look.accounts.snapshot();
    look.at(NOW + 5000);
    assert_eq!(look.look().unwrap(), Outcome::Expired);
    assert!(!look.path().exists());
    assert_eq!(look.accounts.snapshot(), before);
    assert_eq!(look.audit(), entries(&[("user_bootstrap", "admin")]));
}

#[test]
fn an_expired_file_whose_id_the_record_did_not_adopt_leaves_the_record_alone() {
    let look = Look::new("ia-other-id");
    look.put_for("id-1", "admin", 100);
    look.look().unwrap();
    look.put_for("id-0", "admin", 50);
    look.at(NOW + 70);
    // The record was made by `id-1`, which is still good, so `id-0`'s expiry changes nothing.
    assert_eq!(look.look().unwrap(), Outcome::Expired);
    let held = look.accounts.snapshot();
    assert!(!held.user("admin").unwrap().initial_void);
    assert!(!look.path().exists());
}

#[test]
fn an_expired_file_is_deleted_even_when_the_password_would_not_pass_the_policy() {
    let look = Look::new("ia-expired-weak");
    look.put(&text("id-1", "admin", "short", NOW - 1), 0o400);
    assert_eq!(look.look().unwrap(), Outcome::Expired);
    assert!(!look.path().exists());
}

#[test]
fn a_file_that_asks_for_more_than_the_setting_is_refused_and_the_boundary_is_taken() {
    let look = Look::with_max("ia-too-far", 600);
    look.put_for("id-1", "admin", 601);
    assert_eq!(
        look.look().unwrap_err().to_string(),
        "--initial-admin-file: valid_until is more than --initial-password-secs (600) from now"
    );
    assert!(look.accounts.snapshot().is_empty());
    assert!(look.audit().is_empty());
    assert!(look.path().exists());
    look.put_for("id-1", "admin", 600);
    assert_eq!(look.look().unwrap(), Outcome::Adopted);
}

#[test]
fn a_setting_above_an_hour_is_an_hour() {
    let look = Look::with_max("ia-cap", 7200);
    look.put_for("id-1", "admin", MAX_VALID_SECS + 1);
    assert!(matches!(
        look.look(),
        Err(InitialAdminError::TooFar { secs: 3600 })
    ));
    look.put_for("id-1", "admin", MAX_VALID_SECS);
    assert_eq!(look.look().unwrap(), Outcome::Adopted);
}

#[test]
fn a_password_the_policy_refuses_is_refused_by_the_rule_and_never_quoted() {
    let look = Look::new("ia-policy");
    look.put(&text("id-1", "admin", "tooshort", NOW + 100), 0o400);
    let why = look.look().unwrap_err();
    assert!(matches!(
        why,
        InitialAdminError::Policy(PolicyError::TooShort { min: 12 })
    ));
    assert_eq!(
        why.to_string(),
        "--initial-admin-file: password: the password must be at least 12 characters"
    );
    assert!(look.accounts.snapshot().is_empty());
    look.put(
        &text("id-1", "administrators", "administrators", NOW + 100),
        0o400,
    );
    assert_eq!(
        look.look().unwrap_err().to_string(),
        "--initial-admin-file: password: the password must not be the user name"
    );
}

#[test]
fn a_file_that_does_not_read_is_refused_with_text_that_holds_no_password() {
    let look = Look::new("ia-shapes");
    let long_id = "x".repeat(129);
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            b"this is { not toml".to_vec(),
            "--initial-admin-file: it is not TOML holding id, user, password and valid_until",
        ),
        (
            format!("{}other = 1\n", text("a", "admin", PASSWORD, NOW + 9)).into_bytes(),
            "--initial-admin-file: it is not TOML holding id, user, password and valid_until",
        ),
        (
            format!("id = \"a\"\nuser = \"admin\"\npassword = {PASSWORD:?}\n").into_bytes(),
            "--initial-admin-file: it is not TOML holding id, user, password and valid_until",
        ),
        (
            format!(
                "id = \"a\"\nuser = \"admin\"\npassword = 123456789012\nvalid_until = {}\n",
                NOW + 9
            )
            .into_bytes(),
            "--initial-admin-file: it is not TOML holding id, user, password and valid_until",
        ),
        (
            text("", "admin", PASSWORD, NOW + 9).into_bytes(),
            "--initial-admin-file: id is 1 to 128 bytes of visible ASCII",
        ),
        (
            text("has space", "admin", PASSWORD, NOW + 9).into_bytes(),
            "--initial-admin-file: id is 1 to 128 bytes of visible ASCII",
        ),
        (
            text(&long_id, "admin", PASSWORD, NOW + 9).into_bytes(),
            "--initial-admin-file: id is 1 to 128 bytes of visible ASCII",
        ),
        (
            text("a", "Admin", PASSWORD, NOW + 9).into_bytes(),
            "--initial-admin-file: user: a token name is 1 to 32 bytes of a-z, 0-9 and -",
        ),
        (
            text("a", "coordinator", PASSWORD, NOW + 9).into_bytes(),
            "--initial-admin-file: user: a token may not be named coordinator: the audit uses that name for changes that are not an operator's",
        ),
        (
            vec![0xff, 0xfe, 0x00],
            "--initial-admin-file: it is not UTF-8",
        ),
        (
            vec![b'#'; 4097],
            "--initial-admin-file: it is over 4096 bytes",
        ),
    ];
    for (bytes, expected) in cases {
        fs::write(look.path(), &bytes).unwrap();
        fs::set_permissions(look.path(), fs::Permissions::from_mode(0o400)).unwrap();
        let why = look.look().unwrap_err().to_string();
        assert_eq!(why, expected);
        assert!(!why.contains(PASSWORD) && !why.contains("123456789012"));
        assert!(look.accounts.snapshot().is_empty());
        fs::remove_file(look.path()).unwrap();
    }
    assert!(look.audit().is_empty());
}

#[test]
fn no_file_is_nothing_and_a_file_of_the_wrong_mode_is_the_secret_file_error() {
    let look = Look::new("ia-modes");
    assert_eq!(look.look().unwrap(), Outcome::Absent);
    look.put(&text("a", "admin", PASSWORD, NOW + 9), 0o644);
    let why = look.look().unwrap_err();
    assert!(
        matches!(&why, InitialAdminError::Read(SecretError::Rule { flag, .. }) if flag == FLAG),
        "{why}"
    );
    assert!(!why.to_string().contains(PASSWORD));
    assert!(look.accounts.snapshot().is_empty());
}

#[test]
fn a_store_that_cannot_write_leaves_the_file_for_the_next_look() {
    let look = Look::new("ia-store-down");
    look.put_for("id-1", "admin", 3000);
    // The audit is taken; the accounts are not.
    look.fake
        .refuse(|commit| commit.accounts.is_some().then(store_failure));
    assert!(matches!(
        look.look(),
        Err(InitialAdminError::Store(AccountsError::Io(_)))
    ));
    assert!(look.accounts.snapshot().is_empty());
    assert!(look.path().exists());
    look.fake.heal();
    assert_eq!(look.look().unwrap(), Outcome::Adopted);
}

#[test]
fn an_audit_that_cannot_be_written_is_an_error_of_the_look() {
    let look = Look::new("ia-audit-down");
    look.put_for("id-1", "admin", 3000);
    look.fake
        .refuse(|commit| commit.accounts.is_none().then(audit_failure));
    let why = look.look().unwrap_err();
    assert!(matches!(why, InitialAdminError::Audit(_)), "{why}");
    assert!(
        why.to_string()
            .starts_with("--initial-admin-file: the audit entry was not written")
    );
}

#[test]
fn a_bootstrap_whose_audit_failed_once_is_audited_by_the_next_look() {
    let look = Look::new("ia-audit-once");
    look.put_for("id-1", "admin", 3000);
    look.fake
        .refuse(|commit| commit.accounts.is_none().then(audit_failure));
    assert!(matches!(look.look(), Err(InitialAdminError::Audit(_))));
    assert!(look.accounts.snapshot().is_empty());
    look.fake.heal();
    assert_eq!(look.look().unwrap(), Outcome::Adopted);
    assert_eq!(look.look().unwrap(), Outcome::AlreadyAdopted);
    assert_eq!(look.audit(), entries(&[("user_bootstrap", "admin")]));
}

#[test]
fn an_expired_file_that_is_already_gone_when_deleted_is_deleted() {
    let look = Look::new("ia-gone");
    look.put_for("id-1", "admin", 100);
    look.at(NOW + 500);
    // The file is read, then another look deletes it before this one does.
    let file = super::read(&look.path()).unwrap().unwrap();
    fs::remove_file(look.path()).unwrap();
    assert_eq!(
        look.admin.expire(&file, &look.accounts, &look.ops).unwrap(),
        Outcome::Expired
    );
}

#[tokio::test]
async fn a_look_that_panicked_or_was_cancelled_is_a_refusal_that_says_so() {
    let panicked = tokio::task::spawn_blocking(|| -> Result<Option<Outcome>, InitialAdminError> {
        panic!("a look that panics")
    })
    .await;
    let why = super::joined(panicked).unwrap_err();
    assert_eq!(
        why.to_string(),
        "--initial-admin-file: the look did not finish (it panicked)"
    );
    let task = tokio::spawn(std::future::pending::<
        Result<Option<Outcome>, InitialAdminError>,
    >());
    task.abort();
    let why = super::joined(task.await).unwrap_err();
    assert_eq!(
        why.to_string(),
        "--initial-admin-file: the look did not finish (it was cancelled)"
    );
}

#[test]
fn an_expired_file_whose_record_cannot_be_voided_stays_for_the_next_look() {
    let look = Look::new("ia-void-fails");
    look.put_for("id-1", "admin", 100);
    look.look().unwrap();
    look.at(NOW + 500);
    look.fake.fail();
    assert!(matches!(
        look.look(),
        Err(InitialAdminError::Store(AccountsError::Io(_)))
    ));
    assert!(
        look.path().exists(),
        "the file is deleted only after the void"
    );
    look.fake.heal();
    assert_eq!(look.look().unwrap(), Outcome::Expired);
    assert!(look.accounts.snapshot().user("admin").unwrap().initial_void);
    assert!(!look.path().exists());
}

#[test]
fn an_expiry_that_cannot_be_audited_leaves_the_file_for_the_next_look() {
    let look = Look::new("ia-expiry-audit-down");
    look.put_for("id-1", "admin", 100);
    look.at(NOW + 500);
    look.fake
        .refuse(|commit| commit.accounts.is_none().then(audit_failure));
    let why = look.look().unwrap_err();
    assert!(matches!(why, InitialAdminError::Audit(_)), "{why}");
    assert!(look.path().exists());
    look.fake.heal();
    assert_eq!(look.look().unwrap(), Outcome::Expired);
    assert!(!look.path().exists());
    assert_eq!(
        look.audit(),
        entries(&[("initial_password_expired", "admin")])
    );
}

#[test]
fn an_expired_file_in_a_directory_that_cannot_be_written_stays_and_the_error_says_so() {
    if hostkeeper_wire::secret::euid() == 0 {
        eprintln!("skipped: root writes into a directory whatever its mode");
        return;
    }
    let look = Look::new("ia-delete-fails");
    look.put_for("id-1", "admin", 100);
    look.at(NOW + 500);
    fs::set_permissions(look.dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let why = look.look().unwrap_err();
    fs::set_permissions(look.dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        matches!(
            why,
            InitialAdminError::Delete(std::io::ErrorKind::PermissionDenied)
        ),
        "{why}"
    );
    assert_eq!(
        why.to_string(),
        "--initial-admin-file: the expired file was not deleted: permission denied"
    );
    assert!(look.path().exists());
    assert_eq!(
        look.audit(),
        entries(&[("initial_password_expired", "admin")])
    );
}

#[test]
fn password_settings_the_algorithm_refuses_stop_the_set_up() {
    let refused = InitialAdmin::new(
        PathBuf::from("unused"),
        3600,
        PasswordSettings {
            memory_kib: 1,
            ..cheap()
        },
        Arc::new(|| 0),
    );
    assert!(matches!(refused, Err(InitialAdminError::Hash(_))));
}

#[test]
fn nothing_of_the_file_is_in_the_debug_text() {
    let look = Look::new("ia-debug");
    assert_eq!(
        format!("{:?}", look.admin),
        "InitialAdmin { max_secs: 3600, .. }"
    );
}

#[test]
fn a_coordinator_without_the_flag_looks_at_nothing_and_with_it_acts() {
    let dir = scratch("ia-coordinator");
    let clock = Arc::new(AtomicU64::new(1_000));
    let plain = Rig::start(&dir, &clock);
    assert_eq!(plain.look_at_initial_admin().unwrap(), None);
    let now = Arc::new(AtomicU64::new(NOW));
    let path = dir.path().join("initial-admin.toml");
    fs::write(&path, text("id-1", "admin", PASSWORD, NOW + 100)).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
    let watched = Rig::start(&dir, &clock).with_initial_admin(
        InitialAdmin::new(
            path,
            3600,
            cheap(),
            Arc::new(move || now.load(Ordering::SeqCst)),
        )
        .unwrap(),
    );
    assert_eq!(
        watched.look_at_initial_admin().unwrap(),
        Some(Outcome::Adopted)
    );
    assert!(watched.accounts().snapshot().user("admin").is_some());
}

#[tokio::test(start_paused = true)]
async fn the_tick_applies_a_file_that_appears_and_refuses_one_that_is_bad_without_stopping() {
    // A tracing macro evaluates its fields only under a subscriber: the logs go nowhere.
    let _log = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(std::io::sink)
            .finish(),
    );
    let dir = scratch("ia-tick");
    let clock = Arc::new(AtomicU64::new(1_000));
    let now = Arc::new(AtomicU64::new(NOW));
    let path = dir.path().join("initial-admin.toml");
    let clock_of_file = Arc::clone(&now);
    let coordinator = Arc::new(
        Rig::start(&dir, &clock).with_initial_admin(
            InitialAdmin::new(
                path.clone(),
                3600,
                cheap(),
                Arc::new(move || clock_of_file.load(Ordering::SeqCst)),
            )
            .unwrap(),
        ),
    );
    let (done, mut looks) = tokio::sync::mpsc::unbounded_channel();
    let interval = Duration::from_secs(60);
    let watching = tokio::spawn(watch(Arc::clone(&coordinator), interval, done));
    let put = |body: &str, mode: u32| {
        let _ = fs::remove_file(&path);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    };

    // A look with no file does nothing.
    tokio::time::advance(interval).await;
    looks.recv().await.expect("a look");
    assert!(coordinator.accounts().snapshot().is_empty());

    // A bad file is refused and the tick goes on.
    put("not toml {", 0o400);
    tokio::time::advance(interval).await;
    looks.recv().await.expect("a look");
    assert!(coordinator.accounts().snapshot().is_empty());

    // A good one is adopted at the next look, and a look after that does nothing more.
    put(&text("id-1", "admin", PASSWORD, NOW + 3000), 0o400);
    tokio::time::advance(interval).await;
    looks.recv().await.expect("a look");
    assert!(coordinator.accounts().snapshot().user("admin").is_some());
    tokio::time::advance(interval).await;
    looks.recv().await.expect("a look");

    // The hour passes: the file is deleted.
    now.store(NOW + 3000, Ordering::SeqCst);
    tokio::time::advance(interval).await;
    looks.recv().await.expect("a look");
    assert!(!path.exists());
    assert!(
        coordinator
            .accounts()
            .snapshot()
            .user("admin")
            .unwrap()
            .initial_void
    );
    watching.abort();
}

#[test]
fn the_file_a_password_came_from_is_deleted_by_its_user_and_id() {
    let look = Look::new("ia-delete-adopted");
    look.put_for("id-1", "admin", 3000);
    look.admin.delete_if_adopted("admin", "id-1").unwrap();
    assert!(!look.path().exists());
    // Already gone: nothing to do, and no error.
    look.admin.delete_if_adopted("admin", "id-1").unwrap();
}

#[test]
fn a_file_of_another_user_or_another_id_stays_when_a_password_changes() {
    let look = Look::new("ia-delete-other");
    look.put_for("id-2", "admin", 3000);
    look.admin.delete_if_adopted("admin", "id-1").unwrap();
    assert!(look.path().exists(), "another id");
    look.admin.delete_if_adopted("other", "id-2").unwrap();
    assert!(look.path().exists(), "another user");
}

#[test]
fn a_file_that_does_not_read_is_an_error_of_the_deletion_and_stays() {
    let look = Look::new("ia-delete-unreadable");
    look.put(&text("id-1", "admin", PASSWORD, NOW + 3000), 0o644);
    let why = look.admin.delete_if_adopted("admin", "id-1").unwrap_err();
    assert!(matches!(why, InitialAdminError::Read(_)), "{why}");
    assert!(look.path().exists());
}

#[test]
fn a_file_that_cannot_be_deleted_says_so_when_a_password_changes() {
    if hostkeeper_wire::secret::euid() == 0 {
        eprintln!("skipped: root writes into a directory whatever its mode");
        return;
    }
    let look = Look::new("ia-delete-adopted-fails");
    look.put_for("id-1", "admin", 3000);
    fs::set_permissions(look.dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let why = look.admin.delete_if_adopted("admin", "id-1").unwrap_err();
    fs::set_permissions(look.dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        matches!(
            why,
            InitialAdminError::Delete(std::io::ErrorKind::PermissionDenied)
        ),
        "{why}"
    );
    assert!(look.path().exists());
}
