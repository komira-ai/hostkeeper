//! The MDM tables of migration `0010` behind [`MdmStore`]: `mdm_enrollments`, `mdm_identities`,
//! `mdm_commands`, `abm_devices` and `abm_state` (`docs/design/coordinator-mdm.md`, 9.1).
//!
//! A change is one transaction on the writing session, fenced by the lease as every commit is. A
//! call that is refused for a reason of its own (a challenge that exists, an enrollment that
//! ended) answers before its first write, so the transaction that commits has written nothing. A
//! claim is one statement, `UPDATE ... WHERE id = (SELECT ... FOR UPDATE SKIP LOCKED) RETURNING`,
//! so two connections of one Mac cannot take the same command. A read runs on the read-only
//! session and sees committed rows only.

use postgres::Row;

use super::Postgres;
use super::session::Session;
use super::writer::Tx;
use crate::store::mdm::{
    AbmBatch, AbmDevice, AbmState, Answer, Answered, Challenge, Command, CommandState, Consumed,
    Enqueued, Enrollment, EnrollmentState, Identity, IdentityState, Issuance, MdmError, MdmStore,
    NewCommand, Refusal, consume,
};

const ENROLLMENT: &str = "enrollment_id, serial, udid, state, cert_sha256, push_token, \
    push_magic, topic, push_outstanding, last_push_unix, last_seen_unix, created_unix";

const IDENTITY: &str = "challenge_sha256, serial, udid, expires_unix, confirmed, state, \
    key_sha256, cert_sha256, cert_der, not_after_unix, issued_unix";

const COMMAND: &str = "id, seq, enrollment_id, command_type, payload, state, deferred, \
    deliveries, created_unix, sent_unix, done_unix, response";

const DEVICE: &str = "serial, model, description, profile_status, profile_uuid, first_seen_unix, \
    synced_unix, removed_unix";

/// The MDM tables of a [`Postgres`] store.
pub(crate) struct PgMdm {
    store: Postgres,
}

impl PgMdm {
    /// The MDM tables of `store`.
    pub(crate) fn over(store: &Postgres) -> PgMdm {
        PgMdm {
            store: store.clone(),
        }
    }

    /// Runs `body` as one transaction.
    fn write<R: Send + 'static>(
        &self,
        body: impl FnOnce(&mut Tx<'_>) -> Result<R, postgres::Error> + Send + 'static,
    ) -> Result<R, MdmError> {
        self.store
            .write(body)
            .map_err(|fail| MdmError::Store(fail.why().to_owned()))
    }

    /// Runs `body` on the read-only session.
    fn read<R: Send + 'static>(
        &self,
        body: impl FnOnce(&mut Session) -> Result<R, postgres::Error> + Send + 'static,
    ) -> Result<R, MdmError> {
        self.store.read(body).map_err(MdmError::Store)
    }

    /// The rows `sql` answers with `text` as its one parameter, read by `of`.
    fn select<T: Send + 'static>(
        &self,
        sql: String,
        text: &str,
        of: fn(&Row) -> Result<T, MdmError>,
    ) -> Result<Vec<T>, MdmError> {
        let text = text.to_owned();
        let rows = self.read(move |s| s.query(&sql, &[&text]))?;
        rows.iter().map(of).collect()
    }
}

/// The state `text` spells, as the table holds it.
fn spelled<T>(text: &str, parse: fn(&str) -> Option<T>, of: &str) -> Result<T, MdmError> {
    parse(text).ok_or_else(|| {
        MdmError::Store(format!(
            "{of} holds a state this release does not know: {text}"
        ))
    })
}

fn enrollment_of(row: &Row) -> Result<Enrollment, MdmError> {
    Ok(Enrollment {
        enrollment_id: row.get(0),
        serial: row.get(1),
        udid: row.get(2),
        state: spelled(
            &row.get::<_, String>(3),
            EnrollmentState::parse,
            "mdm_enrollments",
        )?,
        cert_sha256: row.get(4),
        push_token: row.get(5),
        push_magic: row.get(6),
        topic: row.get(7),
        push_outstanding: row.get(8),
        last_push_unix: row.get(9),
        last_seen_unix: row.get(10),
        created_unix: row.get(11),
    })
}

