//! The daemon's reader of the SQLite file format (`hostkeeper_daemon::sqlite_read`), against
//! databases SQLite itself wrote (through `rusqlite`, in a scratch directory): a b-tree of three
//! levels read whole and after a rowid, every serial type, payloads that spill to overflow pages,
//! the schema's tables and their columns, a column added later; refused as SQLite would not read
//! them as they are: WAL mode, UTF-16, a hot journal, a write during the read. Then files built
//! page by page, each broken in one way, every one an error and none a panic.

use std::cell::Cell;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use hostkeeper_daemon::sqlite_read::{
    ATTEMPTS, Database, MAX_DEPTH, Row, SqliteError, Table, Value, exists, steady,
};
use hostkeeper_testkit::Scratch;
use rusqlite::Connection;

fn scratch(name: &str) -> Scratch {
    Scratch::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name).expect("a scratch directory")
}

fn opened(path: &Path) -> Database {
    match Database::open(path) {
        Ok(db) => db,
        Err(e) => panic!("{} does not open: {e}", path.display()),
    }
}

fn refused(path: &Path) -> SqliteError {
    match Database::open(path) {
        Ok(_) => panic!("{} opened", path.display()),
        Err(e) => e,
    }
}

/// A database of `sql`, written by SQLite at `dir/<name>`.
fn written(dir: &Scratch, name: &str, sql: &str) -> PathBuf {
    let path = dir.path().join(name);
    let conn = Connection::open(&path).expect("open with SQLite");
    conn.execute_batch(sql).expect("the SQL runs");
    path
}

fn table(db: &Database, name: &str) -> Table {
    db.tables()
        .expect("the schema")
        .remove(name)
        .unwrap_or_else(|| panic!("no table {name}"))
}

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

/// Proves every row of a table of three levels of pages reads as SQLite holds it, in rowid order,
/// the rowid alias `id` as null in the record and the rowid its key; the rows after a rowid are
/// the ones after it; a limit stops the walk; the largest rowid is SQLite's `max(id)`.
///
/// Catches: a child of an interior page skipped or read twice, the right-most child left out, the
/// rows after a rowid starting one early or late, a limit not kept.
#[test]
fn a_deep_table_reads_as_sqlite_holds_it() {
    let dir = scratch("sqlite-deep");
    let mut sql = String::from(
        "PRAGMA page_size = 512; CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT, b INTEGER); \
         BEGIN;",
    );
    for i in 1..=3000 {
        sql.push_str(&format!(
            "INSERT INTO t (a, b) VALUES ('{}', {i});",
            "x".repeat(40 + i % 50)
        ));
    }
    sql.push_str("COMMIT;");
    let path = written(&dir, "deep.db", &sql);
    let db = opened(&path);
    let t = table(&db, "t");
    assert_eq!(t.columns, ["id", "a", "b"]);
    let expect = |i: i64| Row {
        rowid: i,
        values: vec![
            Value::Null,
            text(&"x".repeat(40 + usize::try_from(i).expect("small") % 50)),
            Value::Integer(i),
        ],
    };
    let all = db.rows_after(t.root, 0, usize::MAX).expect("every row");
    assert_eq!(all, (1..=3000).map(expect).collect::<Vec<_>>());
    assert_eq!(
        db.rows_after(t.root, 1500, 10).expect("ten rows"),
        (1501..=1510).map(expect).collect::<Vec<_>>()
    );
    for limit in [1, 5, 36, 37, 38, 700] {
        let rows = db.rows_after(t.root, 0, limit).expect("the first rows");
        assert_eq!(rows, (1..=limit as i64).map(expect).collect::<Vec<_>>());
    }
    assert_eq!(db.rows_after(t.root, 2995, 10).expect("the last").len(), 5);
    assert!(db.rows_after(t.root, 3000, 10).expect("none").is_empty());
    assert_eq!(db.max_rowid(t.root).expect("the largest"), Some(3000));
}

