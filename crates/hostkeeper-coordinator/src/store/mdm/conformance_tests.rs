//! The contract of the [`MdmStore`], as one suite run against both stores: the files and Postgres
//! (`docs/design/coordinator-mdm.md`, 9.1 and 10). Each clause is a function that answers how the
//! store fared, so that the suite's own test can run it against a store wrong in one way each and
//! see the clauses that name the fault fail, and only those.
//!
//! The clauses:
//! - `one_live_enrollment_per_serial`: a serial has one live enrollment; entering another retires
//!   the old one, cancels its commands and retires its identity, and nothing is accepted on it.
//! - `enqueue_is_idempotent_and_claim_is_ordered`: enqueuing an id that exists changes nothing,
//!   a claim takes the oldest queued command not tried, and never one that is sent.
//! - `an_answer_changes_only_a_command_sent_to_its_enrollment`: acknowledged, error and not-now
//!   answers, and the drop of every other.
//! - `a_timed_out_command_is_queued_again`: only a sent command at or before the time.
//! - `the_push_state_follows_pushes_and_contacts`.
//! - `a_challenge_is_consumed_once`: a repeat with the same key gets the stored certificate,
//!   another key is refused, an expired or unknown challenge is refused, nothing is consumed by a
//!   refusal, and a dead identity is refused.
//! - `the_abm_table_follows_the_sync`.
//! - `a_failed_change_is_not_made`.
//! - `what_was_written_is_there_after_a_restart`.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use hostkeeper_testkit::Scratch;

use super::files::FileMdm;
use super::{
    AbmBatch, AbmDevice, AbmState, Answer, Answered, Challenge, Command, CommandState, Consumed,
    Enqueued, Enrollment, EnrollmentState, Identity, IdentityState, Issuance, MdmError, MdmStore,
    NewCommand, Refusal,
};
use crate::store::postgres::Postgres;
use crate::store::postgres::mdm::PgMdm;
use crate::store::postgres::tests::TestDatabase;
use crate::testutil::scratch;

/// A store under the suite: the live one, one started again on what it holds, and a way to make
/// its next changes fail.
pub(crate) trait Subject {
    /// The store that changes.
    fn live(&self) -> Box<dyn MdmStore>;
    /// A store started again on what the live one made durable.
    fn restarted(&self) -> Box<dyn MdmStore>;
    /// From now on every change of commands in the live store fails (`true`), or none does.
    fn failing(&self, on: bool);
}

/// How one clause fared: `Err` names what it found.
type Outcome = Result<(), String>;

type Clause = fn(&dyn Subject) -> Outcome;

fn check(holds: bool, what: impl FnOnce() -> String) -> Outcome {
    if holds { Ok(()) } else { Err(what()) }
}

fn store<T>(r: Result<T, MdmError>) -> Result<T, String> {
    r.map_err(|e| e.to_string())
}

fn enrollment(id: &str, serial: &str, cert: Option<&str>, at: i64) -> Enrollment {
    Enrollment {
        cert_sha256: cert.map(str::to_owned),
        ..Enrollment::new(id, serial, &format!("udid-{id}"), at)
    }
}

fn command(id: &str, enrollment_id: &str, payload: &[u8], at: i64) -> NewCommand {
    NewCommand {
        id: id.to_owned(),
        enrollment_id: enrollment_id.to_owned(),
        command_type: "DeviceInformation".to_owned(),
        payload: payload.to_vec(),
        created_unix: at,
    }
}

fn issuance(cert: &str, der: &[u8]) -> Issuance {
    Issuance {
        cert_sha256: cert.to_owned(),
        cert_der: der.to_vec(),
        not_after_unix: 9_000,
    }
}

fn challenge(hash: &str, serial: &str, expires: i64) -> Challenge {
    Challenge {
        challenge_sha256: hash.to_owned(),
        serial: serial.to_owned(),
        udid: "udid-x".to_owned(),
        expires_unix: expires,
        confirmed: false,
    }
}

fn device(serial: &str, status: &str, seen: i64) -> AbmDevice {
    AbmDevice {
        serial: serial.to_owned(),
        model: Some("Mac15,14".to_owned()),
        description: None,
        profile_status: status.to_owned(),
        profile_uuid: Some("profile-1".to_owned()),
        first_seen_unix: seen,
        synced_unix: seen,
        removed_unix: None,
    }
}

fn state_of(s: &dyn MdmStore, id: &str) -> Result<Option<CommandState>, String> {
    Ok(store(s.command(id))?.map(|c| c.state))
}