fn identity_of(row: &Row) -> Result<Identity, MdmError> {
    let issued = match (row.get(7), row.get(8), row.get(9)) {
        (Some(cert_sha256), Some(cert_der), Some(not_after_unix)) => Some(Issuance {
            cert_sha256,
            cert_der,
            not_after_unix,
        }),
        _ => None,
    };
    Ok(Identity {
        challenge_sha256: row.get(0),
        serial: row.get(1),
        udid: row.get(2),
        expires_unix: row.get(3),
        confirmed: row.get(4),
        state: spelled(
            &row.get::<_, String>(5),
            IdentityState::parse,
            "mdm_identities",
        )?,
        key_sha256: row.get(6),
        issued,
        issued_unix: row.get(10),
    })
}

fn command_of(row: &Row) -> Result<Command, MdmError> {
    Ok(Command {
        id: row.get(0),
        seq: row.get(1),
        enrollment_id: row.get(2),
        command_type: row.get(3),
        payload: row.get(4),
        state: spelled(
            &row.get::<_, String>(5),
            CommandState::parse,
            "mdm_commands",
        )?,
        deferred: row.get(6),
        deliveries: row.get(7),
        created_unix: row.get(8),
        sent_unix: row.get(9),
        done_unix: row.get(10),
        response: row.get(11),
    })
}

fn device_of(row: &Row) -> Result<AbmDevice, MdmError> {
    Ok(AbmDevice {
        serial: row.get(0),
        model: row.get(1),
        description: row.get(2),
        profile_status: row.get(3),
        profile_uuid: row.get(4),
        first_seen_unix: row.get(5),
        synced_unix: row.get(6),
        removed_unix: row.get(7),
    })
}

/// Ends the enrollment `id` as `to`: its queued and sent commands are cancelled and its identity
/// retired.
fn end(tx: &mut Tx<'_>, id: &str, to: EnrollmentState, now: i64) -> Result<(), postgres::Error> {
    tx.execute(
        "UPDATE mdm_enrollments SET state = $2, push_outstanding = false WHERE enrollment_id = $1",
        &[&id, &to.as_str()],
    )?;
    tx.execute(
        "UPDATE mdm_commands SET state = 'cancelled', done_unix = $2 \
         WHERE enrollment_id = $1 AND state IN ('queued', 'sent')",
        &[&id, &now],
    )?;
    tx.execute(
        "UPDATE mdm_identities SET state = 'retired' WHERE cert_sha256 = \
         (SELECT cert_sha256 FROM mdm_enrollments WHERE enrollment_id = $1)",
        &[&id],
    )?;
    Ok(())
}

