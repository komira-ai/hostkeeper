//! The tests of `ssh_setup`: the setup over a scripted host and scratch directories. The host
//! answers `sshd -T` from the files on disk: the stock settings the test gives, plus the drop-in's
//! trust lines and password lines once the drop-in is there, so a rule that reads the effective
//! configuration after a change sees what the change made.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use hostkeeper_link::testing::RecordingLog;
use hostkeeper_testkit::Scratch;

use super::*;
use crate::ssh_files::render_drop_in;
use crate::ssh_host::Account;

const STOCK: &str = "port 22\n\
    listenaddress 0.0.0.0:22\n\
    pubkeyauthentication yes\n\
    authenticationmethods any\n\
    permitrootlogin prohibit-password\n\
    passwordauthentication yes\n\
    kbdinteractiveauthentication yes\n\
    usepam yes\n";

const ED: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

struct Fake {
    unit: Result<SshdUnit, UnitError>,
    stock: BTreeMap<String, String>,
    accounts: Vec<Account>,
    groups: Vec<String>,
    calls: Vec<String>,
    effective_calls: usize,
    start_fails: bool,
    revocations_fail: bool,
    candidate_fails: bool,
    banner_fails: bool,
    /// The first this many reloads fail.
    reload_failures: usize,
    /// A path a reload turns into a directory, so that a file cannot be put back there.
    sabotage: Option<std::path::PathBuf>,
    /// `(user, only once the drop-in is there)`: sshd -T fails for the user.
    fail_effective: Option<(String, bool)>,
    shadow: bool,
    shadow_passwords: bool,
    /// An account whose settings the drop-in changes.
    change_user: Option<String>,
    fingerprints: Vec<String>,
}

struct FakeHost {
    paths: SshPaths,
    fake: Mutex<Fake>,
}

impl FakeHost {
    fn lock(&self) -> std::sync::MutexGuard<'_, Fake> {
        self.fake.lock().expect("the fake")
    }

    fn calls(&self) -> Vec<String> {
        self.lock().calls.clone()
    }
}

impl SshHost for FakeHost {
    fn prepare(&self, requested: &str, _passwords_off: bool) -> Result<String, Refused> {
        Ok(requested.to_owned())
    }

    fn unit(&self) -> Result<SshdUnit, UnitError> {
        self.lock().unit.clone()
    }

    fn start(&self, _unit: &SshdUnit) -> Result<(), String> {
        let mut fake = self.lock();
        fake.calls.push("start".to_owned());
        if fake.start_fails {
            Err("no".to_owned())
        } else {
            Ok(())
        }
    }

    fn reload(&self, unit: &SshdUnit) -> Result<(), String> {
        let mut fake = self.lock();
        fake.calls.push(match unit {
            SshdUnit::Service(_) => "reload".to_owned(),
            other => format!("reload {other:?}"),
        });
        if let Some(path) = &fake.sabotage {
            let _ = fs::remove_file(path);
            fs::create_dir_all(path).expect("a directory in the way");
        }
        if fake.reload_failures > 0 {
            fake.reload_failures -= 1;
            return Err("no".to_owned());
        }
        Ok(())
    }

    fn check_config(&self, candidate: &Path) -> Result<(), String> {
        let mut fake = self.lock();
        let text = fs::read_to_string(candidate).expect("the candidate is where sshd -t reads it");
        assert!(text.contains("TrustedUserCAKeys"), "{text}");
        fake.calls.push("check".to_owned());
        if fake.candidate_fails {
            Err("no".to_owned())
        } else {
            Ok(())
        }
    }

    fn effective(&self, user: &str) -> Result<Effective, String> {
        let mut fake = self.lock();
        fake.effective_calls += 1;
        let installed = self.paths.drop_in().exists();
        if let Some((who, after_only)) = &fake.fail_effective
            && who == user
            && (!after_only || installed)
        {
            return Err("no".to_owned());
        }
        let mut text = fake
            .stock
            .get(user)
            .cloned()
            .unwrap_or_else(|| STOCK.to_owned());
        if installed {
            let drop_in = fs::read_to_string(self.paths.drop_in()).unwrap_or_default();
            if drop_in.contains("PasswordAuthentication no") && !fake.shadow_passwords {
                text = text
                    .replace("passwordauthentication yes", "passwordauthentication no")
                    .replace(
                        "kbdinteractiveauthentication yes",
                        "kbdinteractiveauthentication no",
                    );
            }
            if !fake.shadow {
                text += &format!(
                    "trustedusercakeys {}\nauthorizedprincipalsfile {}\nrevokedkeys {}\n",
                    self.paths.ca_keys().display(),
                    self.paths.principals_template(),
                    self.paths.revoked().display()
                );
            }
            if fake.change_user.as_deref() == Some(user) {
                text += "denyusers zed\n";
            }
        }
        Ok(Effective::parse(&text))
    }

