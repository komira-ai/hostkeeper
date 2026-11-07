use super::inputs::{Events, Serving};
use super::render::{Span, duration, plus, report, span};
use super::{Mac, Mode};

fn mac() -> Mac {
    "52:54:00:4e:10:01".parse().expect("a MAC")
}

/// An install that reached every event.
fn served() -> Events {
    Events {
        power_on: Some(1_000),
        install_started: Some(1_040),
        pending: Some(1_900),
        approved: Some(1_960),
        first_heartbeat: Some(1_975),
        serving: Some(Serving {
            unix: 2_010,
            service: "hello".to_owned(),
        }),
    }
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

/// Catches an event or an interval taken from the wrong pair, the person's wait counted in the
/// machine's time, and a line dropped or reordered: the whole report, line for line.
#[test]
fn the_report_of_a_served_machine_is_its_events_its_intervals_and_the_two_times() {
    assert_eq!(
        report(&mac(), "net-1", Mode::Proxy, &served()),
        lines(
            "\
Network install of 52:54:00:4e:10:01 as host net-1, proxy mode.

| Event | Unix seconds | Recorded by |
|---|---|---|
| power-on | 1000 | hostkeeper-pxe: the first DHCPDISCOVER from the MAC |
| install started | 1040 | hostkeeper-pxe: the seed fetch |
| pending | 1900 | the coordinator: the request's `first_seen_unix` |
| approved | 1960 | the coordinator: the host's `enrolled_unix` |
| first heartbeat | 1975 | the coordinator: the host's `first_heartbeat_unix` |
| serving | 2010 | the host's daemon: the install of hello confirmed healthy (`finished_unix`) |

| Interval | Time |
|---|---|
| power-on to install started | 40 s |
| install started to pending | 860 s (14 min 20 s) |
| pending to approved (the person's wait) | 60 s (1 min 0 s) |
| approved to first heartbeat | 15 s |
| first heartbeat to serving | 35 s |

The machine's time: 950 s (15 min 50 s): power-on to pending, and approved to serving.
The person's wait: 60 s (1 min 0 s): pending to approved.
Power-on to serving: 1010 s (16 min 50 s).
Serving is stamped by the host's clock; the other events by the clocks of the hosts of hostkeeper-pxe and the coordinator."
        )
    );
}

/// Catches an event that never happened printed as zero, or an interval or a total computed
/// from one: every event missing is `not reached`, and so is everything made from it.
#[test]
fn an_event_that_never_happened_is_not_reached_and_never_zero() {
    assert_eq!(
        report(&mac(), "net-1", Mode::Router, &Events::default()),
        lines(
            "\
Network install of 52:54:00:4e:10:01 as host net-1, router mode.

| Event | Unix seconds | Recorded by |
|---|---|---|
| power-on | not reached | hostkeeper-pxe: the first DHCPREQUEST from the MAC |
| install started | not reached | hostkeeper-pxe: the seed fetch |
| pending | not reached | the coordinator: the request's `first_seen_unix` |
| approved | not reached | the coordinator: the host's `enrolled_unix` |
| first heartbeat | not reached | the coordinator: the host's `first_heartbeat_unix` |
| serving | not reached | the host's daemon: the first `--service` installed and confirmed healthy (`finished_unix`) |

| Interval | Time |
|---|---|
| power-on to install started | not reached |
| install started to pending | not reached |
| pending to approved (the person's wait) | not reached |
| approved to first heartbeat | not reached |
| first heartbeat to serving | not reached |

The machine's time: not reached: power-on to pending, and approved to serving.
The person's wait: not reached: pending to approved.
Power-on to serving: not reached.
Serving is stamped by the host's clock; the other events by the clocks of the hosts of hostkeeper-pxe and the coordinator."
        )
    );
}

/// Catches a host whose clock is behind printed as a huge or a negative interval: serving
/// stamped before the first heartbeat is out of order, and so is the machine's time; the
/// intervals that do not use it are unchanged.
#[test]
fn an_event_stamped_before_the_one_it_follows_is_out_of_order() {
    let mut events = served();
    events.serving = Some(Serving {
        unix: 1_970,
        service: "hello".to_owned(),
    });
    let late = report(&mac(), "net-1", Mode::Proxy, &events);
    assert_eq!(
        late[16..22],
        lines(
            "\
| approved to first heartbeat | 15 s |
| first heartbeat to serving | out of order: it ends 5 s before it starts |

The machine's time: 910 s (15 min 10 s): power-on to pending, and approved to serving.
The person's wait: 60 s (1 min 0 s): pending to approved.
Power-on to serving: 970 s (16 min 10 s)."
        ),
        "{late:#?}"
    );
    events.serving = Some(Serving {
        unix: 1_950,
        service: "hello".to_owned(),
    });
    let before = report(&mac(), "net-1", Mode::Proxy, &events);
    assert_eq!(
        before[19],
        "The machine's time: out of order: an interval of it ends before it starts: power-on to \
         pending, and approved to serving.",
        "{before:#?}"
    );
}

/// Catches the minutes or hours of a duration miscounted at their boundaries.
#[test]
fn a_duration_past_a_minute_or_an_hour_says_its_minutes_and_hours() {
    assert_eq!(duration(0), "0 s");
    assert_eq!(duration(59), "59 s");
    assert_eq!(duration(60), "60 s (1 min 0 s)");
    assert_eq!(duration(3_599), "3599 s (59 min 59 s)");
    assert_eq!(duration(3_600), "3600 s (1 h 0 min 0 s)");
    assert_eq!(duration(3_725), "3725 s (1 h 2 min 5 s)");
    assert_eq!(duration(90_061), "90061 s (25 h 1 min 1 s)");
}

/// Catches a span with a missing end computed, an equal pair out of order, and a sum that is
/// not reached or out of order read as a time.
#[test]
fn a_span_needs_both_events_and_a_sum_both_spans() {
    assert_eq!(span(Some(5), Some(5)), Span::Secs(0));
    assert_eq!(span(Some(5), Some(9)), Span::Secs(4));
    assert_eq!(span(Some(9), Some(5)), Span::OutOfOrder(4));
    assert_eq!(span(None, Some(5)), Span::NotReached);
    assert_eq!(span(Some(5), None), Span::NotReached);
    assert_eq!(plus(Span::Secs(2), Span::Secs(3)), Span::Secs(5));
    assert_eq!(
        plus(Span::Secs(u64::MAX), Span::Secs(1)),
        Span::Secs(u64::MAX)
    );
    assert_eq!(plus(Span::NotReached, Span::Secs(3)), Span::NotReached);
    assert_eq!(plus(Span::Secs(3), Span::NotReached), Span::NotReached);
    assert_eq!(
        plus(Span::OutOfOrder(1), Span::NotReached),
        Span::NotReached
    );
    assert_eq!(
        plus(Span::OutOfOrder(1), Span::Secs(3)),
        Span::OutOfOrder(1)
    );
    assert_eq!(
        plus(Span::Secs(3), Span::OutOfOrder(2)),
        Span::OutOfOrder(2)
    );
}