impl MdmStore for PgMdm {
    fn replace_live(&self, new: &Enrollment) -> Result<Vec<String>, MdmError> {
        let new = new.clone();
        self.write(move |tx| {
            let ended = tx.query(
                "SELECT 1 FROM mdm_enrollments WHERE enrollment_id = $1 AND state <> 'live'",
                &[&new.enrollment_id],
            )?;
            if !ended.is_empty() {
                return Ok(Err(MdmError::EnrollmentEnded(new.enrollment_id)));
            }
            let mut replaced: Vec<String> = tx
                .query(
                    "SELECT enrollment_id FROM mdm_enrollments \
                     WHERE serial = $1 AND state = 'live' AND enrollment_id <> $2",
                    &[&new.serial, &new.enrollment_id],
                )?
                .iter()
                .map(|row| row.get(0))
                .collect();
            replaced.sort();
            for id in &replaced {
                end(tx, id, EnrollmentState::Replaced, new.created_unix)?;
            }
            tx.execute(
                &format!(
                    "INSERT INTO mdm_enrollments ({ENROLLMENT}) VALUES \
                     ($1, $2, $3, 'live', $4, $5, $6, $7, $8, $9, $10, $11) \
                     ON CONFLICT (enrollment_id) DO UPDATE SET serial = EXCLUDED.serial, \
                     udid = EXCLUDED.udid, state = 'live', cert_sha256 = EXCLUDED.cert_sha256, \
                     push_token = EXCLUDED.push_token, push_magic = EXCLUDED.push_magic, \
                     topic = EXCLUDED.topic, push_outstanding = EXCLUDED.push_outstanding, \
                     last_push_unix = EXCLUDED.last_push_unix, \
                     last_seen_unix = EXCLUDED.last_seen_unix, \
                     created_unix = EXCLUDED.created_unix"
                ),
                &[
                    &new.enrollment_id,
                    &new.serial,
                    &new.udid,
                    &new.cert_sha256,
                    &new.push_token,
                    &new.push_magic,
                    &new.topic,
                    &new.push_outstanding,
                    &new.last_push_unix,
                    &new.last_seen_unix,
                    &new.created_unix,
                ],
            )?;
            Ok(Ok(replaced))
        })?
    }

    fn check_out(&self, enrollment_id: &str, now_unix: i64) -> Result<bool, MdmError> {
        let id = enrollment_id.to_owned();
        self.write(move |tx| {
            let live = tx.query(
                "SELECT 1 FROM mdm_enrollments WHERE enrollment_id = $1 AND state = 'live'",
                &[&id],
            )?;
            if live.is_empty() {
                return Ok(false);
            }
            end(tx, &id, EnrollmentState::CheckedOut, now_unix)?;
            Ok(true)
        })
    }

    fn set_push(
        &self,
        enrollment_id: &str,
        token: &[u8],
        magic: &str,
        topic: &str,
    ) -> Result<bool, MdmError> {
        let (id, token, magic, topic) = (
            enrollment_id.to_owned(),
            token.to_vec(),
            magic.to_owned(),
            topic.to_owned(),
        );
        self.write(move |tx| {
            tx.execute(
                "UPDATE mdm_enrollments SET push_token = $2, push_magic = $3, topic = $4 \
                 WHERE enrollment_id = $1 AND state = 'live'",
                &[&id, &token, &magic, &topic],
            )
            .map(|n| n > 0)
        })
    }

    fn record_push(&self, enrollment_id: &str, now_unix: i64) -> Result<(), MdmError> {
        let id = enrollment_id.to_owned();
        self.write(move |tx| {
            tx.execute(
                "UPDATE mdm_enrollments SET push_outstanding = true, last_push_unix = $2 \
                 WHERE enrollment_id = $1",
                &[&id, &now_unix],
            )
            .map(|_| ())
        })
    }

    fn record_contact(&self, enrollment_id: &str, now_unix: i64) -> Result<(), MdmError> {
        let id = enrollment_id.to_owned();
        self.write(move |tx| {
            tx.execute(
                "UPDATE mdm_enrollments SET push_outstanding = false, last_seen_unix = $2 \
                 WHERE enrollment_id = $1",
                &[&id, &now_unix],
            )
            .map(|_| ())
        })
    }

    fn enrollment(&self, enrollment_id: &str) -> Result<Option<Enrollment>, MdmError> {
        let sql = format!("SELECT {ENROLLMENT} FROM mdm_enrollments WHERE enrollment_id = $1");
        Ok(self.select(sql, enrollment_id, enrollment_of)?.pop())
    }

    fn live_enrollment(&self, serial: &str) -> Result<Option<Enrollment>, MdmError> {
        let sql = format!(
            "SELECT {ENROLLMENT} FROM mdm_enrollments WHERE serial = $1 AND state = 'live'"
        );
        Ok(self.select(sql, serial, enrollment_of)?.pop())
    }

