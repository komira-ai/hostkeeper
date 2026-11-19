//! The console's sweep on the coordinator's clock, its drain at stop and its start line: each
//! closes the streams it must with their code, and the clean-up and close line follow.

use std::time::Duration;

use hostkeeper_console::session::{Bounds, Sessions};
use tokio::io::AsyncWriteExt;

use super::attach_support::{ALICE, BOB, CAROL, Live};
use super::support::{MAC, NODE, NODE_BMC_ANSWER};

/// `sol info` with both rates at 115.2.
const INFO: &str =
    "Volatile Bit Rate (kbps)        : 115.2\nNon-Volatile Bit Rate (kbps)    : 115.2\n";

/// Gives the live fixture's console the idle and total limits `idle` and `total`.
fn limited(name: &str, idle: u64, total: u64) -> Live {
    let live = Live::new(name);
    live.coordinator.console.lock_table().sessions = Sessions::new(Bounds {
        max: 8,
        window: Duration::from_secs(30),
        idle: Duration::from_secs(idle),
        total: Duration::from_secs(total),
    });
    live
}

/// Proves: on the sweep, a stream that carried no byte for the idle limit closes 4005 `idle`, by
/// `coordinator`, not a second before; bytes that cross push it out; one attached for the total
/// limit closes 4006 `total_limit` though it carried bytes all along.
///
/// Catches: a console left open for ever by a forgotten tab, and one cut while in use.
#[tokio::test]
async fn idle_and_total_timers_close_a_session() {
    let live = limited("sweep-idle", 100, 1_000);
    let mut mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let mut client = live.attach(&session, ALICE).await;
    live.fixture.rig.at(2_060);
    client.binary(b"x").await;
    let mut byte = [0u8; 1];
    tokio::io::AsyncReadExt::read_exact(&mut mac, &mut byte)
        .await
        .expect("the byte");
    live.coordinator.console_sweep();
    live.fixture.rig.at(2_159);
    live.coordinator.console_sweep();
    mac.write_all(b"still here").await.expect("written");
    assert_eq!(client.output(10).await, b"still here");
    live.fixture.rig.at(2_160);
    live.coordinator.console_sweep();
    live.fixture.rig.at(2_259);
    live.coordinator.console_sweep();
    live.fixture.rig.at(2_260);
    live.coordinator.console_sweep();
    client.closed(4005, "idle").await;
    live.gone(MAC).await;
    let (by, ..) = live.lines().pop().expect("a line");
    assert_eq!(by, "coordinator");
    assert!(live.last_reason().contains("cause=idle;"));

    let live = limited("sweep-total", 100, 1_000);
    let mut mac = live.fixture.dials.connect();
    let session = live.open("alice", MAC);
    let mut client = live.attach(&session, ALICE).await;
    for at in (2_050..3_000).step_by(50) {
        live.fixture.rig.at(at);
        mac.write_all(b".").await.expect("written");
        assert_eq!(client.output(1).await, b".");
        live.coordinator.console_sweep();
    }
    live.fixture.rig.at(3_000);
    live.coordinator.console_sweep();
    client.closed(4006, "total_limit").await;
    live.gone(MAC).await;
}

/// Proves: on the sweep, a stream whose operator's token left the tokens file closes 4008
/// `token_revoked`, its payload deactivated; one whose operator the console operators file no
/// longer names closes 4008 too; a stream whose operator keeps both is not touched.
///
/// Catches: a console that outlives the revocation of the token that opened it.
#[tokio::test]
async fn a_revoked_operator_loses_its_stream() {
    let live = Live::new("sweep-revoked");
    let _host = live
        .fixture
        .tool
        .sol_session(&[NODE_BMC_ANSWER, " 01 00 00\n", INFO]);
    live.fixture.tool.session(&[NODE_BMC_ANSWER, ""]);
    let mut mac = live.fixture.dials.connect();
    let serial = live.open("alice", NODE);
    let screen = live.open("bob", MAC);
    let mut alices = live.attach(&serial, ALICE).await;
    let mut bobs = live.attach(&screen, BOB).await;
    live.keep_tokens(&[("bob", BOB), ("carol", CAROL)]);
    live.coordinator.console_sweep();
    alices.closed(4008, "token_revoked").await;
    live.gone(NODE).await;
    assert!(
        live.fixture
            .tool
            .seen()
            .contains(&hostkeeper_console::scripted::Seen::Ask(
                "sol deactivate".to_owned()
            ))
    );
    mac.write_all(b"ok").await.expect("written");
    assert_eq!(bobs.output(2).await, b"ok");
    std::fs::write(
        &live.fixture.files.operators,
        "[[operator]]\nname = \"alice\"\n",
    )
    .expect("written");
    live.coordinator.console().files().reload();
    live.coordinator.console_sweep();
    bobs.closed(4008, "token_revoked").await;
    live.gone(MAC).await;
}

/// Proves: the sweep ends a session not attached in its window and writes its close line, with no
/// `Console` call needed to notice it.
///
/// Catches: a host held `console_busy` until someone happens to call.
#[tokio::test]
async fn the_sweep_ends_an_unattached_session_at_its_window() {
    let live = Live::new("sweep-unattached");
    live.open("alice", NODE);
    live.fixture.rig.at(2_030);
    live.coordinator.console_sweep();
    assert!(
        live.coordinator
            .console
            .lock_table()
            .sessions
            .get(NODE)
            .is_none()
    );
    assert!(live.last_reason().contains("cause=not_attached;"));
}

