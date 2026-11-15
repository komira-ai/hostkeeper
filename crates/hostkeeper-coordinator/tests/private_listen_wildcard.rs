//! `hostkeeper-coordinator serve --private-listen-wildcard`: the opt-in that lets the private
//! listener take a wildcard address, for a network namespace whose interfaces are loopback and
//! one private network. Without it a wildcard is refused (`config.rs`,
//! `a_wildcard_private_listen_is_refused_at_start`); with it each spelling of a wildcard is
//! taken and bound as given, an address of one interface is taken as without it, and the other
//! listeners are checked as before. Driven in process; nothing binds a socket.

mod support;

use std::ffi::OsString;
use std::future::Pending;
use std::io;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use clap::Parser;
use clap::error::ErrorKind;
use futures::FutureExt;
use hostkeeper_coordinator::config::{Bound, Cli, StartError, run};
use support::serve::{Files, build_given, exit_code, flags_of, refusal, renamed, with};
use tokio::io::DuplexStream;

/// The flag this file is about.
const OPT_IN: &str = "--private-listen-wildcard";

/// The wildcard addresses of the private listener, each way it can be spelled: IPv4, IPv6, and
/// the IPv4 one written as IPv6.
const WILDCARDS: [&str; 3] = ["0.0.0.0:2", "[::]:2", "[::ffff:0.0.0.0]:2"];

/// `args` with the opt-in added.
fn opted_in(args: &[OsString]) -> Vec<OsString> {
    let mut args = args.to_vec();
    args.push(OPT_IN.into());
    args
}

fn addr(text: &str) -> SocketAddr {
    text.parse().expect("an address")
}

/// Proves: `serve --help` lists the opt-in; it is off unless given; it takes no value, so
/// `--private-listen-wildcard=true` is a usage error (exit 2); and given twice it is a usage
/// error too.
///
/// Catches: the opt-in on by default, which would take a wildcard without anyone asking.
#[test]
fn the_opt_in_is_a_listed_flag_that_is_off_unless_given() {
    let help = Cli::try_parse_from(["hostkeeper-coordinator", "serve", "--help"])
        .expect_err("help")
        .to_string();
    assert!(help.contains(OPT_IN), "{help}");

    let files = Files::new("wildcard-flag");
    let args = files.args();
    assert!(!flags_of(args.clone()).private_listen_wildcard);
    assert!(flags_of(opted_in(&args)).private_listen_wildcard);

    let mut valued = args.clone();
    valued.push(format!("{OPT_IN}=true").into());
    let why = Cli::try_parse_from(valued.clone()).expect_err("a value");
    assert_eq!(why.kind(), ErrorKind::TooManyValues, "{why}");
    assert_eq!(exit_code(valued), ExitCode::from(2));

    let twice = opted_in(&opted_in(&args));
    let why = Cli::try_parse_from(twice.clone()).expect_err("given twice");
    assert_eq!(why.kind(), ErrorKind::ArgumentConflict, "{why}");
    assert_eq!(exit_code(twice), ExitCode::from(2));
}

/// Proves: with the opt-in, each spelling of a wildcard `--private-listen` builds a start whose
/// private listener is that address unchanged, through the old flag name `--control-listen`
/// too; and the same address without the opt-in is still refused by name.
///
/// Catches: an opt-in that does not take a wildcard, takes only one spelling of it, or turns it
/// into another address.
#[test]
fn the_opt_in_takes_each_spelling_of_a_wildcard_private_listen() {
    let files = Files::new("wildcard-taken");
    let args = files.args();
    for wildcard in WILDCARDS {
        let given = with(&args, "--private-listen", wildcard);
        let parts = build_given(&flags_of(opted_in(&given))).expect("the opt-in takes it");
        assert_eq!(parts.control_listen, addr(wildcard));
        drop(parts);

        let old = renamed(&given, "--private-listen", "--control-listen");
        let parts = build_given(&flags_of(opted_in(&old))).expect("through the old name");
        assert_eq!(parts.control_listen, addr(wildcard));
        drop(parts);

        let why = refusal(given);
        assert!(
            matches!(&why, StartError::PrivateListen(a) if *a == addr(wildcard)),
            "{why}"
        );
    }
}