    fn issue_challenge(&self, challenge: &Challenge) -> Result<(), MdmError> {
        let challenge = challenge.clone();
        let added = self.write(move |tx| {
            tx.execute(
                "INSERT INTO mdm_identities \
                 (challenge_sha256, serial, udid, expires_unix, confirmed, state) \
                 VALUES ($1, $2, $3, $4, $5, 'challenged') ON CONFLICT DO NOTHING",
                &[
                    &challenge.challenge_sha256,
                    &challenge.serial,
                    &challenge.udid,
                    &challenge.expires_unix,
                    &challenge.confirmed,
                ],
            )
        })?;
        if added == 0 {
            Err(MdmError::ChallengeExists)
        } else {
            Ok(())
        }
    }

    fn consume_challenge(
        &self,
        challenge_sha256: &str,
        key_sha256: &str,
        issuance: &Issuance,
        now_unix: i64,
    ) -> Result<Consumed, MdmError> {
        let (hash, key, issuance) = (
            challenge_sha256.to_owned(),
            key_sha256.to_owned(),
            issuance.clone(),
        );
        self.write(move |tx| {
            let rows = tx.query(
                &format!(
                    "SELECT {IDENTITY} FROM mdm_identities WHERE challenge_sha256 = $1 FOR UPDATE"
                ),
                &[&hash],
            )?;
            let Some(row) = rows.first() else {
                return Ok(Ok(Consumed::Refused(Refusal::Unknown)));
            };
            let mut identity = match identity_of(row) {
                Ok(identity) => identity,
                Err(e) => return Ok(Err(e)),
            };
            let consumed = consume(&mut identity, &key, &issuance, now_unix);
            if consumed == Consumed::Issued {
                tx.execute(
                    "UPDATE mdm_identities SET state = 'issued', key_sha256 = $2, \
                     cert_sha256 = $3, cert_der = $4, not_after_unix = $5, issued_unix = $6 \
                     WHERE challenge_sha256 = $1",
                    &[
                        &hash,
                        &key,
                        &issuance.cert_sha256,
                        &issuance.cert_der,
                        &issuance.not_after_unix,
                        &now_unix,
                    ],
                )?;
            }
            Ok(Ok(consumed))
        })?
    }

    fn challenge(&self, challenge_sha256: &str) -> Result<Option<Identity>, MdmError> {
        let sql = format!("SELECT {IDENTITY} FROM mdm_identities WHERE challenge_sha256 = $1");
        Ok(self.select(sql, challenge_sha256, identity_of)?.pop())
    }

    fn identity(&self, cert_sha256: &str) -> Result<Option<Identity>, MdmError> {
        let sql = format!("SELECT {IDENTITY} FROM mdm_identities WHERE cert_sha256 = $1");
        Ok(self.select(sql, cert_sha256, identity_of)?.pop())
    }

    fn kill_identity(&self, cert_sha256: &str) -> Result<bool, MdmError> {
        let cert = cert_sha256.to_owned();
        self.write(move |tx| {
            tx.execute(
                "UPDATE mdm_identities SET state = 'dead' WHERE cert_sha256 = $1",
                &[&cert],
            )
            .map(|n| n > 0)
        })
    }

    fn enqueue(&self, command: &NewCommand) -> Result<Enqueued, MdmError> {
        let command = command.clone();
        self.write(move |tx| {
            let added = tx.execute(
                "INSERT INTO mdm_commands \
                 (id, enrollment_id, command_type, payload, state, created_unix) \
                 SELECT $1, $2, $3, $4, 'queued', $5 WHERE EXISTS \
                 (SELECT 1 FROM mdm_enrollments WHERE enrollment_id = $2 AND state = 'live') \
                 ON CONFLICT (id) DO NOTHING",
                &[
                    &command.id,
                    &command.enrollment_id,
                    &command.command_type,
                    &command.payload,
                    &command.created_unix,
                ],
            )?;
            if added > 0 {
                return Ok(Enqueued::Queued);
            }
            let exists = tx.query("SELECT 1 FROM mdm_commands WHERE id = $1", &[&command.id])?;
            Ok(if exists.is_empty() {
                Enqueued::NotLive
            } else {
                Enqueued::Exists
            })
        })
    }