    fn groups(&self, _user: &str) -> Vec<String> {
        self.lock().groups.clone()
    }

    fn banner(&self, address: &(String, u16)) -> Result<(), String> {
        let mut fake = self.lock();
        fake.calls
            .push(format!("banner {}:{}", address.0, address.1));
        if fake.banner_fails {
            Err("no".to_owned())
        } else {
            Ok(())
        }
    }

    fn accounts(&self) -> Vec<Account> {
        self.lock().accounts.clone()
    }

    fn host_key_fingerprints(&self) -> Vec<String> {
        self.lock().fingerprints.clone()
    }

    fn revocations(&self, _ca_keys: &[String], serials: &[u64]) -> Result<Vec<u8>, String> {
        let mut fake = self.lock();
        fake.calls.push("revocations".to_owned());
        if fake.revocations_fail {
            return Err("no".to_owned());
        }
        let list: Vec<String> = serials.iter().map(u64::to_string).collect();
        Ok(format!("krl:{}\n", list.join(",")).into_bytes())
    }
}

struct Rig {
    host: Arc<FakeHost>,
    setup: SshSetup,
    paths: SshPaths,
    log: Arc<RecordingLog>,
    _dir: Scratch,
}

impl Rig {
    fn new(name: &str) -> Rig {
        let exe = std::env::current_exe().expect("the test binary");
        let target = exe.ancestors().nth(3).expect("the target directory");
        let dir = Scratch::under(&target.join("tmp"), name).expect("a scratch directory");
        let paths = SshPaths {
            config_dir: dir.path().join("sshd_config.d"),
            own_dir: dir.path().join("hostkeeper"),
        };
        let host = Arc::new(FakeHost {
            paths: paths.clone(),
            fake: Mutex::new(Fake {
                unit: Ok(SshdUnit::Service("ssh.service".to_owned())),
                stock: BTreeMap::new(),
                accounts: Vec::new(),
                groups: vec!["root".to_owned()],
                calls: Vec::new(),
                effective_calls: 0,
                start_fails: false,
                revocations_fail: false,
                candidate_fails: false,
                banner_fails: false,
                reload_failures: 0,
                sabotage: None,
                fail_effective: None,
                shadow: false,
                shadow_passwords: false,
                change_user: None,
                fingerprints: vec!["SHA256:aaa".to_owned()],
            }),
        });
        let log = Arc::new(RecordingLog::default());
        let setup = SshSetup::new(
            Arc::clone(&host) as Arc<dyn SshHost>,
            paths.clone(),
            rustix::process::geteuid().as_raw(),
            log.clone(),
        );
        Rig {
            host,
            setup,
            paths,
            log,
            _dir: dir,
        }
    }

    fn apply(&self, access: &PushedSsh) -> SshReport {
        self.setup.apply(access);
        self.setup.report().expect("a report after a record")
    }

    fn files(&self) -> Vec<String> {
        let mut found = Vec::new();
        for dir in [&self.paths.config_dir, &self.paths.own_dir] {
            if let Ok(entries) = fs::read_dir(dir) {
                found.extend(
                    entries
                        .filter_map(Result::ok)
                        .map(|e| e.file_name().to_string_lossy().into_owned()),
                );
            }
        }
        found.sort();
        found
    }
}

fn access(generation: u64) -> PushedSsh {
    PushedSsh {
        generation,
        enabled: true,
        ca_public_keys: vec![ED.to_owned()],
        login_user: "root".to_owned(),
        principal: "hk-host-web-04".to_owned(),
        revoked_serials: vec![3, 9],
        password_auth: "keep".to_owned(),
    }
}

