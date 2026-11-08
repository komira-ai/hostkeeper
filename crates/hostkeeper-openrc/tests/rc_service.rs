//! `RcService`: the exact `rc-service` argument vector of each call, how a failed command and a
//! refused name read, and which names `service_name` accepts. The launcher is the fake of
//! `hostkeeper_placed::testing`, so no program runs.

use hostkeeper_openrc::RcService;
use hostkeeper_openrc::rc_service::{MAX_SERVICE_NAME, RC_SERVICE, service_name};
use hostkeeper_placed::testing::FakeLauncher;
use hostkeeper_placed::{UnitControl, UnitControlError};

fn argv(unit: &str, verb: &str) -> Vec<String> {
    vec![RC_SERVICE.to_owned(), unit.to_owned(), verb.to_owned()]
}

fn refused(unit: &str, verb: &str) -> UnitControlError {
    UnitControlError {
        action: format!("{RC_SERVICE} {unit} {verb}"),
        why: format!("{unit:?} is not the name of an OpenRC service"),
    }
}

#[test]
fn rc_service_is_the_sbin_path() {
    assert_eq!(RC_SERVICE, "/sbin/rc-service");
    assert_eq!(MAX_SERVICE_NAME, 255);
}

#[test]
fn service_name_accepts_the_names_of_init_scripts() {
    let longest = "a".repeat(255);
    for name in [
        "hostkeeper",
        "a",
        "9",
        "7zip",
        "net.eth0",
        "my_service",
        "my-service",
        "A.b_c-d9",
        "a.",
        "a-",
        longest.as_str(),
    ] {
        assert_eq!(service_name(name), Ok(name), "{name:?}");
    }
}

#[test]
fn service_name_refuses_what_rc_service_would_read_as_an_option_or_a_path() {
    let too_long = "a".repeat(256);
    for name in [
        "",
        "-",
        "-s",
        "--help",
        ".hidden",
        ".",
        "..",
        "_x",
        "a/b",
        "/etc/init.d/x",
        "x/",
        "a b",
        "a\n",
        "a\0b",
        "caf\u{e9}",
        "\u{e9}a",
        "a@b",
        "@a",
        "a:b",
        too_long.as_str(),
    ] {
        assert_eq!(
            service_name(name),
            Err(format!("{name:?} is not the name of an OpenRC service")),
            "{name:?}"
        );
    }
}

#[test]
fn service_name_counts_bytes_not_characters() {
    // 128 two-byte characters are 256 bytes, and non-ASCII besides.
    let name = "\u{e9}".repeat(128);
    assert!(service_name(&name).is_err());
}

#[test]
fn daemon_reload_runs_nothing() {
    let control = RcService::new(FakeLauncher::new());
    assert_eq!(control.daemon_reload(), Ok(()));
    assert!(control.launcher().runs().is_empty());
}

#[test]
fn stop_unit_runs_rc_service_stop() {
    let control = RcService::new(FakeLauncher::new());
    assert_eq!(control.stop_unit("hostkeeper"), Ok(()));
    assert_eq!(control.launcher().runs(), vec![argv("hostkeeper", "stop")]);
}

#[test]
fn start_unit_runs_rc_service_start() {
    let control = RcService::new(FakeLauncher::new());
    assert_eq!(control.start_unit("hostkeeper"), Ok(()));
    assert_eq!(control.launcher().runs(), vec![argv("hostkeeper", "start")]);
}

#[test]
fn a_failed_stop_names_the_command_and_the_launchers_words() {
    let control = RcService::new(FakeLauncher::failing());
    assert_eq!(
        control.stop_unit("hostkeeper"),
        Err(UnitControlError {
            action: "/sbin/rc-service hostkeeper stop".to_owned(),
            why: "exit status 1".to_owned(),
        })
    );
    assert_eq!(control.launcher().runs(), vec![argv("hostkeeper", "stop")]);
}

#[test]
fn a_failed_start_names_the_command_and_the_launchers_words() {
    let control = RcService::new(FakeLauncher::failing());
    assert_eq!(
        control.start_unit("mdm"),
        Err(UnitControlError {
            action: "/sbin/rc-service mdm start".to_owned(),
            why: "exit status 1".to_owned(),
        })
    );
    assert_eq!(control.launcher().runs(), vec![argv("mdm", "start")]);
}

#[test]
fn stop_and_start_refuse_a_bad_name_before_anything_runs() {
    let control = RcService::new(FakeLauncher::new());
    assert_eq!(control.stop_unit("--help"), Err(refused("--help", "stop")));
    assert_eq!(control.start_unit("../x"), Err(refused("../x", "start")));
    assert!(control.launcher().runs().is_empty());
}

#[test]
fn is_active_is_true_when_status_exits_0() {
    let control = RcService::new(FakeLauncher::new());
    assert_eq!(control.is_active("hostkeeper"), Ok(true));
    assert_eq!(
        control.launcher().runs(),
        vec![argv("hostkeeper", "status")]
    );
}

#[test]
fn is_active_is_false_when_status_fails() {
    let control = RcService::new(FakeLauncher::failing());
    assert_eq!(control.is_active("hostkeeper"), Ok(false));
    assert_eq!(
        control.launcher().runs(),
        vec![argv("hostkeeper", "status")]
    );
}

#[test]
fn is_active_is_false_for_a_bad_name_without_a_command() {
    let control = RcService::new(FakeLauncher::new());
    assert_eq!(control.is_active("-s"), Ok(false));
    assert_eq!(control.is_active(""), Ok(false));
    assert!(control.launcher().runs().is_empty());
}

#[test]
fn calls_run_in_order_through_the_one_launcher() {
    let control = RcService::new(FakeLauncher::new());
    control.daemon_reload().expect("reload");
    control.stop_unit("a").expect("stop");
    control.start_unit("a").expect("start");
    control.is_active("b").expect("status");
    assert_eq!(
        control.launcher().runs(),
        vec![argv("a", "stop"), argv("a", "start"), argv("b", "status")]
    );
}
