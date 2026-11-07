use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::time::Duration;

use super::*;
use crate::testutil::scratch;

const PASSWORD: &str = "s3cret-text";

fn write_dsn(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("the file is written");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
        .expect("the mode is set");
    path
}

const MALFORMED: &str =
    "--database-dsn-file: it does not hold one libpq connection string in keyword form";

/// Proves: a connection string that asks for TLS (`require`, `verify-ca`, `verify-full`) is
/// refused with the one text that names the flag and says how to connect without it, and the
/// modes that allow a connection without TLS (`disable`, `prefer`, none) are taken.
///
/// Catches: a build that quietly downgrades a string that asked for TLS, or that refuses the
/// client's own default.
#[test]
fn a_dsn_with_sslmode_require_is_refused() {
    let err = Dsn::parse("host=db sslmode=require").expect_err("TLS is refused");
    assert_eq!(err, DsnError::Tls);
    assert_eq!(
        err.to_string(),
        "--database-dsn-file: the connection string asks for TLS (sslmode), which this build does not connect with; set sslmode=disable for a database on this machine's loopback"
    );
    // The client reads no other mode that asks for TLS: those are refused as unreadable.
    for mode in ["verify-ca", "verify-full"] {
        let err = Dsn::parse(&format!("host=db sslmode={mode}")).expect_err("TLS is refused");
        assert_eq!(err, DsnError::Malformed, "sslmode={mode}");
    }
    for text in [
        "host=db sslmode=disable",
        "host=db sslmode=prefer",
        "host=db",
    ] {
        assert!(Dsn::parse(text).is_ok(), "{text}");
    }
}

/// Proves: a string is refused when it is empty, spans lines, does not parse, or names no host,
/// each with a text that names the flag, and an accepted string is trimmed.
///
/// Catches: a text that shows the string, or a string with no host that reaches a connect.
#[test]
fn a_dsn_that_is_not_one_connection_string_is_refused_by_name() {
    for (text, want, shown) in [
        ("   \n ", DsnError::Malformed, MALFORMED),
        ("host=db\nport=5432", DsnError::Malformed, MALFORMED),
        ("host=db port=notanumber", DsnError::Malformed, MALFORMED),
        (
            "dbname=x",
            DsnError::NoHost,
            "--database-dsn-file: the connection string names no host",
        ),
    ] {
        let err = Dsn::parse(text).expect_err("the string is refused");
        assert_eq!(err, want, "{text:?}");
        assert_eq!(err.to_string(), shown, "{text:?}");
    }
    assert!(Dsn::parse("  host=db  \n").is_ok());
}