/// Proves: the drain closes every stream 4010 `coordinator_stopping`, deactivates each serial
/// console's payload, and returns once every close line is written; with no stream it returns at
/// once.
///
/// Catches: a stop that leaves payloads active at the BMCs (each host `bmc_sol_busy` until
/// someone deactivates it by hand) or close lines unwritten.
#[tokio::test]
async fn the_drain_closes_every_stream_and_cleans_up() {
    let live = Live::new("sweep-drain");
    let _host = live
        .fixture
        .tool
        .sol_session(&[NODE_BMC_ANSWER, " 01 00 00\n", INFO]);
    live.fixture.tool.session(&[NODE_BMC_ANSWER, ""]);
    let _mac = live.fixture.dials.connect();
    let serial = live.open("alice", NODE);
    let screen = live.open("bob", MAC);
    let mut alices = live.attach(&serial, ALICE).await;
    let mut bobs = live.attach(&screen, BOB).await;
    let draining = live.coordinator.console_drain(Duration::from_secs(3_600));
    let (_, first, second) = tokio::join!(
        draining,
        alices.closed(4010, "coordinator_stopping"),
        bobs.closed(4010, "coordinator_stopping"),
    );
    assert_eq!((first, second), (Vec::new(), Vec::new()));
    assert!(live.coordinator.console.lock_table().live.is_empty());
    let causes: Vec<String> = live
        .lines()
        .into_iter()
        .filter(|(_, action, ..)| action == "console_close")
        .map(|(.., reason)| reason)
        .collect();
    assert_eq!(causes.len(), 2);
    assert!(
        causes
            .iter()
            .all(|reason| reason.contains("cause=coordinator_stopping;"))
    );
    assert_eq!(live.fixture.tool.unused(), 0);
    live.coordinator
        .console_drain(Duration::from_secs(3_600))
        .await;
}

/// Proves: a drain whose streams do not clean up within its grace gives up and returns, the
/// streams having been told to stop.
///
/// Catches: a stop held for ever by one stream whose clean-up hangs.
#[tokio::test(start_paused = true)]
async fn a_drain_that_runs_out_of_grace_returns() {
    let live = Live::new("sweep-drain-grace");
    let attached = live.attach_unserved(MAC);
    live.coordinator.console_drain(Duration::from_secs(1)).await;
    assert!(attached.stopped.borrow().is_some());
    assert!(live.coordinator.console.lock_table().live.contains_key(MAC));
}

/// Proves: a coordinator with console files writes one `console_coordinator_start` line by
/// `coordinator` at its start; one without writes none.
///
/// Catches: sessions open before a restart left with no line saying they ended there.
#[tokio::test]
async fn the_start_line_is_written_with_console_files_only() {
    let live = Live::new("sweep-start");
    live.coordinator.console_started();
    assert_eq!(
        live.lines(),
        [(
            "coordinator".to_owned(),
            "console_coordinator_start".to_owned(),
            String::new(),
            "every console session open before this start ended".to_owned(),
        )]
    );
    let bare = crate::testutil::Rig::new("sweep-start-bare");
    bare.coordinator.console_started();
    let all = bare.coordinator.ops().audit_log().expect("the log");
    assert!(
        all.iter()
            .all(|entry| entry.action != "console_coordinator_start")
    );
}

/// Proves: the console's part of a stop returns once its streams cleaned up while the listeners
/// go on, and answers the listeners' failure when they fail first.
///
/// Catches: a stop that never reaches the tunnels' drain, or swallows a listener's failure.
#[tokio::test]
async fn the_console_part_of_a_stop_returns_or_answers_the_listeners_failure() {
    let live = Live::new("sweep-drained");
    let serving = std::future::pending::<Result<std::convert::Infallible, &str>>();
    let drained = super::sweep::drained(&live.coordinator, Box::pin(serving)).await;
    assert_eq!(drained, Ok(()));
    let _ = live.attach_unserved(MAC);
    let failed = std::future::ready(Err::<std::convert::Infallible, _>("bound twice"));
    let drained = super::sweep::drained(&live.coordinator, Box::pin(failed)).await;
    assert_eq!(drained, Err("bound twice"));
}

/// Proves: the sweep's loop runs the sweep at its start and then every five seconds of the
/// runtime's clock, and not between: a session whose window passes after the first tick is ended
/// at the next, five seconds on, with no call.
///
/// Catches: a sweep that is written and never run, or run so seldom that a forgotten session
/// holds its host long past its window.
#[tokio::test(start_paused = true)]
async fn the_sweep_runs_on_its_tick() {
    let live = Live::new("sweep-loop");
    live.open("alice", NODE);
    let looping = tokio::spawn(super::sweep_every(std::sync::Arc::clone(&live.coordinator)));
    let present = || {
        live.coordinator
            .console
            .lock_table()
            .sessions
            .get(NODE)
            .is_some()
    };
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert!(present());
    live.fixture.rig.at(2_030);
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(present());
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert!(!present());
    assert!(live.last_reason().contains("cause=not_attached;"));
    looping.abort();
}
