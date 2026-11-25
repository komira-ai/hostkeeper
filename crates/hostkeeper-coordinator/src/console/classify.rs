//! What a probe recognizes in a serial console's bytes (`Console/Probe`): no clock, no socket, the
//! bytes in the order they came, in pieces of any size.
//!
//! The bytes are cut into lines at each carriage return or line feed. A line's text is what is
//! left once its escape sequences (`ESC [` up to the sequence's final byte, or `ESC` and one more
//! byte) and other control characters are taken out (a tab is a space), read as UTF-8 with
//! replacement, and trimmed; a line with no text is passed over, and only a line's first
//! [`MAX_LINE`] bytes are kept. The last line counts without its line feed: a login prompt has
//! none. Each line is, in this order:
//!
//! - a kernel line: `[`, spaces, digits, `.`, digits and `]` at its start, the kernel's log with its
//!   seconds since boot. The first one whose seconds are all zeros is the kernel's first line, and
//!   the read then began before the kernel started (`from_boot`): the first line of text since
//!   the last line of the running system (a kernel line, a login prompt or a line of systemd's
//!   status, below), or since the start, is the firmware's or GRUB's;
//! - a login prompt: its text ends with `login:`;
//! - a line of systemd's status (`[  OK  ]`, `[FAILED]`, `[ INFO ]`, `[DEPEND]`, `[ WARN ]` at its
//!   start), which a system shutting down for a reboot writes: no firmware's, and none of the
//!   classes;
//! - other text: it may become the firmware's, as above.
//!
//! Each class is found once, with the first line that showed it, in the order found: a firmware
//! line is found when the kernel's first line confirms it, just before that line's kernel finding.
//! Text read before the reboot of a running system that wrote none of these lines (a banner,
//! a shell) would be taken for the firmware's; the system's own shutdown writes kernel and systemd
//! lines after it. With none found, bytes that came are `other_output` (with the first line of text, or none), and
//! no byte at all is `silent`.

use hostkeeper_proto::hostkeeper::operator::v1::{ConsoleProbeClass as Class, ConsoleProbeFinding};

/// The most bytes of one line kept to be looked at.
pub(super) const MAX_LINE: usize = 1024;

/// The most bytes of a finding's evidence.
pub(super) const MAX_EVIDENCE: usize = 160;

/// The starts of systemd's status lines on a console.
const SYSTEMD_STATUS: [&str; 5] = ["[  OK  ]", "[FAILED]", "[ INFO ]", "[DEPEND]", "[ WARN ]"];

/// The escape byte, which starts a terminal's control sequence.
const ESC: u8 = 0x1b;

/// Where the reading is in a control sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Escape {
    /// In text.
    None,
    /// After `ESC`.
    Started,
    /// After `ESC [`, up to the final byte.
    Csi,
}

/// The reading of a probe: what came so far, and what it showed.
#[derive(Debug)]
pub(super) struct Reading {
    bytes: u64,
    line: Vec<u8>,
    escape: Escape,
    found: Vec<ConsoleProbeFinding>,
    /// The first line of text since the last kernel line (or the start): the firmware's, should
    /// the kernel's first line follow.
    since_kernel: Option<String>,
    /// The first line of text that is none of the classes.
    other: Option<String>,
    from_boot: bool,
}

/// What a probe saw: each class with its evidence, and whether it read from the boot on.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Seen {
    pub(super) findings: Vec<ConsoleProbeFinding>,
    pub(super) from_boot: bool,
    pub(super) bytes: u64,
}

impl Reading {
    /// A reading of no byte yet.
    pub(super) fn new() -> Self {
        Self {
            bytes: 0,
            line: Vec::new(),
            escape: Escape::None,
            found: Vec::new(),
            since_kernel: None,
            other: None,
            from_boot: false,
        }
    }