/// Proves: with the opt-in, an address of one interface (loopback and a private network) is
/// the private listener as given, as it is without the opt-in.
///
/// Catches: an opt-in that rewrites, or refuses, an address that was already taken.
#[test]
fn the_opt_in_takes_an_address_of_one_interface_as_without_it() {
    let files = Files::new("wildcard-one-interface");
    let args = files.args();
    for one in ["127.0.0.1:2", "192.0.2.1:2", "[::1]:2"] {
        let given = with(&args, "--private-listen", one);
        let with_it = build_given(&flags_of(opted_in(&given))).expect("with the opt-in");
        assert_eq!(with_it.control_listen, addr(one));
        drop(with_it);
        let without_it = build_given(&flags_of(given)).expect("without the opt-in");
        assert_eq!(without_it.control_listen, addr(one));
    }
}

/// Proves: the opt-in is for the private listener only. With it and a wildcard
/// `--private-listen`, a wildcard `--lan-listen` is still refused (`LanListen`), and a
/// `--health-listen` that is not loopback (a wildcard, or a private network's address) is still
/// refused (`HealthListen`), each with exit 1.
///
/// Catches: an opt-in that also opens the LAN listener, whose address the server certificate
/// names, or the health listener, which answers with no token and no TLS.
#[test]
fn the_opt_in_leaves_the_other_listeners_checked_as_before() {
    let files = Files::new("wildcard-others");
    let args = opted_in(&with(&files.args(), "--private-listen", "0.0.0.0:2"));

    // The LAN listener needs the flags of enrollment to parse; its address is checked before
    // any file they name is read.
    let mut lan = args.clone();
    for arg in [
        "--ca-key-file",
        "ca.key",
        "--server-cert-file",
        "server.pem",
        "--server-name",
        "coordinator.example.com",
        "--lan-listen",
        "0.0.0.0:3",
    ] {
        lan.push(arg.into());
    }
    let why = refusal(lan.clone());
    assert!(
        matches!(&why, StartError::LanListen(a) if *a == addr("0.0.0.0:3")),
        "{why}"
    );
    assert_eq!(exit_code(lan), ExitCode::from(1));

    for health in ["0.0.0.0:9", "[::]:9", "192.0.2.1:9"] {
        let mut given = args.clone();
        given.extend([OsString::from("--health-listen"), OsString::from(health)]);
        let why = refusal(given.clone());
        assert!(
            matches!(&why, StartError::HealthListen(a) if *a == addr(health)),
            "{why}"
        );
        assert_eq!(exit_code(given), ExitCode::from(1));
    }
}

type Never = fn() -> Pending<io::Result<DuplexStream>>;

fn never() -> Pending<io::Result<DuplexStream>> {
    std::future::pending()
}

/// Proves: with the opt-in, `run` asks the bind function for the heartbeat address and then the
/// wildcard address of the private listener, as given, and calls `ready` with what it answered.
///
/// Catches: a wildcard that is taken at start but bound somewhere else.
#[tokio::test]
async fn run_binds_the_private_listener_on_the_wildcard_it_was_given() {
    let files = Files::new("wildcard-run");
    for wildcard in WILDCARDS {
        let given = opted_in(&with(&files.args(), "--private-listen", wildcard));
        let parts = build_given(&flags_of(given)).expect("a start");
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&asked);
        let bind = move |at: SocketAddr| {
            log.lock().expect("log").push(at);
            let answered = addr(&format!("198.51.100.7:{}", at.port()));
            async move { Ok::<_, io::Error>((answered, never as Never)) }
        };
        let ready = Arc::new(Mutex::new(None));
        let told = Arc::clone(&ready);
        let outcome = run(parts, bind, move |bound| {
            *told.lock().expect("told") = Some(bound);
        })
        .now_or_never();
        assert!(outcome.is_none(), "it serves until it is dropped");
        assert_eq!(
            *asked.lock().expect("log"),
            vec![addr("192.0.2.1:1"), addr(wildcard)],
            "the heartbeat address, then the wildcard"
        );
        assert_eq!(
            *ready.lock().expect("told"),
            Some(Bound {
                heartbeat: addr("198.51.100.7:1"),
                control: addr("198.51.100.7:2"),
                lan: None,
                health: None,
            })
        );
    }
}
