use std::num::NonZeroUsize;
use std::time::Duration;

use clap::Parser;

use super::*;
use crate::store::postgres::migrate::RETRY;

#[derive(Debug, Parser)]
struct Flags {
    #[command(flatten)]
    database: DatabaseFlags,
}

/// Proves: with none of the flags given, the store's options are the design's defaults (a pool of
/// 4, a beat every 5 seconds, and the runner's lock timeout of 3000 ms with 6 attempts 5 seconds
/// apart, which is [`RETRY`]), with the release and addresses given; each flag given is the option
/// it names.
///
/// Catches: a default that differs from the one the design and the help state, and a flag that is
/// read into another option.
#[test]
fn the_database_flags_default_to_the_design_and_each_sets_its_option() {
    let defaults = Flags::try_parse_from(["serve"]).expect("no flag is needed");
    assert_eq!(
        defaults.database.options("1.2.3", "heartbeat=[::1]:1"),
        Options {
            pool: NonZeroUsize::new(4).expect("four"),
            migrate: RETRY,
            beat: Duration::from_secs(5),
            release: "1.2.3".to_owned(),
            listen: "heartbeat=[::1]:1".to_owned(),
        }
    );
    let given = Flags::try_parse_from([
        "serve",
        "--database-pool",
        "2",
        "--instance-beat-secs",
        "9",
        "--migrate-ddl-lock-timeout-ms",
        "4999",
        "--migrate-retry-secs",
        "0",
        "--migrate-attempts",
        "1",
    ])
    .expect("the flags parse");
    let options = given.database.options("r", "l");
    assert_eq!(options.pool.get(), 2);
    assert_eq!(options.beat, Duration::from_secs(9));
    assert_eq!(
        options.migrate,
        Retry {
            lock_timeout: Duration::from_millis(4999),
            pause: Duration::ZERO,
            attempts: NonZeroU32::new(1).expect("one"),
        }
    );
}

/// Proves: a pool, a beat or a number of attempts of 0, and a lock timeout of 0 or of 5000 ms and
/// more (not below the store's statement timeout), are refused when the flags are parsed.
///
/// Catches: a start with no writing session, a beat with no wait, a runner that never tries, and
/// a lock timeout that the statement timeout ends first, so a busy table fails the start at once.
#[test]
fn the_database_flags_refuse_what_cannot_run() {
    for (flag, value) in [
        ("--database-pool", "0"),
        ("--instance-beat-secs", "0"),
        ("--migrate-attempts", "0"),
        ("--migrate-ddl-lock-timeout-ms", "0"),
        ("--migrate-ddl-lock-timeout-ms", "5000"),
    ] {
        assert!(
            Flags::try_parse_from(["serve", flag, value]).is_err(),
            "{flag} {value}"
        );
    }
}

/// Proves: an instance's addresses are its heartbeat and private listeners', and its LAN
/// listener's after them when it has one.
///
/// Catches: a row that names no LAN listener a Mac reaches the instance by.
#[test]
fn the_listen_addresses_name_each_listener() {
    let at = |text: &str| text.parse::<SocketAddr>().expect("an address");
    assert_eq!(
        listen_of(at("10.0.0.1:8443"), at("127.0.0.1:8444"), None),
        "heartbeat=10.0.0.1:8443 private=127.0.0.1:8444"
    );
    assert_eq!(
        listen_of(
            at("10.0.0.1:8443"),
            at("127.0.0.1:8444"),
            Some(at("[::]:8447"))
        ),
        "heartbeat=10.0.0.1:8443 private=127.0.0.1:8444 lan=[::]:8447"
    );
}
