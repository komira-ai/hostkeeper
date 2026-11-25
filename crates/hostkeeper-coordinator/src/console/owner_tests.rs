//! The console and API tokens: a token acts as its owner, so the console operators file is read
//! for the owner's name, never the token's; the audit line names the owner as `by` and the token
//! as `api_token`; and a stream attached with a token ends when the token is revoked or its owner
//! is disabled. No timing: the sweep is called, and a close is read off the stream.

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::ConsoleRefusal as Refusal;

use super::attach_support::{CAROL, Live};
use super::support::NODE_BMC_ANSWER;
use super::support::{MAC, NODE, caller};
use crate::accounts::{ApiToken, User};
use crate::audit_actions::AuditOnly;
use crate::operator::Caller;
use crate::ops_file::digest;
use crate::service::Unauthorized;
use crate::websocket::tests::client_frame;

/// `sol info` with both rates at 115.2.
const INFO: &str =
    "Volatile Bit Rate (kbps)        : 115.2\nNon-Volatile Bit Rate (kbps)    : 115.2\n";

/// The secret of `alice-bot`, owned by `alice`, whom the console operators file names.
const ALICE_BOT: &str = "hkt_alice-bot-0123456789abcdef0123456789abcdef";
/// The secret of `alice-ops`, also owned by `alice`.
const ALICE_OPS: &str = "hkt_alice-ops-0123456789abcdef0123456789abcdef";
/// The secret of `bob`, a token named as an operator of the file but owned by `zed`, whom the
/// file does not name.
const NAMED_BOB: &str = "hkt_named-bob-0123456789abcdef0123456789abcdef";

/// A console whose tokens file names only `carol`, and whose store holds the users `alice` and
/// `zed` (both enabled) and the API tokens `alice-bot` and `alice-ops` of `alice` and `bob` of
/// `zed`.
fn live(name: &str) -> Live {
    let live = Live::new(name);
    live.keep_tokens(&[("carol", CAROL)]);
    let user = |name: &str| User {
        name: name.to_owned(),
        hash: "$argon2id$v=19$m=1,t=1,p=1$c2FsdA$aGFzaA".to_owned(),
        enabled: true,
        created_by: "coordinator".to_owned(),
        ..User::default()
    };
    let token = |name: &str, owner: &str, secret: &str| ApiToken {
        name: name.to_owned(),
        sha256: digest(secret.as_bytes()),
        owner: owner.to_owned(),
        created_by: owner.to_owned(),
        created_at: 1,
    };
    live.coordinator
        .accounts()
        .change(|accounts| {
            accounts
                .with_user(user("alice"))?
                .with_user(user("zed"))?
                .with_token(token("alice-bot", "alice", ALICE_BOT))?
                .with_token(token("alice-ops", "alice", ALICE_OPS))?
                .with_token(token("bob", "zed", NAMED_BOB))
        })
        .expect("the users and their tokens");
    live
}

/// The caller the bearer `secret` is.
fn bearer(live: &Live, secret: &str) -> Caller {
    Caller::Operator(live.coordinator.operator(Some(secret)).expect("a bearer"))
}

fn open(host: &str) -> proto::OpenConsoleRequest {
    proto::OpenConsoleRequest {
        host_id: host.to_owned(),
        reason: "watch it boot".to_owned(),
    }
}

/// The console's audit lines as `(by, action, api_token)`, oldest first.
fn said(live: &Live) -> Vec<(String, String, Option<String>)> {
    live.coordinator
        .ops()
        .audit_log()
        .expect("the log")
        .into_iter()
        .filter(|entry| AuditOnly::is_audit_only(&entry.action))
        .map(|entry| (entry.by, entry.action, entry.api_token))
        .collect()
}

/// Proves: an API token owned by a user the console operators file names opens a console as that
/// user, and the `console_open` line is by the owner with the token as `api_token`; a token whose
/// own name the file names but whose owner it does not is refused `no_console_access`, and writes
/// no line (the file does not name its operator); once the owner is disabled the token is no
/// bearer at all.
///
/// Catches: an operators file read for the token's name (which refused the lead's token, named
/// for its machine, while its owner was listed; and would let a token named like a listed
/// operator in), an audit that names the token as the actor or forgets it, and a disabled
/// person's token that still opens consoles.
#[tokio::test]
async fn an_api_token_opens_a_console_as_its_owner_whom_the_file_must_name() {
    let live = live("console-owner-open");
    let coordinator = &live.coordinator;
    coordinator
        .console_open(&bearer(&live, ALICE_BOT), &open(NODE))
        .expect("opened as alice");
    assert_eq!(
        coordinator
            .console_open(&bearer(&live, NAMED_BOB), &open(MAC))
            .expect_err("zed is not in the file"),
        Refusal::NoConsoleAccess
    );
    assert_eq!(
        said(&live),
        [(
            "alice".to_owned(),
            "console_open".to_owned(),
            Some("alice-bot".to_owned())
        )]
    );
    coordinator
        .accounts()
        .change(|accounts| accounts.with_enabled("alice", false))
        .expect("alice is disabled");
    assert_eq!(coordinator.operator(Some(ALICE_BOT)), Err(Unauthorized));
    assert_eq!(coordinator.operator(Some(ALICE_OPS)), Err(Unauthorized));
}

