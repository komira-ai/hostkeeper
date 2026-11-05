//! The file of verdicts (`<state-file>.verdicts`): what it holds after each change, that a
//! change is in the file before it is in memory, that nothing is written when nothing changes,
//! and what a restart reads back, skips or refuses.

use std::fs;
use std::path::{Path, PathBuf};

use hostkeeper_proto::hostkeeper::agent::v1 as agent;
use hostkeeper_wire::wire::{OrderId, Outcome, UpdateResult, VerdictAck};

use crate::testutil::{id, scratch, version};
use crate::verdict_file::{VerdictFile, VerdictsError, to_proto, verdicts_path};

/// A verdict about `target` that ends try `attempt` of `order`, or names no order.
fn report(target: &str, outcome: Outcome, order: Option<(&str, u64)>) -> UpdateResult {
    UpdateResult {
        order_id: order.and_then(|(name, _)| OrderId::parse(name)),
        order_try: order.map(|(_, attempt)| attempt),
        ..UpdateResult::new(version(target), outcome)
    }
}

fn failed(order: &str, attempt: u64) -> UpdateResult {
    report("2.0", Outcome::Failed, Some((order, attempt)))
}

fn heard(service: &str, last: Option<UpdateResult>) -> (String, Option<UpdateResult>) {
    (service.to_owned(), last)
}

/// The file of a directory's `desired.json`.
fn file_in(dir: &Path) -> PathBuf {
    verdicts_path(&dir.join("desired.json"))
}

/// Makes every write of the file at `path` fail: its temporary file's name is a directory.
fn block(path: &Path) -> PathBuf {
    let mut temp = path.as_os_str().to_owned();
    temp.push(".new");
    let temp = PathBuf::from(temp);
    fs::create_dir(&temp).expect("a directory in the way");
    temp
}

/// Catches: the file named apart from the state file, so a restart reads another or none:
/// it is `<state-file>.verdicts`.
#[test]
fn the_file_is_beside_the_state_file() {
    assert_eq!(
        verdicts_path(Path::new("/srv/hk/desired.json")),
        PathBuf::from("/srv/hk/desired.json.verdicts")
    );
}

/// Catches: a coordinator that cannot start before its first verdict, or that makes up
/// verdicts: with no file there, it holds none.
#[test]
fn no_file_is_no_verdict() {
    let dir = scratch("verdicts-none");
    let file = VerdictFile::open(&file_in(dir.path())).expect("opens");
    assert_eq!(file.standing("kbf", &id("d1")), None);
    assert!(file.counted().is_empty());
    assert!(!file_in(dir.path()).exists());
}

/// Catches: a verdict lost or changed on its way through the file, or written in a shape a
/// restart reads otherwise: the file holds, by service, the verdict standing for each host and
/// the verdicts counted, exactly as written, and a restart reads both back.
#[test]
fn a_recorded_verdict_is_in_the_file_and_read_back() {
    let dir = scratch("verdicts-round-trip");
    let path = file_in(dir.path());
    let file = VerdictFile::open(&path).expect("opens");
    file.record(
        &id("d1"),
        &[heard("kbf", Some(failed("o-1", 2)))],
        &[("kbf".to_owned(), vec![(id("d1"), failed("o-1", 2))])],
    )
    .expect("written");
    let text = fs::read_to_string(&path).expect("the file");
    assert_eq!(
        text,
        concat!(
            "{\n",
            "  \"services\": {\n",
            "    \"kbf\": {\n",
            "      \"standing\": {\n",
            "        \"d1\": {\n",
            "          \"target\": \"2.0\",\n",
            "          \"outcome\": \"failed\",\n",
            "          \"order_id\": \"o-1\",\n",
            "          \"order_try\": 2\n",
            "        }\n",
            "      },\n",
            "      \"counted\": [\n",
            "        {\n",
            "          \"node_id\": \"d1\",\n",
            "          \"report\": {\n",
            "            \"target\": \"2.0\",\n",
            "            \"outcome\": \"failed\",\n",
            "            \"order_id\": \"o-1\",\n",
            "            \"order_try\": 2\n",
            "          }\n",
            "        }\n",
            "      ]\n",
            "    }\n",
            "  }\n",
            "}\n"
        )
    );
    let again = VerdictFile::open(&path).expect("opens");
    assert_eq!(again.standing("kbf", &id("d1")), Some(failed("o-1", 2)));
    assert_eq!(
        again.counted(),
        vec![("kbf".to_owned(), id("d1"), failed("o-1", 2))]
    );
}