fn one_live_enrollment_per_serial(subject: &dyn Subject) -> Outcome {
    let s = subject.live();
    let s = &*s;
    store(s.replace_live(&enrollment("e1", "SER1", Some("c1"), 10)))?;
    store(s.replace_live(&enrollment("o1", "OTHER", None, 10)))?;
    store(s.issue_challenge(&challenge("h1", "SER1", 1_000)))?;
    store(s.consume_challenge("h1", "k1", &issuance("c1", b"der1"), 11))?;
    store(s.enqueue(&command("q1", "e1", b"one", 12)))?;
    store(s.enqueue(&command("q2", "e1", b"two", 12)))?;
    store(s.enqueue(&command("qo", "o1", b"other", 12)))?;
    store(s.claim("e1", &[], 13))?;
    let replaced = store(s.replace_live(&enrollment("e2", "SER1", Some("c2"), 20)))?;
    check(replaced == ["e1"], || {
        format!("replaced {replaced:?}, not [e1]")
    })?;
    let old = store(s.enrollment("e1"))?.ok_or("e1 is gone")?;
    check(old.state == EnrollmentState::Replaced, || {
        format!("e1 is {:?}", old.state)
    })?;
    let live = store(s.live_enrollment("SER1"))?.ok_or("no live enrollment")?;
    check(live.enrollment_id == "e2", || {
        format!("the live one is {}", live.enrollment_id)
    })?;
    for id in ["q1", "q2"] {
        let got = state_of(s, id)?;
        check(got == Some(CommandState::Cancelled), || {
            format!("{id} is {got:?} after its enrollment was replaced")
        })?;
    }
    let other = state_of(s, "qo")?;
    check(other == Some(CommandState::Queued), || {
        format!("another serial's command is {other:?}")
    })?;
    let identity = store(s.identity("c1"))?.ok_or("the identity of e1 is gone")?;
    check(identity.state == IdentityState::Retired, || {
        format!("the identity of e1 is {:?}", identity.state)
    })?;
    let refused = store(s.enqueue(&command("q3", "e1", b"x", 21)))?;
    check(refused == Enqueued::NotLive, || {
        format!("enqueue for a replaced enrollment answered {refused:?}")
    })?;
    check(store(s.claim("e1", &[], 22))?.is_none(), || {
        "a command was claimed for a replaced enrollment".to_owned()
    })?;
    let late = store(s.answer("e1", "q1", Answer::Acknowledged, None, 23))?;
    check(late == Answered::Dropped, || {
        format!("an answer on a replaced enrollment was {late:?}")
    })?;
    let back = s.replace_live(&enrollment("e1", "SER1", None, 24));
    check(
        back == Err(MdmError::EnrollmentEnded("e1".to_owned())),
        || format!("a replaced enrollment entered again: {back:?}"),
    )?;
    let again = store(s.replace_live(&enrollment("e2", "SER1", Some("c2"), 25)))?;
    check(again.is_empty(), || format!("a refresh replaced {again:?}"))?;
    store(s.set_push("e2", b"tok", "magic", "topic"))?;
    let out = store(s.check_out("e2", 30))?;
    check(out, || "a live enrollment did not check out".to_owned())?;
    let twice = store(s.check_out("e2", 31))?;
    check(!twice, || "an enrollment checked out twice".to_owned())?;
    let missing = store(s.check_out("nobody", 31))?;
    check(!missing, || "an unknown enrollment checked out".to_owned())?;
    let pushed = store(s.set_push("e2", b"t", "m", "p"))?;
    check(!pushed, || {
        "a push token was set on a checked-out enrollment".to_owned()
    })?;
    check(store(s.live_enrollment("SER1"))?.is_none(), || {
        "a checked-out enrollment is live".to_owned()
    })?;
    let kept = store(s.enrollment("o1"))?.ok_or("the other serial's enrollment is gone")?;
    check(kept.state == EnrollmentState::Live, || {
        "another serial's enrollment ended".to_owned()
    })
}

fn enqueue_is_idempotent_and_claim_is_ordered(subject: &dyn Subject) -> Outcome {
    let s = subject.live();
    let s = &*s;
    store(s.replace_live(&enrollment("e1", "SER1", None, 1)))?;
    store(s.replace_live(&enrollment("e2", "SER2", None, 1)))?;
    for (id, who) in [("a", "e1"), ("b", "e1"), ("c", "e2")] {
        let got = store(s.enqueue(&command(id, who, id.as_bytes(), 2)))?;
        check(got == Enqueued::Queued, || format!("enqueue {id}: {got:?}"))?;
    }
    let again = store(s.enqueue(&command("a", "e2", b"changed", 3)))?;
    check(again == Enqueued::Exists, || {
        format!("enqueue of an id that exists answered {again:?}")
    })?;
    let kept = store(s.command("a"))?.ok_or("a is gone")?;
    check(kept.payload == b"a" && kept.enrollment_id == "e1", || {
        "enqueuing an id that exists changed it".to_owned()
    })?;
    let unknown = store(s.enqueue(&command("z", "nobody", b"z", 3)))?;
    check(unknown == Enqueued::NotLive, || {
        format!("enqueue for an unknown enrollment answered {unknown:?}")
    })?;
    let first = store(s.claim("e1", &[], 5))?.ok_or("nothing to claim")?;
    check(first.id == "a" && first.state == CommandState::Sent, || {
        format!("the first claim took {first:?}")
    })?;
    check(
        first.deliveries == 1 && first.sent_unix == Some(5) && first.payload == b"a",
        || format!("the first claim's record is {first:?}"),
    )?;
    let skipped = store(s.claim("e1", &["b".to_owned()], 6))?;
    check(skipped.is_none(), || {
        format!("a claim took {skipped:?}: a is sent and b was tried")
    })?;
    let second = store(s.claim("e1", &["a".to_owned()], 6))?.ok_or("b was not claimed")?;
    check(second.id == "b", || {
        format!("the second claim took {}", second.id)
    })?;
    check(store(s.claim("e1", &[], 7))?.is_none(), || {
        "a claim took a command that is sent".to_owned()
    })?;
    let other = store(s.claim("e2", &[], 7))?.ok_or("c was not claimed")?;
    check(other.id == "c", || format!("e2 was given {}", other.id))?;
    let listed: Vec<String> = store(s.commands("e1"))?.into_iter().map(|c| c.id).collect();
    check(listed == ["a", "b"], || {
        format!("e1's commands are {listed:?}")
    })
}

