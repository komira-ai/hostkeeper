//! The tests of `ssh_host_mac`: a Mac's host over scripted `launchctl`, `dscl` and `dseditgroup`
//! ports, a scripted process port for `sshd`, `id` and `ssh-keygen`, a scratch `sshd_config` and
//! scratch directories for the files, with the sshd setup driven through it. The banner is read
//! from a listener on the loopback.

use std::collections::BTreeMap;
use std::io::Write;
use std::net::TcpListener;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_link::testing::RecordingLog;
use hostkeeper_link::{PushedSsh, SshReport, SshSink};
use hostkeeper_observe::{CommandOutput, RunError};
use hostkeeper_testkit::Scratch;

use super::*;
use crate::ssh_files::SshPaths;
use crate::ssh_setup::SshSetup;

const STOCK_CONFIG: &str = "# stock\nInclude /etc/ssh/sshd_config.d/*\nUsePAM yes\n";

const ED: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

const ACCESS_READ: [&str; 5] = [
    ".",
    "-read",
    "/Groups/com.apple.access_ssh",
    "GroupMembership",
    "NestedGroups",
];

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

/// A process port that answers `sshd -T` from the files on disk (the stock settings plus the
/// drop-in's trust lines once it is there), `sshd -t`, `id` and `ssh-keygen`.
struct Runner {
    paths: SshPaths,
    port: u16,
    calls: Mutex<Vec<String>>,
}

impl CommandRunner for Runner {
    fn run(&self, argv: &[String], _deadline: Duration) -> Result<CommandOutput, RunError> {
        let program = Path::new(&argv[0])
            .file_name()
            .expect("a program")
            .to_string_lossy()
            .into_owned();
        self.calls.lock().expect("calls").push(argv[1..].join(" "));
        let text = match (program.as_str(), argv.get(1).map(String::as_str)) {
            ("sshd", Some("-T")) => {
                let mut text = format!(
                    "port {0}\nlistenaddress 127.0.0.1:{0}\npubkeyauthentication yes\n\
                     authenticationmethods any\npermitrootlogin prohibit-password\n\
                     passwordauthentication yes\nkbdinteractiveauthentication yes\nusepam yes\n",
                    self.port
                );
                if self.paths.drop_in().exists() {
                    text += &format!(
                        "trustedusercakeys {}\nauthorizedprincipalsfile {}\nrevokedkeys {}\n",
                        self.paths.ca_keys().display(),
                        self.paths.principals_template(),
                        self.paths.revoked().display()
                    );
                }
                text
            }
            ("id", _) => "alice admin staff\n".to_owned(),
            _ => String::new(),
        };
        Ok(CommandOutput {
            exit_code: Some(0),
            stdout: text.into_bytes(),
            truncated: false,
        })
    }
}

/// A Mac under test: scripted tools, a scratch `sshd_config` and files, a home for `alice` with a
/// key, and a loopback listener that is sshd's banner.
struct Mac {
    launchctl: FakeLaunchctl,
    dscl: FakeLaunchctl,
    dseditgroup: FakeLaunchctl,
    runner: Arc<Runner>,
    paths: SshPaths,
    log: Arc<RecordingLog>,
    host: Arc<MacHost>,
    dir: Scratch,
}

