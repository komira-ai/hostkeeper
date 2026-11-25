//! How a session of the BMC tool is invoked, and what of it prints.

use super::{Invocation, PASSWORD_VARIABLE, Privilege, Settings, ShellError, ToolText};
use crate::credential::BmcCredential;
use crate::targets::Address;

fn credential() -> BmcCredential {
    BmcCredential::parse("user = \"ADMIN\"\npassword = \"pass word 1\"\n").expect("a credential")
}

/// Proves: the password is what the invocation hands to the environment and is in none of its
/// arguments, while the user is the one argument `-U` that the tool takes; printed, the
/// invocation shows neither.
///
/// Catches: the password passed with `-P`, where every user of the host reads it from the process
/// table, and an invocation logged with its arguments.
#[test]
fn the_password_is_in_the_childs_environment_and_not_in_argv() {
    let credential = credential();
    let address = Address::Ip("192.0.2.12".parse().expect("an address"));
    let invocation = Invocation::new(&address, &credential, &Settings::default(), false);
    assert_eq!(invocation.password(), "pass word 1");
    assert!(
        invocation
            .args()
            .iter()
            .all(|arg| !arg.contains("pass word 1") && arg != "-P"),
        "{:?}",
        invocation.args()
    );
    assert!(invocation.args().iter().any(|arg| arg == "-E"));
    let user = invocation.args().iter().position(|arg| arg == "ADMIN");
    assert_eq!(
        user.map(|at| invocation.args()[at - 1].as_str()),
        Some("-U")
    );
    assert_eq!(format!("{invocation:?}"), "Invocation { <redacted> }");
    assert_eq!(PASSWORD_VARIABLE, "IPMI_PASSWORD");
}

/// Proves: the settings reach their flags, an IPv6 address and a DNS name are written as the tool
/// takes them, and `-v` comes only in a verbose run, before `shell`.
///
/// Catches: a cipher suite or privilege flag that is not the operator's, and a verbose flag in a
/// session that sends commands (whose answers it would change).
#[test]
fn the_settings_and_the_address_reach_their_flags() {
    let credential = credential();
    let settings = Settings {
        cipher: 17,
        privilege: Privilege::Operator,
        timeout_secs: 4,
        retries: 0,
    };
    let address = Address::Ip("2001:db8::7".parse().expect("an address"));
    let verbose = Invocation::new(&address, &credential, &settings, true);
    assert_eq!(
        verbose.args(),
        [
            "-I",
            "lanplus",
            "-H",
            "2001:db8::7",
            "-U",
            "ADMIN",
            "-E",
            "-C",
            "17",
            "-L",
            "OPERATOR",
            "-N",
            "4",
            "-R",
            "0",
            "-v",
            "shell",
        ]
    );
    let named = Address::Name("bmc-02.lab.example".to_owned());
    let quiet = Invocation::new(&named, &credential, &settings, false);
    assert_eq!(quiet.args()[3], "bmc-02.lab.example");
    assert_eq!(quiet.args().last().map(String::as_str), Some("shell"));
    assert!(!quiet.args().iter().any(|arg| arg == "-v"));
    assert_eq!(
        [
            Privilege::User,
            Privilege::Operator,
            Privilege::Administrator
        ]
        .map(Privilege::as_str),
        ["USER", "OPERATOR", "ADMINISTRATOR"]
    );
    assert_eq!(
        Settings::default(),
        Settings {
            cipher: 3,
            privilege: Privilege::Administrator,
            timeout_secs: 1,
            retries: 2,
        }
    );
}

/// Proves: the tool's standard error prints as its length only, inside an error too, and is read
/// back whole for matching.
///
/// Catches: the tool's text, which holds the user name, reaching a log line through `Debug`.
#[test]
fn the_tools_text_prints_as_its_length_only() {
    let text = ToolText::new("user ADMIN refused".to_owned());
    assert_eq!(text.as_str(), "user ADMIN refused");
    assert_eq!(format!("{text:?}"), "ToolText(<18 bytes>)");
    assert_eq!(
        format!("{:?}", ShellError::Ended(text)),
        "Ended(ToolText(<18 bytes>))"
    );
}