fn off(generation: u64) -> PushedSsh {
    PushedSsh {
        enabled: false,
        ..access(generation)
    }
}

fn account(name: &str, keyed: bool, password: bool) -> Account {
    Account {
        name: name.to_owned(),
        keyed,
        password,
    }
}

/// Proves the first record is applied in the design's order: the unit is started, the revoked
/// list built, the candidate checked, the files written (the bytes of the design), sshd reloaded
/// and a banner read from the listening address; the report says `applied` at its generation
/// with sshd's host key fingerprints and no detail, and the log says so.
#[test]
fn the_first_record_is_applied_in_order() {
    let rig = Rig::new("ssh-first");
    let report = rig.apply(&access(1));
    assert_eq!(
        report,
        SshReport {
            applied_generation: 1,
            state: "applied".to_owned(),
            host_key_fingerprints: vec!["SHA256:aaa".to_owned()],
            detail: String::new(),
        }
    );
    assert_eq!(
        rig.host.calls(),
        [
            "start",
            "revocations",
            "check",
            "reload",
            "banner 127.0.0.1:22"
        ]
    );
    assert_eq!(
        rig.files(),
        [
            "10-hostkeeper.conf",
            "principals.root",
            "revoked",
            "user_ca.pub"
        ]
    );
    assert_eq!(
        fs::read(rig.paths.drop_in()).expect("the drop-in"),
        render_drop_in(&rig.paths, false)
    );
    assert_eq!(
        fs::read_to_string(rig.paths.ca_keys()).expect("keys"),
        format!("{ED}\n")
    );
    assert_eq!(
        fs::read_to_string(rig.paths.principals("root")).expect("principals"),
        "hk-host-web-04\n"
    );
    assert_eq!(
        fs::read_to_string(rig.paths.revoked()).expect("revoked"),
        "krl:3,9\n"
    );
    assert_eq!(rig.log.lines(), ["ssh generation 1: applied"]);
    assert!(!rig.paths.candidate().exists());
}

/// Proves a daemon with nothing to report before it has taken up a record says nothing.
#[test]
fn nothing_is_reported_before_a_record() {
    let rig = Rig::new("ssh-silent");
    assert_eq!(rig.setup.report(), None);
}

/// Proves the same generation again does nothing, and a new generation with the same content
/// checks and reads the banner but writes nothing, reloads nothing and builds no new list.
#[test]
fn a_record_again_changes_nothing() {
    let rig = Rig::new("ssh-again");
    rig.apply(&access(1));
    let before = rig.host.calls();
    let effective_calls = rig.host.lock().effective_calls;
    assert_eq!(rig.apply(&access(1)).applied_generation, 1);
    assert_eq!(rig.host.calls(), before);
    assert_eq!(
        rig.host.lock().effective_calls,
        effective_calls,
        "nothing was asked of sshd"
    );

    let report = rig.apply(&access(2));
    assert_eq!(
        (report.applied_generation, report.state.as_str()),
        (2, "applied")
    );
    assert_eq!(
        rig.host.calls()[before.len()..],
        ["start", "check", "banner 127.0.0.1:22"]
    );
}

/// Proves a new list of revoked serials is built and written, at its generation, and a reload
/// follows because a file changed.
#[test]
fn new_revoked_serials_are_written() {
    let rig = Rig::new("ssh-serials");
    rig.apply(&access(1));
    let mut next = access(2);
    next.revoked_serials = vec![3, 9, 12];
    rig.apply(&next);
    assert_eq!(
        fs::read_to_string(rig.paths.revoked()).expect("revoked"),
        "krl:3,9,12\n"
    );
    let calls = rig.host.calls();
    assert_eq!(
        calls[calls.len() - 4..],
        ["revocations", "check", "reload", "banner 127.0.0.1:22"]
    );
}

