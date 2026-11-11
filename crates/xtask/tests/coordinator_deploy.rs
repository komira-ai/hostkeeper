//! The coordinator's compose deployment (`docs/deploy/coordinator/`), as committed: the compose
//! file's two services, held to the rules of `docs/design/coordinator-storage.md` section 9 (both
//! images by digest, Postgres's the one CI's tests run against, no environment, the host's network,
//! the secrets and mounts the runbook and the SQL files name), and the backup unit, started by its
//! timer, whose command is run for real with two substitutions only: a scratch directory for the
//! dumps' directory, and `tests/bin/dump_fixture.rs` for `docker`, which prints its arguments as
//! the dump or fails as files beside it say. No test reaches a server or Docker; the SQL files and
//! the coordinator's command are run against Postgres by the coordinator crate's `tests/deploy.rs`.

#[path = "../src/testdir.rs"]
mod testdir;
#[path = "workflows/yaml.rs"]
#[expect(
    dead_code,
    reason = "the workflow lint's YAML reader, of which this test needs only the loader and the scalars"
)]
mod yaml;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use testdir::TestDir;
use yaml::{Node, Value};

const XTASK: &str = env!("CARGO_BIN_EXE_xtask");
const DOCKER: &str = env!("CARGO_BIN_EXE_xtask-dump-fixture");

fn repo(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

fn deploy_file(name: &str) -> String {
    let path = repo("docs/deploy/coordinator").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The entry `key` of the mapping `map`, if it has one.
fn get<'a>(map: &'a Node, key: &str) -> Option<&'a Node> {
    let Value::Map(entries) = &map.value else {
        panic!("{key}: not under a mapping");
    };
    entries
        .iter()
        .find(|(k, _)| k.as_str() == Some(key))
        .map(|(_, v)| v)
}

/// The entry `key` of the mapping `map`.
fn entry<'a>(map: &'a Node, key: &str) -> &'a Node {
    get(map, key).unwrap_or_else(|| panic!("no {key}"))
}