fn an_answer_changes_only_a_command_sent_to_its_enrollment(subject: &dyn Subject) -> Outcome {
    let s = subject.live();
    let s = &*s;
    store(s.replace_live(&enrollment("e1", "SER1", None, 1)))?;
    store(s.replace_live(&enrollment("e2", "SER2", None, 1)))?;
    for id in ["a", "b", "c"] {
        store(s.enqueue(&command(id, "e1", b"x", 2)))?;
    }
    let early = store(s.answer("e1", "a", Answer::Acknowledged, None, 3))?;
    check(early == Answered::Dropped, || {
        format!("an answer to a queued command was {early:?}")
    })?;
    store(s.claim("e1", &[], 4))?;
    let wrong = store(s.answer("e2", "a", Answer::Acknowledged, None, 5))?;
    check(wrong == Answered::Dropped, || {
        format!("an answer through another enrollment was {wrong:?}")
    })?;
    let unknown = store(s.answer("e1", "nobody", Answer::Acknowledged, None, 5))?;
    check(unknown == Answered::Dropped, || {
        format!("an answer to an unknown command was {unknown:?}")
    })?;
    let taken = store(s.answer("e1", "a", Answer::Acknowledged, Some(b"reply"), 6))?;
    check(taken == Answered::Taken, || {
        format!("a good answer was {taken:?}")
    })?;
    let a = store(s.command("a"))?.ok_or("a is gone")?;
    check(
        a.state == CommandState::Acked
            && a.done_unix == Some(6)
            && a.response.as_deref() == Some(&b"reply"[..]),
        || format!("a after its acknowledgement: {a:?}"),
    )?;
    let twice = store(s.answer("e1", "a", Answer::Error, None, 7))?;
    check(twice == Answered::Dropped, || {
        format!("a second answer was {twice:?}")
    })?;
    check(state_of(s, "a")? == Some(CommandState::Acked), || {
        "a second answer changed an acknowledged command".to_owned()
    })?;
    check(
        store(s.claim("e1", &[], 8))?.is_some_and(|c| c.id == "b"),
        || "the claim after a did not take b".to_owned(),
    )?;
    let deferred = store(s.answer("e1", "b", Answer::NotNow, None, 9))?;
    check(deferred == Answered::Taken, || {
        format!("not-now was {deferred:?}")
    })?;
    let b = store(s.command("b"))?.ok_or("b is gone")?;
    check(
        b.state == CommandState::Queued && b.deferred && b.done_unix.is_none(),
        || format!("b after not-now: {b:?}"),
    )?;
    let next = store(s.claim("e1", &["b".to_owned()], 10))?.ok_or("c was not claimed")?;
    check(next.id == "c", || {
        format!(
            "a deferred command was handed back on its connection: {}",
            next.id
        )
    })?;
    let again = store(s.claim("e1", &["c".to_owned()], 11))?.ok_or("b was not claimed")?;
    check(
        again.id == "b" && !again.deferred && again.deliveries == 2,
        || format!("b on its next connection: {again:?}"),
    )?;
    let failed = store(s.answer("e1", "b", Answer::Error, None, 12))?;
    check(failed == Answered::Taken, || {
        format!("error was {failed:?}")
    })?;
    check(state_of(s, "b")? == Some(CommandState::Errored), || {
        "an error answer did not end the command".to_owned()
    })
}

