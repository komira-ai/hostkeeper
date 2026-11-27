//! A read-only reader of a SQLite 3 database file, for rasdaemon's database
//! ([`crate::rasdaemon`]): the tables its schema lists, and the rows of a table after a rowid.
//!
//! **Why not SQLite.** The daemon's Linux build is static, for musl, with the host's gcc, which
//! builds only C that links against no libc (`.cargo/config.toml`); SQLite's own C is not such
//! C. So this reads the file format itself (<https://www.sqlite.org/fileformat2.html>): the
//! header, the table b-trees (interior and leaf pages, with overflow pages), and the records of
//! their cells. It reads table b-trees only, never an index, and writes nothing.
//!
//! **No lock is taken.** SQLite's readers take a shared lock that holds a writer back; this takes
//! none, so it can never block rasdaemon's writer. A read is judged afterwards instead
//! ([`steady`]): it counts only when no write was in progress before it or after it (no hot
//! rollback journal beside the file) and the file's change counter, which every write
//! transaction moves, is the same after it as before; one that is not is tried again, up to
//! [`ATTEMPTS`] times in all, then refused ([`SqliteError::Busy`]). A database in WAL mode,
//! whose newest pages are not in the file, is refused ([`SqliteError::Wal`]), and so is a text
//! encoding other than UTF-8 ([`SqliteError::Encoding`]).
//!
//! **Bounded.** Pages are read one at a time, at their offset, only those a read walks; a walk
//! deeper than [`MAX_DEPTH`] or visiting more pages than the file holds is a malformed file, so a
//! b-tree that loops ends; a record's payload past [`MAX_PAYLOAD`] is refused. Every offset is
//! checked against its page, so a malformed file is an error, never a panic.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

/// The size of the file's header, at the start of page 1.
const HEADER: usize = 100;

/// The first 16 bytes of every SQLite 3 database.
const MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// The first 8 bytes of a rollback journal that holds a transaction (a hot journal).
const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];

/// The deepest table b-tree read.
pub const MAX_DEPTH: usize = 32;

/// The most bytes of one record's payload.
pub const MAX_PAYLOAD: u64 = 1 << 20;

/// How many times [`steady`] reads before it refuses.
pub const ATTEMPTS: usize = 3;

/// The page sizes a database can have.
const PAGE_SIZES: [u64; 8] = [512, 1024, 2048, 4096, 8192, 16384, 32768, 65536];

/// The page type of an interior page of a table b-tree.
const TABLE_INTERIOR: u8 = 0x05;
/// The page type of a leaf page of a table b-tree.
const TABLE_LEAF: u8 = 0x0d;

/// Why the database could not be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SqliteError {
    /// The file could not be opened or read.
    #[error("cannot be read: {0}")]
    Io(String),
    /// The file does not start with SQLite's header.
    #[error("not a SQLite database")]
    NotSqlite,
    /// The database is in WAL mode.
    #[error("the database is in WAL mode")]
    Wal,
    /// The database's text is not UTF-8.
    #[error("its text encoding is not UTF-8")]
    Encoding,
    /// A write was in progress, or happened, at each of the [`ATTEMPTS`] reads.
    #[error("the database was being written")]
    Busy,
    /// The file breaks the format: what was found wrong.
    #[error("malformed: {0}")]
    Malformed(&'static str),
}

type Result<T> = std::result::Result<T, SqliteError>;

fn io_error(e: &io::Error) -> SqliteError {
    SqliteError::Io(e.to_string())
}

/// One value of a record.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

/// One row of a table: its rowid and its values, in the order of the table's columns. A row
/// written before a column was added has fewer values; the ones it lacks are read as
/// [`Value::Null`] ([`Row::get`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub rowid: i64,
    pub values: Vec<Value>,
}

impl Row {
    /// The value of column `index`; [`Value::Null`] past the row's values.
    #[must_use]
    pub fn get(&self, index: usize) -> &Value {
        self.values.get(index).unwrap_or(&Value::Null)
    }
}

/// A table the schema lists: its root page and its columns' names, in order, lowercased.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    pub root: u32,
    pub columns: Vec<String>,
}

impl Table {
    /// The index of the column named `name` (lowercase); none when the table has no such column.
    #[must_use]
    pub fn column(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c == name)
    }
}