/// The keys of the mapping `map`.
fn keys(map: &Node) -> Vec<String> {
    let Value::Map(entries) = &map.value else {
        panic!("not a mapping");
    };
    entries
        .iter()
        .map(|(k, _)| k.as_str().expect("a key").to_owned())
        .collect()
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

fn text(node: &Node) -> &str {
    node.as_str().expect("a scalar")
}

/// True when `reference` is `<name>:<tag>@sha256:<64 lowercase hex digits>` with `name` and a
/// non-empty tag.
fn pinned(reference: &str, name: &str) -> bool {
    let Some((tagged, digest)) = reference.split_once("@sha256:") else {
        return false;
    };
    let hex = digest.len() == 64
        && digest
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    hex && tagged
        .strip_prefix(name)
        .and_then(|t| t.strip_prefix(':'))
        .is_some_and(|tag| !tag.is_empty())
}

/// Proves: the compose file has exactly the two services; neither has `environment` or
/// `env_file`; both use the host's network; Postgres is `postgres:16` by the digest of CI's test shards'
/// service, optional behind the profile `postgres`, listening on loopback only, with its data on
/// the named volume, the SQL files mounted read-only where the runbook runs them, and a health
/// check of the role and database `init.sql` makes; the coordinator is a release's image by
/// version and digest, never pulled, waits for a healthy Postgres only when it runs, and its
/// command starts `serve` on the connection-string secret with the marker on its local volume and
/// names no file outside its mounts; and the secret `password.sql` reads is the one mounted into
/// Postgres. The runbook runs exactly the SQL files of `sql/`.
///
/// Catches: a variable that configures a container by environment (rule S2), an image by tag
/// alone, a Postgres digest that drifts from the one the tests prove the SQL on, a Postgres that
/// listens beyond loopback or always starts, a coordinator on the file store, a path no mount
/// holds, or a runbook command that names a SQL file that is not there.
#[test]
fn the_compose_file_sets_no_environment_and_pins_both_images_by_digest() {
    let compose = yaml::load(&deploy_file("compose.yaml")).expect("compose.yaml is YAML");
    let services = entry(&compose, "services");
    assert_eq!(keys(services), ["postgres", "coordinator"]);
    for name in ["postgres", "coordinator"] {
        let service = entry(services, name);
        for key in ["environment", "env_file"] {
            assert!(get(service, key).is_none(), "{name} has {key}");
        }
        assert_eq!(text(entry(service, "network_mode")), "host", "{name}");
        assert_eq!(text(entry(service, "restart")), "unless-stopped", "{name}");
    }

    let postgres = entry(services, "postgres");
    let ci =
        yaml::load(&std::fs::read_to_string(repo(".github/workflows/ci.yml")).expect("ci.yml"))
            .expect("ci.yml is YAML");
    let tested = text(entry(
        entry(
            entry(entry(entry(&ci, "jobs"), "test-shard"), "services"),
            "postgres",
        ),
        "image",
    ));
    assert_eq!(text(entry(postgres, "image")), tested);
    assert!(pinned(tested, "postgres"), "{tested}");
    assert!(tested.starts_with("postgres:16@"), "{tested}");
    assert_eq!(strings(entry(postgres, "profiles")), ["postgres"]);
    assert_eq!(
        strings(entry(postgres, "command")),
        [
            "postgres",
            "-c",
            "listen_addresses=127.0.0.1",
            "-c",
            "port=5432"
        ]
    );
    assert_eq!(
        strings(entry(postgres, "volumes")),
        [
            "hostkeeper-db:/var/lib/postgresql/data",
            "./sql:/etc/hostkeeper-db:ro"
        ]
    );
    assert_eq!(
        strings(entry(postgres, "secrets")),
        ["hostkeeper-db-password"]
    );
    assert_eq!(
        strings(entry(entry(postgres, "healthcheck"), "test")),
        [
            "CMD",
            "pg_isready",
            "-h",
            "127.0.0.1",
            "-p",
            "5432",
            "-U",
            "hostkeeper",
            "-d",
            "hostkeeper"
        ]
    );

    let coordinator = entry(services, "coordinator");
    let image = text(entry(coordinator, "image"));
    assert!(pinned(image, "hostkeeper-coordinator"), "{image}");
    assert_eq!(text(entry(coordinator, "pull_policy")), "never");
    let waits = entry(entry(coordinator, "depends_on"), "postgres");
    assert_eq!(text(entry(waits, "condition")), "service_healthy");
    assert_eq!(text(entry(waits, "required")), "false");
    assert_eq!(
        strings(entry(coordinator, "secrets")),
        ["hostkeeper-db-dsn"]
    );
    let mounts = strings(entry(coordinator, "volumes"));
    assert_eq!(
        mounts,
        [
            "hostkeeper-coordinator:/var/lib/hostkeeper-coordinator",
            "./etc:/etc/hostkeeper-coordinator:ro"
        ]
    );
    let command = strings(entry(coordinator, "command"));
    assert_eq!(command.first().map(String::as_str), Some("serve"));
    let value = |flag: &str| {
        let at = command
            .iter()
            .position(|w| w == flag)
            .unwrap_or_else(|| panic!("no {flag}"));
        command[at + 1].clone()
    };
    assert_eq!(
        value("--database-dsn-file"),
        "/run/secrets/hostkeeper-db-dsn"
    );
    assert!(value("--halt-marker-file").starts_with("/var/lib/hostkeeper-coordinator/"));
    for file_flag in ["--state-file", "--ops-file", "--enrollment-dir"] {
        assert!(!command.iter().any(|w| w == file_flag), "{file_flag}");
    }
    for word in command.iter().filter(|w| w.starts_with('/')) {
        let held = [
            "/etc/hostkeeper-coordinator/",
            "/var/lib/hostkeeper-coordinator/",
            "/run/secrets/hostkeeper-db-dsn",
        ]
        .iter()
        .any(|m| word.starts_with(m));
        assert!(held, "{word} is in no mount");
    }

    let secrets = entry(&compose, "secrets");
    assert_eq!(
        text(entry(entry(secrets, "hostkeeper-db-password"), "file")),
        "./secrets/db-password"
    );
    assert_eq!(
        text(entry(entry(secrets, "hostkeeper-db-dsn"), "file")),
        "./secrets/db-dsn"
    );
    assert_eq!(
        keys(entry(&compose, "volumes")),
        ["hostkeeper-db", "hostkeeper-coordinator"]
    );
    assert!(
        deploy_file("sql/password.sql").contains("`cat /run/secrets/hostkeeper-db-password`"),
        "password.sql reads the secret mounted into postgres"
    );

    let runbook = deploy_file("README.md");
    let named: Vec<&str> = runbook
        .split("/etc/hostkeeper-db/")
        .skip(1)
        .map(|rest| {
            rest.split(|c: char| c.is_whitespace() || c == '`')
                .next()
                .unwrap_or("")
        })
        .collect();
    let named: std::collections::BTreeSet<&str> = named.into_iter().collect();
    let committed: std::collections::BTreeSet<String> =
        std::fs::read_dir(repo("docs/deploy/coordinator/sql"))
            .expect("sql/")
            .map(|e| {
                e.expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
    assert_eq!(
        named
            .iter()
            .map(|n| (*n).to_owned())
            .collect::<std::collections::BTreeSet<_>>(),
        committed,
        "the runbook runs exactly the files of sql/"
    );
}

/// The value of `key` in the section `[section]` of a unit file, the last when it repeats.
fn unit_value<'a>(unit: &'a str, section: &str, key: &str) -> Option<&'a str> {
    let mut current = "";
    let mut found = None;
    for line in unit.lines().map(str::trim) {
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            current = name;
        } else if let Some(v) = line
            .strip_prefix(key)
            .and_then(|l| l.strip_prefix('='))
            .filter(|_| current == section)
        {
            found = Some(v);
        }
    }
    found
}

