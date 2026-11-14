//! The compose deployment of `docs/deploy/coordinator/` run against a real server: the committed
//! SQL files and the coordinator's committed command, each read from the checkout. The server is
//! the one `HOSTKEEPER_TEST_POSTGRES_URL` names; a test that needs it fails, saying how to start
//! one, when it is not set, and never skips.
//!
//! The SQL files are run by psql in the deployment. Here they are run statement by statement
//! through the client, with the one psql meta-command each file uses done as psql does it (see
//! [`Psql`]): `\ir` includes a file beside the current one, and `\set name `cat <file>`` sets a
//! variable to the file's text with its trailing newline dropped, which `:'name'` then quotes as
//! a literal. A file that uses any other meta-command fails the test, so the files cannot drift
//! into something this test does not run.
//!
//! What is substituted, and nothing else: the paths inside the containers for scratch files, and,
//! for the role and the database `init.sql` makes, names of the test's own, since a role is the
//! whole server's and tests share the server; the files' own names are checked first.

mod support;

#[path = "../../xtask/tests/workflows/yaml.rs"]
#[expect(
    dead_code,
    reason = "the workflow lint's YAML reader, of which this test needs only the loader and the scalars"
)]
mod yaml;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_coordinator::config::{Parts, StartError};
use postgres::{Client, SimpleQueryMessage};
use support::OPERATOR;
use support::database::{COORDINATOR_LOCK, Database, client, server_url, with_catalog_lock};
use support::serve::{Files, build_given, flags_of, refusal};
use yaml::{Node, Value};

/// The deployment's directory in this checkout.
fn deploy(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/deploy/coordinator")
        .join(name)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Where the postgres container sees the password secret, which `password.sql` reads.
const PASSWORD_SECRET: &str = "/run/secrets/hostkeeper-db-password";

/// psql's handling of the committed SQL files, as far as they use it.
struct Psql {
    /// The scratch file read for each path inside the container.
    files: BTreeMap<&'static str, PathBuf>,
    /// Each identifier spelled otherwise in the statements (whole words only).
    renames: Vec<(&'static str, String)>,
    vars: BTreeMap<String, String>,
}

impl Psql {
    fn new() -> Psql {
        Psql {
            files: BTreeMap::new(),
            renames: Vec::new(),
            vars: BTreeMap::new(),
        }
    }

    /// Runs the file at `path` on `session`, as `psql --set=ON_ERROR_STOP=1 --file` does, and
    /// answers the first column of every row the statements returned, in order.
    fn run(&mut self, session: &mut Client, path: &Path) -> Vec<String> {
        let mut rows = Vec::new();
        let mut statement = String::new();
        for line in read(path).lines() {
            let trimmed = line.trim();
            if let Some(meta) = trimmed.strip_prefix('\\') {
                assert!(
                    statement.is_empty(),
                    "{}: a meta-command inside a statement",
                    path.display()
                );
                rows.extend(self.meta(session, path, meta));
            } else if !trimmed.starts_with("--") && !trimmed.is_empty() {
                statement.push_str(line);
                statement.push('\n');
                if trimmed.ends_with(';') {
                    let sql = self.substitute(&std::mem::take(&mut statement));
                    for message in session
                        .simple_query(&sql)
                        .unwrap_or_else(|e| panic!("{}: {sql}: {e}", path.display()))
                    {
                        if let SimpleQueryMessage::Row(row) = message {
                            rows.push(row.get(0).unwrap_or_default().to_owned());
                        }
                    }
                }
            }
        }
        assert!(
            statement.is_empty(),
            "{}: a statement without its `;`",
            path.display()
        );
        rows
    }

    fn meta(&mut self, session: &mut Client, path: &Path, meta: &str) -> Vec<String> {
        if let Some(file) = meta.strip_prefix("ir ") {
            let dir = path.parent().expect("the file's directory");
            return self.run(session, &dir.join(file.trim()));
        }
        let set = meta
            .strip_prefix("set ")
            .and_then(|rest| rest.split_once(' '));
        let Some((name, value)) = set else {
            panic!(
                "{}: psql's \\{meta} is not one this test runs",
                path.display()
            )
        };
        let file = value
            .strip_prefix("`cat ")
            .and_then(|v| v.strip_suffix('`'))
            .unwrap_or_else(|| panic!("{}: \\set {name} reads no file", path.display()));
        let scratch = self.files.get(file).unwrap_or_else(|| {
            panic!(
                "{}: reads {file}, which the deployment does not mount",
                path.display()
            )
        });
        let text = read(scratch);
        let text = text.strip_suffix('\n').unwrap_or(&text).to_owned();
        self.vars.insert(name.to_owned(), text);
        Vec::new()
    }

    /// `sql` with the renames made and each `:'name'` replaced by its quoted value.
    fn substitute(&self, sql: &str) -> String {
        let mut out = sql.to_owned();
        for (from, to) in &self.renames {
            out = rename_word(&out, from, to);
        }
        for (name, value) in &self.vars {
            let literal = format!("'{}'", value.replace('\'', "''"));
            out = out.replace(&format!(":'{name}'"), &literal);
        }
        out
    }
}

/// `text` with every whole word `from` spelled `to`.
fn rename_word(text: &str, from: &str, to: &str) -> String {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let mut out = String::new();
    let mut last = 0;
    for (at, _) in text.match_indices(from) {
        let before = text[..at].chars().last();
        let after = text[at + from.len()..].chars().next();
        if !word(before) && !word(after) {
            out.push_str(&text[last..at]);
            out.push_str(to);
            last = at + from.len();
        }
    }
    out.push_str(&text[last..]);
    out
}

/// The entry `key` of the mapping `map`.
fn entry<'a>(map: &'a Node, key: &str) -> &'a Node {
    let Value::Map(entries) = &map.value else {
        panic!("{key}: not under a mapping");
    };
    entries
        .iter()
        .find(|(k, _)| k.as_str() == Some(key))
        .map(|(_, v)| v)
        .unwrap_or_else(|| panic!("no {key}"))
}

