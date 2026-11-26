//! An upload that resumes (docs/design/xcode-one-action.md section 2.4). Before the body, `hk`
//! asks `GET /v1/xcode/<sha256>/upload` what the coordinator holds of the archive: `none` (send
//! from byte 0), `partial` (send from the bytes it holds) or `held` (the archive is stored: there
//! is nothing to send). The rest of the archive goes in one `PUT` with
//! `Content-Range: bytes <first>-<last>/<size>`, not in parts: a request that is cut off keeps
//! what the coordinator received of it, synced, so a cut costs the bytes in flight and no round
//! trip is paid for each part.
//!
//! A try that ends in a broken connection, `409 offset_mismatch` (whose answer carries the
//! offset to go on from) or `409 partial_locked` (another writer's lock, not yet stale) is
//! followed by a wait, then, but for `offset_mismatch`, by the query again, and a new try from the
//! offset it reports; a `202` (a part accepted, bytes still due) goes on at once from its
//! `received`. There are [`TRIES`] tries in all; the waits between them double from
//! [`FIRST_WAIT_SECS`] and are made by the world's wait ([`crate::World::tailnet`]), which a test
//! replaces. Then the run stops saying how far the upload got, and the next run continues, since
//! the coordinator keeps the partial upload.
//!
//! A partial that holds every byte of the archive is finished by sending the archive's last byte
//! again (`Content-Range: bytes <size-1>-<size-1>/<size>`): the store keeps what it holds, hashes
//! it and commits it.
//!
//! A `405` of the query is a coordinator that cannot resume (its route takes `PUT` alone): the
//! archive goes in one request, as it did before. A `404` is a coordinator with no Xcode store.
//! Any other status of the query, and every other answer of a `PUT`, ends the run as a refusal
//! ([`super::upload::refused`]).

use std::time::Duration;

use serde::Deserialize;

use super::upload::{Archive, Sent, refused, send, whole};
use super::{Target, say};
use crate::xcode_add::progress::of_text;
use crate::{Flow, PROGRAM, Stop, World};

/// The tries of an upload, the first included.
pub(crate) const TRIES: u32 = 5;

/// The seconds waited before the second try; each wait after it is twice the one before.
pub(crate) const FIRST_WAIT_SECS: u64 = 2;

/// What the store holds of the archive's bytes.
enum Holds {
    /// An archive of these bytes: stored already.
    Archive,
    /// No archive and no part of one.
    Nothing,
    /// The first this many bytes.
    Partial(u64),
}

/// The answer of `GET /v1/xcode/<sha256>/upload`.
#[derive(Deserialize)]
struct Query {
    state: String,
    #[serde(default)]
    received: u64,
}

/// The answer to a `PUT` that carries an offset or a code: `{"received": <n>}` of a `202`, and
/// `{"error": "<code>", "received": <n>}` of an `offset_mismatch`.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Reply {
    error: Option<String>,
    received: Option<u64>,
}

/// How one try ended.
enum Try {
    /// The store answered `201`.
    Stored,
    /// Go on: from `at` when the store said where, else from what the query says; after a wait when
    /// `pause`.
    Again {
        at: Option<u64>,
        pause: bool,
        why: String,
    },
}

/// What the query got: the store's state, or the status and body of any other answer, or no
/// answer.
enum Asked {
    Holds(Holds),
    Status(u16, Vec<u8>),
    Cut(String),
}

/// `GET /v1/xcode/<sha256>/upload`.
fn ask(target: &Target, archive: &Archive<'_>) -> Asked {
    let timeout = Duration::from_secs(u64::from(target.timeout_secs));
    let url = format!("{}/v1/xcode/{}/upload", target.endpoint, archive.sha256);
    let sent = target
        .agent
        .get(&url)
        .config()
        .timeout_global(Some(timeout))
        .build()
        .header("Authorization", format!("Bearer {}", target.token))
        .call();
    let mut response = match sent {
        Ok(response) => response,
        Err(error) => return Asked::Cut(crate::send::one_line(&error.to_string())),
    };
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .with_config()
        .limit(super::upload::MAX_ANSWER_BYTES)
        .read_to_vec()
        .unwrap_or_default();
    if status != 200 {
        return Asked::Status(status, text);
    }
    match serde_json::from_slice::<Query>(&text) {
        Ok(Query { state, .. }) if state == "held" => Asked::Holds(Holds::Archive),
        Ok(Query { state, .. }) if state == "none" => Asked::Holds(Holds::Nothing),
        Ok(Query { state, received }) if state == "partial" => {
            Asked::Holds(Holds::Partial(received))
        }
        _ => Asked::Status(status, text),
    }
}

/// The offset `at` the store reports, which must lie within the archive.
fn within(at: u64, archive: &Archive<'_>) -> Flow<u64> {
    if at > archive.size {
        return Err(Stop::fail(format!(
            "the store reports {at} bytes of this archive, which has {}",
            archive.size
        )));
    }
    Ok(at)
}