/// Proves each serial type SQLite writes reads back as its value: null, 0 and 1 (no bytes),
/// integers of 1, 2, 3, 4, 6 and 8 bytes, a negative one, a real, a text and a blob; a negative
/// rowid (a 9-byte varint) reads in rowid order before the others.
///
/// Catches: an integer read unsigned, of the wrong width, or a serial type taken for another.
#[test]
fn every_serial_type_reads_back() {
    let dir = scratch("sqlite-types");
    let path = written(
        &dir,
        "types.db",
        "CREATE TABLE v (a, b, c, d, e, f, g, h, i, j, k, l, m); \
         INSERT INTO v VALUES (0, 1, 100, 1000, 100000, 10000000, 1099511627776, \
         -9223372036854775808, 1.5, 'text', X'0102', NULL, -1); \
         INSERT INTO v (rowid, a) VALUES (-1, 'negative');",
    );
    let db = opened(&path);
    let v = table(&db, "v");
    let rows = db.rows_after(v.root, i64::MIN, 10).expect("both rows");
    assert_eq!((rows[0].rowid, rows[0].get(0)), (-1, &text("negative")));
    assert!((1..13).all(|i| rows[0].get(i) == &Value::Null));
    assert_eq!(
        rows[1..],
        [Row {
            rowid: 1,
            values: vec![
                Value::Integer(0),
                Value::Integer(1),
                Value::Integer(100),
                Value::Integer(1000),
                Value::Integer(100_000),
                Value::Integer(10_000_000),
                Value::Integer(1_099_511_627_776),
                Value::Integer(i64::MIN),
                Value::Real(1.5),
                text("text"),
                Value::Blob(vec![1, 2]),
                Value::Null,
                Value::Integer(-1),
            ],
        },]
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get(20), &Value::Null);
    assert_eq!(db.max_rowid(v.root).expect("the largest"), Some(1));
}

/// Proves a text that fits its page, one whose local part is the most a cell keeps, and ones that
/// spill to one, two or more overflow pages, by either of SQLite's rules of the local part, each
/// reads back whole.
///
/// Catches: the local part of a spilled payload sized by the wrong rule, an overflow page's link
/// or its first 4 bytes read as data.
#[test]
fn payloads_spilled_to_overflow_pages_read_whole() {
    let dir = scratch("sqlite-overflow");
    let lengths: Vec<usize> = [10, 4050, 4058, 4059, 4060, 4086, 4087, 5000, 9000, 20_000]
        .into_iter()
        .chain(4080..4096)
        .collect();
    let mut sql = String::from("CREATE TABLE o (x TEXT); BEGIN;");
    for len in &lengths {
        sql.push_str(&format!("INSERT INTO o VALUES ('{}');", "y".repeat(*len)));
    }
    sql.push_str("COMMIT;");
    let path = written(&dir, "overflow.db", &sql);
    let db = opened(&path);
    let o = table(&db, "o");
    let rows = db.rows_after(o.root, 0, usize::MAX).expect("every row");
    let read: Vec<Value> = rows.into_iter().map(|r| r.values[0].clone()).collect();
    let expect: Vec<Value> = lengths.iter().map(|len| text(&"y".repeat(*len))).collect();
    assert_eq!(read, expect);
}