impl Mac {
    /// A Mac with the stock `sshd_config`, one visible administrator `alice` (uid 501) and a
    /// system account `_sshd`, no access group, and Remote Login loaded.
    fn new(name: &str) -> Mac {
        let dir = scratch(name);
        let paths = SshPaths {
            config_dir: dir.path().join("sshd_config.d"),
            own_dir: dir.path().join("hostkeeper"),
        };
        let listener = TcpListener::bind("127.0.0.1:0").expect("a listener");
        let port = listener.local_addr().expect("an address").port();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let _ = stream.map(|mut stream| stream.write_all(b"SSH-2.0-OpenSSH_10.3\r\n"));
            }
        });
        let runner = Arc::new(Runner {
            paths: paths.clone(),
            port,
            calls: Mutex::new(Vec::new()),
        });
        let (launchctl, dscl, dseditgroup) = (
            FakeLaunchctl::new(),
            FakeLaunchctl::new(),
            FakeLaunchctl::new(),
        );
        let log = Arc::new(RecordingLog::default());
        let tool = {
            let (launchctl, dscl, dseditgroup) =
                (launchctl.clone(), dscl.clone(), dseditgroup.clone());
            move |program: &str| -> Box<dyn Launchctl> {
                match program {
                    hostkeeper_launchd::LAUNCHCTL => Box::new(launchctl.clone()),
                    DSCL => Box::new(dscl.clone()),
                    _ => Box::new(dseditgroup.clone()),
                }
            }
        };
        let tools = Tools {
            etc: dir.path().join("etc"),
            work: dir.path().join("krl-work"),
            ..Tools::macos()
        };
        let host = Arc::new(MacHost::new(
            Arc::clone(&runner) as Arc<dyn CommandRunner>,
            tools,
            &tool,
            &dir.path().join("sshd_config"),
            log.clone(),
        ));
        let mac = Mac {
            launchctl,
            dscl,
            dseditgroup,
            runner,
            paths,
            log,
            host,
            dir,
        };
        std::fs::write(mac.dir.path().join("sshd_config"), STOCK_CONFIG).expect("sshd_config");
        mac.dscl_ok(
            &[".", "-list", "/Users", "UniqueID"],
            "_sshd   75\nalice   501\nroot    0\n",
        );
        mac.dscl_ok(
            &[".", "-read", "/Groups/admin", "GroupMembership"],
            "GroupMembership: root alice\n",
        );
        mac.dscl_ok(
            &[
                ".",
                "-read",
                "/Users/alice",
                "NFSHomeDirectory",
                "PrimaryGroupID",
                "IsHidden",
            ],
            "NFSHomeDirectory: /Users/alice\nPrimaryGroupID: 20\nIsHidden: 0\n",
        );
        mac.dscl.always(
            &args(&ACCESS_READ),
            Ok(LaunchctlOutput::exit(
                56,
                "<dscl_cl> DS Error: -14136 (eDSRecordNotFound)",
            )),
        );
        let home = mac.dir.path().join("alice");
        std::fs::create_dir_all(home.join(".ssh")).expect("a home");
        std::fs::write(home.join(".ssh/authorized_keys"), format!("{ED}\n")).expect("a key");
        mac.dscl_ok(
            &[".", "-list", "/Users", "NFSHomeDirectory"],
            &format!("alice {}\nroot /var/root\n", home.display()),
        );
        mac.launchctl_loaded();
        mac
    }

    fn dscl_ok(&self, words: &[&str], stdout: &str) {
        self.dscl
            .always(&args(words), Ok(LaunchctlOutput::success(stdout)));
    }

    fn group(&self, stdout: &str) {
        self.dscl_ok(&ACCESS_READ, stdout);
    }

    fn launchctl_loaded(&self) {
        self.launchctl.always(
            &args(&["print", "system/com.openssh.sshd"]),
            Ok(LaunchctlOutput::success("loaded\n")),
        );
    }

    /// Scripts Remote Login as not loaded once, and taking `enable` and `bootstrap`.
    fn launchctl_not_loaded(&self) {
        let print = args(&["print", "system/com.openssh.sshd"]);
        self.launchctl
            .then(&print, Ok(LaunchctlOutput::not_loaded()));
        for words in [
            &["enable", "system/com.openssh.sshd"][..],
            &[
                "bootstrap",
                "system",
                "/System/Library/LaunchDaemons/ssh.plist",
            ][..],
        ] {
            self.launchctl
                .always(&args(words), Ok(LaunchctlOutput::success("")));
        }
    }

    fn setup(&self) -> SshSetup {
        SshSetup::new(
            Arc::clone(&self.host) as Arc<dyn SshHost>,
            self.paths.clone(),
            rustix::process::geteuid().as_raw(),
            self.log.clone(),
        )
    }

    fn apply(&self, setup: &SshSetup, access: &PushedSsh) -> SshReport {
        setup.apply(access);
        setup.report().expect("a report")
    }

    fn files(&self) -> Vec<String> {
        let mut found = Vec::new();
        for dir in [&self.paths.config_dir, &self.paths.own_dir] {
            if let Ok(entries) = std::fs::read_dir(dir) {
                found.extend(
                    entries
                        .filter_map(Result::ok)
                        .map(|entry| entry.file_name().to_string_lossy().into_owned()),
                );
            }
        }
        found.sort();
        found
    }

    fn sshd_runs(&self) -> Vec<String> {
        self.runner.calls.lock().expect("calls").clone()
    }
}