/// Proves: a file others can read, a link, a missing file, a file over 4096 bytes, one that is
/// not UTF-8 and an empty one are each refused with a text that names the flag and the rule and
/// shows neither the path nor the password; a file of mode 0600 or 0400 is read.
///
/// Catches: a secret file read with the wrong mode, followed through a link, or an error that
/// leaks where the file is or what it holds.
#[test]
fn a_dsn_file_others_can_read_is_refused() {
    let dir = scratch("dsn-file");
    let good = format!("host=db user=u password={PASSWORD}");
    let open = write_dsn(dir.path(), "open", good.as_bytes(), 0o644);
    let err = Dsn::read(&open).expect_err("a readable file is refused");
    assert_eq!(
        err.to_string(),
        "--database-dsn-file: its mode must be 0600 or 0400"
    );

    let ok = write_dsn(dir.path(), "ok", good.as_bytes(), 0o600);
    assert!(Dsn::read(&ok).is_ok());
    let read_only = write_dsn(dir.path(), "ro", good.as_bytes(), 0o400);
    assert!(Dsn::read(&read_only).is_ok());

    let link = dir.path().join("link");
    symlink(&ok, &link).expect("a link");
    let long = write_dsn(
        dir.path(),
        "long",
        format!("host=db password={}", "x".repeat(5000)).as_bytes(),
        0o600,
    );
    let binary = write_dsn(dir.path(), "binary", &[0xff, 0xfe, 0x00], 0o600);
    let empty = write_dsn(dir.path(), "empty", b"", 0o600);
    let missing = dir.path().join("missing");
    let refused: Vec<(&str, DsnError)> = [
        ("link", &link),
        ("missing", &missing),
        ("long", &long),
        ("binary", &binary),
        ("empty", &empty),
    ]
    .into_iter()
    .map(|(name, path)| (name, Dsn::read(path).expect_err("the file is refused")))
    .collect();
    let shown: Vec<(&str, String)> = refused
        .iter()
        .map(|(name, err)| (*name, err.to_string()))
        .collect();
    assert_eq!(refused[2].1, DsnError::TooLong);
    assert_eq!(refused[3].1, DsnError::Malformed);
    assert_eq!(refused[4].1, DsnError::Malformed);
    assert_eq!(
        shown,
        vec![
            (
                "link",
                "--database-dsn-file: filesystem loop or indirection limit (e.g. symlink loop)"
                    .to_owned()
            ),
            (
                "missing",
                "--database-dsn-file: entity not found".to_owned()
            ),
            (
                "long",
                "--database-dsn-file: it holds more than 4096 bytes".to_owned()
            ),
            ("binary", MALFORMED.to_owned()),
            ("empty", MALFORMED.to_owned()),
        ]
    );
    let dir_text = dir.path().display().to_string();
    for (_, text) in &shown {
        assert!(!text.contains(&dir_text), "{text}");
        assert!(!text.contains(PASSWORD), "{text}");
    }
}

/// Proves: the `Debug` of a connection string shows nothing of it, and an application name the
/// string names is kept while one it does not is the coordinator's.
///
/// Catches: a password in a log line, and an application name that overrides the operator's.
#[test]
fn a_dsn_shows_nothing_of_itself_and_names_the_application() {
    let dsn = Dsn::parse(&format!("host=db password={PASSWORD}")).expect("a string");
    assert_eq!(format!("{dsn:?}"), "Dsn(..)");
    assert_eq!(
        dsn.config(TIMEOUTS).get_application_name(),
        Some(APPLICATION)
    );
    let named = Dsn::parse("host=db application_name=mine").expect("a string");
    assert_eq!(named.config(TIMEOUTS).get_application_name(), Some("mine"));
    assert_eq!(
        named
            .named("theirs")
            .config(TIMEOUTS)
            .get_application_name(),
        Some("theirs")
    );
}

/// Proves: the configuration for a session carries the bounded waits over whatever the string
/// says: connect timeout, TCP user timeout, keepalives, and the statement, lock and idle
/// timeouts as server options, with TLS off.
///
/// Catches: a session that can wait forever on a stalled database.
#[test]
fn the_config_of_a_session_carries_the_bounded_waits() {
    let dsn = Dsn::parse("host=db connect_timeout=99").expect("a string");
    let timeouts = Timeouts {
        connect: Duration::from_secs(2),
        statement: Duration::from_millis(4500),
        lock: Duration::from_millis(1500),
        tcp: Duration::from_secs(7),
    };
    let config = dsn.config(timeouts);
    assert_eq!(config.get_connect_timeout(), Some(&Duration::from_secs(2)));
    assert_eq!(config.get_tcp_user_timeout(), Some(&Duration::from_secs(7)));
    assert!(config.get_keepalives());
    assert_eq!(config.get_keepalives_idle(), Duration::from_secs(7));
    assert_eq!(
        config.get_keepalives_interval(),
        Some(Duration::from_secs(1))
    );
    assert_eq!(config.get_keepalives_retries(), Some(3));
    assert_eq!(
        config.get_options(),
        Some(
            "-c statement_timeout=4500 -c lock_timeout=1500 -c idle_in_transaction_session_timeout=4500"
        )
    );
    assert_eq!(config.get_ssl_mode(), SslMode::Disable);
}