/// Catches: memory ahead of the file, so an answer could acknowledge a verdict a crash would
/// lose: when the write fails, the error says so, nothing is in memory and nothing in the
/// file; the same record once the file can be written lands in both.
#[test]
fn a_change_that_is_not_written_is_not_kept() {
    let dir = scratch("verdicts-unwritten");
    let path = file_in(dir.path());
    let file = VerdictFile::open(&path).expect("opens");
    let temp = block(&path);
    let refused = file
        .record(&id("d1"), &[heard("kbf", Some(failed("o-1", 1)))], &[])
        .expect_err("not written");
    assert!(matches!(refused, VerdictsError::Io(_)), "{refused:?}");
    assert_eq!(file.standing("kbf", &id("d1")), None);
    assert!(!path.exists());
    fs::remove_dir(&temp).expect("out of the way");
    file.record(&id("d1"), &[heard("kbf", Some(failed("o-1", 1)))], &[])
        .expect("written");
    assert_eq!(file.standing("kbf", &id("d1")), Some(failed("o-1", 1)));
    assert_eq!(
        VerdictFile::open(&path)
            .expect("opens")
            .standing("kbf", &id("d1")),
        Some(failed("o-1", 1))
    );
}

/// Catches: a write at every heartbeat (an fsync per resend, under the fleet's lock), or a
/// record that changes nothing failing because the disk does: the same verdict and the same
/// counted set again write nothing, so they succeed with every write failing; and a host's
/// heartbeat that says nothing of a service keeps the service's standing verdict.
#[test]
fn a_record_that_changes_nothing_writes_nothing() {
    let dir = scratch("verdicts-unchanged");
    let path = file_in(dir.path());
    let file = VerdictFile::open(&path).expect("opens");
    let counted = [("kbf".to_owned(), vec![(id("d1"), failed("o-1", 1))])];
    file.record(&id("d1"), &[heard("kbf", Some(failed("o-1", 1)))], &counted)
        .expect("written");
    let before = fs::read(&path).expect("the file");
    block(&path);
    file.record(&id("d1"), &[heard("kbf", Some(failed("o-1", 1)))], &counted)
        .expect("nothing to write");
    file.record(&id("d1"), &[heard("kbf", None)], &[])
        .expect("nothing to write");
    file.record(&id("d2"), &[heard("kbf", None)], &[])
        .expect("nothing to write");
    assert_eq!(fs::read(&path).expect("the file"), before);
    assert_eq!(file.standing("kbf", &id("d1")), Some(failed("o-1", 1)));
    // A counted set that differs in its host, its report or its length is a change.
    for differs in [
        vec![(id("d2"), failed("o-1", 1))],
        vec![(id("d1"), failed("o-1", 2))],
        vec![],
    ] {
        let refused = file.record(&id("d1"), &[], &[("kbf".to_owned(), differs.clone())]);
        assert!(refused.is_err(), "{differs:?} is a change");
    }
}