fn access(generation: u64, login_user: &str) -> PushedSsh {
    PushedSsh {
        generation,
        enabled: true,
        ca_public_keys: vec![ED.to_owned()],
        login_user: login_user.to_owned(),
        principal: "hk-host-web-04".to_owned(),
        revoked_serials: Vec::new(),
        password_auth: "keep".to_owned(),
    }
}

fn refused(state: SshState, detail: &'static str) -> Result<String, Refused> {
    Err(Refused::new(state, detail))
}

fn report(state: &str, detail: &str, generation: u64) -> SshReport {
    SshReport {
        applied_generation: generation,
        state: state.to_owned(),
        host_key_fingerprints: Vec::new(),
        detail: detail.to_owned(),
    }
}

fn no_answer() -> Result<LaunchctlOutput, LaunchctlError> {
    Err(LaunchctlError::Spawn {
        args: String::new(),
        why: "no such program".to_owned(),
    })
}

fn print_call() -> Vec<String> {
    args(&["print", "system/com.openssh.sshd"])
}

/// Proves a record is taken up on a Mac in the design's order: Remote Login is checked (loaded, so
/// only `launchctl print` runs), the login user is the single administrator the empty one stands
/// for, the candidate is checked with `sshd -t`, the three files are written, there is no reload,
/// and the banner is read from the listening address; the report says `applied`.
///
/// Catches: a Mac that announces `ssh-v1` and writes the files for the wrong account or not at
/// all, or that reloads a launchd-started sshd.
#[test]
fn a_mac_applies_the_record_for_its_one_administrator() {
    let mac = Mac::new("ssh-mac-applies");
    let setup = mac.setup();
    assert_eq!(mac.apply(&setup, &access(1, "")), report("applied", "", 1));
    assert_eq!(
        mac.files(),
        [
            "10-hostkeeper.conf",
            "principals.alice",
            "revoked",
            "user_ca.pub"
        ]
    );
    assert_eq!(
        std::fs::read_to_string(mac.paths.principals("alice")).expect("principals"),
        "hk-host-web-04\n"
    );
    assert_eq!(mac.launchctl.calls(), [print_call()]);
    let kinds: Vec<String> = mac
        .sshd_runs()
        .iter()
        .map(|run| run.split_whitespace().next().unwrap_or("").to_owned())
        .collect();
    assert_eq!(kinds, ["-T", "-Gn", "-t", "-T"]);
    assert!(mac.dseditgroup.calls().is_empty());
    assert_eq!(mac.log.lines(), ["ssh generation 1: applied".to_owned()]);
}

/// Proves a Mac whose Remote Login is not loaded gets it by `launchctl print`, `enable`,
/// `bootstrap` and `print`, in that order, and the log says so.
///
/// Catches: an ensure step that skips `enable` (the service is not kept at the next boot) or that
/// reaches for `systemsetup`.
#[test]
fn a_mac_that_lost_remote_login_gets_it_back() {
    let mac = Mac::new("ssh-mac-lost");
    mac.launchctl_not_loaded();
    let setup = mac.setup();
    assert_eq!(mac.apply(&setup, &access(1, "")).state, "applied");
    assert_eq!(
        mac.launchctl.calls(),
        [
            print_call(),
            args(&["enable", "system/com.openssh.sshd"]),
            args(&[
                "bootstrap",
                "system",
                "/System/Library/LaunchDaemons/ssh.plist"
            ]),
            print_call(),
        ]
    );
    assert!(
        mac.log
            .lines()
            .iter()
            .any(|line| line.starts_with("ssh: Remote Login turned on"))
    );
}