/// `args` with the value after `flag` replaced by `with`; answers the committed value.
fn replace_value(args: &mut [String], flag: &str, with: &str) -> String {
    let at = args
        .iter()
        .position(|a| a == flag)
        .unwrap_or_else(|| panic!("the committed command has no {flag}"));
    std::mem::replace(&mut args[at + 1], with.to_owned())
}

/// The dumps' directory, mode 0700 as systemd's `StateDirectoryMode` makes it, with `dumps`
/// (number, text) in it, and a `bin/` holding the fake docker.
struct Site {
    root: TestDir,
    dumps: PathBuf,
    docker: PathBuf,
}

fn site(name: &str, dumps: &[(u64, &str)]) -> Site {
    let root = TestDir::under(Path::new(env!("CARGO_TARGET_TMPDIR")), name);
    let dir = root.join("dumps");
    std::fs::create_dir(&dir).expect("the dumps' directory");
    for (n, text) in dumps {
        std::fs::write(dir.join(format!("hostkeeper-db-{n}.dump")), text).expect("a dump");
    }
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("mode");
    let bin = root.join("bin");
    std::fs::create_dir(&bin).expect("bin");
    // A hard link, not a copy, so no test runs a file another test still holds open for writing.
    let docker = bin.join("docker");
    std::fs::hard_link(DOCKER, &docker).expect("install the fake docker");
    Site {
        root,
        dumps: dir,
        docker,
    }
}

/// The dumps' directory's files, by name, with their text.
fn files(dir: &Path) -> BTreeMap<String, String> {
    std::fs::read_dir(dir)
        .expect("read the directory")
        .map(|e| {
            let e = e.expect("an entry");
            let text = std::fs::read_to_string(e.path()).expect("read a file");
            (e.file_name().to_string_lossy().into_owned(), text)
        })
        .collect()
}

