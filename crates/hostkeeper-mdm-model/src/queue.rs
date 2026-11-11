//! The coordinator's per-Mac command queue (docs/design/coordinator-mdm.md sections 4.3, 5.2 and
//! 5.4), for one serial with two commands, two enrollments of that serial and two connections.
//!
//! The server side is a table of commands with a status, claimed with one statement, and a push
//! flag. The Mac side is what it can do at any time: connect, send `Idle`, answer a command it
//! holds with `Acknowledged`, `Error` or `NotNow`, drop a connection, or be wiped and enroll
//! again. Time is not modelled: the command timeout, the push floor and the push expiry are
//! actions that may fire at any point, so a result holds for every schedule.
//!
//! Every rule the checks depend on is a field of [`Rules`]. [`Rules::design`] is what CM11a
//! implements; each other setting is one rule given up, and the tests show the invariant that
//! the search then breaks, so the checks are known to fail on a defect.

use crate::explore::{Model, Violation};

/// A command is delivered to at most one connection at a time.
pub const SINGLE_DELIVERY: &str = "a_command_is_delivered_to_at_most_one_connection_at_a_time";
/// An acknowledged command is never re-sent.
pub const ACKED_NEVER_RESENT: &str = "an_acknowledged_command_is_never_re_sent";
/// A result is matched only to a command in `Sent`: a command that reached an end state keeps it.
pub const RESULT_ONLY_TO_SENT: &str = "a_result_is_matched_only_to_a_command_in_sent";
/// A `NotNow` command is kept in the queue.
pub const NOT_NOW_KEPT: &str = "a_not_now_command_is_never_lost";
/// A `NotNow` command is not handed back on the connection that deferred it.
pub const NOT_NOW_NO_LOOP: &str = "a_not_now_command_is_not_resent_on_the_same_connection";
/// No command is delivered to a connection of a superseded enrollment.
pub const NO_SUPERSEDED_DELIVERY: &str = "no_command_is_delivered_to_a_superseded_identity";
/// No queued or sent command belongs to a superseded enrollment.
pub const NO_STRANDED: &str = "no_command_is_stranded_on_a_superseded_enrollment";
/// A queued command has a way to reach the Mac: a push, a floor timer or a usable connection.
pub const ALWAYS_A_WAKE: &str = "a_queued_command_always_has_a_wake";

/// The rules the queue's implementation follows. `true` is the rule kept.
#[derive(Clone, Copy, Debug)]
pub struct Rules {
    /// A claim takes only a `Queued` command.
    pub claim_only_queued: bool,
    /// A claim skips a command the connection already tried (a `NotNow` loop otherwise).
    pub skip_tried_on_connection: bool,
    /// A result changes a command only while it is `Sent`; any other result is dropped.
    pub match_result_to_sent_only: bool,
    /// Enqueuing a command id that exists changes nothing.
    pub enqueue_is_idempotent: bool,
    /// Enqueuing for an enrollment that is no longer the live one is refused.
    pub reject_superseded_target: bool,
    /// A request on the identity of a replaced enrollment is refused.
    pub refuse_superseded_identity: bool,
    /// `NotNow` returns the command to the queue.
    pub not_now_keeps_command: bool,
    /// `NotNow` arms the push floor, so the command gets a later push.
    pub not_now_arms_repush: bool,
    /// A command that timed out is pushed again.
    pub timeout_requeue_pushes: bool,
    /// The timeout may fire while the Mac still holds the command it was sent. The design
    /// assumes not (the Mac answers within `--mdm-command-timeout-s`, or never); `true` is the
    /// world in which a slow Mac answers after the re-send.
    pub timeout_while_held: bool,
    /// Judge [`SINGLE_DELIVERY`]. Off, the search shows what else holds when a command can be
    /// delivered twice.
    pub require_single_delivery: bool,
}

impl Rules {
    /// The design of sections 4.3 and 5.2 with the corrections this model asked for (the
    /// `NotNow` re-push and the timeout re-push), under the assumption that a Mac answers within
    /// the timeout.
    pub fn design() -> Rules {
        Rules {
            claim_only_queued: true,
            skip_tried_on_connection: true,
            match_result_to_sent_only: true,
            enqueue_is_idempotent: true,
            reject_superseded_target: true,
            refuse_superseded_identity: true,
            not_now_keeps_command: true,
            not_now_arms_repush: true,
            timeout_requeue_pushes: true,
            timeout_while_held: false,
            require_single_delivery: true,
        }
    }
}

/// Where a command is. `Absent` is a command id nobody enqueued yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    Absent,
    Queued,
    Sent,
    Acked,
    Errored,
    Cancelled,
}

/// What a Mac answers a command with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Answer {
    Acknowledged,
    Error,
    NotNow,
}

/// One command's row, with the truth about it that the server cannot see (`held`, `done`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cmd {
    pub status: Status,
    /// The enrollment of the serial the command was queued for.
    pub epoch: u8,
    /// How many times the gate enqueued this id.
    pub enqueues: u8,
    /// Deliveries of the command that the Mac holds and has not answered.
    pub held: u8,
    /// The last accepted result was `NotNow` and the command has not been delivered since.
    pub deferred: bool,
    /// The first end state the command reached.
    pub done: Option<Status>,
}