/// Proves a Remote Login that cannot be loaded is `reload_failed` with the step `remote_login` in
/// the detail, and nothing is written.
///
/// Catches: a failed `launchctl enable` reported as a generic `start`, or files written for a Mac
/// whose sshd is not running.
#[test]
fn a_remote_login_that_cannot_be_loaded_is_reload_failed_and_writes_nothing() {
    let mac = Mac::new("ssh-mac-no-remote-login");
    mac.launchctl_not_loaded();
    mac.launchctl.always(
        &args(&["enable", "system/com.openssh.sshd"]),
        Ok(LaunchctlOutput::exit(1, "denied")),
    );
    let setup = mac.setup();
    assert_eq!(
        mac.apply(&setup, &access(1, "")),
        report("reload_failed", "remote_login", 1)
    );
    assert!(mac.files().is_empty());
}

/// Proves a daemon that starts over files already in place runs the ensure step again for the
/// record it is given, loads Remote Login again when the Mac lost it, and writes no file: the
/// files' modification times do not change.
///
/// Catches: a restart that trusts the files and never looks at Remote Login again, or one that
/// rewrites the files.
#[test]
fn a_restarted_daemon_ensures_remote_login_again_without_writing_a_file() {
    let mac = Mac::new("ssh-mac-restart");
    assert_eq!(mac.apply(&mac.setup(), &access(1, "")).state, "applied");
    let stamps = |mac: &Mac| -> BTreeMap<String, std::time::SystemTime> {
        [
            mac.paths.drop_in(),
            mac.paths.ca_keys(),
            mac.paths.principals("alice"),
        ]
        .into_iter()
        .map(|path| {
            let modified = std::fs::metadata(&path).and_then(|meta| meta.modified());
            (path.display().to_string(), modified.expect("a time"))
        })
        .collect()
    };
    let before = stamps(&mac);
    let calls = mac.launchctl.calls().len();
    mac.launchctl_not_loaded();
    assert_eq!(mac.apply(&mac.setup(), &access(1, "")).state, "applied");
    assert_eq!(stamps(&mac), before);
    assert_eq!(
        mac.launchctl.calls().len() - calls,
        4,
        "print, enable, bootstrap, print"
    );
}

/// Proves a revert removes the files and runs nothing of `launchctl`: Remote Login stays on as it
/// was, and no `disable` or `bootout` is ever run.
///
/// Catches: a revert that turns Remote Login off and cuts a login that came by `authorized_keys`.
#[test]
fn a_revert_removes_the_files_and_leaves_remote_login_on() {
    let mac = Mac::new("ssh-mac-revert");
    mac.launchctl_not_loaded();
    let setup = mac.setup();
    assert_eq!(mac.apply(&setup, &access(1, "")).state, "applied");
    let calls = mac.launchctl.calls();
    let revert = PushedSsh {
        enabled: false,
        ..access(2, "")
    };
    assert_eq!(mac.apply(&setup, &revert), report("disabled", "", 2));
    assert!(mac.files().is_empty());
    assert_eq!(mac.launchctl.calls(), calls);
    assert!(
        calls
            .iter()
            .all(|call| matches!(call[0].as_str(), "print" | "enable" | "bootstrap"))
    );
}

/// Proves a Mac whose `sshd_config` has no `Include` of the drop-in directory, or none that can be
/// read, is `no_include` before any tool is run or file written, and `sshd_config` is not edited.
///
/// Catches: a setup that writes a drop-in nothing reads and reports it applied, or one that edits
/// a file Apple owns.
#[test]
fn a_mac_without_the_include_stays_untrusted_and_nothing_is_run() {
    let mac = Mac::new("ssh-mac-no-include");
    let config = mac.dir.path().join("sshd_config");
    std::fs::write(&config, "UsePAM yes\n").expect("config");
    let setup = mac.setup();
    assert_eq!(
        mac.apply(&setup, &access(1, "")),
        report("no_include", "Include", 1)
    );
    assert_eq!(
        std::fs::read_to_string(&config).expect("config"),
        "UsePAM yes\n"
    );
    std::fs::remove_file(&config).expect("remove");
    assert_eq!(
        mac.apply(&setup, &access(2, "")),
        report("no_include", "sshd_config", 2)
    );
    assert!(mac.files().is_empty());
    assert!(mac.launchctl.calls().is_empty() && mac.dscl.calls().is_empty());
    assert!(mac.sshd_runs().is_empty());
}