/// The dump command the committed unit runs, after `--`, with `docker` as the program.
const DUMP: [&str; 12] = [
    "compose",
    "--project-directory",
    "/srv/hostkeeper-coordinator",
    "--profile",
    "postgres",
    "exec",
    "-T",
    "postgres",
    "pg_dump",
    "--format=custom",
    "--username=hostkeeper",
    "--dbname=hostkeeper",
];

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Proves: the timer starts the committed unit every six hours and catches up on a time missed
/// while the machine was off; the unit is a oneshot whose state directory is the dumps' directory
/// its command writes, mode 0700; and its command, run as committed, writes a new dump numbered
/// past the 28 there, holding exactly what `docker compose ... exec -T postgres pg_dump
/// --format=custom --username=hostkeeper --dbname=hostkeeper` printed, mode 0600, removes the
/// oldest, and keeps 28. A run whose dump fails, and one whose dump is empty, exit 1 naming why,
/// with `pg_dump`'s own message on standard error, and leave the 28 good dumps as they were.
///
/// Catches: a timer that never fires or fires at another rate, a unit that keeps more or fewer
/// than a week of dumps, dumps written where the sandbox or the state directory does not say, a
/// dump command that is not the database's `pg_dump` in its custom format, or failed runs that
/// keep a cut-short dump or rotate the good ones away.
#[test]
fn the_timer_starts_the_unit_whose_command_keeps_the_newest_28_dumps_and_no_failed_one() {
    let timer = deploy_file("hostkeeper-db-backup.timer");
    let service = unit_value(&timer, "Timer", "Unit").unwrap_or("hostkeeper-db-backup.service");
    assert_eq!(service, "hostkeeper-db-backup.service");
    assert_eq!(
        unit_value(&timer, "Timer", "OnCalendar"),
        Some("*-*-* 00/6:00:00")
    );
    assert_eq!(unit_value(&timer, "Timer", "Persistent"), Some("true"));
    assert_eq!(
        unit_value(&timer, "Install", "WantedBy"),
        Some("timers.target")
    );

    let unit = deploy_file(service);
    assert_eq!(unit_value(&unit, "Service", "Type"), Some("oneshot"));
    assert_eq!(
        unit_value(&unit, "Service", "StateDirectory"),
        Some("hostkeeper-db-backup")
    );
    assert_eq!(
        unit_value(&unit, "Service", "StateDirectoryMode"),
        Some("0700")
    );
    let exec = unit_value(&unit, "Service", "ExecStart").expect("the unit's ExecStart");
    let mut words = exec.split_whitespace().map(str::to_owned);
    assert_eq!(
        words.next().as_deref(),
        Some("/usr/local/libexec/hostkeeper/xtask")
    );
    let mut args: Vec<String> = words.collect();
    let dash = args
        .iter()
        .position(|a| a == "--")
        .expect("the dump command after --");
    let dump: Vec<String> = args.split_off(dash + 1);
    assert_eq!(dump[0], "/usr/bin/docker");
    assert_eq!(dump[1..], DUMP);
    assert_eq!(args.first().map(String::as_str), Some("db-backup"));

    let week: Vec<(u64, String)> = (1..=28).map(|n| (n, format!("dump {n}"))).collect();
    let week_refs: Vec<(u64, &str)> = week.iter().map(|(n, t)| (*n, t.as_str())).collect();
    let s = site("db-backup-unit", &week_refs);
    let committed = replace_value(&mut args, "--into", s.dumps.to_str().expect("UTF-8"));
    assert_eq!(
        committed, "/var/lib/hostkeeper-db-backup",
        "the unit's state directory"
    );
    assert_eq!(replace_value(&mut args, "--keep", "28"), "28");
    let run = || {
        Command::new(XTASK)
            .args(&args)
            .arg(&s.docker)
            .args(&dump[1..])
            .output()
            .expect("run the unit's command")
    };

    let good = run();
    assert_eq!(good.status.code(), Some(0), "{}", stderr(&good));
    let printed = stdout(&good);
    assert_eq!(
        printed,
        format!(
            "db-backup: wrote hostkeeper-db-29.dump, {} bytes\n\
             db-backup: removed hostkeeper-db-1.dump\n\
             db-backup: 28 dumps kept, the newest hostkeeper-db-29.dump\n",
            "a dump by:\n".len() + dump[1..].iter().map(|w| w.len() + 1).sum::<usize>()
        )
    );
    let mut want: BTreeMap<String, String> = (2..=28)
        .map(|n| (format!("hostkeeper-db-{n}.dump"), format!("dump {n}")))
        .collect();
    want.insert(
        "hostkeeper-db-29.dump".to_owned(),
        format!("a dump by:\n{}\n", dump[1..].join("\n")),
    );
    assert_eq!(files(&s.dumps), want);
    let mode = std::fs::metadata(s.dumps.join("hostkeeper-db-29.dump"))
        .expect("the dump")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    for (mode, why) in [
        (
            "fail",
            "the dump command ended with exit status: 3; no dump was written",
        ),
        (
            "empty",
            "the dump command wrote nothing; no dump was written",
        ),
    ] {
        let marker = s.root.join("bin").join(mode);
        std::fs::write(&marker, "").expect("set the fake's mode");
        let refused = run();
        assert_eq!(refused.status.code(), Some(1), "{mode}");
        let said = stderr(&refused);
        assert!(said.ends_with(&format!("xtask: {why}\n")), "{mode}: {said}");
        if mode == "fail" {
            assert!(
                said.starts_with("pg_dump: error: connection to server failed\n"),
                "{said}"
            );
        }
        assert_eq!(stdout(&refused), "", "{mode}");
        assert_eq!(files(&s.dumps), want, "{mode}: the good dumps stay");
        std::fs::remove_file(&marker).expect("clear the fake's mode");
    }
}