/// Proves the schema lists its tables alone (not an index or a view), by their name lowercased,
/// each with its columns: quoted names unquoted, a type with a comma in its parentheses, a
/// default with a comma in its quotes, and the table constraints left out; a column it lacks is
/// none.
///
/// Catches: an index taken for a table, a constraint taken for a column, a list split inside
/// parentheses or quotes.
#[test]
fn the_schema_lists_tables_and_their_columns() {
    let dir = scratch("sqlite-schema");
    let path = written(
        &dir,
        "schema.db",
        "CREATE TABLE \"q t\" (\"a b\" TEXT, `c` INT, [d e] REAL, f NUMERIC(10, 2), \
         g TEXT DEFAULT 'x,y', PRIMARY KEY (f), UNIQUE (g), CHECK (f > 0), \
         CONSTRAINT k FOREIGN KEY (c) REFERENCES MixedCase (a)); \
         CREATE TABLE MixedCase (A INT); \
         CREATE INDEX i ON MixedCase (A); \
         CREATE VIEW w AS SELECT * FROM MixedCase;",
    );
    let db = opened(&path);
    let tables = db.tables().expect("the schema");
    assert_eq!(
        tables.keys().cloned().collect::<Vec<_>>(),
        ["mixedcase", "q t"]
    );
    let q = &tables["q t"];
    assert_eq!(q.columns, ["a b", "c", "d e", "f", "g"]);
    assert_eq!((q.column("g"), q.column("zz")), (Some(4), None));
    assert_eq!(tables["mixedcase"].columns, ["a"]);
    assert_eq!(db.max_rowid(tables["mixedcase"].root).expect("empty"), None);
}

/// Proves a row written before a column was added reads with fewer values, the added one null.
///
/// Catches: a short record taken for a malformed one, or its values shifted.
#[test]
fn a_row_before_an_added_column_lacks_it() {
    let dir = scratch("sqlite-alter");
    let path = written(
        &dir,
        "alter.db",
        "CREATE TABLE t (a TEXT); INSERT INTO t VALUES ('old'); \
         ALTER TABLE t ADD COLUMN b TEXT; INSERT INTO t VALUES ('new', 'b');",
    );
    let db = opened(&path);
    let t = table(&db, "t");
    assert_eq!(t.columns, ["a", "b"]);
    let rows = db.rows_after(t.root, 0, 10).expect("both");
    assert_eq!(rows[0].values, [text("old")]);
    assert_eq!(rows[0].get(1), &Value::Null);
    assert_eq!(rows[1].values, [text("new"), text("b")]);
}

/// Proves a database in WAL mode and one whose text is UTF-16 are refused, by name.
///
/// Catches: a WAL database read from its file alone, which lacks its newest pages, or UTF-16
/// text read as UTF-8.
#[test]
fn wal_mode_and_utf16_are_refused() {
    let dir = scratch("sqlite-modes");
    let wal = written(
        &dir,
        "wal.db",
        "PRAGMA journal_mode = WAL; CREATE TABLE t (a); INSERT INTO t VALUES (1);",
    );
    assert_eq!(refused(&wal), SqliteError::Wal);
    let utf16 = written(
        &dir,
        "utf16.db",
        "PRAGMA encoding = 'UTF-16le'; CREATE TABLE t (a); INSERT INTO t VALUES ('x');",
    );
    assert_eq!(refused(&utf16), SqliteError::Encoding);
    assert_eq!(
        SqliteError::Encoding.to_string(),
        "its text encoding is not UTF-8"
    );
}

fn journal(path: &Path) -> PathBuf {
    let mut journal = path.as_os_str().to_owned();
    journal.push("-journal");
    PathBuf::from(journal)
}

/// A rollback journal's header as SQLite writes it once it holds a transaction.
const HOT: [u8; 12] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7, 0, 0, 0, 1];

