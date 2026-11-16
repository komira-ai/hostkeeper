//! The Mac's sshd setup checks against scripted `systemsetup` and `dscl` tools and a scratch
//! `sshd_config`: what each refusal is, and that nothing is turned on before every earlier check
//! passed.

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_testkit::Scratch;

use super::*;

const STOCK_CONFIG: &str = "# stock\nInclude /etc/ssh/sshd_config.d/*\nUsePAM yes\n";

fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// A Mac under test: the two scripted tools and the scratch holding `sshd_config`.
struct Mac {
    systemsetup: FakeLaunchctl,
    dscl: FakeLaunchctl,
    dir: Scratch,
}

impl Mac {
    /// A Mac with the stock `sshd_config`, one visible administrator `alice` (uid 501) and a
    /// system account `_sshd`, no access group, and Remote Login off.
    fn new(name: &str) -> Mac {
        let mac = Mac {
            systemsetup: FakeLaunchctl::new(),
            dscl: FakeLaunchctl::new(),
            dir: scratch(name),
        };
        mac.config(STOCK_CONFIG);
        mac.dscl_ok(
            &[".", "-list", "/Users", "UniqueID"],
            "_sshd   75\nalice   501\nroot    0\n",
        );
        mac.dscl_ok(
            &[".", "-read", "/Groups/admin", "GroupMembership"],
            "GroupMembership: root alice\n",
        );
        mac.dscl_ok(
            &[".", "-read", "/Users/alice", "IsHidden"],
            "No such key: IsHidden\n",
        );
        mac.dscl.always(
            &args(&[
                ".",
                "-read",
                "/Groups/com.apple.access_ssh",
                "GroupMembership",
                "NestedGroups",
            ]),
            Ok(LaunchctlOutput::exit(
                56,
                "<dscl_cl> DS Error: -14136 (eDSRecordNotFound)",
            )),
        );
        mac.systemsetup.always(
            &args(&["-getremotelogin"]),
            Ok(LaunchctlOutput::success("Remote Login: Off\n")),
        );
        mac.systemsetup.always(
            &args(&["-setremotelogin", "on"]),
            Ok(LaunchctlOutput::success("")),
        );
        mac
    }

    fn config(&self, text: &str) {
        std::fs::write(self.dir.path().join("sshd_config"), text).expect("write sshd_config");
    }

    fn dscl_ok(&self, words: &[&str], stdout: &str) {
        self.dscl
            .always(&args(words), Ok(LaunchctlOutput::success(stdout)));
    }

    fn prepare(&self, requested: &str) -> Result<Ready, Blocked> {
        MacSsh::new(
            self.systemsetup.clone(),
            self.dscl.clone(),
            &self.dir.path().join("sshd_config"),
        )
        .prepare(requested)
    }

    /// Whether the daemon asked `systemsetup` to change anything.
    fn changed_remote_login(&self) -> bool {
        self.systemsetup
            .calls()
            .contains(&args(&["-setremotelogin", "on"]))
    }
}

fn blocked(state: &'static str, detail: &'static str) -> Result<Ready, Blocked> {
    Err(Blocked { state, detail })
}

fn spawn_failure() -> Result<LaunchctlOutput, LaunchctlError> {
    Err(LaunchctlError::Spawn {
        args: String::new(),
        why: "no such program".to_owned(),
    })
}

/// Scripts a Mac whose Remote Login is off and takes the setting: the first read says Off and the
/// ones after the change say On.
fn takes_the_setting(mac: &Mac) {
    mac.systemsetup.then(
        &args(&["-getremotelogin"]),
        Ok(LaunchctlOutput::success("Remote Login: Off\n")),
    );
    mac.systemsetup.always(
        &args(&["-getremotelogin"]),
        Ok(LaunchctlOutput::success("Remote Login: On\n")),
    );
}

