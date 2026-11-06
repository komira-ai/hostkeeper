//! Waits that end. A test waits for a signal that a fake (or a hook of the daemon) sends when the
//! daemon reaches a point: a held step entered, a unit stopped, a package asked for. A signal that
//! never comes would leave a plain `recv` waiting for good, since the fake that holds the sender
//! lives as long as the test. Here every wait names the [`Party`] that would send the signal, and
//! it ends with the signal or, once that party is over without having sent it, with [`Missed`],
//! which says what ended. Nothing reads the wall clock: a party is over when its thread ends, or
//! when it has been seen past the point where it would have sent the signal.

use std::cell::Cell;
use std::fmt::Debug;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use super::lock;

/// Told, once, what ended when a [`Party`] is over.
pub type Tell = Box<dyn FnOnce(&str) + Send>;

/// What sends a signal a test waits for, and can be over without having sent it.
pub trait Party {
    /// Calls `tell` once with what ended, when this party is over; at once when it already is.
    /// A party sends its signals before it is over, so a signal sent comes before the `tell`.
    fn when_over(&self, tell: Tell);
}

/// What a wait hears on its channel: a signal, or that the party of a wait is over.
enum Heard<T> {
    Signal(T),
    Over { wait: u64, ended: String },
}

/// A wait ended without its signal: its party was over first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missed {
    /// What ended: the thread and how it returned, or what was seen.
    pub over: String,
}

/// The sending end of a [`Signal`]: what a fake, or a hook of the daemon, holds.
pub struct Notice<T>(Sender<Heard<T>>);

impl<T> Clone for Notice<T> {
    fn clone(&self) -> Self {
        Notice(self.0.clone())
    }
}

impl<T> Notice<T> {
    /// Sends `value` to the test; nothing happens when the test no longer listens.
    pub fn send(&self, value: T) {
        self.0.send(Heard::Signal(value)).unwrap_or_default();
    }
}

/// The test's end of a signal: each [`Signal::wait`] takes the next one, or fails once the party
/// it names is over.
pub struct Signal<T> {
    rx: Receiver<Heard<T>>,
    /// Given to each party, to tell its end; held here, so the channel never closes under a wait.
    tx: Sender<Heard<T>>,
    /// The waits so far: a party's end tells only the wait that named it.
    waits: Cell<u64>,
}

impl<T: Send + 'static> Signal<T> {
    /// A signal and its sending end.
    pub fn new() -> (Notice<T>, Signal<T>) {
        let (tx, rx) = mpsc::channel();
        let signal = Signal {
            rx,
            tx: tx.clone(),
            waits: Cell::new(0),
        };
        (Notice(tx), signal)
    }

    /// The next signal; [`Missed`] when `by` is over before it comes. A signal already sent is
    /// taken first, however `by` ended.
    pub fn wait(&self, by: &dyn Party) -> Result<T, Missed> {
        let wait = self.waits.get() + 1;
        self.waits.set(wait);
        let tx = self.tx.clone();
        by.when_over(Box::new(move |ended: &str| {
            let ended = ended.to_owned();
            tx.send(Heard::Over { wait, ended }).unwrap_or_default();
        }));
        loop {
            // `self.tx` is held, so `recv` never fails: the loop ends with a signal or with the
            // end of this wait's party.
            match self.rx.recv() {
                Ok(Heard::Signal(value)) => return Ok(value),
                Ok(Heard::Over { wait: of, ended }) if of == wait => {
                    return Err(Missed { over: ended });
                }
                // The party of an earlier wait, which had its signal, ended since.
                _ => {}
            }
        }
    }
}

/// Who is told when an [`Ending`] is over, and whether it is.
#[derive(Default)]
struct EndingState {
    /// What ended, once it has.
    over: Option<String>,
    tells: Vec<Tell>,
}

/// A party something declares over ([`Ending::over`]): a clock seen slept on, a watcher of the
/// daemon that ended, a task a test runs. A clone is the same party.
#[derive(Clone, Default)]
pub struct Ending(Arc<Mutex<EndingState>>);

impl Ending {
    /// The party is over: `ended` says what ended. Every wait that names it is told, and every
    /// wait that names it from now on is told at once. A second `over` changes nothing.
    pub fn over(&self, ended: &str) {
        let (ended, tells) = {
            let mut state = lock(&self.0);
            let ended = state.over.get_or_insert_with(|| ended.to_owned()).clone();
            (ended, std::mem::take(&mut state.tells))
        };
        tells.into_iter().for_each(|tell| tell(&ended));
    }
}

impl Party for Ending {
    fn when_over(&self, tell: Tell) {
        let mut state = lock(&self.0);
        match state.over.clone() {
            Some(ended) => {
                drop(state);
                tell(&ended);
            }
            None => state.tells.push(tell),
        }
    }
}

/// A thread a test runs (an order, an abort): the party of the signals it sends, over when it
/// ends, by a return or a panic.
pub struct Spawned<T> {
    thread: JoinHandle<T>,
    ending: Ending,
}

/// Ends a [`Spawned`] party when its thread ends: on its way out, or as a panic unwinds it.
struct EndOnDrop {
    ending: Ending,
    ended: String,
}

impl Drop for EndOnDrop {
    fn drop(&mut self) {
        self.ending.over(&self.ended);
    }
}

/// Runs `body` on a thread of its own, named `name` in what a [`Missed`] says: "`name` returned
/// <value>" or "`name` panicked".
pub fn spawn<T, F>(name: &str, body: F) -> Spawned<T>
where
    T: Debug + Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let ending = Ending::default();
    let mut on_drop = EndOnDrop {
        ending: ending.clone(),
        ended: format!("{name} panicked"),
    };
    let name = name.to_owned();
    let thread = thread::spawn(move || {
        let value = body();
        on_drop.ended = format!("{name} returned {value:?}");
        drop(on_drop);
        value
    });
    Spawned { thread, ending }
}

impl<T> Spawned<T> {
    /// Waits for the thread to end, as [`JoinHandle::join`].
    pub fn join(self) -> thread::Result<T> {
        self.thread.join()
    }
}

impl<T> Party for Spawned<T> {
    fn when_over(&self, tell: Tell) {
        self.ending.when_over(tell);
    }
}
