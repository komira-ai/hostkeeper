//! The progress events the network-install tests wait on, in the one order they arrived: each line
//! of the guest's serial console, of `pxe`'s log and of the router's DHCP, and the end
//! of a program that was not told to stop.
//!
//! Every wait ends on its event or fails on the end of the guest (or of a program the test still
//! needs); nothing here reads a clock or bounds a wait by time, and the job's `timeout-minutes` is
//! the only bound. A wait on an answer of `hostkeeper ctl` ([`Bus::until`]) asks again after a
//! pause, and ends the same way.

use std::fmt;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

/// The pause between two asks of [`Bus::until`].
const PAUSE: Duration = Duration::from_millis(500);

/// Where a line came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The guest's serial console.
    Guest,
    /// `pxe`'s standard error and output.
    Pxe,
    /// The router's DHCP.
    Router,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Source::Guest => "the guest's console",
            Source::Pxe => "pxe",
            Source::Router => "the router's DHCP",
        })
    }
}

/// One event.
#[derive(Clone, Debug)]
pub enum Event {
    /// A line, with no line end and no terminal control sequence.
    Line(Source, String),
    /// A program ended that was not told to stop; the text says which and how.
    Ended(Source, String),
}

/// A place in the order of events: a wait from a mark looks only at the events after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Mark(usize);

/// The events of a test, and where each source's lines are kept for the job's artifacts.
pub struct Bus {
    sender: Sender<Event>,
    receiver: Receiver<Event>,
    seen: Vec<Event>,
}

impl Default for Bus {
    fn default() -> Self {
        let (sender, receiver) = channel();
        Bus {
            sender,
            receiver,
            seen: Vec::new(),
        }
    }
}

impl Bus {
    /// A sender for a source that pushes its own lines (the router's DHCP).
    pub fn sender(&self) -> Sender<Event> {
        self.sender.clone()
    }

    /// The mark after every event that has arrived so far.
    pub fn mark(&mut self) -> Mark {
        self.drain();
        Mark(self.seen.len())
    }

    /// Reads `from` as the lines of `source` until it ends, in a thread of its own: each line
    /// is pushed to the bus and appended to `log`. At the end, unless `stopping` was set, an
    /// [`Event::Ended`] naming `name` is pushed.
    pub fn read(
        &self,
        source: Source,
        name: &str,
        from: impl Read + Send + 'static,
        log: &Path,
        stopping: Arc<AtomicBool>,
    ) {
        let sender = self.sender.clone();
        let name = name.to_owned();
        let log = File::options()
            .create(true)
            .append(true)
            .open(log)
            .unwrap_or_else(|e| panic!("open {}: {e}", log.display()));
        thread::spawn(move || {
            let mut log = BufWriter::new(log);
            let mut lines = Lines::default();
            let mut from = from;
            let mut buffer = [0u8; 4096];
            loop {
                let read = match from.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                let _ = log.write_all(&buffer[..read]);
                let _ = log.flush();
                for line in lines.push(&buffer[..read]) {
                    if sender.send(Event::Line(source, line)).is_err() {
                        return;
                    }
                }
            }
            if !stopping.load(Ordering::SeqCst) {
                let _ = sender.send(Event::Ended(source, format!("{name} ended")));
            }
        });
    }

    /// Takes every event that has arrived into the record.
    fn drain(&mut self) {
        loop {
            match self.receiver.try_recv() {
                Ok(event) => self.seen.push(event),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return,
            }
        }
    }

    /// The first line of `source` after `from` that `matches`, if one has arrived.
    fn find(&self, from: Mark, source: Source, matches: &impl Fn(&str) -> bool) -> Option<Mark> {
        self.seen
            .iter()
            .enumerate()
            .skip(from.0)
            .find_map(|(at, event)| match event {
                Event::Line(s, line) if *s == source && matches(line) => Some(Mark(at + 1)),
                _ => None,
            })
    }

    /// Fails the test if a program it needs has ended: an end is pushed only for a program the
    /// test did not stop, so any end, however long ago, is one no later event can come after.
    fn assert_alive(&self, what: &str) {
        for event in &self.seen {
            if let Event::Ended(_, why) = event {
                panic!("{what}: not reached, because {why}");
            }
        }
    }