/// Proves an `Include` is found in each spelling, a blank line before it being skipped.
///
/// Catches: an Include check that refuses the relative or `*.conf` spellings of a stock file.
#[test]
fn an_include_of_the_drop_in_directory_is_found_in_each_spelling() {
    for line in [
        "Include /etc/ssh/sshd_config.d/*",
        "Include /etc/ssh/sshd_config.d/*.conf",
        "include sshd_config.d/*",
        "INCLUDE other.conf sshd_config.d/*.conf",
        "   Include\t/etc/ssh/sshd_config.d/*",
    ] {
        assert!(
            includes_drop_in_dir(&format!("Port 22\n\n{line}\n")),
            "{line}"
        );
    }
}

/// Proves an `Include` that does not read the drop-in directory for every connection is refused:
/// a comment, another directory, one file, or one inside a `Match` block.
///
/// Catches: an Include check that takes a commented line or a `Match`-scoped one for the real
/// thing.
#[test]
fn an_include_that_does_not_read_the_drop_in_directory_for_every_connection_is_refused() {
    for text in [
        "",
        "Port 22\n",
        "#Include /etc/ssh/sshd_config.d/*\n",
        "# Include /etc/ssh/sshd_config.d/*\n",
        "Include /etc/ssh/other.d/*\n",
        "Include /etc/ssh/sshd_config.d/10-one.conf\n",
        "Match User bob\nInclude /etc/ssh/sshd_config.d/*\n",
    ] {
        assert!(!includes_drop_in_dir(text), "{text:?}");
    }
}

/// Proves the login user a Mac takes: a named account the directory knows is taken without
/// looking for administrators; a name that is not a plain account name is refused unread; an
/// unknown account is `no_login_user`; a `dscl` that cannot run is `unsupported`.
///
/// Catches: a named user written into a file name unchecked, or one that does not exist.
#[test]
fn a_named_login_user_is_checked_against_the_directory() {
    let mac = Mac::new("ssh-mac-named");
    mac.dscl_ok(&[".", "-read", "/Users/bob", "UniqueID"], "UniqueID: 502\n");
    assert_eq!(mac.host.prepare("bob", false), Ok("bob".to_owned()));
    assert!(
        !mac.dscl
            .calls()
            .contains(&args(&[".", "-list", "/Users", "UniqueID"]))
    );
    let reads = mac.dscl.calls().len();
    assert_eq!(
        mac.host.prepare("../x", false),
        refused(SshState::ConfigInvalid, "login_user")
    );
    assert_eq!(mac.dscl.calls().len(), reads);
    assert_eq!(
        mac.host.prepare("carol", false),
        refused(SshState::Unsupported, "dscl"),
        "an answer the fake does not have is a tool that cannot run"
    );
    mac.dscl.always(
        &args(&[".", "-read", "/Users/carol", "UniqueID"]),
        Ok(LaunchctlOutput::exit(56, "eDSRecordNotFound")),
    );
    assert_eq!(
        mac.host.prepare("carol", false),
        refused(SshState::NoLoginUser, "not_an_account")
    );
}

/// Proves an empty login user is the single local administrator, and that none, several, or a
/// directory that cannot be read are each a refusal that writes nothing.
///
/// Catches: a Mac with two administrators that trusts certificates for an arbitrary one.
#[test]
fn an_empty_login_user_is_the_single_administrator_or_no_login_user() {
    let mac = Mac::new("ssh-mac-admin");
    assert_eq!(mac.host.prepare("", false), Ok("alice".to_owned()));
    mac.dscl_ok(
        &[".", "-read", "/Groups/admin", "GroupMembership"],
        "GroupMembership: root\n",
    );
    assert_eq!(
        mac.host.prepare("", false),
        refused(SshState::NoLoginUser, "administrator")
    );
    mac.dscl_ok(
        &[".", "-list", "/Users", "UniqueID"],
        "alice 501\nbob 502\n",
    );
    mac.dscl_ok(
        &[".", "-read", "/Groups/admin", "GroupMembership"],
        "GroupMembership: root alice bob\n",
    );
    mac.dscl_ok(
        &[
            ".",
            "-read",
            "/Users/bob",
            "NFSHomeDirectory",
            "PrimaryGroupID",
            "IsHidden",
        ],
        "NFSHomeDirectory: /Users/bob\nPrimaryGroupID: 20\n",
    );
    assert_eq!(
        mac.host.prepare("", false),
        refused(SshState::NoLoginUser, "administrator")
    );
    mac.dscl
        .always(&args(&[".", "-list", "/Users", "UniqueID"]), no_answer());
    assert_eq!(
        mac.host.prepare("", false),
        refused(SshState::Unsupported, "dscl")
    );
    let setup = mac.setup();
    assert_eq!(
        mac.apply(&setup, &access(1, "")),
        report("unsupported", "dscl", 1)
    );
    assert!(mac.files().is_empty());
}