fn a_timed_out_command_is_queued_again(subject: &dyn Subject) -> Outcome {
    let s = subject.live();
    let s = &*s;
    store(s.replace_live(&enrollment("e1", "SER1", None, 1)))?;
    for id in ["x", "y", "z"] {
        store(s.enqueue(&command(id, "e1", b"p", 2)))?;
    }
    store(s.claim("e1", &[], 100))?;
    store(s.claim("e1", &[], 200))?;
    store(s.answer("e1", "x", Answer::Acknowledged, None, 101))?;
    let none = store(s.requeue_timed_out(99))?;
    check(none.is_empty(), || {
        format!("requeued {none:?} before any timed out")
    })?;
    check(state_of(s, "y")? == Some(CommandState::Sent), || {
        "y left sent before its time".to_owned()
    })?;
    let back = store(s.requeue_timed_out(200))?;
    check(back == [("y".to_owned(), "e1".to_owned())], || {
        format!("requeued {back:?}, not y alone")
    })?;
    let y = store(s.command("y"))?.ok_or("y is gone")?;
    check(
        y.state == CommandState::Queued && y.deliveries == 1 && !y.deferred,
        || format!("y after the timeout: {y:?}"),
    )?;
    check(state_of(s, "x")? == Some(CommandState::Acked), || {
        "an acknowledged command was queued again".to_owned()
    })?;
    let resent = store(s.claim("e1", &[], 300))?.ok_or("nothing to claim")?;
    check(resent.id == "y" && resent.deliveries == 2, || {
        format!("the claim after the timeout took {resent:?}")
    })
}

fn the_push_state_follows_pushes_and_contacts(subject: &dyn Subject) -> Outcome {
    let s = subject.live();
    let s = &*s;
    store(s.replace_live(&enrollment("e1", "SER1", None, 1)))?;
    let set = store(s.set_push("e1", b"token", "magic", "com.apple.mgmt.test"))?;
    check(set, || "the push token was not set".to_owned())?;
    let missing = store(s.set_push("nobody", b"t", "m", "p"))?;
    check(!missing, || "a push token was set for nobody".to_owned())?;
    store(s.record_push("e1", 50))?;
    let pushed = store(s.enrollment("e1"))?.ok_or("e1 is gone")?;
    check(
        pushed.push_outstanding
            && pushed.last_push_unix == Some(50)
            && pushed.push_token.as_deref() == Some(&b"token"[..])
            && pushed.push_magic.as_deref() == Some("magic")
            && pushed.topic.as_deref() == Some("com.apple.mgmt.test"),
        || format!("after a push: {pushed:?}"),
    )?;
    store(s.record_contact("e1", 60))?;
    let seen = store(s.enrollment("e1"))?.ok_or("e1 is gone")?;
    check(
        !seen.push_outstanding
            && seen.last_seen_unix == Some(60)
            && seen.last_push_unix == Some(50),
        || format!("after a contact: {seen:?}"),
    )
}

fn a_challenge_is_consumed_once(subject: &dyn Subject) -> Outcome {
    let s = subject.live();
    let s = &*s;
    store(s.issue_challenge(&challenge("h1", "SER1", 1_000)))?;
    let twice = s.issue_challenge(&challenge("h1", "SER2", 5_000));
    check(twice == Err(MdmError::ChallengeExists), || {
        format!("a challenge issued twice: {twice:?}")
    })?;
    let row = store(s.challenge("h1"))?.ok_or("the challenge is gone")?;
    check(
        row.serial == "SER1" && row.state == IdentityState::Challenged && row.issued.is_none(),
        || format!("the challenge row: {row:?}"),
    )?;
    let want = |got: Consumed, want: Consumed, what: &str| {
        check(got == want, || format!("{what}: {got:?}, not {want:?}"))
    };
    let first = issuance("c1", b"der-one");
    want(
        store(s.consume_challenge("nope", "k1", &first, 10))?,
        Consumed::Refused(Refusal::Unknown),
        "an unknown challenge",
    )?;
    want(
        store(s.consume_challenge("h1", "k1", &first, 1_000))?,
        Consumed::Refused(Refusal::Expired),
        "an expired challenge",
    )?;
    check(
        store(s.challenge("h1"))?.is_some_and(|r| r.state == IdentityState::Challenged),
        || "an expired request consumed the challenge".to_owned(),
    )?;
    want(
        store(s.consume_challenge("h1", "k1", &first, 10))?,
        Consumed::Issued,
        "the first request",
    )?;
    let by_cert = store(s.identity("c1"))?.ok_or("no identity by its certificate")?;
    check(
        by_cert.challenge_sha256 == "h1"
            && by_cert.key_sha256.as_deref() == Some("k1")
            && by_cert.state == IdentityState::Issued
            && by_cert.issued_unix == Some(10)
            && by_cert.issued.as_ref() == Some(&first),
        || format!("the identity after issue: {by_cert:?}"),
    )?;
    want(
        store(s.consume_challenge("h1", "k1", &issuance("c9", b"other"), 20))?,
        Consumed::Replayed(first.clone()),
        "a repeat with the same key",
    )?;
    want(
        store(s.consume_challenge("h1", "k2", &issuance("c8", b"other"), 20))?,
        Consumed::Refused(Refusal::KeyMismatch),
        "another key on a consumed challenge",
    )?;
    check(store(s.identity("c8"))?.is_none(), || {
        "a refused request recorded a certificate".to_owned()
    })?;
    check(store(s.kill_identity("c1"))?, || {
        "a certificate was not killed".to_owned()
    })?;
    check(!store(s.kill_identity("c-unknown"))?, || {
        "an unknown certificate was killed".to_owned()
    })?;
    want(
        store(s.consume_challenge("h1", "k1", &first, 30))?,
        Consumed::Refused(Refusal::Ended),
        "a dead identity",
    )?;
    check(store(s.identity("c-unknown"))?.is_none(), || {
        "an identity was found for an unknown certificate".to_owned()
    })
}

