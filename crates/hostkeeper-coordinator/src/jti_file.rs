//! The spent OIDC token ids of CI's tokens, kept in `<state-file>.jti` so that a restart opens no
//! window to replay a token (design section 8.8): the coordinator's [`SpentJtis`].
//!
//! One line per id: `<jti> <until>\n`, the id and the second since the epoch until which it is
//! refused (the token's `exp` and the leeway). A spend appends its line and syncs the file before
//! the verifier accepts the token; a line that cannot be written refuses the token. An id that is
//! not one word of visible ASCII is not written (it would not read back as one line), so its
//! token is refused.
//!
//! The file is read strictly when the coordinator starts: a line that is not an id and a whole
//! number stops the start, naming the line. The ids already expired are dropped then, and the
//! file is rewritten without them (a temporary file, fsync, rename, a directory fsync). It is
//! rewritten the same way while the coordinator runs, once the lines it holds outnumber the live
//! ids by [`COMPACT_SLACK`]. Expiry is the one reading of the wall clock here, the OIDC validity
//! of a token (design section 8.10, clock skew); nothing the coordinator decides reads it.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use hostkeeper_oidc::{Clock, SpentJtis};

use crate::desired::write_whole;

/// How many more lines than live ids the file may hold before it is rewritten.
pub const COMPACT_SLACK: usize = 1024;

/// Why the file of spent ids was refused at start.
#[derive(Debug, thiserror::Error)]
pub enum JtiError {
    /// The file could not be read or rewritten.
    #[error("{path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// A line is not an id and a whole number.
    #[error(
        "{path}: line {line} is not `<jti> <until>`; the coordinator does not guess which tokens were spent, so fix or remove the line to start"
    )]
    Malformed {
        /// The file.
        path: PathBuf,
        /// The line, counting from 1.
        line: usize,
    },
}

/// The file of spent ids, and what it holds.
pub struct JtiFile {
    path: PathBuf,
    clock: Clock,
    kept: Mutex<Kept>,
}

/// The live ids, and how many lines the file holds.
struct Kept {
    live: BTreeMap<String, u64>,
    lines: usize,
}

impl std::fmt::Debug for JtiFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JtiFile")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Whether `jti` can be written as the first word of a line.
fn is_word(jti: &str) -> bool {
    !jti.is_empty() && jti.bytes().all(|b| b.is_ascii_graphic())
}

/// The id and expiry of `line`, if it is `<jti> <until>`.
fn parse_line(line: &str) -> Option<(String, u64)> {
    let (jti, until) = line.split_once(' ')?;
    let digits = !until.is_empty() && until.bytes().all(|b| b.is_ascii_digit());
    let until = until.parse::<u64>().ok().filter(|_| digits)?;
    is_word(jti).then(|| (jti.to_owned(), until))
}

/// The text of a file holding `live`.
fn text_of(live: &BTreeMap<String, u64>) -> String {
    live.iter()
        .map(|(jti, until)| format!("{jti} {until}\n"))
        .collect()
}

/// What a file of spent ids holds: the ids not yet past, how many lines it has, and whether it is
/// empty or not there.
struct Parsed {
    live: BTreeMap<String, u64>,
    lines: usize,
    empty: bool,
}

/// Reads the file at `path`, its ids past at `now` left out of the live ones.
fn parse(path: &Path, now: u64) -> Result<Parsed, JtiError> {
    let io_at = |source: io::Error| JtiError::Io {
        path: path.to_owned(),
        source,
    };
    let text = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(io_at(e)),
    };
    let malformed = |line: usize| JtiError::Malformed {
        path: path.to_owned(),
        line,
    };
    let text = String::from_utf8(text).map_err(|_| malformed(1))?;
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(malformed(text.lines().count()));
    }
    let mut live = BTreeMap::new();
    let mut lines = 0;
    for (n, line) in text.lines().enumerate() {
        let (jti, until) = parse_line(line).ok_or_else(|| malformed(n + 1))?;
        lines += 1;
        if until > now {
            live.insert(jti, until);
        }
    }
    Ok(Parsed {
        live,
        lines,
        empty: text.is_empty(),
    })
}