    fn claim(
        &self,
        enrollment_id: &str,
        tried: &[String],
        now_unix: i64,
    ) -> Result<Option<Command>, MdmError> {
        let (id, tried) = (enrollment_id.to_owned(), tried.to_vec());
        let rows = self.write(move |tx| {
            tx.query(
                &format!(
                    "UPDATE mdm_commands SET state = 'sent', deferred = false, \
                     deliveries = deliveries + 1, sent_unix = $3 WHERE id = \
                     (SELECT id FROM mdm_commands \
                      WHERE enrollment_id = $1 AND state = 'queued' AND NOT (id = ANY($2)) \
                      AND EXISTS (SELECT 1 FROM mdm_enrollments \
                                  WHERE enrollment_id = $1 AND state = 'live') \
                      ORDER BY seq LIMIT 1 FOR UPDATE SKIP LOCKED) RETURNING {COMMAND}"
                ),
                &[&id, &tried, &now_unix],
            )
        })?;
        rows.first().map(command_of).transpose()
    }

    fn answer(
        &self,
        enrollment_id: &str,
        id: &str,
        answer: Answer,
        response: Option<&[u8]>,
        now_unix: i64,
    ) -> Result<Answered, MdmError> {
        let (enrollment, id, response) = (
            enrollment_id.to_owned(),
            id.to_owned(),
            response.map(<[u8]>::to_vec),
        );
        let (state, deferred, done) = match answer {
            Answer::Acknowledged => (CommandState::Acked, false, Some(now_unix)),
            Answer::Error => (CommandState::Errored, false, Some(now_unix)),
            Answer::NotNow => (CommandState::Queued, true, None),
        };
        let taken = self.write(move |tx| {
            tx.execute(
                "UPDATE mdm_commands SET state = $3, deferred = $4, done_unix = $5, \
                 response = $6 WHERE id = $1 AND enrollment_id = $2 AND state = 'sent'",
                &[
                    &id,
                    &enrollment,
                    &state.as_str(),
                    &deferred,
                    &done,
                    &response,
                ],
            )
        })?;
        Ok(if taken > 0 {
            Answered::Taken
        } else {
            Answered::Dropped
        })
    }

    fn requeue_timed_out(&self, sent_before_unix: i64) -> Result<Vec<(String, String)>, MdmError> {
        let mut back: Vec<(i64, String, String)> = self
            .write(move |tx| {
                tx.query(
                    "UPDATE mdm_commands SET state = 'queued', deferred = false \
                     WHERE state = 'sent' AND sent_unix <= $1 RETURNING seq, id, enrollment_id",
                    &[&sent_before_unix],
                )
            })?
            .iter()
            .map(|row| (row.get(0), row.get(1), row.get(2)))
            .collect();
        back.sort();
        Ok(back.into_iter().map(|(_, id, e)| (id, e)).collect())
    }

    fn command(&self, id: &str) -> Result<Option<Command>, MdmError> {
        let sql = format!("SELECT {COMMAND} FROM mdm_commands WHERE id = $1");
        Ok(self.select(sql, id, command_of)?.pop())
    }

    fn commands(&self, enrollment_id: &str) -> Result<Vec<Command>, MdmError> {
        let sql =
            format!("SELECT {COMMAND} FROM mdm_commands WHERE enrollment_id = $1 ORDER BY seq");
        self.select(sql, enrollment_id, command_of)
    }