    /// Waits for a line of `source` after `from` that `matches`, and gives the mark after it
    /// and the line. `what` names the event for the failure.
    pub fn wait(
        &mut self,
        from: Mark,
        source: Source,
        what: &str,
        matches: impl Fn(&str) -> bool,
    ) -> (Mark, String) {
        self.drain();
        let mut looked = from;
        loop {
            if let Some(found) = self.find(looked, source, &matches) {
                let Event::Line(_, line) = &self.seen[found.0 - 1] else {
                    unreachable!("find gives the mark after a line")
                };
                println!("reached: {what}: {line}");
                return (found, line.clone());
            }
            self.assert_alive(what);
            looked = Mark(self.seen.len());
            match self.receiver.recv() {
                Ok(event) => self.seen.push(event),
                Err(_) => panic!("{what}: not reached, and every source has ended"),
            }
        }
    }

    /// Whether a line of `source` after `from` and before `to` `matches`.
    pub fn saw(
        &mut self,
        from: Mark,
        to: Mark,
        source: Source,
        matches: impl Fn(&str) -> bool,
    ) -> bool {
        self.drain();
        self.find(from, source, &matches).is_some_and(|at| at <= to)
    }

    /// The lines of `source` after `from`, for a failure message.
    pub fn lines_since(&mut self, from: Mark, source: Source) -> Vec<String> {
        self.drain();
        self.seen
            .iter()
            .skip(from.0)
            .filter_map(|event| match event {
                Event::Line(s, line) if *s == source => Some(line.clone()),
                _ => None,
            })
            .collect()
    }

    /// Asks `look` until it gives a value, with a pause between two asks; fails the test,
    /// naming `what` and the last answer, if a program it needs has ended.
    pub fn until<T>(&mut self, what: &str, mut look: impl FnMut() -> Result<T, String>) -> T {
        loop {
            let last = match look() {
                Ok(value) => {
                    println!("reached: {what}");
                    return value;
                }
                Err(now) => now,
            };
            self.drain();
            for event in &self.seen {
                if let Event::Ended(_, why) = event {
                    panic!("{what}: not reached, because {why}; the last answer was: {last}");
                }
            }
            thread::sleep(PAUSE);
        }
    }
}

/// The lines of a byte stream, as they complete. A serial console's line ends are `\r\n`, and it
/// draws with terminal control sequences: both are taken out. A login prompt ends with
/// `login: ` and no line end, so a partial line that ends so is a line too.
#[derive(Default)]
pub struct Lines {
    pending: Vec<u8>,
}

impl Lines {
    /// The lines `bytes` completes.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        for &byte in bytes {
            if byte == b'\n' {
                out.push(clean(&std::mem::take(&mut self.pending)));
            } else {
                self.pending.push(byte);
                if self.pending.ends_with(b"login: ") {
                    out.push(clean(&std::mem::take(&mut self.pending)));
                }
            }
        }
        out
    }
}

/// `line` with every terminal control sequence (`ESC [ ... final`, `ESC ( x`, `ESC x`) and control
/// character taken out, and its ends trimmed.
pub fn clean(line: &[u8]) -> String {
    let text = String::from_utf8_lossy(line);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    // Parameters and intermediates, to the final byte (`@` to `~`).
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // A character set's designation, `ESC ( B`: one more character.
                Some('(' | ')') => {
                    chars.next();
                }
                Some(_) | None => {}
            },
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.trim().to_owned()
}

/// A flag a program's reader checks when its stream ends: set before the test stops the
/// program, so that its end is not a failure.
pub fn stopping() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// A line sink of the router's DHCP, which runs in a thread of the test: each line goes to the
/// bus and to the log file.
pub struct LineSink {
    sender: Sender<Event>,
    log: Mutex<BufWriter<File>>,
}

impl LineSink {
    pub fn new(bus: &Bus, log: &Path) -> LineSink {
        let file = File::options()
            .create(true)
            .append(true)
            .open(log)
            .unwrap_or_else(|e| panic!("open {}: {e}", log.display()));
        LineSink {
            sender: bus.sender(),
            log: Mutex::new(BufWriter::new(file)),
        }
    }

    pub fn line(&self, source: Source, line: String) {
        {
            let mut log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = writeln!(log, "{line}");
            let _ = log.flush();
        }
        let _ = self.sender.send(Event::Line(source, line));
    }
}