/// Proves a file of an applied record that was changed or removed is put right at the next
/// heartbeat of the same generation, and a file that is as it should be is not rewritten.
#[test]
fn a_drifted_file_is_put_right_at_the_same_generation() {
    let rig = Rig::new("ssh-drift");
    rig.apply(&access(1));
    fs::write(rig.paths.ca_keys(), "ssh-ed25519 AAAA\n").expect("a drift");
    rig.apply(&access(1));
    assert_eq!(
        fs::read_to_string(rig.paths.ca_keys()).expect("keys"),
        format!("{ED}\n")
    );
    assert_eq!(
        rig.host.calls().iter().filter(|c| *c == "reload").count(),
        2
    );
    fs::remove_file(rig.paths.revoked()).expect("a drift");
    rig.apply(&access(1));
    assert!(rig.paths.revoked().is_file());
    let calls = rig.host.calls().len();
    rig.apply(&access(1));
    assert_eq!(rig.host.calls().len(), calls, "nothing drifted");

    let mut serials = access(1);
    serials.revoked_serials = vec![3, 9, 12];
    rig.apply(&serials);
    assert_eq!(
        fs::read_to_string(rig.paths.revoked()).expect("revoked"),
        "krl:3,9,12\n",
        "other serials at the same generation are taken up too"
    );
}

/// Proves every refusal before a change leaves the host untouched: no file, no start, no reload,
/// and the report names the state and the keyword or class that failed, never an account: no login
/// user, a record that cannot be written safely, no unit (missing, masked, no manager), an
/// effective configuration sshd cannot print, each allow and deny list that keeps the login user
/// out, and each setting that keeps a certificate from logging in.
#[test]
fn a_refusal_before_a_change_touches_nothing() {
    type Case = (
        &'static str,
        Box<dyn Fn(&mut PushedSsh, &mut Fake)>,
        &'static str,
        &'static str,
    );
    fn case(
        name: &'static str,
        change: impl Fn(&mut PushedSsh, &mut Fake) + 'static,
        state: &'static str,
        detail: &'static str,
    ) -> Case {
        (name, Box::new(change), state, detail)
    }
    let cases: Vec<Case> = vec![
        case(
            "no login user",
            |a, _| a.login_user.clear(),
            "no_login_user",
            "login_user",
        ),
        case(
            "bad principal",
            |a, _| a.principal = "root".to_owned(),
            "config_invalid",
            "principal",
        ),
        case(
            "no unit",
            |_, f| f.unit = Err(UnitError::Missing),
            "sshd_missing",
            "unit",
        ),
        case(
            "masked",
            |_, f| f.unit = Err(UnitError::Masked),
            "unsupported",
            "unit_masked",
        ),
        case(
            "no manager",
            |_, f| f.unit = Err(UnitError::NoManager),
            "unsupported",
            "service_manager",
        ),
        case(
            "no effective",
            |_, f| f.fail_effective = Some(("root".to_owned(), false)),
            "config_invalid",
            "sshd_config",
        ),
        case(
            "allowusers",
            |_, f| {
                f.stock
                    .insert("root".to_owned(), STOCK.to_owned() + "allowusers alice\n");
            },
            "blocked_by_allow_list",
            "allowusers",
        ),
        case(
            "denyusers",
            |_, f| {
                f.stock
                    .insert("root".to_owned(), STOCK.to_owned() + "denyusers root\n");
            },
            "blocked_by_allow_list",
            "denyusers",
        ),
        case(
            "allowgroups",
            |_, f| {
                f.stock
                    .insert("root".to_owned(), STOCK.to_owned() + "allowgroups ops\n");
            },
            "blocked_by_allow_list",
            "allowgroups",
        ),
        case(
            "root login off",
            |_, f| {
                f.stock
                    .insert("root".to_owned(), STOCK.replace("prohibit-password", "no"));
            },
            "cert_login_not_possible",
            "permitrootlogin",
        ),
        case(
            "password too",
            |_, f| {
                f.stock.insert(
                    "root".to_owned(),
                    STOCK.replace("methods any", "methods publickey,password"),
                );
            },
            "cert_login_not_possible",
            "authenticationmethods",
        ),
        case(
            "no pubkeys",
            |_, f| {
                f.stock.insert(
                    "root".to_owned(),
                    STOCK.replace("pubkeyauthentication yes", "pubkeyauthentication no"),
                );
            },
            "cert_login_not_possible",
            "pubkeyauthentication",
        ),
        case(
            "other account unreadable",
            |_, f| {
                f.accounts = vec![account("alice", true, false)];
                f.fail_effective = Some(("alice".to_owned(), false));
            },
            "config_invalid",
            "sshd_config",
        ),
        case(
            "a password without a key",
            |a, f| {
                a.password_auth = "off".to_owned();
                f.accounts = vec![account("bob", false, true), account("alice", true, true)];
            },
            "would_lock_out",
            "account_without_key",
        ),
    ];
    for (name, change, state, detail) in cases {
        let rig = Rig::new("ssh-refused");
        let mut record = access(5);
        {
            let mut fake = rig.host.lock();
            change(&mut record, &mut fake);
        }
        let report = rig.apply(&record);
        assert_eq!(
            (
                report.state.as_str(),
                report.detail.as_str(),
                report.applied_generation
            ),
            (state, detail, 5),
            "{name}"
        );
        assert!(rig.files().is_empty(), "{name}: {:?}", rig.files());
        assert!(
            rig.host.calls().is_empty(),
            "{name}: {:?}",
            rig.host.calls()
        );
        let line = rig.log.lines().pop().expect("a line");
        assert!(
            line.starts_with("error: ssh generation 5: ") && line.ends_with(&format!("({detail})")),
            "{name}: {line}"
        );
    }
}