fn the_abm_table_follows_the_sync(subject: &dyn Subject) -> Outcome {
    let s = subject.live();
    let s = &*s;
    check(
        store(s.abm_state())?.is_none() && store(s.abm_devices())?.is_empty(),
        || "the ABM tables are not empty at first".to_owned(),
    )?;
    let state = |cursor: &str| AbmState {
        cursor: cursor.to_owned(),
        profile_uuid: Some("profile-1".to_owned()),
        profile_digest: Some("digest".to_owned()),
        synced_unix: Some(100),
    };
    store(s.abm_apply(&AbmBatch {
        upserts: vec![device("ZZZ", "empty", 100), device("AAA", "assigned", 100)],
        state: Some(state("cursor-1")),
        ..AbmBatch::default()
    }))?;
    let serials: Vec<String> = store(s.abm_devices())?
        .into_iter()
        .map(|d| d.serial)
        .collect();
    check(serials == ["AAA", "ZZZ"], || {
        format!("the devices are {serials:?}")
    })?;
    check(store(s.abm_state())? == Some(state("cursor-1")), || {
        "the state is not the one written".to_owned()
    })?;
    store(s.abm_apply(&AbmBatch {
        removed: vec!["AAA".to_owned(), "NOBODY".to_owned()],
        removed_unix: 150,
        ..AbmBatch::default()
    }))?;
    let removed = store(s.abm_device("AAA"))?.ok_or("AAA is gone")?;
    check(removed.removed_unix == Some(150), || {
        format!("AAA after removal: {removed:?}")
    })?;
    check(store(s.abm_device("NOBODY"))?.is_none(), || {
        "a removal made a device of an unknown serial".to_owned()
    })?;
    check(store(s.abm_state())? == Some(state("cursor-1")), || {
        "a batch with no state changed the state".to_owned()
    })?;
    store(s.abm_apply(&AbmBatch {
        upserts: vec![AbmDevice {
            synced_unix: 200,
            ..device("AAA", "pushed", 200)
        }],
        state: Some(state("cursor-2")),
        ..AbmBatch::default()
    }))?;
    let back = store(s.abm_device("AAA"))?.ok_or("AAA is gone")?;
    check(
        back.profile_status == "pushed"
            && back.removed_unix.is_none()
            && back.first_seen_unix == 100
            && back.synced_unix == 200,
        || format!("AAA after it came back: {back:?}"),
    )?;
    check(store(s.abm_state())? == Some(state("cursor-2")), || {
        "the second state did not replace the first".to_owned()
    })
}

fn a_failed_change_is_not_made(subject: &dyn Subject) -> Outcome {
    let s = subject.live();
    let s = &*s;
    store(s.replace_live(&enrollment("e1", "SER1", None, 1)))?;
    subject.failing(true);
    let refused = s.enqueue(&command("q", "e1", b"x", 2));
    subject.failing(false);
    check(matches!(refused, Err(MdmError::Store(_))), || {
        format!("a change that cannot be written answered {refused:?}")
    })?;
    check(store(s.command("q"))?.is_none(), || {
        "a change that failed is held in memory".to_owned()
    })?;
    let after = store(s.enqueue(&command("q", "e1", b"x", 3)))?;
    check(after == Enqueued::Queued, || {
        format!("the store did not take a change after a failed one: {after:?}")
    })
}