#[test]
fn a_stock_mac_gets_remote_login_turned_on_for_its_one_administrator() {
    let mac = Mac::new("ssh-mac-stock");
    takes_the_setting(&mac);
    assert_eq!(
        mac.prepare(""),
        Ok(Ready {
            login_user: "alice".to_owned(),
            remote_login: RemoteLogin::TurnedOn
        })
    );
    assert!(mac.changed_remote_login());
}

#[test]
fn remote_login_already_on_is_not_touched() {
    let mac = Mac::new("ssh-mac-on");
    mac.systemsetup.always(
        &args(&["-getremotelogin"]),
        Ok(LaunchctlOutput::success("Remote Login: On\n")),
    );
    assert_eq!(
        mac.prepare("").map(|ready| ready.remote_login),
        Ok(RemoteLogin::AlreadyOn)
    );
    assert!(!mac.changed_remote_login());
}

#[test]
fn full_disk_access_missing_when_reading_is_reported_and_changes_nothing() {
    let mac = Mac::new("ssh-mac-fda-get");
    mac.systemsetup.always(
        &args(&["-getremotelogin"]),
        Ok(LaunchctlOutput::success(
            "getremotelogin: Turning Remote Login on or off requires Full Disk Access privileges.\n",
        )),
    );
    assert_eq!(
        mac.prepare(""),
        blocked(NEEDS_FULL_DISK_ACCESS, "systemsetup")
    );
    assert!(!mac.changed_remote_login());
}

#[test]
fn full_disk_access_missing_when_turning_on_is_reported() {
    let mac = Mac::new("ssh-mac-fda-set");
    mac.systemsetup.always(
        &args(&["-setremotelogin", "on"]),
        Ok(LaunchctlOutput::exit(
            1,
            "setremotelogin: Turning Remote Login on or off requires Full Disk Access privileges.",
        )),
    );
    assert_eq!(
        mac.prepare(""),
        blocked(NEEDS_FULL_DISK_ACCESS, "systemsetup")
    );
}

#[test]
fn remote_login_that_stays_off_after_the_tool_ran_is_unsupported() {
    let mac = Mac::new("ssh-mac-stays-off");
    // The scripted read-back keeps saying Off, as a Mac whose setting did not take would.
    assert_eq!(mac.prepare(""), blocked(UNSUPPORTED, "systemsetup"));
    assert!(mac.changed_remote_login());
}

#[test]
fn a_status_line_that_is_neither_on_nor_off_is_unsupported() {
    let mac = Mac::new("ssh-mac-garbage");
    mac.systemsetup.always(
        &args(&["-getremotelogin"]),
        Ok(LaunchctlOutput::success("something else\n")),
    );
    assert_eq!(mac.prepare(""), blocked(UNSUPPORTED, "systemsetup"));
    assert!(!mac.changed_remote_login());
}

#[test]
fn systemsetup_that_cannot_run_is_unsupported() {
    let mac = Mac::new("ssh-mac-no-systemsetup");
    mac.systemsetup
        .always(&args(&["-getremotelogin"]), spawn_failure());
    assert_eq!(mac.prepare(""), blocked(UNSUPPORTED, "systemsetup"));
}

#[test]
fn a_mac_that_turns_on_remote_login_reads_it_back_as_on() {
    let mac = Mac::new("ssh-mac-readback");
    takes_the_setting(&mac);
    assert_eq!(
        mac.prepare("alice").map(|ready| ready.remote_login),
        Ok(RemoteLogin::TurnedOn)
    );
    assert_eq!(
        mac.systemsetup.calls(),
        vec![
            args(&["-getremotelogin"]),
            args(&["-setremotelogin", "on"]),
            args(&["-getremotelogin"]),
        ]
    );
}

#[test]
fn an_include_of_the_drop_in_directory_is_found_in_each_spelling() {
    for line in [
        "Include /etc/ssh/sshd_config.d/*",
        "Include /etc/ssh/sshd_config.d/*.conf",
        "include sshd_config.d/*",
        "INCLUDE other.conf sshd_config.d/*.conf",
        "   Include\t/etc/ssh/sshd_config.d/*",
    ] {
        assert!(includes_drop_in_dir(&format!("Port 22\n{line}\n")), "{line}");
    }
}

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

