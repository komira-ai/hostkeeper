//! The usage rules the clap tree itself enforces: conflicting and paired global flags, and the
//! names the stability golden lists.

use bytes::Bytes;

use super::{HELP_FLAG, REQUEST_FLAG, build};
use crate::exit;
use crate::testutil::{Fake, fixture, run_fixture, with_call};

/// Runs `args` and checks for exit 2 with `message` on stderr and no transport made; a usage error
/// that connects anyway fails here.
fn refused(args: &[&str], message: &str) {
    let fake = Fake::with(Ok(Bytes::new()));
    let ran = run_fixture(&with_call(args), &fake, b"");
    assert_eq!(ran.code, exit::USAGE, "{args:?}: {ran:?}");
    assert_eq!(ran.stdout, "", "{args:?}");
    // The wording is clap's; ours is that the flags are named.
    assert!(ran.stderr.contains(message), "{args:?}: {}", ran.stderr);
    assert!(fake.targets().is_empty() && fake.calls().is_empty());
}

/// Catches a token read from both a file and stdin silently preferring one of them.
#[test]
fn token_file_with_token_stdin_is_a_usage_error() {
    refused(
        &["gadgets", "list", "--token-file", "t.txt", "--token-stdin"],
        "--token-stdin",
    );
    refused(
        &["gadgets", "list", "--token-stdin", "--token-file", "t.txt"],
        "--token-file",
    );
}

/// Catches a lone client certificate or key being dropped, so that the call goes without client
/// authentication.
#[test]
fn a_client_certificate_and_key_come_together() {
    refused(
        &["gadgets", "list", "--client-cert-file", "c.pem"],
        "--client-key-file",
    );
    refused(
        &["gadgets", "list", "--client-key-file", "k.pem"],
        "--client-cert-file",
    );
}

/// Catches the `--request` and `--help` spellings of the tree drifting from the constants the
/// stability lines are written from.
#[test]
fn every_rpc_takes_the_named_request_and_help_flags() {
    let cli = build(fixture::COMMANDS);
    for service in cli.get_subcommands() {
        for rpc in service.get_subcommands() {
            let longs: Vec<&str> = rpc.get_arguments().filter_map(|a| a.get_long()).collect();
            assert!(longs.contains(&REQUEST_FLAG), "{longs:?}");
        }
    }
    let mut cli = cli;
    cli.build();
    assert!(cli.get_arguments().any(|a| a.get_long() == Some(HELP_FLAG)));
}