/// Proves: a stream attached with an API token is held to that token on the sweep: it stays while
/// the token is stored and its owner enabled, closes 4008 `token_revoked` once the token is
/// revoked (a stream attached with the owner's other token is not touched), and the other closes
/// 4008 too once the owner is disabled.
///
/// Catches: a console held open by a revoked token, or by the token of a person who was disabled,
/// until it hits its time limit; and a sweep that looks for a token named after the owner, which
/// would cut every stream attached with an API token at the first sweep.
#[tokio::test]
async fn a_stream_attached_with_an_api_token_ends_with_the_token_or_its_owner() {
    let live = live("console-owner-sweep");
    let _host = live
        .fixture
        .tool
        .sol_session(&[NODE_BMC_ANSWER, " 01 00 00\n", INFO]);
    live.fixture.tool.session(&[NODE_BMC_ANSWER, ""]);
    let _mac = live.fixture.dials.connect();
    let serial = live
        .coordinator
        .console_open(&bearer(&live, ALICE_BOT), &open(NODE))
        .expect("opened")
        .session;
    let screen = live
        .coordinator
        .console_open(&bearer(&live, ALICE_OPS), &open(MAC))
        .expect("opened")
        .session;
    let mut by_bot = live.attach(&serial, ALICE_BOT).await;
    let mut by_ops = live.attach(&screen, ALICE_OPS).await;
    live.coordinator.console_sweep();
    assert_eq!(live.coordinator.console.lock_table().live.len(), 2);

    live.coordinator
        .accounts()
        .change(|accounts| accounts.without_token("alice-ops"))
        .expect("the token is revoked");
    live.coordinator.console_sweep();
    by_ops.closed(4008, "token_revoked").await;
    live.gone(MAC).await;
    assert_eq!(live.coordinator.console.lock_table().live.len(), 1);

    live.coordinator
        .accounts()
        .change(|accounts| accounts.with_enabled("alice", false))
        .expect("alice is disabled");
    live.coordinator.console_sweep();
    by_bot.closed(4008, "token_revoked").await;
    live.gone(NODE).await;
}

/// `Console/Close` of `host`'s session by `closer`.
fn close(live: &Live, closer: &Caller, host: &str) {
    live.coordinator
        .console_close(
            closer,
            &proto::CloseConsoleRequest {
                host_id: host.to_owned(),
            },
        )
        .expect("closed");
}

/// The last console line as `(by, action, api_token)`.
fn last(live: &Live) -> (String, String, Option<String>) {
    said(live).pop().expect("a line")
}

/// Proves: the `console_close` line of a stream records the API token as `api_token`, as its
/// `console_open` does: when the client leaves, the token the stream attached with; when an
/// operator's `Console/Close` ends it, the closer's token (here the owner's other token), and
/// none for a closer that holds no API token (a token of the tokens file); when the coordinator
/// ends it (a revoked token), none, by `coordinator`. `Console/Close` of a session nothing
/// attached records the closer's token too, so both kinds of close agree.
///
/// Catches: a stream's close line that keeps the owner's name and drops the token, so the audit
/// cannot tell which of a person's scripts (or the person's own sign-in) held or ended a console.
#[tokio::test]
async fn a_streams_close_line_records_the_api_token_as_its_open_line_does() {
    let live = live("console-owner-close");
    let coordinator = &live.coordinator;
    let opened = |token: &str| {
        coordinator
            .console_open(&bearer(&live, token), &open(MAC))
            .expect("opened")
            .session
    };
    let alice_bot = Some("alice-bot".to_owned());
    let line = |by: &str, token: Option<String>| (by.to_owned(), "console_close".to_owned(), token);

    let _mac = live.fixture.dials.connect();
    let mut client = live.attach(&opened(ALICE_BOT), ALICE_BOT).await;
    client.raw(&client_frame(8, &1000u16.to_be_bytes())).await;
    client.closed(1000, "client_left").await;
    live.gone(MAC).await;
    assert_eq!(last(&live), line("alice", alice_bot.clone()));

    let _mac = live.fixture.dials.connect();
    let mut client = live.attach(&opened(ALICE_BOT), ALICE_BOT).await;
    close(&live, &bearer(&live, ALICE_OPS), MAC);
    client.closed(4007, "closed_by_operator").await;
    live.gone(MAC).await;
    assert_eq!(last(&live), line("alice", Some("alice-ops".to_owned())));

    let _mac = live.fixture.dials.connect();
    let mut client = live.attach(&opened(ALICE_BOT), ALICE_BOT).await;
    close(&live, &caller("bob"), MAC);
    client.closed(4007, "closed_by_operator").await;
    live.gone(MAC).await;
    assert_eq!(last(&live), line("bob", None));

    opened(ALICE_BOT);
    close(&live, &bearer(&live, ALICE_OPS), MAC);
    assert_eq!(last(&live), line("alice", Some("alice-ops".to_owned())));

    let _mac = live.fixture.dials.connect();
    let mut client = live.attach(&opened(ALICE_BOT), ALICE_BOT).await;
    coordinator
        .accounts()
        .change(|accounts| accounts.without_token("alice-bot"))
        .expect("the token is revoked");
    coordinator.console_sweep();
    client.closed(4008, "token_revoked").await;
    live.gone(MAC).await;
    assert_eq!(last(&live), line("coordinator", None));
    assert_eq!(
        said(&live)
            .iter()
            .filter(|(_, action, _)| action == "console_open")
            .count(),
        5
    );
}
