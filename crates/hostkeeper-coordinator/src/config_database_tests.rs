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
/// 4, a beat every 5 seconds, the runner's lock timeout of 3000 ms with 6 attempts 5 seconds
/// apart, which is [`RETRY`], an edit's wait for the gate of 30000 ms, and a beat older by its
/// host's clock believed once the stored one is 30 seconds old), with the release, addresses and
/// quiet time given; each flag given is the option it names, and a quiet time too long for the
/// database's clock to add is the longest it takes.
///
/// Catches: a default that differs from the one the design and the help state, and a flag that is
/// read into another option.
#[test]
fn the_database_flags_default_to_the_design_and_each_sets_its_option() {
    let defaults = Flags::try_parse_from(["serve"]).expect("no flag is needed");
    assert_eq!(
        defaults.database.options("1.2.3", "heartbeat=[::1]:1", 40),
        Options {
            pool: NonZeroUsize::new(4).expect("four"),
            migrate: RETRY,
            beat: Duration::from_secs(5),
            release: "1.2.3".to_owned(),
            listen: "heartbeat=[::1]:1".to_owned(),
            gate_wait: Duration::from_secs(30),
            fleet: Timing {
                down_after: 45,
                reorder: 30,
                quiet: 40,
            },
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
        "--operator-gate-wait-ms",
        "250",
        "--beat-reorder-secs",
        "44",
    ])
    .expect("the flags parse");
    let options = given.database.options("r", "l", 30);
    assert_eq!(options.fleet.reorder, 44);
    let longest = given.database.options("r", "l", u64::MAX);
    assert_eq!(
        longest.fleet.quiet, 4_294_967_295,
        "a quiet time past u32 is u32's largest"
    );
    assert_eq!(options.pool.get(), 2);
    assert_eq!(options.gate_wait, Duration::from_millis(250));
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

/// Proves: a pool, a beat, a number of attempts or a wait for the gate of 0, a lock timeout of 0
/// or of 5000 ms and more (not below the store's statement timeout), and a reorder time of 0 or of
/// 45 seconds and more (not below the silence after which a host is down), are refused when the
/// flags are parsed.
///
/// Catches: a start with no writing session, a beat with no wait, a runner that never tries, and
/// a lock timeout that the statement timeout ends first, so a busy table fails the start at once;
/// and a reorder time that drops no beat, or that keeps a host whose clock stepped back unbelieved
/// until it counts as down.
#[test]
fn the_database_flags_refuse_what_cannot_run() {
    for (flag, value) in [
        ("--database-pool", "0"),
        ("--instance-beat-secs", "0"),
        ("--migrate-attempts", "0"),
        ("--operator-gate-wait-ms", "0"),
        ("--migrate-ddl-lock-timeout-ms", "0"),
        ("--migrate-ddl-lock-timeout-ms", "5000"),
        ("--beat-reorder-secs", "0"),
        ("--beat-reorder-secs", "45"),
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
        listen_of(at("192.0.2.1:8443"), at("127.0.0.1:8444"), None),
        "heartbeat=192.0.2.1:8443 private=127.0.0.1:8444"
    );
    assert_eq!(
        listen_of(
            at("192.0.2.1:8443"),
            at("127.0.0.1:8444"),
            Some(at("[::]:8447"))
        ),
        "heartbeat=192.0.2.1:8443 private=127.0.0.1:8444 lan=[::]:8447"
    );
}

/// The flags given, parsed.
fn parsed(args: &[&str]) -> DatabaseFlags {
    let mut all = vec!["serve"];
    all.extend_from_slice(args);
    Flags::try_parse_from(all)
        .expect("the flags parse")
        .database
}

/// Proves: `--job-every NAME=SECS` is read for each of the four jobs, as the job's name and its
/// period.
///
/// Catches: a job the flag does not name, and a period read into another job.
#[test]
fn job_every_parses_name_equals_seconds_for_each_job() {
    for (name, secs) in [
        ("alerts", 7),
        ("request-expiry", 8),
        ("prune", 9),
        ("xcode-feed", 10),
    ] {
        let given = parsed(&["--job-every", &format!("{name}={secs}")]);
        assert_eq!(
            given.job_every,
            vec![JobEvery {
                name,
                secs: NonZeroU64::new(secs).expect("not zero"),
            }]
        );
    }
}

/// Proves: `--job-every` refuses a job that has no such name, a value with no `=`, a period of 0
/// and a period that is not a number, each with the message that says what is wrong.
///
/// Catches: a typo in a job name that is accepted and changes nothing, and a period of 0 that
/// would make a job due at every tick.
#[test]
fn job_every_refuses_what_is_not_a_job_and_a_period() {
    assert_eq!(
        job_every("nothing"),
        Err("\"nothing\" is not NAME=SECS".to_owned())
    );
    assert_eq!(
        job_every("sweep=5"),
        Err(
            "no job is named \"sweep\": one of alerts, request-expiry, prune, xcode-feed"
                .to_owned()
        )
    );
    assert_eq!(
        job_every("alerts=0"),
        Err("\"0\" is not a number of seconds of at least 1".to_owned())
    );
    assert_eq!(
        job_every("prune=soon"),
        Err("\"soon\" is not a number of seconds of at least 1".to_owned())
    );
    assert_eq!(
        job_every("prune=-5"),
        Err("\"-5\" is not a number of seconds of at least 1".to_owned())
    );
    for bad in ["nothing", "sweep=5", "alerts=0", "prune=soon"] {
        assert!(
            Flags::try_parse_from(["serve", "--job-every", bad]).is_err(),
            "{bad}"
        );
    }
}

/// Proves: with no `--job-every` the periods are 15 seconds for `alerts` and for
/// `request-expiry` and an hour for `prune`; one given changes only its job, and the last one
/// given for a job wins.
///
/// Catches: a default that differs from the design's, a flag that changes another job's period,
/// and an earlier flag that wins over a later one.
#[test]
fn job_periods_are_the_defaults_and_the_last_flag_of_a_job_wins() {
    let period = |name: &str, secs: i64| (name.to_owned(), secs);
    assert_eq!(
        parsed(&[]).job_periods(false),
        vec![
            period("alerts", 15),
            period("request-expiry", 15),
            period("prune", 3600)
        ]
    );
    assert_eq!(
        parsed(&[
            "--job-every",
            "prune=60",
            "--job-every",
            "alerts=5",
            "--job-every",
            "prune=120"
        ])
        .job_periods(false),
        vec![
            period("alerts", 5),
            period("request-expiry", 15),
            period("prune", 120)
        ]
    );
}

/// Proves: the jobs are claimed every `--job-tick-secs` seconds, 5 by default, and 0 is refused.
///
/// Catches: a tick that ignores its flag, a default other than the design's, and a tick of 0 that
/// would claim in a busy loop.
#[test]
fn job_tick_is_the_flag_in_seconds_and_five_by_default() {
    assert_eq!(parsed(&[]).job_tick(), Duration::from_secs(5));
    assert_eq!(
        parsed(&["--job-tick-secs", "30"]).job_tick(),
        Duration::from_secs(30)
    );
    assert!(Flags::try_parse_from(["serve", "--job-tick-secs", "0"]).is_err());
}

/// Proves: `--session-touch-secs` is 30 by default and is the number given otherwise, 0 included.
///
/// Catches: a default other than the one the help states, and a flag that is not read.
#[test]
fn session_touch_secs_is_thirty_by_default() {
    assert_eq!(parsed(&[]).session_touch_secs, 30);
    assert_eq!(
        parsed(&["--session-touch-secs", "90"]).session_touch_secs,
        90
    );
    assert_eq!(parsed(&["--session-touch-secs", "0"]).session_touch_secs, 0);
}

/// Proves: the periods hold no `xcode-feed` when the feed is off, and hold it at 86400 seconds
/// when it is on; `--job-every xcode-feed=SECS` sets its period, and is not read for a feed that
/// is off.
///
/// Catches: a job row seeded and claimed for a feed that is not set, a default that is not a
/// day, and a flag that does not reach the job.
#[test]
fn the_xcode_feed_job_is_in_the_periods_only_with_a_feed() {
    let period = |name: &str, secs: i64| (name.to_owned(), secs);
    let off = parsed(&[]).job_periods(false);
    assert!(off.iter().all(|(name, _)| name != "xcode-feed"), "{off:?}");
    assert_eq!(
        parsed(&[]).job_periods(true),
        vec![
            period("alerts", 15),
            period("request-expiry", 15),
            period("prune", 3600),
            period("xcode-feed", 86_400)
        ]
    );
    let given = parsed(&["--job-every", "xcode-feed=600"]);
    assert_eq!(given.job_periods(true)[3], period("xcode-feed", 600));
    assert_eq!(given.job_periods(false).len(), 3);
}