fn what_was_written_is_there_after_a_restart(subject: &dyn Subject) -> Outcome {
    let s = subject.live();
    let s = &*s;
    store(s.replace_live(&enrollment("e1", "SER1", Some("c1"), 1)))?;
    store(s.set_push("e1", b"token", "magic", "topic"))?;
    store(s.issue_challenge(&challenge("h1", "SER1", 1_000)))?;
    store(s.consume_challenge("h1", "k1", &issuance("c1", b"der"), 5))?;
    store(s.enqueue(&command("a", "e1", b"a", 6)))?;
    store(s.enqueue(&command("b", "e1", b"b", 6)))?;
    store(s.claim("e1", &[], 7))?;
    store(s.abm_apply(&AbmBatch {
        upserts: vec![device("AAA", "pushed", 8)],
        state: Some(AbmState {
            cursor: "c".to_owned(),
            profile_uuid: None,
            profile_digest: None,
            synced_unix: None,
        }),
        ..AbmBatch::default()
    }))?;
    let r = subject.restarted();
    let r = &*r;
    check(
        store(r.enrollment("e1"))? == store(s.enrollment("e1"))?,
        || "the enrollment differs after a restart".to_owned(),
    )?;
    check(store(r.commands("e1"))? == store(s.commands("e1"))?, || {
        "the commands differ after a restart".to_owned()
    })?;
    check(
        store(r.challenge("h1"))? == store(s.challenge("h1"))?,
        || "the identity differs after a restart".to_owned(),
    )?;
    check(store(r.abm_devices())? == store(s.abm_devices())?, || {
        "the ABM devices differ after a restart".to_owned()
    })?;
    check(store(r.abm_state())? == store(s.abm_state())?, || {
        "the ABM state differs after a restart".to_owned()
    })?;
    let c = store(r.enqueue(&command("c", "e1", b"c", 9)))?;
    check(c == Enqueued::Queued, || {
        format!("a restarted store enqueued: {c:?}")
    })?;
    let seqs: Vec<i64> = store(r.commands("e1"))?
        .into_iter()
        .map(|c| c.seq)
        .collect();
    check(
        seqs.len() == 3 && seqs.windows(2).all(|w| w[0] < w[1]),
        || format!("the order of the commands is not kept: {seqs:?}"),
    )
}

const CLAUSES: [(&str, Clause); 9] = [
    (
        "one_live_enrollment_per_serial",
        one_live_enrollment_per_serial,
    ),
    (
        "enqueue_is_idempotent_and_claim_is_ordered",
        enqueue_is_idempotent_and_claim_is_ordered,
    ),
    (
        "an_answer_changes_only_a_command_sent_to_its_enrollment",
        an_answer_changes_only_a_command_sent_to_its_enrollment,
    ),
    (
        "a_timed_out_command_is_queued_again",
        a_timed_out_command_is_queued_again,
    ),
    (
        "the_push_state_follows_pushes_and_contacts",
        the_push_state_follows_pushes_and_contacts,
    ),
    ("a_challenge_is_consumed_once", a_challenge_is_consumed_once),
    (
        "the_abm_table_follows_the_sync",
        the_abm_table_follows_the_sync,
    ),
    ("a_failed_change_is_not_made", a_failed_change_is_not_made),
    (
        "what_was_written_is_there_after_a_restart",
        what_was_written_is_there_after_a_restart,
    ),
];

/// Runs every clause on a subject of its own, made by `make` with the clause's name: the names of
/// the clauses that failed, each with what it found.
fn run(make: &dyn Fn(&str) -> Box<dyn Subject>) -> Vec<(&'static str, String)> {
    CLAUSES
        .iter()
        .filter_map(|(name, clause)| clause(&*make(name)).err().map(|why| (*name, why)))
        .collect()
}

// ---- the subjects --------------------------------------------------------------------------

/// The same files store handed out more than once: the live store, and what a call borrows.
struct Handle(Arc<FileMdm>);

macro_rules! forward {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)+) => {
        $(fn $name(&self, $($arg: $ty),*) -> $ret { self.0.$name($($arg),*) })+
    };
}

impl MdmStore for Handle {
    forward! {
        replace_live(new: &Enrollment) -> Result<Vec<String>, MdmError>;
        check_out(id: &str, now: i64) -> Result<bool, MdmError>;
        set_push(id: &str, token: &[u8], magic: &str, topic: &str) -> Result<bool, MdmError>;
        record_push(id: &str, now: i64) -> Result<(), MdmError>;
        record_contact(id: &str, now: i64) -> Result<(), MdmError>;
        enrollment(id: &str) -> Result<Option<Enrollment>, MdmError>;
        live_enrollment(serial: &str) -> Result<Option<Enrollment>, MdmError>;
        issue_challenge(c: &Challenge) -> Result<(), MdmError>;
        consume_challenge(h: &str, k: &str, i: &Issuance, now: i64) -> Result<Consumed, MdmError>;
        challenge(h: &str) -> Result<Option<Identity>, MdmError>;
        identity(c: &str) -> Result<Option<Identity>, MdmError>;
        kill_identity(c: &str) -> Result<bool, MdmError>;
        enqueue(c: &NewCommand) -> Result<Enqueued, MdmError>;
        claim(id: &str, tried: &[String], now: i64) -> Result<Option<Command>, MdmError>;
        answer(e: &str, id: &str, a: Answer, r: Option<&[u8]>, now: i64) -> Result<Answered, MdmError>;
        requeue_timed_out(before: i64) -> Result<Vec<(String, String)>, MdmError>;
        command(id: &str) -> Result<Option<Command>, MdmError>;
        commands(id: &str) -> Result<Vec<Command>, MdmError>;
        abm_apply(b: &AbmBatch) -> Result<(), MdmError>;
        abm_device(serial: &str) -> Result<Option<AbmDevice>, MdmError>;
        abm_devices() -> Result<Vec<AbmDevice>, MdmError>;
        abm_state() -> Result<Option<AbmState>, MdmError>;
    }
}