/// Proves a refused record is not tried again at the same generation, and is at the next.
#[test]
fn a_refused_record_waits_for_its_next_generation() {
    let rig = Rig::new("ssh-refused-once");
    rig.host
        .lock()
        .stock
        .insert("root".to_owned(), STOCK.to_owned() + "allowusers alice\n");
    assert_eq!(rig.apply(&access(1)).state, "blocked_by_allow_list");
    let asked = rig.host.lock().effective_calls;
    rig.host.lock().stock.clear();
    assert_eq!(rig.apply(&access(1)).state, "blocked_by_allow_list");
    assert_eq!(rig.host.lock().effective_calls, asked);
    assert_eq!(rig.apply(&access(2)).state, "applied");
}

/// Proves a start that fails, a list of revoked serials that cannot be made, a candidate that
/// `sshd -t` refuses and a candidate that cannot be written each leave no file of the setup behind
/// and name what failed.
#[test]
fn a_failure_before_the_files_are_written_leaves_none() {
    let rig = Rig::new("ssh-pre-start");
    rig.host.lock().start_fails = true;
    let report = rig.apply(&access(1));
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("reload_failed", "start")
    );
    assert!(rig.files().is_empty());

    let rig = Rig::new("ssh-pre-krl");
    rig.host.lock().revocations_fail = true;
    let report = rig.apply(&access(1));
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("config_invalid", "revocations")
    );
    assert!(rig.files().is_empty());

    let rig = Rig::new("ssh-pre-candidate");
    rig.host.lock().candidate_fails = true;
    let report = rig.apply(&access(1));
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("config_invalid", "sshd_t")
    );
    assert!(
        rig.files().is_empty(),
        "the candidate is removed: {:?}",
        rig.files()
    );
    assert!(!rig.host.calls().contains(&"reload".to_owned()));

    let rig = Rig::new("ssh-pre-candidate-write");
    fs::write(&rig.paths.own_dir, "a file where the directory goes").expect("a file");
    let report = rig.apply(&access(1));
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("config_invalid", "sshd_t")
    );
    assert!(!rig.host.calls().contains(&"check".to_owned()));
}

/// Proves a file that cannot be written puts everything back, says so when a file cannot be put
/// back, and has sshd read the files again.
#[test]
fn a_write_that_fails_is_rolled_back_and_sshd_reads_the_files_again() {
    let rig = Rig::new("ssh-write-fail");
    fs::create_dir_all(rig.paths.drop_in()).expect("a directory where the drop-in goes");
    let report = rig.apply(&access(1));
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("config_invalid", "write")
    );
    let lines = rig.log.lines();
    assert!(
        lines[0].starts_with("error: ssh: not restored: ")
            && lines[0].contains("10-hostkeeper.conf"),
        "{lines:?}"
    );
    assert_eq!(rig.host.calls().last().map(String::as_str), Some("reload"));
}