/// Proves the access group admits a member, an administrator when it nests `admin` (whatever the
/// case of the group id), and everyone when it does not exist; it blocks anyone else; and a read
/// that fails for another reason, or cannot run, is `unsupported`. No group is edited and no
/// `launchctl` runs.
///
/// Catches: a login user that `com.apple.access_ssh` refuses at login but the setup calls applied.
#[test]
fn the_access_group_admits_its_members_and_blocks_the_rest_without_editing_it() {
    let mac = Mac::new("ssh-mac-group");
    let blocked = || refused(SshState::BlockedByAccessGroup, "access_ssh_group");
    mac.group("GroupMembership: bob alice\n");
    assert_eq!(mac.host.prepare("", false), Ok("alice".to_owned()));
    mac.group("GroupMembership: bob\n");
    assert_eq!(mac.host.prepare("", false), blocked());
    mac.group("No such key: GroupMembership\n");
    assert_eq!(mac.host.prepare("", false), blocked());
    mac.group(&format!(
        "NestedGroups: {}\n",
        ADMIN_GROUP_GUID.to_lowercase()
    ));
    assert_eq!(mac.host.prepare("", false), Ok("alice".to_owned()));
    mac.group("NestedGroups: AAAAAAAA-2222-3333-4444-555555555555\n");
    assert_eq!(mac.host.prepare("", false), blocked());
    mac.dscl_ok(&[".", "-read", "/Users/bob", "UniqueID"], "UniqueID: 502\n");
    mac.group(&format!("NestedGroups: {ADMIN_GROUP_GUID}\n"));
    assert_eq!(
        mac.host.prepare("bob", false),
        blocked(),
        "bob is not in admin"
    );
    mac.dscl.always(
        &args(&ACCESS_READ),
        Ok(LaunchctlOutput::exit(1, "dscl: Permission denied")),
    );
    assert_eq!(
        mac.host.prepare("", false),
        refused(SshState::Unsupported, "dscl")
    );
    mac.dscl.always(&args(&ACCESS_READ), no_answer());
    assert_eq!(
        mac.host.prepare("", false),
        refused(SshState::Unsupported, "dscl")
    );
    assert!(mac.dseditgroup.calls().is_empty());
    assert!(mac.launchctl.calls().is_empty());
}

/// Proves a nested `admin` group that cannot be read is `unsupported`, not an admission.
///
/// Catches: an admission by nesting on the strength of a group nobody could read.
#[test]
fn an_admin_group_that_cannot_be_read_does_not_admit_through_nesting() {
    let mac = Mac::new("ssh-mac-group-admin-unread");
    mac.dscl_ok(&[".", "-read", "/Users/bob", "UniqueID"], "UniqueID: 502\n");
    mac.group(&format!("NestedGroups: {ADMIN_GROUP_GUID}\n"));
    mac.dscl.always(
        &args(&[".", "-read", "/Groups/admin", "GroupMembership"]),
        no_answer(),
    );
    assert_eq!(
        mac.host.prepare("bob", false),
        refused(SshState::Unsupported, "dscl")
    );
}