/// The files: a directory of the suite's own.
struct OnFiles {
    dir: Scratch,
    live: Arc<FileMdm>,
}

impl OnFiles {
    fn new(name: &str) -> Self {
        let dir = scratch(&format!("mdm-conformance-{name}"));
        let live = Arc::new(FileMdm::open(&dir.path().join("mdm")).expect("the files open"));
        Self { dir, live }
    }

    fn blocker(&self) -> PathBuf {
        self.dir.path().join("mdm").join("commands.json.new")
    }
}

impl Subject for OnFiles {
    fn live(&self) -> Box<dyn MdmStore> {
        Box::new(Handle(Arc::clone(&self.live)))
    }

    fn restarted(&self) -> Box<dyn MdmStore> {
        Box::new(FileMdm::open(&self.dir.path().join("mdm")).expect("the files open again"))
    }

    // A directory where the new file is written makes the write of the commands fail.
    fn failing(&self, on: bool) {
        if on {
            fs::create_dir(self.blocker()).expect("the blocker is made");
        } else {
            fs::remove_dir(self.blocker()).expect("the blocker is removed");
        }
    }
}

/// Postgres: a database of the suite's own.
struct OnPostgres {
    database: TestDatabase,
    store: Postgres,
}

impl OnPostgres {
    fn new() -> Self {
        let database = TestDatabase::new();
        let (store, _) = database.open("mdm-conformance").expect("the store opens");
        Self { database, store }
    }
}

impl Subject for OnPostgres {
    fn live(&self) -> Box<dyn MdmStore> {
        Box::new(PgMdm::over(&self.store))
    }

    fn restarted(&self) -> Box<dyn MdmStore> {
        // Every row is committed when a call returns, so what a started store reads is what this
        // one reads.
        Box::new(PgMdm::over(&self.store))
    }

    fn failing(&self, on: bool) {
        let sql = if on {
            "CREATE FUNCTION test_refuse() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN RAISE EXCEPTION 'the test refuses this change'; END $$; \
             CREATE TRIGGER test_refuse BEFORE INSERT ON mdm_commands FOR EACH ROW \
             EXECUTE FUNCTION test_refuse()"
        } else {
            "DROP TRIGGER test_refuse ON mdm_commands; DROP FUNCTION test_refuse()"
        };
        self.database.run(sql).expect("the refusal is set");
    }
}

/// How a store is wrong: the clauses that name it must fail, and no other.
#[derive(Clone, Copy, Debug)]
enum Flaw {
    /// A claim ignores the commands the connection already tried.
    IgnoresTried,
    /// An answer is taken from any enrollment.
    AnswersFromAnyone,
    /// A consumed challenge gives its certificate to any key.
    ReplaysToAnyKey,
    /// Nothing written is there after a restart.
    ForgetsOnRestart,
    /// The timeout never queues a command again.
    NeverRequeues,
}

/// A files store with one flaw.
struct Flawed {
    inner: Handle,
    flaw: Flaw,
}

macro_rules! delegate {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)+) => {
        $(fn $name(&self, $($arg: $ty),*) -> $ret { self.inner.$name($($arg),*) })+
    };
}

impl MdmStore for Flawed {
    fn claim(&self, id: &str, tried: &[String], now: i64) -> Result<Option<Command>, MdmError> {
        let tried = if matches!(self.flaw, Flaw::IgnoresTried) {
            &[]
        } else {
            tried
        };
        self.inner.claim(id, tried, now)
    }

    fn answer(
        &self,
        enrollment_id: &str,
        id: &str,
        answer: Answer,
        response: Option<&[u8]>,
        now: i64,
    ) -> Result<Answered, MdmError> {
        let who = match (self.flaw, self.inner.command(id)?) {
            (Flaw::AnswersFromAnyone, Some(c)) => c.enrollment_id,
            _ => enrollment_id.to_owned(),
        };
        self.inner.answer(&who, id, answer, response, now)
    }

    fn consume_challenge(
        &self,
        hash: &str,
        key: &str,
        issuance: &Issuance,
        now: i64,
    ) -> Result<Consumed, MdmError> {
        let stored = self.inner.challenge(hash)?.and_then(|row| row.key_sha256);
        let key = match (self.flaw, stored) {
            (Flaw::ReplaysToAnyKey, Some(stored)) => stored,
            _ => key.to_owned(),
        };
        self.inner.consume_challenge(hash, &key, issuance, now)
    }