/// Proves a reload that fails puts the previous files back, byte for byte, reloads again, and
/// reports `reload_failed`; and that when that second reload fails too the log says so.
#[test]
fn a_failed_reload_restores_the_previous_files() {
    let rig = Rig::new("ssh-reload-fail");
    rig.apply(&access(1));
    let before = fs::read(rig.paths.drop_in()).expect("the drop-in");
    let mut next = access(2);
    next.password_auth = "off".to_owned();
    rig.host.lock().reload_failures = 1;
    let report = rig.apply(&next);
    assert_eq!(
        (
            report.state.as_str(),
            report.detail.as_str(),
            report.applied_generation
        ),
        ("reload_failed", "reload", 2)
    );
    assert_eq!(fs::read(rig.paths.drop_in()).expect("the drop-in"), before);
    let calls = rig.host.calls();
    assert_eq!(calls[calls.len() - 2..], ["reload", "reload"]);

    let rig = Rig::new("ssh-reload-fail-twice");
    rig.host.lock().reload_failures = 2;
    assert_eq!(rig.apply(&access(1)).state, "reload_failed");
    assert!(rig.files().is_empty(), "{:?}", rig.files());
    assert_eq!(
        rig.log.lines()[0],
        "error: ssh: sshd is not reloaded after the files were put back: no"
    );
}

/// Proves a reload after which a file cannot be put back (it was replaced by a directory) says
/// which, in the log.
#[test]
fn a_file_that_cannot_be_put_back_is_logged() {
    let rig = Rig::new("ssh-sabotage");
    rig.apply(&access(1));
    let mut next = access(2);
    next.password_auth = "off".to_owned();
    {
        let mut fake = rig.host.lock();
        fake.reload_failures = 1;
        fake.sabotage = Some(rig.paths.ca_keys());
    }
    assert_eq!(rig.apply(&next).state, "reload_failed");
    assert!(
        rig.log.lines()[1].starts_with("error: ssh: not restored: "),
        "{:?}",
        rig.log.lines()
    );
}

/// Proves a banner that does not come after the reload puts the files back, reloads again and
/// reports `reload_failed` with `banner`.
#[test]
fn no_banner_after_the_reload_restores_the_files() {
    let rig = Rig::new("ssh-banner");
    rig.host.lock().banner_fails = true;
    let report = rig.apply(&access(1));
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("reload_failed", "banner")
    );
    assert!(rig.files().is_empty(), "{:?}", rig.files());
    let calls = rig.host.calls();
    assert_eq!(calls[calls.len() - 2..], ["banner 127.0.0.1:22", "reload"]);
}

/// Proves a stock configuration that wins over the drop-in is reported as shadowed with the
/// keyword, and nothing stays: the files are put back.
#[test]
fn a_stock_line_that_wins_is_reported_and_nothing_stays() {
    let rig = Rig::new("ssh-shadowed");
    rig.host.lock().shadow = true;
    let report = rig.apply(&access(1));
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("shadowed_by_stock", "trustedusercakeys")
    );
    assert!(rig.files().is_empty(), "{:?}", rig.files());

    let rig = Rig::new("ssh-shadowed-passwords");
    rig.host.lock().shadow_passwords = true;
    let mut record = access(1);
    record.password_auth = "off".to_owned();
    let report = rig.apply(&record);
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("shadowed_by_stock", "passwordauthentication")
    );
    assert!(rig.files().is_empty(), "{:?}", rig.files());

    let rig = Rig::new("ssh-shadowed-kbd");
    let mut record = access(1);
    record.password_auth = "off".to_owned();
    rig.host.lock().stock.insert(
        "root".to_owned(),
        STOCK.replace(
            "kbdinteractiveauthentication yes",
            "kbdinteractiveauthentication no",
        ),
    );
    assert_eq!(rig.apply(&record).state, "applied", "kbd already off");
}

/// Proves turning passwords off writes the two keywords and is applied when every account with a
/// password has a key path (or is the login user), and that the other accounts' settings are
/// otherwise held to what they were.
#[test]
fn passwords_off_is_applied_when_every_password_has_a_key() {
    let rig = Rig::new("ssh-passwords-off");
    rig.host.lock().accounts = vec![
        account("root", false, true),
        account("alice", true, true),
        account("svc", false, false),
    ];
    let mut record = access(1);
    record.password_auth = "off".to_owned();
    let report = rig.apply(&record);
    assert_eq!(report.state, "applied");
    let drop_in = fs::read_to_string(rig.paths.drop_in()).expect("the drop-in");
    assert!(
        drop_in.ends_with("PasswordAuthentication no\nKbdInteractiveAuthentication no\n"),
        "{drop_in}"
    );
    // Back to keep: the keywords go.
    rig.apply(&access(2));
    assert!(
        !fs::read_to_string(rig.paths.drop_in())
            .expect("the drop-in")
            .contains("PasswordAuthentication")
    );
}