/// Proves a read is counted only when no write was in progress around it and the change counter
/// did not move: a hot journal before every attempt is refused `Busy` with no read made; a write
/// during the first read is read again, and counted the second time; a hot journal found after the
/// read is refused; a write during every read is refused after the attempts; a journal SQLite
/// left with its header zeroed, or shorter than its magic, is no write.
///
/// Catches: a read taken while rasdaemon wrote, a torn read counted, or a journal left behind
/// read as a write forever.
#[test]
fn a_read_counts_only_when_no_write_crossed_it() {
    let dir = scratch("sqlite-steady");
    let path = written(
        &dir,
        "steady.db",
        "CREATE TABLE t (a); INSERT INTO t VALUES (1);",
    );
    let db = opened(&path);
    let root = table(&db, "t").root;
    let reads = Cell::new(0);
    let count = |db: &Database| {
        reads.set(reads.get() + 1);
        db.rows_after(root, 0, 100).map(|rows| rows.len())
    };
    assert_eq!(steady(&db, count), Ok(1));
    assert_eq!(reads.replace(0), 1);

    fs::write(journal(&path), HOT).expect("a hot journal");
    assert_eq!(steady(&db, count), Err(SqliteError::Busy));
    assert_eq!(reads.replace(0), 0);
    fs::write(journal(&path), [0u8; 12]).expect("a zeroed journal");
    assert_eq!(steady(&db, count), Ok(1));
    fs::write(journal(&path), [0xd9, 0xd5, 0x05]).expect("a short journal");
    assert_eq!(steady(&db, count), Ok(1));
    fs::remove_file(journal(&path)).expect("no journal");
    reads.set(0);

    let writer = Connection::open(&path).expect("a writer");
    let once = |db: &Database| {
        reads.set(reads.get() + 1);
        if reads.get() == 1 {
            writer
                .execute("INSERT INTO t VALUES (2)", [])
                .expect("a write");
        }
        db.rows_after(root, 0, 100).map(|rows| rows.len())
    };
    assert_eq!(steady(&db, once), Ok(2));
    assert_eq!(reads.replace(0), 2);

    let always = |db: &Database| {
        reads.set(reads.get() + 1);
        writer
            .execute("INSERT INTO t VALUES (3)", [])
            .expect("a write");
        db.rows_after(root, 0, 100).map(|rows| rows.len())
    };
    assert_eq!(steady(&db, always), Err(SqliteError::Busy));
    assert_eq!(reads.replace(0), ATTEMPTS);

    let broken = |db: &Database| {
        reads.set(reads.get() + 1);
        if reads.get() == 1 {
            writer
                .execute("INSERT INTO t VALUES (4)", [])
                .expect("a write");
            return db.rows_after(99, 0, 1).map(|rows| rows.len());
        }
        db.rows_after(root, 0, 100).map(|rows| rows.len())
    };
    assert_eq!(steady(&db, broken), Ok(6));
    assert_eq!(reads.replace(0), 2);
    let own = |db: &Database| db.rows_after(99, 0, 1);
    assert_eq!(
        steady(&db, own),
        Err(malformed("a page number past the file"))
    );

    let mut grow = String::from("BEGIN;");
    for _ in 0..200 {
        grow.push_str(&format!("INSERT INTO t VALUES ('{}');", "g".repeat(300)));
    }
    grow.push_str("COMMIT;");
    writer.execute_batch(&grow).expect("the file grows");
    assert_eq!(
        steady(&db, |db| db
            .rows_after(root, 0, usize::MAX)
            .map(|rows| rows.len())),
        Ok(206)
    );
    drop(writer);

    let hot_after = |db: &Database| {
        reads.set(reads.get() + 1);
        fs::write(journal(&path), HOT).expect("a hot journal");
        db.rows_after(root, 0, 100).map(|rows| rows.len())
    };
    assert_eq!(steady(&db, hot_after), Err(SqliteError::Busy));
    assert_eq!(reads.replace(0), 1);
    assert_eq!(
        SqliteError::Busy.to_string(),
        "the database was being written"
    );
}

/// Proves a journal that cannot be looked at (a directory, a link that loops) is an error of
/// its own, not a write and not a read; and that a file is there, is not, or cannot be looked
/// for.
///
/// Catches: a journal that cannot be read taken for no write.
#[test]
fn a_journal_or_file_that_cannot_be_looked_at_is_an_error() {
    let dir = scratch("sqlite-journal");
    let path = written(&dir, "j.db", "CREATE TABLE t (a);");
    let db = opened(&path);
    let rows = |db: &Database| db.rows_after(2, 0, 1);
    fs::create_dir(journal(&path)).expect("a directory journal");
    assert!(matches!(steady(&db, rows), Err(SqliteError::Io(_))));
    fs::remove_dir(journal(&path)).expect("removed");
    symlink(journal(&path), journal(&path)).expect("a looping link");
    assert!(matches!(steady(&db, rows), Err(SqliteError::Io(_))));

    assert_eq!(exists(&path), Ok(true));
    assert_eq!(exists(&dir.path().join("none.db")), Ok(false));
    assert!(matches!(
        exists(&path.join("child")),
        Err(SqliteError::Io(_))
    ));
}

