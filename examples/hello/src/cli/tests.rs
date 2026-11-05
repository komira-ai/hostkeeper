use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

use super::{Command, DEFAULT_PORT, STEPS, parse};

fn args(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

fn serve(port: u16, fail_ready: bool) -> Result<Command, String> {
    Ok(Command::Serve { port, fail_ready })
}

/// Catches a default port other than the one the package's unit and health URL use.
#[test]
fn no_arguments_serve_on_the_default_port() {
    assert_eq!(DEFAULT_PORT, 9310);
    assert_eq!(parse(&[]), serve(9310, false));
}

/// Catches `--port` or `--fail-ready` being ignored, or read only in one order.
#[test]
fn port_and_fail_ready_are_read_in_any_order() {
    assert_eq!(parse(&args(&["--port", "8080"])), serve(8080, false));
    assert_eq!(parse(&args(&["--fail-ready"])), serve(9310, true));
    assert_eq!(
        parse(&args(&["--fail-ready", "--port", "1"])),
        serve(1, true)
    );
    assert_eq!(
        parse(&args(&["--port", "65535", "--fail-ready"])),
        serve(65535, true)
    );
}

/// Catches a port outside 1 to 65535, or one that is not plain decimal digits, being accepted.
#[test]
fn a_port_outside_one_to_65535_or_not_decimal_is_refused() {
    for bad in [
        "0",
        "65536",
        "-1",
        "+80",
        "80x",
        "",
        " 80",
        "99999999999999999999",
        "0x50",
    ] {
        let got = parse(&args(&["--port", bad]));
        assert!(
            got.as_ref()
                .is_err_and(|why| why.starts_with("--port needs a number from 1 to 65535")),
            "{bad:?} gave {got:?}"
        );
    }
    let not_text = [
        OsString::from("--port"),
        OsStr::from_bytes(b"8\xff").to_owned(),
    ];
    assert!(parse(&not_text).is_err());
}

/// Catches `--port` with no value, or given twice, being accepted.
#[test]
fn a_port_flag_without_value_or_given_twice_is_refused() {
    assert_eq!(
        parse(&args(&["--port"])),
        Err("--port needs a port number".to_owned())
    );
    assert_eq!(
        parse(&args(&["--port", "1", "--port", "2"])),
        Err("--port given twice".to_owned())
    );
}

/// Catches an unknown argument (or one that is not text) starting a server.
#[test]
fn an_unknown_argument_is_refused_and_named() {
    assert_eq!(
        parse(&args(&["--bogus"])),
        Err("unknown argument \"--bogus\"".to_owned())
    );
    assert_eq!(
        parse(&args(&["--port", "80", "extra"])),
        Err("unknown argument \"extra\"".to_owned())
    );
    let not_text = [OsStr::from_bytes(b"\xff").to_owned()];
    assert!(parse(&not_text).is_err());
}

/// Catches `--version` starting the server, or being lost beside other flags.
#[test]
fn version_is_asked_with_the_flag() {
    assert_eq!(parse(&args(&["--version"])), Ok(Command::Version));
    assert_eq!(
        parse(&args(&["--port", "80", "--version"])),
        Ok(Command::Version)
    );
}

/// Catches a hook invocation (with or without `update-hook`) being read as server flags, or the
/// arguments being cut.
#[test]
fn update_hook_and_a_bare_step_both_reach_the_hook() {
    assert_eq!(
        parse(&args(&["update-hook", "verify", "--params", "f"])),
        Ok(Command::Hook(args(&["verify", "--params", "f"])))
    );
    assert_eq!(
        parse(&args(&["update-hook"])),
        Ok(Command::Hook(Vec::new()))
    );
    for step in STEPS {
        assert_eq!(
            parse(&args(&[step, "--params", "f"])),
            Ok(Command::Hook(args(&[step, "--params", "f"]))),
            "{step}"
        );
    }
    assert_eq!(
        parse(&args(&["prepare", "/staged", "--params", "f"])),
        Ok(Command::Hook(args(&[
            "prepare", "/staged", "--params", "f"
        ])))
    );
}