/// Catches: a verdict heard standing after the next one is heard (the agent sends its verdicts
/// oldest first to a coordinator that acknowledges them), or a verdict that cannot be
/// acknowledged standing for a host: the verdict heard last replaces the standing one, one with
/// no order id or no try ends it, and another host's or service's is its own.
#[test]
fn the_last_verdict_heard_stands() {
    let dir = scratch("verdicts-replaced");
    let file = VerdictFile::open(&file_in(dir.path())).expect("opens");
    let succeeded = report("2.0", Outcome::Succeeded, Some(("o-2", 1)));
    file.record(
        &id("d1"),
        &[
            heard("kbf", Some(failed("o-1", 1))),
            heard("other", Some(failed("o-7", 1))),
        ],
        &[],
    )
    .expect("written");
    file.record(&id("d2"), &[heard("kbf", Some(failed("o-1", 1)))], &[])
        .expect("written");
    file.record(&id("d1"), &[heard("kbf", Some(succeeded.clone()))], &[])
        .expect("written");
    assert_eq!(file.standing("kbf", &id("d1")), Some(succeeded));
    assert_eq!(file.standing("kbf", &id("d2")), Some(failed("o-1", 1)));
    assert_eq!(file.standing("other", &id("d1")), Some(failed("o-7", 1)));
    let mut no_try = failed("o-3", 1);
    no_try.order_try = None;
    let no_id = report("2.0", Outcome::Failed, None);
    file.record(&id("d1"), &[heard("kbf", Some(no_try))], &[])
        .expect("written");
    file.record(&id("d2"), &[heard("kbf", Some(no_id))], &[])
        .expect("written");
    assert_eq!(file.standing("kbf", &id("d1")), None);
    assert_eq!(file.standing("kbf", &id("d2")), None);
    assert_eq!(file.standing("other", &id("d1")), Some(failed("o-7", 1)));
}

/// Catches: a counted set kept for a service after it was emptied (the file grows with every
/// service that ever failed), or one service's set replacing another's: the set given for a
/// service replaces that service's alone, and a service with nothing left leaves the file.
#[test]
fn a_counted_set_replaces_its_services_and_an_empty_service_leaves_the_file() {
    let dir = scratch("verdicts-counted");
    let path = file_in(dir.path());
    let file = VerdictFile::open(&path).expect("opens");
    file.record(
        &id("d1"),
        &[],
        &[
            ("kbf".to_owned(), vec![(id("d1"), failed("o-1", 1))]),
            ("other".to_owned(), vec![(id("d2"), failed("o-2", 1))]),
        ],
    )
    .expect("written");
    file.record(&id("d1"), &[], &[("kbf".to_owned(), vec![])])
        .expect("written");
    assert_eq!(
        file.counted(),
        vec![("other".to_owned(), id("d2"), failed("o-2", 1))]
    );
    file.record(&id("d1"), &[], &[("other".to_owned(), vec![])])
        .expect("written");
    assert_eq!(
        fs::read_to_string(&path).expect("the file"),
        "{\n  \"services\": {}\n}\n"
    );
}

/// Catches: a file of a later build refused, which would stop a rolled-back coordinator from
/// starting: a key this build does not know is skipped, at every level, and what it knows
/// is read.
#[test]
fn a_key_this_build_does_not_know_is_skipped() {
    let dir = scratch("verdicts-later");
    let path = file_in(dir.path());
    fs::write(
        &path,
        concat!(
            r#"{"later":1,"services":{"kbf":{"later":[],"#,
            r#""standing":{"d1":{"target":"2.0","outcome":"failed","order_id":"o-1","order_try":1,"later":true}},"#,
            r#""counted":[{"node_id":"d1","later":0,"report":{"target":"2.0","outcome":"failed","order_id":"o-1","order_try":1}}]}}}"#
        ),
    )
    .expect("a file");
    let file = VerdictFile::open(&path).expect("opens");
    assert_eq!(file.standing("kbf", &id("d1")), Some(failed("o-1", 1)));
    assert_eq!(
        file.counted(),
        vec![("kbf".to_owned(), id("d1"), failed("o-1", 1))]
    );
    // Empty: no services is none stored.
    fs::write(&path, "{}").expect("a file");
    assert!(
        VerdictFile::open(&path)
            .expect("opens")
            .counted()
            .is_empty()
    );
}