/// An open connection of the Mac: the enrollment whose identity it presents, and the commands
/// delivered or deferred on it (a bit each).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Conn {
    pub epoch: u8,
    pub tried: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    /// The live enrollment of the serial.
    pub live: u8,
    pub cmds: [Cmd; 2],
    pub conns: [Option<Conn>; 2],
    /// A push is outstanding: sent, and no request has reached the server since.
    pub push: bool,
    /// The floor timer is armed: a later push repeats for a Mac that has not connected.
    pub armed: bool,
}

#[derive(Clone, Debug)]
pub enum Action {
    /// The gate enqueues command `cmd` for the enrollment `target` it last read.
    Enqueue {
        cmd: usize,
        target: u8,
    },
    Connect {
        slot: usize,
    },
    Close {
        slot: usize,
    },
    /// `Idle`, and the server's answer; `lost` is an answer that never reaches the Mac.
    Idle {
        slot: usize,
        lost: bool,
    },
    Answer {
        slot: usize,
        cmd: usize,
        answer: Answer,
    },
    Timeout {
        cmd: usize,
    },
    FloorElapsed,
    /// The same serial authenticates again, wiped or not.
    Reenroll {
        wiped: bool,
    },
}

pub struct QueueModel {
    pub rules: Rules,
}

fn bit(cmd: usize) -> u8 {
    1 << cmd
}

impl Model for QueueModel {
    type State = State;
    type Action = Action;

    fn init(&self) -> Vec<State> {
        let cmd = Cmd {
            status: Status::Absent,
            epoch: 0,
            enqueues: 0,
            held: 0,
            deferred: false,
            done: None,
        };
        vec![State {
            live: 0,
            cmds: [cmd; 2],
            conns: [None; 2],
            push: false,
            armed: false,
        }]
    }

    fn actions(&self, s: &State) -> Vec<Action> {
        let mut out = Vec::new();
        for cmd in 0..2 {
            let c = &s.cmds[cmd];
            for target in 0..=s.live {
                if c.enqueues < 2 {
                    out.push(Action::Enqueue { cmd, target });
                }
            }
            if c.status == Status::Sent
                && (c.held == 0 || (self.rules.timeout_while_held && c.held < 2))
            {
                out.push(Action::Timeout { cmd });
            }
        }
        for slot in 0..2 {
            if s.conns[slot].is_none() {
                out.push(Action::Connect { slot });
                continue;
            }
            out.push(Action::Close { slot });
            out.push(Action::Idle { slot, lost: false });
            out.push(Action::Idle { slot, lost: true });
            for cmd in 0..2 {
                if s.cmds[cmd].held > 0 {
                    for answer in [Answer::Acknowledged, Answer::Error, Answer::NotNow] {
                        out.push(Action::Answer { slot, cmd, answer });
                    }
                }
            }
        }
        if s.armed {
            out.push(Action::FloorElapsed);
        }
        if s.live == 0 {
            out.push(Action::Reenroll { wiped: false });
            out.push(Action::Reenroll { wiped: true });
        }
        out
    }

    fn step(&self, s: &State, action: &Action) -> Result<State, Violation> {
        let mut n = s.clone();
        match *action {
            Action::Enqueue { cmd, target } => self.enqueue(&mut n, cmd, target),
            Action::Connect { slot } => {
                n.conns[slot] = Some(Conn {
                    epoch: s.live,
                    tried: 0,
                });
            }
            Action::Close { slot } => n.conns[slot] = None,
            Action::Idle { slot, lost } => self.idle(&mut n, slot, lost)?,
            Action::Answer { slot, cmd, answer } => self.answer(&mut n, slot, cmd, answer),
            Action::Timeout { cmd } => {
                n.cmds[cmd].status = Status::Queued;
                if self.rules.timeout_requeue_pushes {
                    self.push(&mut n);
                }
            }
            Action::FloorElapsed => {
                n.armed = false;
                if n.cmds
                    .iter()
                    .any(|c| c.status == Status::Queued && c.epoch == n.live)
                {
                    self.push(&mut n);
                }
            }
            Action::Reenroll { wiped } => reenroll(&mut n, wiped),
        }
        Ok(n)
    }