/// The scalars of the sequence `node`.
fn strings(node: &Node) -> Vec<String> {
    let Value::Seq(items) = &node.value else {
        panic!("not a sequence");
    };
    items
        .iter()
        .map(|i| i.as_str().expect("a scalar").to_owned())
        .collect()
}

/// The coordinator's committed command in `compose.yaml`.
fn compose_command() -> Vec<String> {
    let compose = yaml::load(&read(&deploy("compose.yaml"))).expect("compose.yaml is YAML");
    strings(entry(
        entry(entry(&compose, "services"), "coordinator"),
        "command",
    ))
}

/// The connection string the runbook's `printf` writes into `secrets/db-dsn`, for `password`; the
/// runbook writes it twice (the setup and the password change), and both must say the same.
fn runbook_dsn(password: &str) -> String {
    let runbook = read(&deploy("README.md"));
    let formats: Vec<&str> = runbook
        .split("printf '")
        .skip(1)
        .filter_map(|rest| rest.split('\'').next())
        .collect();
    assert_eq!(formats.len(), 2, "{formats:?}");
    assert_eq!(
        formats[0], formats[1],
        "the two connection strings of the runbook"
    );
    let dsn = formats[0].strip_suffix("\\n").expect("one line");
    assert_eq!(dsn.matches("%s").count(), 1, "{dsn}");
    dsn.replace("%s", password)
}

