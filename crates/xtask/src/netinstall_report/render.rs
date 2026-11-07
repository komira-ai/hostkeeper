//! The report of one machine's install, from its [`Events`]: a table of the events, a table of
//! the intervals between them, and the machine's time apart from the person's wait. Pure: no
//! clock is read, and an event that is not there is `not reached`, never a number.

use super::inputs::{Events, Serving};
use super::{Mac, Mode};

/// The time between two events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Span {
    /// Either event is not there.
    NotReached,
    /// The later event, this many seconds after the earlier.
    Secs(u64),
    /// The later event is stamped this many seconds before the earlier: the two come from
    /// clocks that disagree.
    OutOfOrder(u64),
}

/// The time from `from` to `to`.
pub(crate) fn span(from: Option<u64>, to: Option<u64>) -> Span {
    match (from, to) {
        (Some(from), Some(to)) if to >= from => Span::Secs(to - from),
        (Some(from), Some(to)) => Span::OutOfOrder(from - to),
        _ => Span::NotReached,
    }
}

/// The sum of two spans: not reached when either is, out of order when either is.
pub(crate) fn plus(a: Span, b: Span) -> Span {
    match (a, b) {
        (Span::Secs(a), Span::Secs(b)) => Span::Secs(a.saturating_add(b)),
        (Span::NotReached, _) | (_, Span::NotReached) => Span::NotReached,
        (Span::OutOfOrder(by), _) | (_, Span::OutOfOrder(by)) => Span::OutOfOrder(by),
    }
}

/// Seconds as text: `40 s`, and past a minute with its minutes and hours, `950 s (15 min 50 s)`.
pub(crate) fn duration(secs: u64) -> String {
    let (h, min, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    match (h, min) {
        (0, 0) => format!("{secs} s"),
        (0, _) => format!("{secs} s ({min} min {s} s)"),
        _ => format!("{secs} s ({h} h {min} min {s} s)"),
    }
}

/// One interval of the table.
fn interval(span: Span) -> String {
    match span {
        Span::NotReached => "not reached".to_owned(),
        Span::Secs(secs) => duration(secs),
        Span::OutOfOrder(by) => {
            format!("out of order: it ends {} before it starts", duration(by))
        }
    }
}

/// A sum of intervals.
fn total(span: Span) -> String {
    match span {
        Span::OutOfOrder(_) => "out of order: an interval of it ends before it starts".to_owned(),
        span => interval(span),
    }
}

/// An event's unix seconds, or `not reached`.
fn at(unix: Option<u64>) -> String {
    unix.map_or_else(|| "not reached".to_owned(), |unix| unix.to_string())
}

/// The report of `events`, the install of `mac` as the record's host `host_id` by `serve` in
/// `mode`, as lines.
pub(crate) fn report(mac: &Mac, host_id: &str, mode: Mode, events: &Events) -> Vec<String> {
    let serving = events.serving.as_ref().map(|s| s.unix);
    let served_by = match &events.serving {
        Some(Serving { service, .. }) => format!("the install of {service} confirmed healthy"),
        None => "the first `--service` installed and confirmed healthy".to_owned(),
    };
    let rows = [
        (
            "power-on",
            events.power_on,
            format!("hostkeeper-pxe: the first {} from the MAC", mode.power_on()),
        ),
        (
            "install started",
            events.install_started,
            "hostkeeper-pxe: the seed fetch".to_owned(),
        ),
        (
            "pending",
            events.pending,
            "the coordinator: the request's `first_seen_unix`".to_owned(),
        ),
        (
            "approved",
            events.approved,
            "the coordinator: the host's `enrolled_unix`".to_owned(),
        ),
        (
            "first heartbeat",
            events.first_heartbeat,
            "the coordinator: the host's `first_heartbeat_unix`".to_owned(),
        ),
        (
            "serving",
            serving,
            format!("the host's daemon: {served_by} (`finished_unix`)"),
        ),
    ];
    let mut lines = vec![
        format!(
            "Network install of {mac} as host {host_id}, {} mode.",
            mode.name()
        ),
        String::new(),
        "| Event | Unix seconds | Recorded by |".to_owned(),
        "|---|---|---|".to_owned(),
    ];
    for (event, unix, by) in &rows {
        lines.push(format!("| {event} | {} | {by} |", at(*unix)));
    }
    let intervals = [
        (
            "power-on to install started",
            span(events.power_on, events.install_started),
        ),
        (
            "install started to pending",
            span(events.install_started, events.pending),
        ),
        (
            "pending to approved (the person's wait)",
            span(events.pending, events.approved),
        ),
        (
            "approved to first heartbeat",
            span(events.approved, events.first_heartbeat),
        ),
        (
            "first heartbeat to serving",
            span(events.first_heartbeat, serving),
        ),
    ];
    lines.extend([
        String::new(),
        "| Interval | Time |".to_owned(),
        "|---|---|".to_owned(),
    ]);
    for (name, span) in intervals {
        lines.push(format!("| {name} | {} |", interval(span)));
    }
    let machine = plus(
        span(events.power_on, events.pending),
        span(events.approved, serving),
    );
    lines.extend([
        String::new(),
        format!(
            "The machine's time: {}: power-on to pending, and approved to serving.",
            total(machine)
        ),
        format!(
            "The person's wait: {}: pending to approved.",
            interval(span(events.pending, events.approved))
        ),
        format!(
            "Power-on to serving: {}.",
            interval(span(events.power_on, serving))
        ),
        "Serving is stamped by the host's clock; the other events by the clocks of the hosts of \
         hostkeeper-pxe and the coordinator."
            .to_owned(),
    ]);
    lines
}