/// Proves a login user the access group does not admit ends the record as
/// `blocked_by_access_group`: no file and no `launchctl` is touched and the group is not edited
/// (the design's section 3: the daemon never edits a group).
///
/// Catches: a setup that adds the user to the group, or writes files for a user who cannot log in.
#[test]
fn a_login_user_the_access_group_blocks_changes_nothing() {
    let mac = Mac::new("ssh-mac-blocked");
    mac.group("GroupMembership: bob\n");
    let setup = mac.setup();
    assert_eq!(
        mac.apply(&setup, &access(1, "")),
        report("blocked_by_access_group", "access_ssh_group", 1)
    );
    assert!(mac.files().is_empty());
    assert!(mac.launchctl.calls().is_empty());
    assert!(mac.dseditgroup.calls().is_empty());
}

/// Proves `password_auth = off` is `unsupported` on a Mac, which has no shadow file to prove every
/// account has another way in, and nothing is written or loaded.
///
/// Catches: a Mac that turns passwords off without knowing which accounts would be locked out.
#[test]
fn turning_passwords_off_is_unsupported_on_a_mac() {
    let mac = Mac::new("ssh-mac-passwords");
    let setup = mac.setup();
    let record = PushedSsh {
        password_auth: "off".to_owned(),
        ..access(1, "")
    };
    assert_eq!(
        mac.apply(&setup, &record),
        report("unsupported", "password_auth", 1)
    );
    assert!(mac.files().is_empty());
    assert!(mac.launchctl.calls().is_empty());
}

/// Proves the accounts of a Mac that matter are those whose home has a non-empty
/// `authorized_keys`, read from the directory service, with no password path; a line with no home
/// is skipped, and a listing that cannot be read gives none.
///
/// Catches: an account with keys that the rule-3 settings check never sees on a Mac.
#[test]
fn the_accounts_with_keys_come_from_the_directory_service() {
    let mac = Mac::new("ssh-mac-accounts");
    assert_eq!(
        mac.host.accounts(),
        [Account {
            name: "alice".to_owned(),
            keyed: true,
            password: false
        }]
    );
    mac.dscl_ok(&[".", "-list", "/Users", "NFSHomeDirectory"], "orphan\n");
    assert!(mac.host.accounts().is_empty());
    mac.dscl.always(
        &args(&[".", "-list", "/Users", "NFSHomeDirectory"]),
        no_answer(),
    );
    assert!(mac.host.accounts().is_empty());
}

/// Proves a Mac's unit is Remote Login under launchd, which has no reload, that the Mac's tools
/// are the Linux ones but for `systemctl`, and that the real host is built over them.
///
/// Catches: a Mac unit that is reloaded with a `systemctl` that does not exist.
#[test]
fn a_macs_unit_is_remote_login_with_nothing_to_reload() {
    let mac = Mac::new("ssh-mac-unit");
    assert_eq!(mac.host.unit(), Ok(SshdUnit::Launchd));
    assert_eq!(mac.host.reload(&SshdUnit::Launchd), Ok(()));
    assert!(mac.launchctl.calls().is_empty());
    assert_eq!(Tools::macos().systemctl, PathBuf::new());
    assert_eq!(Tools::macos().sshd, Tools::linux().sshd);
    let real = MacHost::system(
        Arc::clone(&mac.runner) as Arc<dyn CommandRunner>,
        mac.log.clone(),
    );
    assert_eq!(real.unit(), Ok(SshdUnit::Launchd));
    assert_eq!(real.sshd_config, Path::new(SSHD_CONFIG));
}

/// Proves the Mac host passes `sshd -T`, `id`, `sshd -t`, the host keys, the revocation list and
/// the banner to the steps it shares with the Linux host.
///
/// Catches: a Mac adapter that answers these steps itself and drifts from the Linux ones.
#[test]
fn a_mac_reads_sshd_and_the_groups_as_the_linux_host_does() {
    let mac = Mac::new("ssh-mac-shared");
    let at = ("127.0.0.1".to_owned(), mac.runner.port);
    assert_eq!(
        mac.host.effective("alice").expect("effective").listening(),
        at
    );
    assert_eq!(mac.host.groups("alice"), ["alice", "admin", "staff"]);
    assert_eq!(mac.host.check_config(Path::new("/x/candidate")), Ok(()));
    assert!(mac.host.host_key_fingerprints().is_empty());
    assert_eq!(mac.host.revocations(&[ED.to_owned()], &[]), Ok(Vec::new()));
    assert_eq!(mac.host.banner(&at), Ok(()));
}