/// `dsn` pointed at the test server, the database `dbname` and the role `user`: the host and the
/// port swapped for the server's, and the names for the test's own.
fn at_test_server(dsn: &str, dbname: &str, user: &str) -> String {
    let config: postgres::Config = server_url().parse().expect("the server's URL parses");
    let host = match config.get_hosts().first() {
        Some(postgres::config::Host::Tcp(host)) => host.clone(),
        other => panic!("the test server is not named by a TCP host: {other:?}"),
    };
    let port = config.get_ports().first().copied().unwrap_or(5432);
    dsn.split(' ')
        .map(|pair| match pair.split_once('=') {
            Some(("host", _)) => format!("host={host}"),
            Some(("port", _)) => format!("port={port}"),
            Some(("dbname", "hostkeeper")) => format!("dbname={dbname}"),
            Some(("user", "hostkeeper")) => format!("user={user}"),
            Some((key, _)) if ["host", "port", "dbname", "user"].contains(&key) => {
                panic!("the runbook's connection string has {pair}")
            }
            _ => pair.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A role and a database of one test's own, dropped with it.
struct Made {
    role: String,
    database: String,
}

impl Made {
    fn new() -> Made {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut random = [0_u8; 4];
        getrandom::fill(&mut random).expect("random bytes");
        let tag = format!(
            "{}_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            u32::from_le_bytes(random)
        );
        Made {
            role: format!("hk_r_{tag}"),
            database: format!("hk_d_{tag}"),
        }
    }
}

impl Drop for Made {
    fn drop(&mut self) {
        // Two statements, each its own: `DROP DATABASE` does not run inside a transaction, which
        // a batch of several is. What cannot be dropped is the server's to clean; the test is over.
        let database = format!("DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)", self.database);
        let role = format!("DROP ROLE IF EXISTS \"{}\"", self.role);
        let _ = std::panic::catch_unwind(|| {
            with_catalog_lock(|admin| {
                admin
                    .batch_execute(&database)
                    .expect("the database is dropped");
                admin.batch_execute(&role).expect("the role is dropped");
            });
        });
    }
}

/// The arguments of the coordinator's committed command, with each path inside the container
/// replaced by its file in `files`: `/etc/hostkeeper-coordinator/<f>` by `<f>`,
/// `/var/lib/hostkeeper-coordinator/<f>` by `state/<f>`, and the connection-string secret by
/// `dsn`.
fn committed_args(files: &Files, dsn: &Path) -> Vec<OsString> {
    let mut args = vec![OsString::from("hostkeeper-coordinator")];
    for word in compose_command() {
        let mapped = if word == "/run/secrets/hostkeeper-db-dsn" {
            dsn.to_owned()
        } else if let Some(f) = word.strip_prefix("/etc/hostkeeper-coordinator/") {
            files.path(f)
        } else if let Some(f) = word.strip_prefix("/var/lib/hostkeeper-coordinator/") {
            files.path("state").join(f)
        } else {
            assert!(!word.starts_with('/'), "a path no mount holds: {word}");
            PathBuf::from(word)
        };
        args.push(mapped.into_os_string());
    }
    args
}

/// The configuration `etc/` holds, as the compose command names it, in `files`.
fn configuration(files: &Files) {
    std::fs::copy(files.path("ca.pem"), files.path("client-ca.pem")).expect("the client CA");
    files.tokens(&[("operator", OPERATOR)]);
    std::fs::create_dir(files.path("state")).expect("the local volume");
    std::fs::set_permissions(files.path("state"), std::fs::Permissions::from_mode(0o700))
        .expect("owner-only");
}

/// Writes `text` into the secret file `path` (replacing one there), mode 0400.
fn put_secret(path: &Path, text: &str) {
    let _ = std::fs::remove_file(path);
    std::fs::write(path, text).expect("write a secret");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400)).expect("mode 0400");
}

/// The SCRAM verifier the server keeps for `role`.
fn verifier(admin: &mut Client, role: &str) -> String {
    admin
        .query_one(
            "SELECT rolpassword FROM pg_authid WHERE rolname = $1",
            &[&role],
        )
        .expect("the role's password")
        .get::<_, Option<String>>(0)
        .expect("a password is set")
}

/// Proves: on an empty server, the committed `init.sql` (with the `password.sql` it includes)
/// makes a role that logs in, is no superuser and cannot make roles or databases, with a
/// SCRAM-SHA-256 password read from the secret file, and a database it owns; the coordinator's
/// committed command, over the connection string the runbook writes, starts on that database (with
/// `--ca-init`, as the runbook's first start of a new farm, and the legacy service the test's
/// allow-list admits), makes its schema owned by the role,
/// raises the lease's epoch to 1 under the first `--server-name`, takes the operator's change,
/// and holds the lock, so a second start is refused; and `password.sql` run again sets the new
/// password of the secret file.
///
/// Catches: SQL that psql would stop on, a role made a superuser or without a login, a database
/// the coordinator does not own (so its migrations are refused), a password left unset or set to
/// the file's text with its newline, a committed command whose flags do not start a coordinator on
/// the database or name a file no mount holds, or a password change that does not reach the role.
#[test]
fn the_committed_setup_makes_the_database_the_committed_command_starts_on_and_locks() {
    let init = read(&deploy("sql/init.sql"));
    assert!(init.contains("CREATE ROLE hostkeeper LOGIN;"), "{init}");
    assert!(
        init.contains("CREATE DATABASE hostkeeper OWNER hostkeeper;"),
        "{init}"
    );
    let password_sql = read(&deploy("sql/password.sql"));
    assert!(
        password_sql.contains(&format!("`cat {PASSWORD_SECRET}`")),
        "{password_sql}"
    );

    let made = Made::new();
    let files = Files::new("deploy-setup");
    configuration(&files);
    put_secret(&files.path("db-password"), "first-password-0123\n");
    let mut psql = Psql::new();
    psql.files
        .insert(PASSWORD_SECRET, files.path("db-password"));
    psql.renames = vec![("hostkeeper", made.role.clone())];
    // The database is named as the role in init.sql; the test gives it a name of its own.
    let script = read(&deploy("sql/init.sql")).replace(
        "CREATE DATABASE hostkeeper OWNER",
        &format!("CREATE DATABASE {} OWNER", made.database),
    );
    let local_init = files.path("init.sql");
    std::fs::write(&local_init, script).expect("the script");
    std::fs::copy(deploy("sql/password.sql"), files.path("password.sql")).expect("password.sql");
    with_catalog_lock(|admin| psql.run(admin, &local_init));

    let mut admin = client(None);
    let role = admin
        .query_one(
            "SELECT rolcanlogin, rolsuper, rolcreaterole, rolcreatedb FROM pg_roles WHERE rolname = $1",
            &[&made.role],
        )
        .expect("the role");
    let flags: (bool, bool, bool, bool) = (role.get(0), role.get(1), role.get(2), role.get(3));
    assert_eq!(
        flags,
        (true, false, false, false),
        "login, and nothing more"
    );
    let first = verifier(&mut admin, &made.role);
    assert!(first.starts_with("SCRAM-SHA-256$4096:"), "{first}");
    let owner: String = admin
        .query_one(
            "SELECT pg_get_userbyid(datdba)::text FROM pg_database WHERE datname = $1",
            &[&made.database],
        )
        .expect("the database")
        .get(0);
    assert_eq!(owner, made.role);

    let dsn = files.path("db-dsn");
    let text = at_test_server(
        &runbook_dsn("first-password-0123"),
        &made.database,
        &made.role,
    );
    assert!(text.contains(" password=first-password-0123 "), "{text}");
    put_secret(&dsn, &text);
    let mut args = committed_args(&files, &dsn);
    // A new farm's first start makes the CA (the runbook's `--ca-init`); the test's OIDC
    // allow-list admits the legacy service, which a coordinator then names (the runbook's "any
    // other flag the coordinator runs with today").
    args.extend(["--ca-init", "--legacy-service", "kbf"].map(OsString::from));
    let parts: Parts = build_given(&flags_of(args.clone())).expect("the committed command starts");
    let mut db = client(Some(&made.database));
    db.batch_execute("SET search_path = hostkeeper")
        .expect("the coordinator's schema");
    let lease = db
        .query_one("SELECT epoch, holder FROM lease", &[])
        .expect("the lease");
    assert_eq!(
        (lease.get::<_, i64>(0), lease.get::<_, String>(1)),
        (1, "coordinator.example.com".to_owned())
    );
    let schema_owner: String = db
        .query_one(
            "SELECT pg_get_userbyid(nspowner)::text FROM pg_namespace WHERE nspname = 'hostkeeper'",
            &[],
        )
        .expect("the schema")
        .get(0);
    assert_eq!(schema_owner, made.role);
    parts
        .coordinator
        .operate(
            Some(OPERATOR),
            br#"{"action":"set_labels","node_id":"d1","labels":{"rack":"r1"}}"#,
        )
        .expect("an operator's change");
    let why = refusal(args);
    assert!(
        matches!(why, StartError::Database(_)),
        "a second coordinator: {why}"
    );
    drop(parts);

    put_secret(&files.path("db-password"), "second-password-4567\n");
    let mut again = Psql::new();
    again
        .files
        .insert(PASSWORD_SECRET, files.path("db-password"));
    again.renames = vec![("hostkeeper", made.role.clone())];
    again.run(&mut admin, &deploy("sql/password.sql"));
    let second = verifier(&mut admin, &made.role);
    assert!(second.starts_with("SCRAM-SHA-256$4096:"), "{second}");
    assert_ne!(second, first, "the new password is set");
}

/// The arguments of a `serve` on `database` that `files` names, owner-only connection string.
fn serve_on(files: &Files, database: &Database) -> Vec<OsString> {
    files.put("database.dsn", &database.dsn_text(), 0o600);
    let mut args = support::serve::without(&files.args(), "--state-file");
    for (flag, file) in [
        ("--database-dsn-file", "database.dsn"),
        ("--halt-marker-file", "halt.marker"),
    ] {
        args.push(flag.into());
        args.push(files.path(file).into_os_string());
    }
    args
}

/// Proves: `after-restore.sql` moves the enrollment serial counter of a database the coordinator
/// made on by exactly 1,048,576, and each run moves it on again.
///
/// Catches: a statement that names a table or schema the migrations do not make, or moves the
/// counter by another amount.
#[test]
fn after_restore_moves_the_serial_counter_on_by_1048576() {
    let files = Files::new("deploy-after-restore");
    let database = Database::new();
    drop(build_given(&flags_of(serve_on(&files, &database))).expect("a start makes the schema"));
    database.lock_is_free();
    let mut session = client(Some(database.name()));
    let next = |session: &mut Client| -> i64 {
        session
            .query_one("SELECT next FROM hostkeeper.enrollment_serial", &[])
            .expect("the counter")
            .get(0)
    };
    let before = next(&mut session);
    Psql::new().run(&mut session, &deploy("sql/after-restore.sql"));
    assert_eq!(next(&mut session), before + 1_048_576);
    Psql::new().run(&mut session, &deploy("sql/after-restore.sql"));
    assert_eq!(next(&mut session), before + 2 * 1_048_576);
}

/// Proves: on a database that holds an operator's changes and two spent CI token ids, one past and
/// one not, `counts.sql` prints exactly the lines `hostkeeper-coordinator export` prints for the
/// rows of each table, in its order, so the drill compares the two line for line; both count every
/// row, the past id among them.
///
/// Catches: a table the export counts and the drill's query leaves out, or one it counts that the
/// export does not, a count of the wrong table, a count that leaves out the past ids the export
/// counts (a drill that fails on good dumps), or another spelling of the lines.
#[test]
fn counts_sql_prints_the_rows_of_each_table_as_export_prints_them() {
    let files = Files::new("deploy-counts");
    let database = Database::new();
    let parts = build_given(&flags_of(serve_on(&files, &database))).expect("a start");
    for node in ["d1", "d2"] {
        let body =
            format!(r#"{{"action":"set_labels","node_id":"{node}","labels":{{"rack":"r1"}}}}"#);
        parts
            .coordinator
            .operate(Some(OPERATOR), body.as_bytes())
            .expect("an operator's change");
    }
    drop(parts);
    database.lock_is_free();
    let mut session = client(Some(database.name()));
    // Written as the coordinator writes, under its lock: one id long past, one far ahead.
    session
        .batch_execute(&format!(
            "SELECT pg_advisory_lock({COORDINATOR_LOCK}); \
             INSERT INTO hostkeeper.spent_tokens (jti, until_unix) VALUES ('past', 1), \
             ('live', 4000000000); \
             SELECT pg_advisory_unlock({COORDINATOR_LOCK})"
        ))
        .expect("two spent ids");
    let counted = Psql::new().run(&mut session, &deploy("sql/counts.sql"));
    drop(session);
    database.lock_is_free();

    let out = files.path("exported");
    std::fs::create_dir(&out).expect("the export's directory");
    let exported = Command::new(env!("CARGO_BIN_EXE_hostkeeper-coordinator"))
        .arg("export")
        .arg("--database-dsn-file")
        .arg(files.path("database.dsn"))
        .arg("--state-file")
        .arg(out.join("desired.json"))
        .arg("--legacy-service")
        .arg("kbf")
        .arg("--into-empty")
        .output()
        .expect("run export");
    let printed = String::from_utf8_lossy(&exported.stdout).into_owned();
    assert!(
        exported.status.success(),
        "{printed}{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let lines: Vec<String> = printed
        .lines()
        .skip_while(|l| *l != "the rows of each table:")
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .map(str::to_owned)
        .collect();
    assert_eq!(counted, lines);
    assert_eq!(counted.len(), 44, "{counted:?}");
    assert!(
        counted.contains(&"  spent_tokens: 2 rows".to_owned()),
        "{counted:?}"
    );
    assert!(
        counted.contains(&"  host_labels: 2 rows".to_owned()),
        "{counted:?}"
    );
    assert!(
        counted.contains(&"  audit: 2 rows".to_owned()),
        "{counted:?}"
    );
}

/// Proves: a whole word is renamed and a word that only holds it is not.
///
/// Catches: a rename that reaches a longer identifier holding the name (`hostkeeper_x`,
/// `xhostkeeper`). Paths are never renamed: the file a `\set` reads is not substituted.
#[test]
fn a_rename_reaches_whole_words_only() {
    assert_eq!(
        rename_word(
            "ALTER ROLE hostkeeper PASSWORD; hostkeeper_x xhostkeeper hostkeeper.t (hostkeeper)",
            "hostkeeper",
            "r"
        ),
        "ALTER ROLE r PASSWORD; hostkeeper_x xhostkeeper r.t (r)"
    );
}