/// One `PUT` of the rest of the archive from `at`, and how it ended.
fn attempt(w: &mut World<'_>, target: &Target, archive: &mut Archive<'_>, at: u64) -> Flow<Try> {
    // A store that holds every byte (a partial whose last block was synced and whose commit did
    // not happen) has no byte left to be sent, and no range of none: the last byte is sent again,
    // which tells the store to hash what it holds and commit it.
    let from = if at >= archive.size {
        archive.size.saturating_sub(1)
    } else {
        at
    };
    let (status, text) = match send(w, target, archive, from, true)? {
        Sent::Cut(why) => {
            return Ok(Try::Again {
                at: None,
                pause: true,
                why: format!("the connection broke ({why})"),
            });
        }
        Sent::Answer { status, text } => (status, text),
    };
    let reply: Reply = serde_json::from_slice(&text).unwrap_or_default();
    match (status, reply.error.as_deref(), reply.received) {
        (201, ..) => Ok(Try::Stored),
        (202, _, Some(received)) => Ok(Try::Again {
            at: Some(within(received, archive)?),
            pause: false,
            why: format!(
                "the store accepted a part, {} held",
                of_text(received, archive.size)
            ),
        }),
        (409, Some("offset_mismatch"), Some(received)) => Ok(Try::Again {
            at: Some(within(received, archive)?),
            pause: true,
            why: "the store holds another number of bytes than were sent from".to_owned(),
        }),
        (409, Some("partial_locked"), _) => Ok(Try::Again {
            at: None,
            pause: true,
            why: "another upload of these bytes holds the store's lock".to_owned(),
        }),
        _ => Err(refused(w, status, &text, archive)),
    }
}

/// Uploads `archive`: asks what the store holds, then sends the rest, up to [`TRIES`] times.
pub(super) fn put(w: &mut World<'_>, target: &Target, archive: &mut Archive<'_>) -> Flow<()> {
    let held = match ask(target, archive) {
        Asked::Holds(held) => held,
        Asked::Cut(why) => {
            return Err(Stop::fail(format!(
                "no answer from {} ({why}); nothing was uploaded",
                target.endpoint
            )));
        }
        Asked::Status(404, _) => {
            return Err(Stop::fail(
                "the coordinator has no Xcode store: it was started without --xcode-dir",
            ));
        }
        Asked::Status(405, _) => {
            say(
                w,
                "this coordinator cannot resume an upload: it takes the archive in one request, \
                 and keeps nothing of one that does not end",
            );
            return whole(w, target, archive);
        }
        Asked::Status(status, text) => return Err(refused(w, status, &text, archive)),
    };
    let first = match held {
        // Stored since the store was listed, by another run: the bytes are named by their
        // SHA-256, so what the store holds is this archive.
        Holds::Archive => return Ok(()),
        Holds::Nothing => 0,
        Holds::Partial(received) => within(received, archive)?,
    };
    // Where the next try goes on from, when the store has said; else the query says.
    let mut at = Some(first);
    // The last offset the store reported, which the stop says.
    let mut known = first;
    let mut pause = false;
    let mut waits = 0_u32;
    let mut why = String::new();
    for tried in 1..=TRIES {
        if tried > 1 {
            let secs = FIRST_WAIT_SECS.saturating_mul(2_u64.saturating_pow(waits));
            if pause {
                waits = waits.saturating_add(1);
                say(
                    w,
                    &format!("{why}; try {tried} of {TRIES} in {secs} seconds"),
                );
                (w.tailnet.wait)(Duration::from_secs(secs));
            } else {
                say(w, &format!("{why}; try {tried} of {TRIES}"));
            }
        }
        let from = match at {
            Some(from) => from,
            None => match ask(target, archive) {
                Asked::Holds(Holds::Archive) => return Ok(()),
                Asked::Holds(Holds::Nothing) => 0,
                Asked::Holds(Holds::Partial(received)) => within(received, archive)?,
                Asked::Cut(because) => {
                    why = format!("no answer to the question what the store holds ({because})");
                    pause = true;
                    continue;
                }
                Asked::Status(status, text) => return Err(refused(w, status, &text, archive)),
            },
        };
        known = from;
        match attempt(w, target, archive, from)? {
            Try::Stored => return Ok(()),
            Try::Again {
                at: next,
                pause: waited,
                why: because,
            } => {
                at = next;
                pause = waited;
                why = because;
                if let Some(next) = next {
                    known = next;
                }
            }
        }
    }
    say(w, &why);
    // The offset the store holds now, when it can say; else the last it said.
    match ask(target, archive) {
        Asked::Holds(Holds::Archive) => return Ok(()),
        Asked::Holds(Holds::Nothing) => known = 0,
        Asked::Holds(Holds::Partial(received)) => known = received.min(archive.size),
        Asked::Cut(_) | Asked::Status(..) => {}
    }
    Err(Stop::fail(format!(
        "the upload stopped at {}; run {PROGRAM} xcode add again to continue",
        of_text(known, archive.size)
    )))
}
