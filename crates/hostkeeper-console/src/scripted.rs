//! A scripted double of the [`crate::ipmi::Tool`] port, for the tests of this crate and of the
//! coordinator (feature `testing`): each [`Tool::open`] takes the next script, which either refuses
//! the session or answers its commands in turn, and everything the double was asked is recorded.
//! It starts no process and opens no socket.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::ipmi::{Invocation, Shell, ShellError, Tool, ToolText};

/// What the double was asked, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    /// A session was asked for with these arguments, and this was the password the invocation
    /// held for the environment.
    Open {
        /// The tool's arguments.
        args: Vec<String>,
        /// The password the invocation hands to the environment.
        password: String,
    },
    /// A command was sent in the open session.
    Ask(String),
    /// The open session was dropped.
    Closed,
}

/// The next session: refused, or answering its commands in turn.
enum Script {
    Refuse(ShellError),
    Answer {
        answers: VecDeque<Result<String, ShellError>>,
        hold: Option<(Sender<()>, Receiver<()>)>,
    },
}

#[derive(Default)]
struct State {
    scripts: VecDeque<Script>,
    seen: Vec<Seen>,
}

/// The double. Clones share their scripts and what was seen.
#[derive(Clone, Default)]
pub struct Scripted {
    state: Arc<Mutex<State>>,
}

/// Holds an open of a held session until released: [`Held::entered`] receives once the open has
/// begun, and the open returns once [`Held::release`] is sent.
pub struct Held {
    /// Receives once the held open has begun.
    pub entered: Receiver<()>,
    /// Lets the held open return.
    pub release: Sender<()>,
}

impl Scripted {
    /// A double with nothing scripted: every session is refused as a tool that cannot start.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The next session opens and answers each command with the next of `answers`; past them,
    /// the program ends with nothing on its standard error.
    pub fn session(&self, answers: &[&str]) {
        self.push(Script::Answer {
            answers: answers.iter().map(|a| Ok((*a).to_owned())).collect(),
            hold: None,
        });
    }

    /// The next session opens and gives each of `answers` in turn, an error among them ending
    /// the program at that command.
    pub fn session_with(&self, answers: Vec<Result<String, ShellError>>) {
        self.push(Script::Answer {
            answers: answers.into(),
            hold: None,
        });
    }

    /// As [`Self::session`], and its open waits until released.
    #[must_use]
    pub fn held_session(&self, answers: &[&str]) -> Held {
        let (entered_tx, entered) = channel();
        let (release, release_rx) = channel();
        self.push(Script::Answer {
            answers: answers.iter().map(|a| Ok((*a).to_owned())).collect(),
            hold: Some((entered_tx, release_rx)),
        });
        Held { entered, release }
    }

    /// The next session is refused with `why`.
    pub fn refuse(&self, why: ShellError) {
        self.push(Script::Refuse(why));
    }

    /// The next session ends at its start, the program having written `stderr`.
    pub fn end_at_start(&self, stderr: &str) {
        self.refuse(ShellError::Ended(ToolText::new(stderr.to_owned())));
    }

    /// Everything asked so far, in order.
    #[must_use]
    pub fn seen(&self) -> Vec<Seen> {
        self.lock().seen.clone()
    }

    /// How many scripts are still unused.
    #[must_use]
    pub fn unused(&self) -> usize {
        self.lock().scripts.len()
    }

    fn push(&self, script: Script) {
        self.lock().scripts.push_back(script);
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Tool for Scripted {
    fn open(&self, invocation: &Invocation<'_>) -> Result<Box<dyn Shell>, ShellError> {
        let script = {
            let mut state = self.lock();
            state.seen.push(Seen::Open {
                args: invocation.args().to_vec(),
                password: invocation.password().to_owned(),
            });
            state.scripts.pop_front()
        };
        match script {
            None => Err(ShellError::Start),
            Some(Script::Refuse(why)) => Err(why),
            Some(Script::Answer { answers, hold }) => {
                if let Some((entered, release)) = hold {
                    let _ = entered.send(());
                    let _ = release.recv();
                }
                Ok(Box::new(ScriptedShell {
                    state: Arc::clone(&self.state),
                    answers,
                }))
            }
        }
    }
}

/// An open session of the double.
struct ScriptedShell {
    state: Arc<Mutex<State>>,
    answers: VecDeque<Result<String, ShellError>>,
}

impl ScriptedShell {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Shell for ScriptedShell {
    fn ask(&mut self, command: &str) -> Result<String, ShellError> {
        self.lock().seen.push(Seen::Ask(command.to_owned()));
        self.answers
            .pop_front()
            .unwrap_or_else(|| Err(ShellError::Ended(ToolText::new(String::new()))))
    }
}

impl Drop for ScriptedShell {
    fn drop(&mut self) {
        self.lock().seen.push(Seen::Closed);
    }
}

#[cfg(test)]
#[path = "scripted_tests.rs"]
mod tests;