/// An open database file.
pub struct Database {
    path: PathBuf,
    file: File,
    page_size: u64,
    usable: usize,
    /// The pages the file holds, as of the start of the last read ([`steady`]): rasdaemon
    /// appends pages while the file is open.
    pages: Cell<u64>,
}

impl Database {
    /// Opens `path` read-only and reads its header.
    ///
    /// # Errors
    /// [`SqliteError::Io`] when it cannot be opened or read; [`SqliteError::NotSqlite`],
    /// [`SqliteError::Wal`], [`SqliteError::Encoding`] or [`SqliteError::Malformed`] by its
    /// header.
    pub fn open(path: &Path) -> Result<Database> {
        let file = File::open(path).map_err(|e| io_error(&e))?;
        let len = file.metadata().map_err(|e| io_error(&e))?.len();
        let mut header = [0u8; HEADER];
        file.read_exact_at(&mut header, 0).map_err(|e| {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                SqliteError::NotSqlite
            } else {
                io_error(&e)
            }
        })?;
        if &header[..16] != MAGIC {
            return Err(SqliteError::NotSqlite);
        }
        let page_size = match u16::from_be_bytes([header[16], header[17]]) {
            1 => 65536,
            size => u64::from(size),
        };
        if !PAGE_SIZES.contains(&page_size) {
            return Err(SqliteError::Malformed("its page size"));
        }
        // The write and read versions: 1 for a rollback journal, 2 for WAL.
        let versions: &[u8] = &header[18..20];
        if versions.contains(&2) {
            return Err(SqliteError::Wal);
        }
        if versions != [1u8, 1].as_slice() {
            return Err(SqliteError::Malformed("its file format version"));
        }
        let usable = usize::try_from(page_size)
            .ok()
            .and_then(|size| size.checked_sub(usize::from(header[20])))
            .filter(|usable| *usable >= 480)
            .ok_or(SqliteError::Malformed("its reserved space"))?;
        if !matches!(be32(&header, 56), 0 | 1) {
            return Err(SqliteError::Encoding);
        }
        Ok(Database {
            path: path.to_path_buf(),
            file,
            page_size,
            usable,
            pages: Cell::new(len / page_size),
        })
    }

    /// Counts the file's pages again, from its length now.
    fn recount(&self) -> Result<()> {
        let len = self.file.metadata().map_err(|e| io_error(&e))?.len();
        self.pages.set(len / self.page_size);
        Ok(())
    }

    /// The file's change counter, read again from its header.
    fn change_counter(&self) -> Result<u32> {
        let mut header = [0u8; HEADER];
        self.file
            .read_exact_at(&mut header, 0)
            .map_err(|e| io_error(&e))?;
        Ok(be32(&header, 24))
    }

    /// Whether a rollback journal holding a transaction is beside the file: a write is in
    /// progress, or one was cut off and not yet rolled back.
    fn journal_hot(&self) -> Result<bool> {
        let mut journal = self.path.clone().into_os_string();
        journal.push("-journal");
        let file = match File::open(&journal) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(io_error(&e)),
        };
        let mut magic = [0u8; 8];
        match file.read_exact_at(&mut magic, 0) {
            Ok(()) => Ok(magic == JOURNAL_MAGIC),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
            Err(e) => Err(io_error(&e)),
        }
    }

    /// Page `number` (from 1), whole.
    fn page(&self, number: u32) -> Result<Vec<u8>> {
        if !(1..=self.pages.get()).contains(&u64::from(number)) {
            return Err(SqliteError::Malformed("a page number past the file"));
        }
        let mut page = vec![0u8; usize::try_from(self.page_size).unwrap_or(usize::MAX)];
        self.file
            .read_exact_at(&mut page, (u64::from(number) - 1) * self.page_size)
            .map_err(|e| io_error(&e))?;
        Ok(page)
    }

    /// The tables of the schema, by name (lowercase): every row of `sqlite_schema` of type
    /// `table`.
    ///
    /// # Errors
    /// The read's own; [`SqliteError::Malformed`] when a table's row or `CREATE` statement is.
    pub fn tables(&self) -> Result<BTreeMap<String, Table>> {
        let mut tables = BTreeMap::new();
        for row in self.rows_after(1, i64::MIN, usize::MAX)? {
            if row.get(0) != &Value::Text("table".to_owned()) {
                continue;
            }
            let (Value::Text(name), Value::Integer(root), Value::Text(sql)) =
                (row.get(1), row.get(3), row.get(4))
            else {
                return Err(SqliteError::Malformed("a table's row of the schema"));
            };
            let root = u32::try_from(*root).map_err(|_| SqliteError::Malformed("a root page"))?;
            let columns =
                columns(sql).ok_or(SqliteError::Malformed("a table's CREATE statement"))?;
            tables.insert(name.to_lowercase(), Table { root, columns });
        }
        Ok(tables)
    }

    /// The largest rowid of the table whose root page is `root`; none when it has no row.
    ///
    /// # Errors
    /// The read's own; [`SqliteError::Malformed`] when its b-tree is.
    pub fn max_rowid(&self, root: u32) -> Result<Option<i64>> {
        let mut number = root;
        for _ in 0..MAX_DEPTH {
            let page = self.page(number)?;
            let node = Node::of(&page, number, self.usable)?;
            match node.kind {
                TABLE_LEAF => {
                    return match node.cells.checked_sub(1) {
                        None => Ok(None),
                        Some(last) => {
                            let at = node.cell(&page, last)?;
                            let (_, at) = varint(&page, at)?;
                            Ok(Some(varint(&page, at)?.0.cast_signed()))
                        }
                    };
                }
                _ => number = node.right,
            }
        }
        Err(SqliteError::Malformed("a b-tree deeper than its bound"))
    }

    /// At most `limit` rows of the table whose root page is `root` whose rowid is greater than
    /// `after`, in rowid order.
    ///
    /// # Errors
    /// The read's own; [`SqliteError::Malformed`] when the b-tree or a record is.
    pub fn rows_after(&self, root: u32, after: i64, limit: usize) -> Result<Vec<Row>> {
        let mut walk = Walk {
            after,
            limit,
            rows: Vec::new(),
            visits: 0,
        };
        self.walk(root, 0, &mut walk)?;
        Ok(walk.rows)
    }

    fn walk(&self, number: u32, depth: usize, walk: &mut Walk) -> Result<()> {
        walk.visits += 1;
        if walk.visits > self.pages.get() {
            return Err(SqliteError::Malformed("a b-tree that loops"));
        }
        if depth >= MAX_DEPTH {
            return Err(SqliteError::Malformed("a b-tree deeper than its bound"));
        }
        let page = self.page(number)?;
        let node = Node::of(&page, number, self.usable)?;
        for index in 0..node.cells {
            if walk.rows.len() >= walk.limit {
                return Ok(());
            }
            let at = node.cell(&page, index)?;
            if node.kind == TABLE_LEAF {
                let (size, at) = varint(&page, at)?;
                let (rowid, at) = varint(&page, at)?;
                let rowid = rowid.cast_signed();
                if rowid > walk.after {
                    let payload = self.payload(&page, at, size)?;
                    walk.rows.push(Row {
                        rowid,
                        values: record(&payload)?,
                    });
                }
            } else {
                let child = be32(take(&page, at, 4)?, 0);
                let (key, _) = varint(&page, at + 4)?;
                if key.cast_signed() > walk.after {
                    self.walk(child, depth + 1, walk)?;
                }
            }
        }
        // Walked when the rows are already in hand too: its first cell then ends it, so a walk
        // that is full reads at most one page more.
        if node.kind == TABLE_INTERIOR {
            self.walk(node.right, depth + 1, walk)?;
        }
        Ok(())
    }

    /// The payload of a table leaf cell of `size` bytes whose local part starts at `at` of
    /// `page`, its overflow pages followed.
    fn payload(&self, page: &[u8], at: usize, size: u64) -> Result<Vec<u8>> {
        if size > MAX_PAYLOAD {
            return Err(SqliteError::Malformed("a record past its bound"));
        }
        let size = usize::try_from(size).unwrap_or(usize::MAX);
        let usable = self.usable;
        let most_local = usable - 35;
        let least_local = (usable - 12) * 32 / 255 - 23;
        let local = if size <= most_local {
            size
        } else {
            let spill = least_local + (size - least_local) % (usable - 4);
            if spill <= most_local {
                spill
            } else {
                least_local
            }
        };
        let mut payload = take(page, at, local)?.to_vec();
        if local == size {
            return Ok(payload);
        }
        let mut next = be32(take(page, at + local, 4)?, 0);
        let mut hops = 0;
        while payload.len() < size {
            hops += 1;
            if next == 0 || hops > self.pages.get() {
                return Err(SqliteError::Malformed("an overflow chain"));
            }
            let overflow = self.page(next)?;
            next = be32(&overflow, 0);
            let part = (size - payload.len()).min(usable - 4);
            payload.extend_from_slice(take(&overflow, 4, part)?);
        }
        Ok(payload)
    }
}

