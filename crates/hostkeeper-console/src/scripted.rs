//! A scripted double of the [`crate::ipmi::Tool`] port, for the tests of this crate and of the
//! coordinator (feature `testing`): each [`Tool::open`] takes the next script, which either refuses
//! the session or answers its commands in turn, and everything the double was asked is recorded.
//! [`Scripted::sol_session`] scripts the checks' session and then an activation
//! ([`Tool::activate`]) whose streams' other end, the host's serial port, the test holds
//! ([`SolHost`]). It
//! starts no process and opens no socket.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::mpsc;

use crate::ipmi::{Activated, Invocation, Shell, ShellError, Tool, ToolText};

/// What the double was asked, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    /// A session was asked for with these arguments, and this was the password the invocation
    /// held for the environment.
    Open {
        /// The tool's arguments.
        args: Vec<String>,
        /// The escape byte of a session that activates Serial over LAN.
        escape: Option<u8>,
        /// The password the invocation hands to the environment.
        password: String,
    },
    /// A command was sent in the open session.
    Ask(String),
    /// The open session was dropped, or its streams ended.
    Closed,
}

/// The next session: refused, answering its commands in turn, or an activation's streams.
enum Script {
    Refuse(ShellError),
    Answer {
        answers: VecDeque<Result<String, ShellError>>,
        hold: Option<(Sender<()>, Receiver<()>)>,
    },
    Activate(Streams),
}

/// The tool's side of a scripted serial port.
struct Streams {
    /// What the host says, which the tool prints.
    said: mpsc::Receiver<Vec<u8>>,
    /// The sender of what the host says, which ending the streams drops.
    saying: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>,
    /// Where what is typed goes.
    typed: mpsc::Sender<Vec<u8>>,
    /// What the tool wrote to its standard error, once it ended.
    stderr: Arc<Mutex<String>>,
}

/// The test's end of a scripted serial port: the host's side of a session scripted with
/// [`Scripted::sol_session`] once it is activated.
pub struct SolHost {
    saying: Arc<Mutex<Option<mpsc::Sender<Vec<u8>>>>>,
    /// What was written to the tool's standard input, in the writes it came in.
    pub typed: mpsc::Receiver<Vec<u8>>,
    stderr: Arc<Mutex<String>>,
}

impl SolHost {
    /// The host says `bytes`: the tool prints them. Nothing happens once the streams ended.
    pub async fn say(&self, bytes: &[u8]) {
        let saying = lock(&self.saying).clone();
        if let Some(saying) = saying {
            let _ = saying.send(bytes.to_vec()).await;
        }
    }

    /// The tool ends on its own, having written `stderr` to its standard error.
    pub fn exit(&self, stderr: &str) {
        stderr.clone_into(&mut lock(&self.stderr));
        lock(&self.saying).take();
    }
}

/// `mutex`, poisoned or not.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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

    /// As [`Self::session`] for the checks, and then the next activation: the host's end of its
    /// streams is answered. Each channel holds one write.
    #[must_use]
    pub fn sol_session(&self, answers: &[&str]) -> SolHost {
        self.session(answers);
        let (saying, said) = mpsc::channel(1);
        let (typed, typed_rx) = mpsc::channel(1);
        let saying = Arc::new(Mutex::new(Some(saying)));
        let stderr = Arc::new(Mutex::new(String::new()));
        self.push(Script::Activate(Streams {
            said,
            saying: Arc::clone(&saying),
            typed,
            stderr: Arc::clone(&stderr),
        }));
        SolHost {
            saying,
            typed: typed_rx,
            stderr,
        }
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

impl Scripted {
    /// Records the open of `invocation` and takes the next script.
    fn next(&self, invocation: &Invocation<'_>) -> Option<Script> {
        let mut state = self.lock();
        state.seen.push(Seen::Open {
            args: invocation.args().to_vec(),
            escape: invocation.escape(),
            password: invocation.password().to_owned(),
        });
        state.scripts.pop_front()
    }
}

impl Tool for Scripted {
    fn open(&self, invocation: &Invocation<'_>) -> Result<Box<dyn Shell>, ShellError> {
        match self.next(invocation) {
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
            None | Some(Script::Activate(_)) => Err(ShellError::Start),
        }
    }

    fn activate(&self, invocation: &Invocation<'_>) -> Result<Activated, ShellError> {
        let Some(Script::Activate(streams)) = self.next(invocation) else {
            return Err(ShellError::Start);
        };
        let Streams {
            said,
            saying,
            typed,
            stderr,
        } = streams;
        let state = Arc::clone(&self.state);
        Ok(Activated {
            input: Box::new(Typing(typed)),
            output: Box::new(Saying {
                said,
                rest: Vec::new(),
            }),
            end: Box::new(move || {
                lock(&saying).take();
                lock(&state).seen.push(Seen::Closed);
                ToolText::new(lock(&stderr).clone())
            }),
        })
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

/// The tool's standard input of a scripted serial port.
struct Typing(mpsc::Sender<Vec<u8>>);

impl Write for Typing {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .blocking_send(bytes.to_vec())
            .map_err(|_| std::io::ErrorKind::BrokenPipe)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The tool's standard output of a scripted serial port.
struct Saying {
    said: mpsc::Receiver<Vec<u8>>,
    rest: Vec<u8>,
}

impl Read for Saying {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.rest.is_empty() {
            match self.said.blocking_recv() {
                Some(bytes) => self.rest = bytes,
                None => return Ok(0),
            }
        }
        let taken = self.rest.len().min(buffer.len());
        buffer[..taken].copy_from_slice(&self.rest[..taken]);
        self.rest.drain(..taken);
        Ok(taken)
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