    fn check(&self, s: &State) -> Result<(), Violation> {
        for (i, c) in s.cmds.iter().enumerate() {
            if c.held > 1 && self.rules.require_single_delivery {
                return Err(Violation::new(
                    SINGLE_DELIVERY,
                    format!(
                        "command {i} is held, unanswered, by the Mac {} times",
                        c.held
                    ),
                ));
            }
            match c.done {
                Some(Status::Acked) if c.status != Status::Acked => {
                    return Err(Violation::new(
                        ACKED_NEVER_RESENT,
                        format!("command {i} was acknowledged and is now {:?}", c.status),
                    ));
                }
                Some(done) if c.status != done => {
                    return Err(Violation::new(
                        RESULT_ONLY_TO_SENT,
                        format!(
                            "command {i} ended as {done:?} and a result made it {:?}",
                            c.status
                        ),
                    ));
                }
                _ => {}
            }
            if c.deferred && c.status != Status::Queued {
                return Err(Violation::new(
                    NOT_NOW_KEPT,
                    format!("command {i} answered NotNow and is now {:?}", c.status),
                ));
            }
            if matches!(c.status, Status::Queued | Status::Sent) && c.epoch < s.live {
                return Err(Violation::new(
                    NO_STRANDED,
                    format!(
                        "command {i} is {:?} for enrollment {}, live is {}",
                        c.status, c.epoch, s.live
                    ),
                ));
            }
            let usable = s
                .conns
                .iter()
                .flatten()
                .any(|conn| conn.epoch == s.live && conn.tried & bit(i) == 0);
            let waiting = (c.status == Status::Queued) & (c.epoch == s.live);
            if waiting & !(s.push | s.armed | usable) {
                return Err(Violation::new(
                    ALWAYS_A_WAKE,
                    format!(
                        "command {i} is queued with no push, no floor timer and no usable connection"
                    ),
                ));
            }
        }
        Ok(())
    }
}

impl QueueModel {
    /// Send a push if none is outstanding, and arm the floor for the next one.
    fn push(&self, n: &mut State) {
        n.push = true;
        n.armed = true;
    }

    fn enqueue(&self, n: &mut State, cmd: usize, target: u8) {
        let live = n.live;
        let c = &mut n.cmds[cmd];
        c.enqueues += 1;
        if c.status != Status::Absent && self.rules.enqueue_is_idempotent {
            return;
        }
        if target != live && self.rules.reject_superseded_target {
            return;
        }
        c.status = Status::Queued;
        c.epoch = target;
        if target == live && !n.push {
            self.push(n);
        }
    }

    fn idle(&self, n: &mut State, slot: usize, lost: bool) -> Result<(), Violation> {
        let conn = n.conns[slot].unwrap_or_default();
        n.push = false;
        if (conn.epoch != n.live) & self.rules.refuse_superseded_identity {
            return Ok(());
        }
        let pick = (0..2).find(|&i| {
            let c = &n.cmds[i];
            let status_ok = (c.status == Status::Queued)
                | (!self.rules.claim_only_queued & (c.status == Status::Sent));
            let epoch_ok = (c.epoch == n.live) | !self.rules.refuse_superseded_identity;
            let fresh = (conn.tried & bit(i) == 0) | !self.rules.skip_tried_on_connection;
            status_ok & epoch_ok & fresh
        });
        let Some(i) = pick else {
            return Ok(());
        };
        if conn.tried & bit(i) != 0 {
            return Err(Violation::new(
                NOT_NOW_NO_LOOP,
                format!("command {i} is handed again to the connection that deferred it"),
            ));
        }
        if conn.epoch != n.cmds[i].epoch {
            return Err(Violation::new(
                NO_SUPERSEDED_DELIVERY,
                format!(
                    "command {i} of enrollment {} goes to a connection of enrollment {}",
                    n.cmds[i].epoch, conn.epoch
                ),
            ));
        }
        let c = &mut n.cmds[i];
        c.status = Status::Sent;
        c.held = (c.held + u8::from(!lost)).min(2);
        c.deferred = false;
        n.conns[slot] = Some(Conn {
            tried: conn.tried | bit(i),
            ..conn
        });
        Ok(())
    }

    fn answer(&self, n: &mut State, slot: usize, cmd: usize, answer: Answer) {
        let conn = n.conns[slot].unwrap_or_default();
        if (conn.epoch != n.live) & self.rules.refuse_superseded_identity {
            return;
        }
        n.push = false;
        n.cmds[cmd].held -= 1;
        let status = n.cmds[cmd].status;
        if status != Status::Sent && self.rules.match_result_to_sent_only {
            return;
        }
        let c = &mut n.cmds[cmd];
        match answer {
            Answer::Acknowledged => c.status = Status::Acked,
            Answer::Error => c.status = Status::Errored,
            Answer::NotNow => {
                c.deferred = true;
                c.status = if self.rules.not_now_keeps_command {
                    Status::Queued
                } else {
                    Status::Errored
                };
                if self.rules.not_now_arms_repush {
                    n.armed = true;
                }
                n.conns[slot] = Some(Conn {
                    tried: conn.tried | bit(cmd),
                    ..conn
                });
            }
        }
        let c = &mut n.cmds[cmd];
        if c.done.is_none() && matches!(c.status, Status::Acked | Status::Errored) {
            c.done = Some(c.status);
        }
    }
}

fn reenroll(n: &mut State, wiped: bool) {
    n.live += 1;
    for c in &mut n.cmds {
        if matches!(c.status, Status::Queued | Status::Sent) {
            c.status = Status::Cancelled;
            c.done = Some(Status::Cancelled);
            c.deferred = false;
        }
        if wiped {
            c.held = 0;
        }
    }
    if wiped {
        n.conns = [None; 2];
    }
    n.push = false;
    n.armed = false;
}