/// What a walk of a b-tree has gathered.
struct Walk {
    after: i64,
    limit: usize,
    rows: Vec<Row>,
    visits: u64,
}

/// A b-tree page's header.
struct Node {
    kind: u8,
    cells: usize,
    /// Where its cell pointers start.
    pointers: usize,
    /// The right-most child of an interior page.
    right: u32,
    usable: usize,
}

impl Node {
    fn of(page: &[u8], number: u32, usable: usize) -> Result<Node> {
        let start = if number == 1 { HEADER } else { 0 };
        let header = take(page, start, 8)?;
        let kind = header[0];
        let cells = usize::from(u16::from_be_bytes([header[3], header[4]]));
        match kind {
            TABLE_LEAF => Ok(Node {
                kind,
                cells,
                pointers: start + 8,
                right: 0,
                usable,
            }),
            TABLE_INTERIOR => Ok(Node {
                kind,
                cells,
                pointers: start + 12,
                right: be32(take(page, start + 8, 4)?, 0),
                usable,
            }),
            _ => Err(SqliteError::Malformed(
                "a page that is not of a table b-tree",
            )),
        }
    }

    /// Where cell `index` starts.
    fn cell(&self, page: &[u8], index: usize) -> Result<usize> {
        let pointer = take(page, self.pointers + 2 * index, 2)?;
        let at = usize::from(u16::from_be_bytes([pointer[0], pointer[1]]));
        if at >= self.usable {
            return Err(SqliteError::Malformed("a cell past its page"));
        }
        Ok(at)
    }
}