/// Proves an account other than the login user whose effective settings change, or that sshd can
/// no longer print, puts the files back and reports `reload_failed` or `config_invalid`.
#[test]
fn another_accounts_changed_settings_put_the_files_back() {
    let rig = Rig::new("ssh-others-changed");
    {
        let mut fake = rig.host.lock();
        fake.accounts = vec![account("alice", true, false)];
        fake.change_user = Some("alice".to_owned());
    }
    let report = rig.apply(&access(1));
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("reload_failed", "account_settings")
    );
    assert!(rig.files().is_empty());

    let rig = Rig::new("ssh-others-unreadable");
    {
        let mut fake = rig.host.lock();
        fake.accounts = vec![account("alice", true, false)];
        fake.fail_effective = Some(("alice".to_owned(), true));
    }
    let report = rig.apply(&access(1));
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("config_invalid", "sshd_config")
    );
    assert!(rig.files().is_empty());

    let rig = Rig::new("ssh-login-unreadable");
    rig.host.lock().fail_effective = Some(("root".to_owned(), true));
    let report = rig.apply(&access(1));
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("config_invalid", "sshd_config")
    );
    assert!(rig.files().is_empty());
}

/// Proves an account's settings the drop-in does not touch are left alone and the setup is
/// applied: an account with keys whose settings are the same before and after.
#[test]
fn an_account_with_keys_that_is_unchanged_is_fine() {
    let rig = Rig::new("ssh-others-same");
    rig.host.lock().accounts = vec![account("alice", true, true), account("root", true, true)];
    assert_eq!(rig.apply(&access(1)).state, "applied");
}

/// Proves under socket activation a change asks the host to reload the socket's sshd before the
/// banner, as for a service: whether a socket has a listening sshd to reload is the host's to
/// say (`SystemHost::reload`), and the setup never takes it that one does not.
/// Catches: a setup that skips the reload for a socket unit, so that a listener the socket
/// started before the change keeps refusing every certificate.
#[test]
fn a_socket_is_reloaded_by_the_host_after_a_change() {
    let rig = Rig::new("ssh-socket");
    rig.host.lock().unit = Ok(SshdUnit::Socket);
    assert_eq!(rig.apply(&access(1)).state, "applied");
    assert_eq!(
        rig.host.calls(),
        [
            "start",
            "revocations",
            "check",
            "reload Socket",
            "banner 127.0.0.1:22"
        ]
        .map(str::to_owned)
    );
}

/// Proves the first record a daemon takes up after it starts reloads sshd even when every file
/// is already what the record asks for (an earlier start wrote them), and that the same
/// content at a later generation then reloads nothing.
/// Catches: a daemon upgraded from one that wrote the files and never had the listening sshd
/// read them, which would stay `applied` with every certificate refused.
#[test]
fn the_first_record_after_a_start_reloads_files_already_in_place() {
    let rig = Rig::new("ssh-restart");
    rig.apply(&access(1));
    let written = rig.files();
    let restarted = SshSetup::new(
        Arc::clone(&rig.host) as Arc<dyn SshHost>,
        rig.paths.clone(),
        rustix::process::geteuid().as_raw(),
        rig.log.clone(),
    );
    let before = rig.host.calls().len();
    restarted.apply(&access(1));
    assert_eq!(
        restarted.report().map(|report| report.state),
        Some("applied".to_owned())
    );
    assert_eq!(rig.files(), written);
    assert_eq!(
        rig.host.calls()[before..],
        [
            "start",
            "revocations",
            "check",
            "reload",
            "banner 127.0.0.1:22"
        ]
    );
    let again = rig.host.calls().len();
    restarted.apply(&access(2));
    assert_eq!(
        rig.host.calls()[again..],
        ["start", "check", "banner 127.0.0.1:22"]
    );
}