    /// The bytes read so far.
    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Reads `bytes`, the next that came.
    pub(super) fn take(&mut self, bytes: &[u8]) {
        self.bytes = self
            .bytes
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        for &byte in bytes {
            self.escape = match (self.escape, byte) {
                (Escape::None, ESC) | (Escape::Started | Escape::Csi, ESC) => Escape::Started,
                (Escape::Started, b'[') => Escape::Csi,
                (Escape::Started, _) | (Escape::Csi, 0x40..=0x7e) => Escape::None,
                (Escape::Csi, _) => Escape::Csi,
                (Escape::None, b'\r' | b'\n') => {
                    self.end_line();
                    Escape::None
                }
                (Escape::None, b'\t') => {
                    self.keep(b' ');
                    Escape::None
                }
                (Escape::None, 0x00..=0x1f | 0x7f) => Escape::None,
                (Escape::None, _) => {
                    self.keep(byte);
                    Escape::None
                }
            };
        }
    }

    /// What the reading saw, its last line counted.
    pub(super) fn finish(mut self) -> Seen {
        self.end_line();
        let mut findings = self.found;
        if findings.is_empty() {
            let (class, evidence) = if self.bytes == 0 {
                (Class::Silent, String::new())
            } else {
                (Class::OtherOutput, self.other.unwrap_or_default())
            };
            findings.push(finding(class, evidence));
        }
        Seen {
            findings,
            from_boot: self.from_boot,
            bytes: self.bytes,
        }
    }

    /// Keeps `byte` in the line, up to its most bytes.
    fn keep(&mut self, byte: u8) {
        if self.line.len() < MAX_LINE {
            self.line.push(byte);
        }
    }

    /// Ends the line read so far and looks at its text.
    fn end_line(&mut self) {
        let line = std::mem::take(&mut self.line);
        let text = String::from_utf8_lossy(&line);
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if let Some(first) = kernel_seconds_zero(text) {
            if first && !self.from_boot {
                self.from_boot = true;
                if let Some(firmware) = self.since_kernel.take() {
                    self.find(Class::FirmwareOutput, firmware);
                }
            }
            self.since_kernel = None;
            self.find(Class::KernelOutput, text.to_owned());
        } else if text.ends_with("login:") {
            self.since_kernel = None;
            self.find(Class::LoginPrompt, text.to_owned());
        } else if SYSTEMD_STATUS.iter().any(|status| text.starts_with(status)) {
            self.since_kernel = None;
            self.other.get_or_insert_with(|| text.to_owned());
        } else {
            self.since_kernel.get_or_insert_with(|| text.to_owned());
            self.other.get_or_insert_with(|| text.to_owned());
        }
    }

    /// Finds `class` with `evidence`, unless it was found already.
    fn find(&mut self, class: Class, evidence: String) {
        if self.found.iter().all(|found| found.class() != class) {
            self.found.push(finding(class, evidence));
        }
    }
}

/// The finding of `class` with `evidence`, cut to [`MAX_EVIDENCE`] bytes.
fn finding(class: Class, mut evidence: String) -> ConsoleProbeFinding {
    let mut end = evidence.len().min(MAX_EVIDENCE);
    while !evidence.is_char_boundary(end) {
        end -= 1;
    }
    evidence.truncate(end);
    ConsoleProbeFinding {
        class: class.into(),
        evidence,
    }
}

/// For a kernel line (`[`, spaces, digits, `.`, digits and `]` at the start of `text`), whether its
/// seconds are all zeros; `None` for any other line.
fn kernel_seconds_zero(text: &str) -> Option<bool> {
    let stamp = text.strip_prefix('[')?.trim_start_matches(' ');
    let (stamp, _) = stamp.split_once(']')?;
    let (whole, fraction) = stamp.split_once('.')?;
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || !digits(fraction) {
        return None;
    }
    Some(whole.bytes().chain(fraction.bytes()).all(|b| b == b'0'))
}

#[cfg(test)]
#[path = "classify_tests.rs"]
mod tests;
