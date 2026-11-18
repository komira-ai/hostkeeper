//! The bridge, stepped by hand: each step polls it once, which makes every move the channels
//! allow and then waits, so what it took and what it left is read exactly after each step, with
//! no runtime, clock or sleep.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use tokio::sync::mpsc::error::{TryRecvError, TrySendError};
use tokio::sync::mpsc::{self, Receiver, Sender};

use super::{Ended, Moved, Pipe, bridge};
use crate::session::Kind;

/// The test's ends of a bridge: what it types and reads as the browser, what it says and hears
/// as the device.
struct Ends {
    typed: Sender<Vec<u8>>,
    shown: Receiver<Vec<u8>>,
    said: Sender<Vec<u8>>,
    told: Receiver<Vec<u8>>,
}

/// A bridge of `kind` over channels of one message each, and the test's ends of it.
fn pair(kind: Kind, moved: &Moved) -> (Pin<Box<dyn Future<Output = Ended> + '_>>, Ends) {
    let (typed, from_browser) = mpsc::channel(1);
    let (to_browser, shown) = mpsc::channel(1);
    let (said, from_device) = mpsc::channel(1);
    let (to_device, told) = mpsc::channel(1);
    let browser = Pipe {
        from: from_browser,
        to: to_browser,
    };
    let device = Pipe {
        from: from_device,
        to: to_device,
    };
    (
        Box::pin(bridge(kind, browser, device, moved)),
        Ends {
            typed,
            shown,
            said,
            told,
        },
    )
}

/// One step of the bridge: every move it can make, then whether it ended.
fn step(bridge: &mut Pin<Box<dyn Future<Output = Ended> + '_>>) -> Poll<Ended> {
    bridge
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
}

/// Proves: the bytes of a screen cross both ways exactly as sent, every byte value among them
/// (0xff and the banner's text included), each message as one message, in order; the counts of
/// bytes each way are kept.
///
/// Catches: a bridge that changes, merges, reorders or drops a screen's bytes, which would break
/// RFB and its authentication inside the stream.
#[test]
fn bytes_cross_the_bridge_both_ways_unchanged() {
    let moved = Moved::default();
    let (mut bridge, mut ends) = pair(Kind::Screen, &moved);
    let every: Vec<u8> = (0..=255).collect();
    let banner = b"[SOL Session operational.  Use ~? for help]\n".to_vec();
    for (up, down) in [
        (every.clone(), banner.clone()),
        (banner.clone(), every.clone()),
    ] {
        ends.typed.try_send(up.clone()).expect("typed");
        ends.said.try_send(down.clone()).expect("said");
        assert_eq!(step(&mut bridge), Poll::Pending);
        assert_eq!(ends.told.try_recv(), Ok(up));
        assert_eq!(ends.shown.try_recv(), Ok(down));
    }
    let total = u64::try_from(every.len() + banner.len()).expect("a count");
    assert_eq!((moved.to_device(), moved.to_browser()), (total, total));
    assert_eq!(moved.total(), 2 * total);
}

/// Proves: a serial console drops the escape byte 0xff from what the browser sends (a message of
/// nothing else is not passed on at all) and passes every other byte; and drops the tool's banner
/// line from the start of the device's output, even when it comes in pieces and with the host's
/// first bytes after it, passing what follows unchanged, a later banner text included.
///
/// Catches: a client that ends the tool's session or reaches its escape commands by sending
/// 0xff, and the banner, which holds 0xff, shown in the terminal.
#[test]
fn a_serial_console_drops_the_escape_byte_and_the_tools_banner() {
    let moved = Moved::default();
    let (mut bridge, mut ends) = pair(Kind::Serial, &moved);
    ends.typed.try_send(b"ls\xff.\r".to_vec()).expect("typed");
    assert_eq!(step(&mut bridge), Poll::Pending);
    assert_eq!(ends.told.try_recv(), Ok(b"ls.\r".to_vec()));
    ends.typed.try_send(vec![0xff, 0xff]).expect("typed");
    assert_eq!(step(&mut bridge), Poll::Pending);
    assert_eq!(ends.told.try_recv(), Err(TryRecvError::Empty));
    for piece in [&b"[SOL Session"[..], b" operational.  Use \xff? for help]"] {
        ends.said.try_send(piece.to_vec()).expect("said");
        assert_eq!(step(&mut bridge), Poll::Pending);
        assert_eq!(ends.shown.try_recv(), Err(TryRecvError::Empty));
    }
    ends.said.try_send(b"\r\nlogin: ".to_vec()).expect("said");
    assert_eq!(step(&mut bridge), Poll::Pending);
    assert_eq!(ends.shown.try_recv(), Ok(b"login: ".to_vec()));
    let again = b"[SOL Session operational.\n".to_vec();
    ends.said.try_send(again.clone()).expect("said");
    assert_eq!(step(&mut bridge), Poll::Pending);
    assert_eq!(ends.shown.try_recv(), Ok(again));
    assert_eq!(moved.to_device(), 4);
}