/// Proves a record with `enabled = false` removes the files, reloads, reads the banner and
/// reports `disabled`, then is a no-op, then puts a file that came back away again, and a later
/// enabling builds the list again.
#[test]
fn the_revert_removes_the_files_and_reloads() {
    let rig = Rig::new("ssh-revert");
    rig.apply(&access(1));
    let report = rig.apply(&off(2));
    assert_eq!(
        (
            report.state.as_str(),
            report.detail.as_str(),
            report.applied_generation
        ),
        ("disabled", "", 2)
    );
    assert!(rig.files().is_empty(), "{:?}", rig.files());
    let calls = rig.host.calls();
    assert_eq!(calls[calls.len() - 2..], ["reload", "banner 127.0.0.1:22"]);
    assert_eq!(
        rig.log.lines().last().map(String::as_str),
        Some("ssh generation 2: disabled")
    );

    let count = calls.len();
    rig.apply(&off(2));
    assert_eq!(rig.host.calls().len(), count, "nothing to do");
    fs::create_dir_all(&rig.paths.config_dir).expect("a directory");
    fs::write(rig.paths.drop_in(), "stray\n").expect("a file that came back");
    rig.apply(&off(2));
    assert!(!rig.paths.drop_in().exists());

    rig.apply(&access(3));
    assert_eq!(
        rig.host
            .calls()
            .iter()
            .filter(|c| *c == "revocations")
            .count(),
        2
    );
}

/// Proves reverting a host with nothing set up reloads nothing, and reverting one with no unit
/// removes the files and reloads nothing.
#[test]
fn a_revert_with_nothing_to_remove_or_no_unit_reloads_nothing() {
    let rig = Rig::new("ssh-revert-nothing");
    assert_eq!(rig.apply(&off(1)).state, "disabled");
    assert!(rig.host.calls().is_empty());

    let rig = Rig::new("ssh-revert-no-unit");
    rig.apply(&access(1));
    rig.host.lock().unit = Err(UnitError::Missing);
    assert_eq!(rig.apply(&off(2)).state, "disabled");
    assert!(rig.files().is_empty());
    assert_eq!(
        rig.host.calls().iter().filter(|c| *c == "reload").count(),
        1
    );
}

/// Proves a revert whose reload fails, whose banner does not come, or whose effective
/// configuration cannot be read puts the files back; one whose files cannot be removed says so.
#[test]
fn a_revert_that_fails_puts_the_files_back() {
    let rig = Rig::new("ssh-revert-reload");
    rig.apply(&access(1));
    rig.host.lock().reload_failures = 1;
    let report = rig.apply(&off(2));
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("reload_failed", "reload")
    );
    assert_eq!(rig.files().len(), 4, "{:?}", rig.files());

    let rig = Rig::new("ssh-revert-banner");
    rig.apply(&access(1));
    rig.host.lock().banner_fails = true;
    let report = rig.apply(&off(2));
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("reload_failed", "banner")
    );
    assert_eq!(rig.files().len(), 4, "{:?}", rig.files());

    let rig = Rig::new("ssh-revert-effective");
    rig.apply(&access(1));
    rig.host.lock().fail_effective = Some(("root".to_owned(), false));
    let report = rig.apply(&off(2));
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("config_invalid", "sshd_config")
    );
    assert_eq!(rig.files().len(), 4, "{:?}", rig.files());

    let rig = Rig::new("ssh-revert-write");
    fs::create_dir_all(rig.paths.drop_in().join("inside")).expect("a directory in the way");
    let report = rig.apply(&off(1));
    assert_eq!(
        (report.state.as_str(), report.detail.as_str()),
        ("config_invalid", "write")
    );
}

/// Proves the report follows the last record taken up, refused or not, and an unreadable
/// fingerprint list is simply empty.
#[test]
fn the_report_follows_the_last_record() {
    let rig = Rig::new("ssh-report");
    rig.host.lock().fingerprints.clear();
    let report = rig.apply(&access(1));
    assert!(report.host_key_fingerprints.is_empty());
    rig.host.lock().unit = Err(UnitError::Missing);
    let report = rig.apply(&access(2));
    assert_eq!(
        (report.applied_generation, report.state.as_str()),
        (2, "sshd_missing")
    );
    assert_eq!(
        rig.log.lines(),
        [
            "ssh generation 1: applied",
            "error: ssh generation 2: sshd_missing (unit)"
        ]
    );
}