#[test]
fn a_mac_without_the_include_stays_untrusted_and_changes_nothing() {
    let mac = Mac::new("ssh-mac-no-include");
    mac.config("UsePAM yes\n");
    assert_eq!(mac.prepare(""), blocked(NO_INCLUDE, "Include"));
    assert!(mac.systemsetup.calls().is_empty());
    assert!(mac.dscl.calls().is_empty());
}

#[test]
fn an_unreadable_sshd_config_is_no_include() {
    let mac = Mac::new("ssh-mac-no-config");
    std::fs::remove_file(mac.dir.path().join("sshd_config")).expect("remove");
    assert_eq!(mac.prepare(""), blocked(NO_INCLUDE, "sshd_config"));
}

#[test]
fn a_named_login_user_that_exists_is_taken_without_looking_for_administrators() {
    let mac = Mac::new("ssh-mac-named");
    mac.dscl_ok(
        &[".", "-read", "/Users/builder", "UniqueID"],
        "UniqueID: 502\n",
    );
    assert_eq!(
        mac.prepare("builder").map(|ready| ready.login_user),
        Ok("builder".to_owned())
    );
    assert!(
        !mac.dscl
            .calls()
            .contains(&args(&[".", "-list", "/Users", "UniqueID"]))
    );
}

#[test]
fn a_named_login_user_that_is_not_an_account_name_is_refused_unread() {
    let too_long = "a".repeat(65);
    for name in ["../etc", "a/b", "-x", ".x", "a b", too_long.as_str()] {
        let mac = Mac::new("ssh-mac-bad-name");
        assert_eq!(mac.prepare(name), blocked(NO_LOGIN_USER, "not_an_account"));
        assert!(mac.dscl.calls().is_empty(), "{name}");
    }
}

#[test]
fn a_named_login_user_the_directory_does_not_know_is_refused() {
    let mac = Mac::new("ssh-mac-unknown");
    mac.dscl.always(
        &args(&[".", "-read", "/Users/ghost", "UniqueID"]),
        Ok(LaunchctlOutput::exit(
            56,
            "<dscl_cl> DS Error: -14136 (eDSRecordNotFound)",
        )),
    );
    assert_eq!(mac.prepare("ghost"), blocked(NO_LOGIN_USER, "not_an_account"));
    assert!(!mac.changed_remote_login());
}

#[test]
fn a_named_login_user_whose_record_has_no_id_is_refused() {
    let mac = Mac::new("ssh-mac-no-id");
    mac.dscl_ok(&[".", "-read", "/Users/ghost", "UniqueID"], "No such key: UniqueID\n");
    assert_eq!(mac.prepare("ghost"), blocked(NO_LOGIN_USER, "not_an_account"));
}

#[test]
fn dscl_that_cannot_run_is_unsupported() {
    let mac = Mac::new("ssh-mac-no-dscl");
    mac.dscl.always(
        &args(&[".", "-read", "/Users/alice", "UniqueID"]),
        spawn_failure(),
    );
    assert_eq!(mac.prepare("alice"), blocked(UNSUPPORTED, "dscl"));
}

#[test]
fn no_administrator_is_no_login_user() {
    let mac = Mac::new("ssh-mac-none");
    mac.dscl_ok(
        &[".", "-read", "/Groups/admin", "GroupMembership"],
        "GroupMembership: root\n",
    );
    assert_eq!(mac.prepare(""), blocked(NO_LOGIN_USER, "no_administrator"));
    assert!(!mac.changed_remote_login());
}

#[test]
fn two_administrators_are_no_login_user() {
    let mac = Mac::new("ssh-mac-two");
    mac.dscl_ok(
        &[".", "-list", "/Users", "UniqueID"],
        "alice 501\nbob 502\n",
    );
    mac.dscl_ok(
        &[".", "-read", "/Groups/admin", "GroupMembership"],
        "GroupMembership: root alice bob\n",
    );
    mac.dscl_ok(&[".", "-read", "/Users/bob", "IsHidden"], "IsHidden: 0\n");
    assert_eq!(
        mac.prepare(""),
        blocked(NO_LOGIN_USER, "several_administrators")
    );
}