/// `len` bytes of `bytes` from `at`.
fn take(bytes: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    at.checked_add(len)
        .and_then(|end| bytes.get(at..end))
        .ok_or(SqliteError::Malformed("a field past its page"))
}

/// The big-endian `u32` at `at` of `bytes`, which holds 4 bytes there.
fn be32(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0u8; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    u32::from_be_bytes(word)
}

/// The varint at `at` of `bytes` and where it ends: at most 9 bytes, seven bits of each of the
/// first eight and the whole ninth.
fn varint(bytes: &[u8], at: usize) -> Result<(u64, usize)> {
    let byte = |i: usize| {
        bytes
            .get(at + i)
            .copied()
            .ok_or(SqliteError::Malformed("a varint past its page"))
    };
    let mut value = 0u64;
    for i in 0..8 {
        let b = byte(i)?;
        value = (value << 7) | u64::from(b & 0x7f);
        if b & 0x80 == 0 {
            return Ok((value, at + i + 1));
        }
    }
    Ok(((value << 8) | u64::from(byte(8)?), at + 9))
}

/// The values of the record `payload`.
fn record(payload: &[u8]) -> Result<Vec<Value>> {
    let (header_len, mut at) = varint(payload, 0)?;
    let header_end = usize::try_from(header_len)
        .ok()
        .filter(|end| *end <= payload.len() && *end >= at)
        .ok_or(SqliteError::Malformed("a record's header"))?;
    let mut body = header_end;
    let mut values = Vec::new();
    while at < header_end {
        let (serial, next) = varint(payload, at)?;
        at = next;
        let (value, len) = value(serial, &payload[body..])?;
        body += len;
        values.push(value);
    }
    Ok(values)
}