    fn enrollments_with_queued(&self) -> Result<Vec<String>, MdmError> {
        let rows = self.read(|s| {
            s.query(
                "SELECT DISTINCT c.enrollment_id FROM mdm_commands c \
                 JOIN mdm_enrollments e ON e.enrollment_id = c.enrollment_id \
                 WHERE c.state = 'queued' AND e.state = 'live'",
                &[],
            )
        })?;
        let mut ids: Vec<String> = rows.iter().map(|row| row.get(0)).collect();
        ids.sort();
        Ok(ids)
    }

    fn identities_expiring(&self, before_unix: i64) -> Result<Vec<(String, i64)>, MdmError> {
        let rows = self.read(move |s| {
            s.query(
                "SELECT e.serial, i.not_after_unix FROM mdm_identities i \
                 JOIN mdm_enrollments e ON e.cert_sha256 = i.cert_sha256 \
                 WHERE i.state = 'issued' AND e.state = 'live' AND i.not_after_unix < $1 \
                 ORDER BY i.not_after_unix, e.serial",
                &[&before_unix],
            )
        })?;
        Ok(rows.iter().map(|row| (row.get(0), row.get(1))).collect())
    }

    fn abm_apply(&self, batch: &AbmBatch) -> Result<(), MdmError> {
        let batch = batch.clone();
        self.write(move |tx| {
            for d in &batch.upserts {
                tx.execute(
                    &format!(
                        "INSERT INTO abm_devices ({DEVICE}) VALUES ($1, $2, $3, $4, $5, $6, $7, NULL) \
                         ON CONFLICT (serial) DO UPDATE SET model = EXCLUDED.model, \
                         description = EXCLUDED.description, \
                         profile_status = EXCLUDED.profile_status, \
                         profile_uuid = EXCLUDED.profile_uuid, \
                         synced_unix = EXCLUDED.synced_unix, removed_unix = NULL"
                    ),
                    &[
                        &d.serial,
                        &d.model,
                        &d.description,
                        &d.profile_status,
                        &d.profile_uuid,
                        &d.first_seen_unix,
                        &d.synced_unix,
                    ],
                )?;
            }
            for serial in &batch.removed {
                tx.execute(
                    "UPDATE abm_devices SET removed_unix = $2 WHERE serial = $1",
                    &[serial, &batch.removed_unix],
                )?;
            }
            if let Some(state) = &batch.state {
                tx.execute(
                    "INSERT INTO abm_state (id, cursor, profile_uuid, profile_digest, synced_unix) \
                     VALUES (1, $1, $2, $3, $4) ON CONFLICT (id) DO UPDATE SET \
                     cursor = EXCLUDED.cursor, profile_uuid = EXCLUDED.profile_uuid, \
                     profile_digest = EXCLUDED.profile_digest, synced_unix = EXCLUDED.synced_unix",
                    &[
                        &state.cursor,
                        &state.profile_uuid,
                        &state.profile_digest,
                        &state.synced_unix,
                    ],
                )?;
            }
            Ok(())
        })
    }

    fn abm_device(&self, serial: &str) -> Result<Option<AbmDevice>, MdmError> {
        let sql = format!("SELECT {DEVICE} FROM abm_devices WHERE serial = $1");
        Ok(self.select(sql, serial, device_of)?.pop())
    }

    fn abm_devices(&self) -> Result<Vec<AbmDevice>, MdmError> {
        let rows = self.read(|s| {
            s.query(
                &format!("SELECT {DEVICE} FROM abm_devices ORDER BY serial COLLATE \"C\""),
                &[],
            )
        })?;
        rows.iter().map(device_of).collect()
    }

    fn abm_state(&self) -> Result<Option<AbmState>, MdmError> {
        let rows = self.read(|s| {
            s.query(
                "SELECT cursor, profile_uuid, profile_digest, synced_unix FROM abm_state",
                &[],
            )
        })?;
        Ok(rows.first().map(|row| AbmState {
            cursor: row.get(0),
            profile_uuid: row.get(1),
            profile_digest: row.get(2),
            synced_unix: row.get(3),
        }))
    }
}