#[test]
fn a_hidden_administrator_and_a_system_or_ordinary_account_are_not_candidates() {
    let mac = Mac::new("ssh-mac-hidden");
    mac.dscl_ok(
        &[".", "-list", "/Users", "UniqueID"],
        "daemon 1\nalice 501\nsvc 502\ncarol 503\nodd line here 9\nbroken x\n",
    );
    mac.dscl_ok(
        &[".", "-read", "/Groups/admin", "GroupMembership"],
        "GroupMembership: root daemon alice svc\n",
    );
    mac.dscl_ok(&[".", "-read", "/Users/svc", "IsHidden"], "IsHidden: 1\n");
    assert_eq!(
        mac.prepare("").map(|ready| ready.login_user),
        Ok("alice".to_owned())
    );
}

#[test]
fn an_administrator_whose_name_is_not_a_plain_account_name_is_skipped() {
    let mac = Mac::new("ssh-mac-odd-name");
    mac.dscl_ok(
        &[".", "-list", "/Users", "UniqueID"],
        "alice 501\n../x 502\n",
    );
    assert_eq!(
        mac.prepare("").map(|ready| ready.login_user),
        Ok("alice".to_owned())
    );
}

#[test]
fn an_is_hidden_read_that_cannot_run_is_unsupported() {
    let mac = Mac::new("ssh-mac-hidden-fail");
    mac.dscl.always(
        &args(&[".", "-read", "/Users/alice", "IsHidden"]),
        spawn_failure(),
    );
    assert_eq!(mac.prepare(""), blocked(UNSUPPORTED, "dscl"));
}

fn access_group(mac: &Mac, stdout: &str) {
    mac.dscl_ok(
        &[
            ".",
            "-read",
            "/Groups/com.apple.access_ssh",
            "GroupMembership",
            "NestedGroups",
        ],
        stdout,
    );
}

#[test]
fn a_login_user_listed_in_the_access_group_is_admitted() {
    let mac = Mac::new("ssh-mac-group-member");
    access_group(&mac, "GroupMembership: bob alice\n");
    assert_eq!(
        mac.prepare("").map(|ready| ready.login_user),
        Ok("alice".to_owned())
    );
}

#[test]
fn a_login_user_missing_from_the_access_group_is_blocked_and_nothing_is_changed() {
    let mac = Mac::new("ssh-mac-group-out");
    access_group(&mac, "GroupMembership: bob\n");
    assert_eq!(
        mac.prepare(""),
        blocked(BLOCKED_BY_ACCESS_GROUP, "com.apple.access_ssh")
    );
    assert!(!mac.changed_remote_login());
}

#[test]
fn a_group_with_no_members_blocks_the_login_user() {
    let mac = Mac::new("ssh-mac-group-empty");
    access_group(&mac, "No such key: GroupMembership\n");
    assert_eq!(
        mac.prepare(""),
        blocked(BLOCKED_BY_ACCESS_GROUP, "com.apple.access_ssh")
    );
}

#[test]
fn an_access_group_that_nests_admin_admits_an_administrator() {
    let mac = Mac::new("ssh-mac-group-nested");
    access_group(
        &mac,
        &format!("NestedGroups: {}\n", ADMIN_GROUP_GUID.to_lowercase()),
    );
    assert_eq!(
        mac.prepare("").map(|ready| ready.login_user),
        Ok("alice".to_owned())
    );
}

#[test]
fn an_access_group_that_nests_admin_does_not_admit_a_user_outside_admin() {
    let mac = Mac::new("ssh-mac-group-nested-out");
    mac.dscl_ok(
        &[".", "-read", "/Users/builder", "UniqueID"],
        "UniqueID: 502\n",
    );
    access_group(&mac, &format!("NestedGroups: {ADMIN_GROUP_GUID}\n"));
    assert_eq!(
        mac.prepare("builder"),
        blocked(BLOCKED_BY_ACCESS_GROUP, "com.apple.access_ssh")
    );
}