// Files built page by page, with 512-byte pages: each test breaks one thing.

const PAGE: usize = 512;

fn varint(v: u64) -> Vec<u8> {
    if v > 0x00ff_ffff_ffff_ffff {
        let mut out = vec![0u8; 9];
        out[8] = (v & 0xff) as u8;
        let mut rest = v >> 8;
        for byte in out[..8].iter_mut().rev() {
            *byte = 0x80 | (rest & 0x7f) as u8;
            rest >>= 7;
        }
        return out;
    }
    let mut groups = Vec::new();
    let mut rest = v;
    loop {
        groups.push((rest & 0x7f) as u8);
        rest >>= 7;
        if rest == 0 {
            break;
        }
    }
    groups.reverse();
    let last = groups.len() - 1;
    for byte in &mut groups[..last] {
        *byte |= 0x80;
    }
    groups
}

/// A record value as the builder writes it: an 8-byte integer, a text, or a serial type and body
/// given whole.
enum V {
    I(i64),
    S(&'static str),
    Raw(u64, Vec<u8>),
}

fn record(values: &[V]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut body = Vec::new();
    for value in values {
        match value {
            V::I(n) => {
                types.extend(varint(6));
                body.extend(n.to_be_bytes());
            }
            V::S(s) => {
                types.extend(varint(13 + 2 * s.len() as u64));
                body.extend(s.as_bytes());
            }
            V::Raw(serial, bytes) => {
                types.extend(varint(*serial));
                body.extend(bytes);
            }
        }
    }
    [varint(types.len() as u64 + 1), types, body].concat()
}

fn cell(rowid: i64, payload: &[u8]) -> Vec<u8> {
    [
        varint(payload.len() as u64),
        varint(rowid as u64),
        payload.to_vec(),
    ]
    .concat()
}

/// A page of type `kind` whose b-tree header is at `at` (100 on page 1), holding `cells` from its
/// end, its cell pointers after a header of `header_len` bytes, `right` its right-most child.
fn page(kind: u8, at: usize, cells: &[Vec<u8>], right: u32) -> Vec<u8> {
    let mut page = vec![0u8; PAGE];
    page[at] = kind;
    page[at + 3..at + 5].copy_from_slice(&(cells.len() as u16).to_be_bytes());
    let header_len = if kind == 0x05 { 12 } else { 8 };
    if kind == 0x05 {
        page[at + 8..at + 12].copy_from_slice(&right.to_be_bytes());
    }
    let mut end = PAGE;
    for (i, cell) in cells.iter().enumerate() {
        end -= cell.len();
        page[end..end + cell.len()].copy_from_slice(cell);
        let pointer = at + header_len + 2 * i;
        page[pointer..pointer + 2].copy_from_slice(&(end as u16).to_be_bytes());
    }
    page[at + 5..at + 7].copy_from_slice(&(end as u16).to_be_bytes());
    page
}

fn leaf(at: usize, cells: &[Vec<u8>]) -> Vec<u8> {
    page(0x0d, at, cells, 0)
}

fn interior(cells: &[(u32, i64)], right: u32) -> Vec<u8> {
    let cells: Vec<Vec<u8>> = cells
        .iter()
        .map(|(child, key)| [child.to_be_bytes().to_vec(), varint(*key as u64)].concat())
        .collect();
    page(0x05, 0, &cells, right)
}

/// The 100-byte header of a file of `pages` pages of [`PAGE`] bytes.
fn header(pages: u32) -> [u8; 100] {
    let mut h = [0u8; 100];
    h[..16].copy_from_slice(b"SQLite format 3\0");
    h[16..18].copy_from_slice(&(PAGE as u16).to_be_bytes());
    (h[18], h[19], h[21], h[22], h[23]) = (1, 1, 64, 32, 32);
    h[24..28].copy_from_slice(&1u32.to_be_bytes());
    h[28..32].copy_from_slice(&pages.to_be_bytes());
    h[44..48].copy_from_slice(&4u32.to_be_bytes());
    h[56..60].copy_from_slice(&1u32.to_be_bytes());
    h[92..96].copy_from_slice(&1u32.to_be_bytes());
    h
}

/// A file of `pages` (page 1 first) at `dir/<name>`, page 1's header written over its first 100
/// bytes and then `patch` applied to it.
fn built(dir: &Scratch, name: &str, mut pages: Vec<Vec<u8>>, patch: impl Fn(&mut [u8])) -> PathBuf {
    let count = u32::try_from(pages.len()).expect("few pages");
    pages[0][..100].copy_from_slice(&header(count));
    let mut bytes = pages.concat();
    patch(&mut bytes);
    let path = dir.path().join(name);
    fs::write(&path, bytes).expect("write the file");
    path
}

/// A file whose page 1 is an empty schema and whose page 2 is `second`.
fn with_page(dir: &Scratch, name: &str, second: Vec<u8>) -> Database {
    opened(&built(dir, name, vec![leaf(100, &[]), second], |_| {}))
}

fn malformed(what: &'static str) -> SqliteError {
    SqliteError::Malformed(what)
}

/// Proves a header that is not SQLite's, or breaks it, is refused by what is wrong: too short, no
/// magic, a page size that is none, a file format version that is neither, a WAL version, too
/// much reserved space, an encoding past UTF-16; that a page size of 1 is 65536; and that a
/// directory or no file cannot be read.
///
/// Catches: a header field taken on trust, or an error that is a panic.
#[test]
fn a_broken_header_is_refused_by_what_is_wrong() {
    let dir = scratch("sqlite-header");
    let short = dir.path().join("short.db");
    fs::write(&short, [0u8; 50]).expect("write");
    assert_eq!(refused(&short), SqliteError::NotSqlite);
    let magic = dir.path().join("magic.db");
    fs::write(&magic, [0u8; 100]).expect("write");
    assert_eq!(refused(&magic), SqliteError::NotSqlite);
    assert_eq!(SqliteError::NotSqlite.to_string(), "not a SQLite database");
    let cases: [(&str, fn(&mut [u8]), SqliteError); 5] = [
        (
            "size.db",
            |b| b[16..18].copy_from_slice(&1000u16.to_be_bytes()),
            malformed("its page size"),
        ),
        (
            "version.db",
            |b| (b[18], b[19]) = (3, 3),
            malformed("its file format version"),
        ),
        ("wal-read.db", |b| b[19] = 2, SqliteError::Wal),
        (
            "reserved.db",
            |b| b[20] = 100,
            malformed("its reserved space"),
        ),
        ("encoding.db", |b| b[59] = 3, SqliteError::Encoding),
    ];
    for (name, patch, error) in cases {
        let path = built(&dir, name, vec![leaf(100, &[])], patch);
        assert_eq!(refused(&path), error, "{name}");
    }
    let mut big = leaf(100, &[]);
    big.resize(65536, 0);
    let big = built(&dir, "big.db", vec![big], |b| {
        b[16..18].copy_from_slice(&1u16.to_be_bytes());
    });
    assert!(opened(&big).tables().expect("an empty schema").is_empty());
    assert!(matches!(refused(dir.path()), SqliteError::Io(_)));
    assert!(matches!(
        refused(&dir.path().join("none.db")),
        SqliteError::Io(_)
    ));
    assert_eq!(
        malformed("its page size").to_string(),
        "malformed: its page size"
    );
}

/// Proves a schema row of a table that breaks its shape is refused: a name that is no text, a
/// root page out of range, a statement with no column list; and a root page past the file, or 0,
/// is refused when it is read.
///
/// Catches: a broken schema row passed over silently, or a root page read from outside the file.
#[test]
fn a_broken_schema_row_is_refused() {
    let dir = scratch("sqlite-schema-broken");
    let schema = |name: &str, values: Vec<V>| {
        let path = built(
            &dir,
            name,
            vec![leaf(100, &[cell(1, &record(&values))])],
            |_| {},
        );
        opened(&path).tables()
    };
    use V::{I, S};
    assert_eq!(
        schema(
            "name.db",
            vec![S("table"), I(5), S("t"), I(2), S("CREATE TABLE t (a)")]
        ),
        Err(malformed("a table's row of the schema"))
    );
    assert_eq!(
        schema(
            "root.db",
            vec![S("table"), S("t"), S("t"), I(-1), S("CREATE TABLE t (a)")]
        ),
        Err(malformed("a root page"))
    );
    assert_eq!(
        schema(
            "sql.db",
            vec![S("table"), S("t"), S("t"), I(2), S("CREATE TABLE t")]
        ),
        Err(malformed("a table's CREATE statement"))
    );
    let far = schema(
        "far.db",
        vec![S("table"), S("t"), S("t"), I(9), S("CREATE TABLE t (a)")],
    )
    .expect("the schema");
    assert_eq!(far["t"].root, 9);
    let db = opened(&dir.path().join("far.db"));
    assert_eq!(
        db.rows_after(9, 0, 1),
        Err(malformed("a page number past the file"))
    );
    assert_eq!(
        db.rows_after(0, 0, 1),
        Err(malformed("a page number past the file"))
    );
}

/// Proves pages and cells that break the format are refused by what is wrong: an index page
/// where a table page belongs, a cell pointer past the usable page, pointers past the page, a
/// varint that runs off the page, a record header longer than its payload, a reserved serial
/// type, a text longer than the record, a payload past its bound, an overflow chain that ends
/// early, one that loops, an interior page pointing at page 0.
///
/// Catches: any of these read as data, or a panic on an offset past a page.
#[test]
fn broken_pages_and_cells_are_refused() {
    let dir = scratch("sqlite-cells");
    let index_page = with_page(&dir, "index.db", page(0x0a, 0, &[], 0));
    assert_eq!(
        index_page.rows_after(2, 0, 1),
        Err(malformed("a page that is not of a table b-tree"))
    );
    assert_eq!(
        index_page.max_rowid(2),
        Err(malformed("a page that is not of a table b-tree"))
    );

    let mut pointer = leaf(0, &[cell(1, &record(&[V::I(1)]))]);
    pointer[8..10].copy_from_slice(&600u16.to_be_bytes());
    let pointer = with_page(&dir, "pointer.db", pointer);
    assert_eq!(
        pointer.rows_after(2, 0, 1),
        Err(malformed("a cell past its page"))
    );

    let mut many = leaf(0, &[]);
    many[3..5].copy_from_slice(&300u16.to_be_bytes());
    let many = with_page(&dir, "many.db", many);
    assert_eq!(many.max_rowid(2), Err(malformed("a field past its page")));

    let mut runs_off = leaf(0, &[vec![0x01]]);
    runs_off[PAGE - 2..].copy_from_slice(&[0xff, 0xff]);
    runs_off[8..10].copy_from_slice(&((PAGE - 2) as u16).to_be_bytes());
    let runs_off = with_page(&dir, "varint.db", runs_off);
    assert_eq!(
        runs_off.rows_after(2, 0, 1),
        Err(malformed("a varint past its page"))
    );

    let cases: [(&str, Vec<u8>, SqliteError); 4] = [
        ("header.db", vec![0x7f], malformed("a record's header")),
        ("zero-header.db", vec![0x00], malformed("a record's header")),
        (
            "serial.db",
            vec![0x02, 10],
            malformed("a reserved serial type"),
        ),
        (
            "text.db",
            vec![0x02, 13 + 2 * 50, b'a', b'b'],
            malformed("a field past its page"),
        ),
    ];
    for (name, payload, error) in cases {
        let db = with_page(&dir, name, leaf(0, &[cell(1, &payload)]));
        assert_eq!(db.rows_after(2, 0, 1), Err(error), "{name}");
    }

    let huge = [varint(2_000_000), varint(1), vec![0u8; 8]].concat();
    let huge = with_page(&dir, "huge.db", leaf(0, &[huge]));
    assert_eq!(
        huge.rows_after(2, 0, 1),
        Err(malformed("a record past its bound"))
    );

    // 600 bytes on a 512-byte page: 92 local bytes, then the first overflow page, here none.
    let ends = [
        varint(600),
        varint(1),
        vec![0u8; 92],
        0u32.to_be_bytes().to_vec(),
    ]
    .concat();
    let ends = with_page(&dir, "ends.db", leaf(0, &[ends]));
    assert_eq!(
        ends.rows_after(2, 0, 1),
        Err(malformed("an overflow chain"))
    );

    // 900000 bytes: 332 local bytes, then page 3, which links to itself.
    let loops = [
        varint(900_000),
        varint(1),
        vec![0u8; 332],
        3u32.to_be_bytes().to_vec(),
    ]
    .concat();
    let mut overflow = vec![0u8; PAGE];
    overflow[..4].copy_from_slice(&3u32.to_be_bytes());
    let path = built(
        &dir,
        "loops.db",
        vec![leaf(100, &[]), leaf(0, &[loops]), overflow],
        |_| {},
    );
    assert_eq!(
        opened(&path).rows_after(2, 0, 1),
        Err(malformed("an overflow chain"))
    );

    let zero = with_page(&dir, "zero.db", interior(&[(0, 5)], 0));
    assert_eq!(
        zero.rows_after(2, 0, 1),
        Err(malformed("a page number past the file"))
    );
}

/// Proves a b-tree that loops ends as malformed when its walk visits more pages than the file
/// holds, one deeper than its bound when it is deeper than [`MAX_DEPTH`], and the largest rowid's
/// walk down the right-most children stops at the same depth.
///
/// Catches: a malformed file that walks forever, or overflows the stack.
#[test]
fn a_b_tree_that_loops_or_is_too_deep_ends() {
    let dir = scratch("sqlite-loops");
    let looped = with_page(&dir, "self.db", interior(&[], 2));
    assert_eq!(
        looped.rows_after(2, 0, 1),
        Err(malformed("a b-tree that loops"))
    );
    assert_eq!(
        looped.max_rowid(2),
        Err(malformed("a b-tree deeper than its bound"))
    );
    let last = u32::try_from(MAX_DEPTH).expect("small") + 9;
    let mut pages = vec![leaf(100, &[])];
    pages.extend((2..last).map(|n| interior(&[], n + 1)));
    pages.push(leaf(0, &[cell(7, &record(&[V::I(7)]))]));
    let deep = opened(&built(&dir, "deep.db", pages, |_| {}));
    assert_eq!(
        deep.rows_after(2, 0, 1),
        Err(malformed("a b-tree deeper than its bound"))
    );
    assert_eq!(
        deep.max_rowid(2),
        Err(malformed("a b-tree deeper than its bound"))
    );
    assert_eq!(deep.max_rowid(last - 2), Ok(Some(7)));
    assert_eq!(
        deep.rows_after(last - 2, 0, 1),
        Ok(vec![Row {
            rowid: 7,
            values: vec![Value::Integer(7)],
        }])
    );
    let null = with_page(
        &dir,
        "null.db",
        leaf(0, &[cell(1, &record(&[V::Raw(0, vec![])]))]),
    );
    assert_eq!(
        null.rows_after(2, 0, 1),
        Ok(vec![Row {
            rowid: 1,
            values: vec![Value::Null],
        }])
    );
}