/// Catches: a file the coordinator cannot read taken as no verdicts (it would order again
/// what it acknowledged), or a refusal that does not say which file and why: text that is not
/// the file's JSON, and a host id that is not one, standing or counted, stop the start with the
/// file named, the reason, and what to do.
#[test]
fn a_file_that_does_not_read_stops_the_start() {
    let dir = scratch("verdicts-bad");
    let path = file_in(dir.path());
    let shown = path.display().to_string();
    let advice = "; the coordinator does not guess which verdicts it acknowledged, so fix or remove the file to start";
    let report = r#"{"target":"2.0","outcome":"failed"}"#;
    for (text, why) in [
        ("not json".to_owned(), "expected ident at line 1 column 2"),
        (
            format!(r#"{{"services":{{"kbf":{{"standing":{{"D 1":{report}}}}}}}}}"#),
            r#"kbf: host "D 1": a host id has a character outside a-z, 0-9 and -"#,
        ),
        (
            format!(
                r#"{{"services":{{"kbf":{{"counted":[{{"node_id":"-x","report":{report}}}]}}}}}}"#
            ),
            r#"host "-x": a host id starts or ends with - at line 1 column 46"#,
        ),
        (
            r#"{"services":{"kbf":{"standing":{"d1":{"outcome":"failed"}}}}}"#.to_owned(),
            "missing field `target` at line 1 column 57",
        ),
    ] {
        fs::write(&path, &text).expect("a file");
        let refused = VerdictFile::open(&path).expect_err(&text);
        assert!(matches!(refused, VerdictsError::Invalid { .. }), "{text}");
        assert_eq!(
            refused.to_string(),
            format!("{shown}: {why}{advice}"),
            "{text}"
        );
    }
}

/// Catches: a file that cannot be read taken as none: an error reading it (here the path is a
/// directory) stops the start, naming the path.
#[test]
fn a_file_that_cannot_be_read_stops_the_start() {
    let dir = scratch("verdicts-unreadable");
    let path = file_in(dir.path());
    fs::create_dir(&path).expect("a directory");
    let refused = VerdictFile::open(&path).expect_err("unreadable");
    assert!(matches!(refused, VerdictsError::Io(_)), "{refused:?}");
    assert!(
        refused.to_string().starts_with(&path.display().to_string()),
        "{refused}"
    );
}

/// Catches: a coordinator without a state file that refuses to answer: the store that keeps
/// nothing on disk takes every change in memory.
#[test]
fn a_volatile_store_keeps_its_verdicts_in_memory() {
    let file = VerdictFile::volatile();
    file.record(
        &id("d1"),
        &[heard("kbf", Some(failed("o-1", 1)))],
        &[("kbf".to_owned(), vec![(id("d1"), failed("o-1", 1))])],
    )
    .expect("kept");
    assert_eq!(file.standing("kbf", &id("d1")), Some(failed("o-1", 1)));
    assert_eq!(file.counted().len(), 1);
    assert!(format!("{file:?}").starts_with("VerdictFile"));
}

/// Catches: the gRPC acks differing from the JSON ones in a field: each ack is the same
/// service, order id and try in `hostkeeper.agent.v1`.
#[test]
fn an_ack_is_the_same_in_the_proto() {
    let acks = [
        VerdictAck {
            service: "kbf".to_owned(),
            order_id: OrderId::parse("o-1").expect("an id"),
            order_try: 3,
        },
        VerdictAck {
            service: "other".to_owned(),
            order_id: OrderId::parse("force-1").expect("an id"),
            order_try: 1,
        },
    ];
    assert_eq!(
        to_proto(&acks),
        vec![
            agent::VerdictAck {
                service: "kbf".to_owned(),
                order_id: "o-1".to_owned(),
                order_try: 3,
            },
            agent::VerdictAck {
                service: "other".to_owned(),
                order_id: "force-1".to_owned(),
                order_try: 1,
            },
        ]
    );
    assert!(to_proto(&[]).is_empty());
}