#[test]
fn an_access_group_that_nests_another_group_does_not_admit_an_administrator() {
    let mac = Mac::new("ssh-mac-group-nested-other");
    access_group(&mac, "NestedGroups: 11111111-2222-3333-4444-555555555555\n");
    assert_eq!(
        mac.prepare(""),
        blocked(BLOCKED_BY_ACCESS_GROUP, "com.apple.access_ssh")
    );
}

#[test]
fn an_access_group_read_that_fails_for_another_reason_is_unsupported() {
    let mac = Mac::new("ssh-mac-group-error");
    mac.dscl.always(
        &args(&[
            ".",
            "-read",
            "/Groups/com.apple.access_ssh",
            "GroupMembership",
            "NestedGroups",
        ]),
        Ok(LaunchctlOutput::exit(1, "dscl: Permission denied")),
    );
    assert_eq!(mac.prepare(""), blocked(UNSUPPORTED, "dscl"));
}

#[test]
fn values_wrap_onto_the_lines_below_their_key() {
    let text = "GroupMembership: a b\n c d\nOther: x\n e\nGroupMembership: z\n";
    assert_eq!(values_of(text, "GroupMembership"), ["a", "b", "c", "d", "z"]);
    assert_eq!(values_of(text, "Other"), ["x", "e"]);
    assert!(values_of(text, "Missing").is_empty());
}

#[test]
fn the_ids_of_a_listing_skip_lines_that_are_not_a_name_and_a_number() {
    assert_eq!(
        parse_ids("a 1\n\nb\nc x\nd 2 3\ne 7\n"),
        [("a".to_owned(), 1), ("e".to_owned(), 7)]
    );
}

/// The Full Disk Access profile that docs/macos.md tells Apple Business to push: a property list
/// whose one payload grants `SystemPolicyAllFiles` to the daemon by its signing identifier and
/// pins the Team ID in the code requirement, so an update that moves the binary keeps the grant.
#[test]
fn the_full_disk_access_profile_grants_the_daemon_alone_all_files_by_identifier_and_team() {
    let profile = plist::Value::from_reader_xml(
        include_str!("../../../docs/deploy/mac/hostkeeper-full-disk-access.mobileconfig")
            .as_bytes(),
    )
    .expect("a property list");
    let top = profile.as_dictionary().expect("a dictionary");
    assert_eq!(
        top.get("PayloadType").and_then(plist::Value::as_string),
        Some("Configuration")
    );
    let payloads = top
        .get("PayloadContent")
        .and_then(plist::Value::as_array)
        .expect("payloads");
    assert_eq!(payloads.len(), 1);
    let payload = payloads[0].as_dictionary().expect("a payload");
    assert_eq!(
        payload.get("PayloadType").and_then(plist::Value::as_string),
        Some("com.apple.TCC.configuration-profile-policy")
    );
    let services = payload
        .get("Services")
        .and_then(plist::Value::as_dictionary)
        .expect("services");
    assert_eq!(services.len(), 1);
    let grants = services
        .get("SystemPolicyAllFiles")
        .and_then(plist::Value::as_array)
        .expect("grants");
    assert_eq!(grants.len(), 1);
    let grant = grants[0].as_dictionary().expect("a grant");
    let text = |key: &str| grant.get(key).and_then(plist::Value::as_string);
    assert_eq!(text("Identifier"), Some("dev.hostkeeper.hostkeeper"));
    assert_eq!(text("IdentifierType"), Some("bundleID"));
    assert_eq!(text("Authorization"), Some("Allow"));
    let requirement = text("CodeRequirement").expect("a code requirement");
    assert!(requirement.contains("identifier \"dev.hostkeeper.hostkeeper\""));
    assert!(requirement.contains("certificate leaf[subject.OU] = \"U7RTX4XD3X\""));
    assert!(requirement.contains("anchor apple generic"));
}