/// Proves: a serial console's output that does not start with the banner is passed whole at once
/// (one that differs at its first byte, and one that starts like the banner and then differs); a
/// first line longer than a banner can be is passed once it is, newline or not; a first line that
/// is the banner's start and no more is held, and dropped with the device's end.
///
/// Catches: a host's first bytes held back for ever, or eaten, because they are not the banner.
#[test]
fn output_that_is_not_the_banner_is_passed_whole() {
    let moved = Moved::default();
    let cases: [&[u8]; 3] = [b"login: ", b"[SOL Sessions are over\n", b"[SOL Session"];
    for case in cases {
        let (mut bridge, mut ends) = pair(Kind::Serial, &moved);
        ends.said.try_send(case.to_vec()).expect("said");
        assert_eq!(step(&mut bridge), Poll::Pending);
        let shown = ends.shown.try_recv();
        if case == b"[SOL Session" {
            assert_eq!(shown, Err(TryRecvError::Empty));
            drop(ends.said);
            assert_eq!(step(&mut bridge), Poll::Ready(Ended::Device));
            assert_eq!(ends.shown.try_recv(), Err(TryRecvError::Disconnected));
        } else {
            assert_eq!(shown, Ok(case.to_vec()), "{case:?}");
        }
    }
    let (mut bridge, mut ends) = pair(Kind::Serial, &moved);
    let mut long = b"[SOL Session operational.".to_vec();
    long.resize(129, b'x');
    ends.said.try_send(long[..100].to_vec()).expect("said");
    assert_eq!(step(&mut bridge), Poll::Pending);
    assert_eq!(ends.shown.try_recv(), Err(TryRecvError::Empty));
    ends.said.try_send(long[100..].to_vec()).expect("said");
    assert_eq!(step(&mut bridge), Poll::Pending);
    assert_eq!(ends.shown.try_recv(), Ok(long));
}

/// Proves: with the device taking nothing, the bridge takes one message of the browser and passes
/// it, and no more: the next waits in the browser's channel and the one after does not fit; once
/// the device takes one, the bridge takes the next. The same the other way, with a browser that
/// reads nothing.
///
/// Catches: a bridge that reads a side while the other's write is blocked, which a slow browser
/// or a stuck device would turn into memory without bound.
#[test]
fn reads_pause_while_the_write_side_blocks() {
    let moved = Moved::default();
    let (mut bridge, mut ends) = pair(Kind::Screen, &moved);
    for (to, from) in [(&ends.typed, Direction::Up), (&ends.said, Direction::Down)] {
        to.try_send(b"one".to_vec()).expect("one");
        assert_eq!(step(&mut bridge), Poll::Pending);
        to.try_send(b"two".to_vec()).expect("two");
        assert_eq!(step(&mut bridge), Poll::Pending);
        assert!(
            matches!(to.try_send(b"three".to_vec()), Err(TrySendError::Full(_))),
            "{from:?}"
        );
        let heard = match from {
            Direction::Up => &mut ends.told,
            Direction::Down => &mut ends.shown,
        };
        assert_eq!(heard.try_recv(), Ok(b"one".to_vec()));
        assert_eq!(step(&mut bridge), Poll::Pending);
        to.try_send(b"three".to_vec()).expect("three fits now");
        assert_eq!(heard.try_recv(), Ok(b"two".to_vec()));
        assert_eq!(step(&mut bridge), Poll::Pending);
        assert_eq!(heard.try_recv(), Ok(b"three".to_vec()));
    }
}

#[derive(Debug)]
enum Direction {
    Up,
    Down,
}

/// Proves: the bridge ends, naming the side, when the browser's messages end, when the browser
/// takes no more, when the device's bytes end, and when the device takes no more; even when the
/// side that ended is not the one the bridge was waiting on.
///
/// Catches: a stream that outlives its browser tab or its device, holding the host's one session.
#[test]
fn either_side_ending_ends_the_bridge() {
    let moved = Moved::default();
    let (mut bridge, ends) = pair(Kind::Serial, &moved);
    assert_eq!(step(&mut bridge), Poll::Pending);
    drop(ends.typed);
    assert_eq!(step(&mut bridge), Poll::Ready(Ended::Browser));

    let (mut bridge, ends) = pair(Kind::Serial, &moved);
    drop(ends.shown);
    assert_eq!(step(&mut bridge), Poll::Ready(Ended::Browser));

    let (mut bridge, ends) = pair(Kind::Screen, &moved);
    assert_eq!(step(&mut bridge), Poll::Pending);
    drop(ends.said);
    assert_eq!(step(&mut bridge), Poll::Ready(Ended::Device));

    let (mut bridge, ends) = pair(Kind::Screen, &moved);
    drop(ends.told);
    assert_eq!(step(&mut bridge), Poll::Ready(Ended::Device));
}