/// The value of serial type `serial` at the start of `body`, and its length.
fn value(serial: u64, body: &[u8]) -> Result<(Value, usize)> {
    let int = |len: usize| -> Result<(Value, usize)> {
        let bytes = take(body, 0, len)?;
        let first = i64::from(bytes[0].cast_signed());
        let value = bytes[1..]
            .iter()
            .fold(first, |acc, byte| (acc << 8) | i64::from(*byte));
        Ok((Value::Integer(value), len))
    };
    match serial {
        0 => Ok((Value::Null, 0)),
        1..=4 => int(usize::try_from(serial).unwrap_or(0)),
        5 => int(6),
        6 => int(8),
        7 => {
            let mut word = [0u8; 8];
            word.copy_from_slice(take(body, 0, 8)?);
            Ok((Value::Real(f64::from_be_bytes(word)), 8))
        }
        8 => Ok((Value::Integer(0), 0)),
        9 => Ok((Value::Integer(1), 0)),
        10 | 11 => Err(SqliteError::Malformed("a reserved serial type")),
        _ => {
            let len = usize::try_from((serial - 12) / 2).unwrap_or(usize::MAX);
            let bytes = take(body, 0, len)?;
            if serial % 2 == 0 {
                Ok((Value::Blob(bytes.to_vec()), len))
            } else {
                Ok((
                    Value::Text(String::from_utf8_lossy(bytes).into_owned()),
                    len,
                ))
            }
        }
    }
}

/// The columns' names, lowercased, of the `CREATE TABLE` statement `sql`: the first word of each
/// definition between its outer parentheses that is not a table constraint, unquoted. None when
/// the statement has no parenthesized list.
fn columns(sql: &str) -> Option<Vec<String>> {
    let open = sql.find('(')?;
    let close = sql.rfind(')').filter(|close| *close > open)?;
    let mut columns = Vec::new();
    for part in split_top(&sql[open + 1..close]) {
        let part = part.trim_start();
        let name = match part.chars().next() {
            Some(quote @ ('"' | '`' | '[')) => part[1..].split(closing(quote)).next(),
            _ => part
                .split_whitespace()
                .next()
                .filter(|word| !CONSTRAINTS.contains(&word.to_lowercase().as_str())),
        };
        if let Some(name) = name.filter(|name| !name.is_empty()) {
            columns.push(name.to_lowercase());
        }
    }
    Some(columns)
}

/// The first words of a table constraint, which names no column.
const CONSTRAINTS: [&str; 5] = ["constraint", "primary", "unique", "check", "foreign"];

/// The character that closes a quote opened by `open`.
fn closing(open: char) -> char {
    if open == '[' { ']' } else { open }
}

/// `body` split at the commas outside parentheses and quotes.
fn split_top(body: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (at, c) in body.char_indices() {
        match (quote, c) {
            (Some(close), _) if c == close => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'' | '`' | '[') => quote = Some(closing(c)),
            (None, '(') => depth += 1,
            (None, ')') => depth = depth.saturating_sub(1),
            (None, ',') if depth == 0 => {
                parts.push(&body[start..at]);
                start = at + 1;
            }
            (None, _) => {}
        }
    }
    parts.push(&body[start..]);
    parts
}

/// `read` over `db`, its pages counted again first, counted only when no write was in progress
/// before or after it and the file's change counter did not move during it; tried up to
/// [`ATTEMPTS`] times. A read that failed is judged the same way: one a write crossed (a torn
/// walk can look malformed) is tried again, and one no write crossed is the file's own error.
///
/// # Errors
/// [`SqliteError::Busy`] when no attempt was steady; the error of a read no write crossed; the
/// journal's or header's error at once.
pub fn steady<T>(db: &Database, read: impl Fn(&Database) -> Result<T>) -> Result<T> {
    for _ in 0..ATTEMPTS {
        if db.journal_hot()? {
            continue;
        }
        let before = db.change_counter()?;
        db.recount()?;
        let found = read(db);
        let crossed = db.change_counter()? != before || db.journal_hot()?;
        if !crossed {
            return found;
        }
    }
    Err(SqliteError::Busy)
}

/// Whether a file is at `path`: false when it is not there; the error of a look that failed.
///
/// # Errors
/// [`SqliteError::Io`] when the look fails other than for want of the file.
pub fn exists(path: &Path) -> Result<bool> {
    match fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_error(&e)),
    }
}