    fn requeue_timed_out(&self, before: i64) -> Result<Vec<(String, String)>, MdmError> {
        if matches!(self.flaw, Flaw::NeverRequeues) {
            return Ok(Vec::new());
        }
        self.inner.requeue_timed_out(before)
    }

    delegate! {
        replace_live(new: &Enrollment) -> Result<Vec<String>, MdmError>;
        check_out(id: &str, now: i64) -> Result<bool, MdmError>;
        set_push(id: &str, token: &[u8], magic: &str, topic: &str) -> Result<bool, MdmError>;
        record_push(id: &str, now: i64) -> Result<(), MdmError>;
        record_contact(id: &str, now: i64) -> Result<(), MdmError>;
        enrollment(id: &str) -> Result<Option<Enrollment>, MdmError>;
        live_enrollment(serial: &str) -> Result<Option<Enrollment>, MdmError>;
        issue_challenge(c: &Challenge) -> Result<(), MdmError>;
        challenge(h: &str) -> Result<Option<Identity>, MdmError>;
        identity(c: &str) -> Result<Option<Identity>, MdmError>;
        kill_identity(c: &str) -> Result<bool, MdmError>;
        enqueue(c: &NewCommand) -> Result<Enqueued, MdmError>;
        command(id: &str) -> Result<Option<Command>, MdmError>;
        commands(id: &str) -> Result<Vec<Command>, MdmError>;
        abm_apply(b: &AbmBatch) -> Result<(), MdmError>;
        abm_device(serial: &str) -> Result<Option<AbmDevice>, MdmError>;
        abm_devices() -> Result<Vec<AbmDevice>, MdmError>;
        abm_state() -> Result<Option<AbmState>, MdmError>;
    }
}

struct OnFlawed {
    flaw: Flaw,
    files: OnFiles,
}

impl Subject for OnFlawed {
    fn live(&self) -> Box<dyn MdmStore> {
        Box::new(Flawed {
            inner: Handle(Arc::clone(&self.files.live)),
            flaw: self.flaw,
        })
    }

    fn restarted(&self) -> Box<dyn MdmStore> {
        let path = if matches!(self.flaw, Flaw::ForgetsOnRestart) {
            self.files.dir.path().join("another")
        } else {
            self.files.dir.path().join("mdm")
        };
        Box::new(Flawed {
            inner: Handle(Arc::new(
                FileMdm::open(&path).expect("the files open again"),
            )),
            flaw: self.flaw,
        })
    }

    fn failing(&self, on: bool) {
        self.files.failing(on);
    }
}

/// Proves: the files and Postgres each keep every clause of the MDM store's contract: one live
/// enrollment per serial that retires its predecessor's commands and identity, an idempotent
/// enqueue and an ordered claim that skips what a connection tried, answers taken only for a
/// command sent to their enrollment, a timeout that queues a command again, the push state, a
/// challenge consumed once with a stored certificate for a repeat, the ABM table, a failed change
/// that is not made, and everything written there after a restart.
///
/// Catches: a store that takes two live enrollments of a serial, strands a replaced enrollment's
/// commands, hands a deferred command back on its connection, accepts an answer for a command that
/// is not sent or is another enrollment's, or issues two certificates under one challenge.
#[test]
fn the_conformance_suite_passes_on_the_files_and_postgres() {
    assert_eq!(run(&|name| Box::new(OnFiles::new(name))), []);
    assert_eq!(run(&|_| Box::new(OnPostgres::new())), []);
}

/// Proves: the suite catches what it claims. Against a store wrong in one way each, the clauses
/// that name the fault fail, and every other clause passes.
///
/// Catches: a clause that passes whatever the store does, or one that fails for a fault that is
/// another clause's.
#[test]
fn the_suite_fails_against_a_store_that_breaks_each_clause() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let cases: [(Flaw, &[&str]); 5] = [
        (
            Flaw::IgnoresTried,
            &[
                "enqueue_is_idempotent_and_claim_is_ordered",
                "an_answer_changes_only_a_command_sent_to_its_enrollment",
            ],
        ),
        (
            Flaw::AnswersFromAnyone,
            &["an_answer_changes_only_a_command_sent_to_its_enrollment"],
        ),
        (Flaw::ReplaysToAnyKey, &["a_challenge_is_consumed_once"]),
        (
            Flaw::ForgetsOnRestart,
            &["what_was_written_is_there_after_a_restart"],
        ),
        (
            Flaw::NeverRequeues,
            &["a_timed_out_command_is_queued_again"],
        ),
    ];
    for (flaw, clauses) in cases {
        let run_id = NEXT.fetch_add(1, Ordering::Relaxed);
        let failed: Vec<&str> = run(&|name| {
            Box::new(OnFlawed {
                flaw,
                files: OnFiles::new(&format!("flawed-{run_id}-{name}")),
            })
        })
        .into_iter()
        .map(|(name, _)| name)
        .collect();
        assert_eq!(failed, clauses, "{flaw:?}");
    }
}