/// The ids the file at `path` holds that are not yet past at `now`, sorted, read as
/// [`JtiFile::open`] reads them and without writing anything: a file that is not there holds
/// none.
///
/// # Errors
/// [`JtiError`]: the file cannot be read, or a line of it is malformed.
pub(crate) fn read_live(path: &Path, now: u64) -> Result<Vec<(String, u64)>, JtiError> {
    Ok(parse(path, now)?.live.into_iter().collect())
}

/// The text of a file holding the spent ids `spent`, as the file is written.
pub(crate) fn text_of_spent(spent: &[(String, u64)]) -> String {
    text_of(&spent.iter().cloned().collect())
}

impl JtiFile {
    /// The file at `path` (created if it is not there), its expired ids dropped by `clock` and
    /// the file rewritten without them.
    ///
    /// # Errors
    /// [`JtiError`]: the file cannot be read or rewritten, or a line of it is malformed.
    pub fn open(path: &Path, clock: Clock) -> Result<Self, JtiError> {
        let io_at = |source: io::Error| JtiError::Io {
            path: path.to_owned(),
            source,
        };
        let Parsed { live, lines, empty } = parse(path, clock())?;
        if lines != live.len() || empty {
            write_whole(path, text_of(&live).as_bytes())
                .map_err(|why| io_at(io::Error::other(why)))?;
        }
        let lines = live.len();
        Ok(Self {
            path: path.to_owned(),
            clock,
            kept: Mutex::new(Kept { live, lines }),
        })
    }

    /// Appends `line` to the file and syncs it.
    fn append(&self, line: &str) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .append(true)
            .mode(0o600)
            .open(&self.path)?;
        file.write_all(line.as_bytes())?;
        file.sync_data()
    }
}

impl JtiFile {
    /// The file.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Records that `jti` is spent until `until`, its line appended and synced, the ids past at
    /// `now` dropped first.
    ///
    /// # Errors
    /// [`io::ErrorKind::InvalidInput`]: the id is not one word of visible ASCII.
    /// [`io::ErrorKind::AlreadyExists`]: the id is spent and not yet past, a replay. Any other:
    /// the line could not be written; the id is not spent.
    pub(crate) fn spend_at(&self, jti: &str, until: u64, now: u64) -> io::Result<()> {
        if !is_word(jti) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the token id is not one word of visible ASCII",
            ));
        }
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        kept.live.retain(|_, until| *until > now);
        if kept.live.contains_key(jti) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the token id is spent",
            ));
        }
        self.append(&format!("{jti} {until}\n"))?;
        kept.lines += 1;
        kept.live.insert(jti.to_owned(), until);
        if kept.lines > kept.live.len() + COMPACT_SLACK {
            // A rewrite that fails leaves the longer file, which reads the same: it is tried
            // again at the next spend.
            match write_whole(&self.path, text_of(&kept.live).as_bytes()) {
                Ok(()) => kept.lines = kept.live.len(),
                Err(why) => tracing::warn!(
                    path = %self.path.display(),
                    %why,
                    "the file of spent token ids could not be rewritten shorter"
                ),
            }
        }
        Ok(())
    }
}

impl SpentJtis for JtiFile {
    fn load(&self) -> Vec<(String, u64)> {
        let kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        kept.live.iter().map(|(j, u)| (j.clone(), *u)).collect()
    }

    fn record(&self, jti: &str, until: u64) -> io::Result<()> {
        self.spend_at(jti, until, (self.clock)())
    }
}

/// The file of spent ids of the state file at `state`: its name with `.jti` added.
#[must_use]
pub fn jti_path(state: &Path) -> PathBuf {
    let mut path = state.as_os_str().to_owned();
    path.push(".jti");
    PathBuf::from(path)
}
