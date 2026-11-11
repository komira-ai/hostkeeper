//! Enrollment of one serial (docs/design/coordinator-mdm.md sections 4.1, 4.2, 4.6, 5.1, 5.4 and
//! 7): `/enroll`, SCEP, `Authenticate`, `TokenUpdate`, `CheckOut`, the same serial enrolling
//! again after a wipe, a Mac that is still enrolled in the old stack, and the approval that
//! follows from a nonce.
//!
//! Two parties talk to the listener: the Mac (key 0) and a peer on the LAN (key 1) that can
//! replay a device-information blob it once captured, which carries no freshness, and can use any
//! challenge it holds. The model keeps what the server cannot see (whose key an identity is, what
//! the device really is) as plain fields so the invariants can be judged against the truth. The
//! rules the implementation must follow are fields of [`Rules`]; the tests turn each off and
//! expect the invariant it protects to break.
//!
//! Owner decision B (2026-11-10): a serial in the ABM table replaces its live enrollment with no
//! operator and without the old enrollment having checked out, so a wiped Mac enrolls again with
//! no click. The accepted risk is the other side of the same rule: the peer's replay of a blob
//! verifies as well, so the peer can take the serial's identity while the serial is in the ABM.
//! The model does not call that a violation; a test walks it, and the nonce still never approves
//! the peer's enrollment.

use crate::explore::{Model, Violation};

/// A challenge issues at most one certificate.
pub const CHALLENGE_ONCE: &str = "a_challenge_is_consumed_at_most_once";
/// A retried `PKIOperation` with the key that consumed the challenge gets the certificate again.
pub const LOST_CERTREP_RECOVERS: &str =
    "a_lost_certrep_retried_with_the_same_key_gets_the_certificate";
/// A serial has one live enrollment.
pub const ONE_LIVE: &str = "a_serial_has_one_live_enrollment";
/// A replaced or checked-out certificate is never accepted.
pub const SUPERSEDED_NEVER_ACCEPTED: &str = "a_superseded_certificate_is_never_accepted";
/// A certificate this CA did not issue is never accepted.
pub const FOREIGN_NEVER_ACCEPTED: &str = "a_certificate_of_another_ca_is_never_accepted";
/// A live enrollment is replaced only while the serial is in the ABM table, or after an operator
/// confirmed the Mac in front of them.
pub const REPLACE_ONLY_IN_ABM: &str =
    "a_live_enrollment_is_replaced_only_while_the_serial_is_in_the_abm_or_an_operator_confirmed";
/// The stored push token belongs to the live enrollment.
pub const TOKEN_IS_LIVE: &str = "the_push_token_is_the_live_enrollments";
/// No challenge is issued on a blob that did not verify, without an operator.
pub const NO_UNVERIFIED_CHALLENGE: &str =
    "no_challenge_is_issued_on_an_unverified_blob_without_an_operator";
/// No challenge is issued for a serial the ABM did not assign to this server.
pub const NO_CHALLENGE_OUTSIDE_ABM: &str =
    "no_challenge_is_issued_for_a_serial_the_abm_does_not_assign";
/// A nonce approves once.
pub const NONCE_ONCE: &str = "a_nonce_approves_at_most_once";
/// A nonce approves only the enrollment it was issued for.
pub const NONCE_BOUND: &str = "a_nonce_approves_only_the_enrollment_it_was_issued_for";
/// Approval needs a supervised Mac, enrolled by ADE, with a group, whose serial is in the ABM.
pub const APPROVAL_CONDITIONS: &str =
    "a_mac_is_approved_only_when_supervised_via_dep_in_abm_with_a_group";

/// The rules the implementation follows. `true` is the rule kept.
#[derive(Clone, Copy, Debug)]
pub struct Rules {
    /// A challenge is issued only for a serial the ABM has assigned the profile to, checked when
    /// `/enroll` arrives and again when an operator confirms a request that waited.
    pub challenge_needs_abm: bool,
    /// A blob that did not verify waits for an operator instead of getting a challenge.
    pub unverified_waits_for_operator: bool,
    /// A new `Authenticate` replaces the live enrollment of the serial only while the serial is
    /// in the ABM table, read again at that moment (owner decision B of 2026-11-10): no operator
    /// and no check-out of the old enrollment is needed, and a serial removed from the ABM since
    /// the challenge was issued is refused unless an operator confirmed the Mac.
    pub replace_needs_abm_at_authenticate: bool,
    /// A consumed challenge issues nothing new.
    pub consume_once: bool,
    /// The same key presenting a consumed challenge gets the stored certificate again.
    pub same_key_gets_stored_certificate: bool,
    /// A new `Authenticate` marks the live enrollment of the serial replaced.
    pub supersede_old: bool,
    /// A request on a replaced or checked-out certificate changes nothing.
    pub refuse_dead_identity: bool,
    /// A request on a certificate that is not this CA's changes nothing.
    pub refuse_foreign_identity: bool,
    /// A nonce approves once.
    pub nonce_single_use: bool,
    /// A nonce approves only the enrollment it was issued for.
    pub nonce_bound_to_enrollment: bool,
    /// The approval conditions of 4.6 (the first four).
    pub require_approval_conditions: bool,
}

impl Rules {
    /// The design with owner decision B: a serial in the ABM table replaces its live enrollment
    /// with no operator.
    pub fn design() -> Rules {
        Rules {
            challenge_needs_abm: true,
            unverified_waits_for_operator: true,
            replace_needs_abm_at_authenticate: true,
            consume_once: true,
            same_key_gets_stored_certificate: true,
            supersede_old: true,
            refuse_dead_identity: true,
            refuse_foreign_identity: true,
            nonce_single_use: true,
            nonce_bound_to_enrollment: true,
            require_approval_conditions: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Abm {
    /// Listed, the ADE profile not assigned yet.
    Listed,
    Assigned,
    /// Disowned in ABM.
    Removed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChallengeState {
    Free,
    Unused,
    Consumed,
    Expired,
}

/// A challenge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Challenge {
    pub state: ChallengeState,
    /// The key of the holder the challenge was issued to.
    pub holder: u8,
    /// Issued after an operator confirmed, not on a verified blob alone.
    pub by_operator: bool,
    /// The certificate it issued and the key that took it, once consumed.
    pub cert: u8,
    pub consumed_by: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CertState {
    Free,
    /// Issued by SCEP, no `Authenticate` yet.
    Issued,
    Live,
    Replaced,
    CheckedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cert {
    pub state: CertState,
    /// Whose private key the identity is: 0 the Mac, 1 the LAN peer.
    pub key: u8,
    pub challenge: u8,
    /// The `CertRep` reached its client.
    pub delivered: bool,
    /// The Mac was wiped and no longer holds this identity.
    pub lost: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Nonce {
    /// Issued for the certificate of the same index: a certificate has at most one nonce.
    pub issued: bool,
    pub cert: u8,
    pub used: bool,
    pub expired: bool,
}

/// What the device really is, and what the ABM and the inventory say of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Device {
    pub supervised: bool,
    pub via_dep: bool,
    pub group: bool,
    /// Still enrolled in the old stack with that stack's identity.
    pub foreign: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub device: Device,
    pub abm: Abm,
    pub challenges: [Challenge; 2],
    pub certs: [Cert; 2],
    pub token_for: Option<u8>,
    pub nonces: [Nonce; 2],
    /// A request waiting for an operator, and the key of whoever sent it.
    pub pending: Option<u8>,
    pub approved: Option<u8>,
    pub wiped: bool,
}

#[derive(Clone, Debug)]
pub enum Action {
    AbmAssign,
    AbmRemove,
    PostEnroll {
        key: u8,
        verified: bool,
    },
    OperatorConfirm,
    OperatorReject,
    /// The operator confirms the Mac in front of them for an enrollment held at `Authenticate`
    /// because the serial is no longer in the ABM table.
    OperatorConfirmCert {
        cert: usize,
    },
    Expire {
        challenge: usize,
    },
    /// `PKIOperation` with `key`; `lost` is a `CertRep` that never reaches the client.
    Scep {
        challenge: usize,
        key: u8,
        lost: bool,
    },
    Authenticate {
        cert: usize,
    },
    TokenUpdate {
        cert: usize,
    },
    CheckOut {
        cert: usize,
    },
    ForeignCheckin,
    Wipe,
    MintNonce {
        cert: usize,
    },
    ExpireNonce {
        nonce: usize,
    },
    Approve {
        nonce: usize,
    },
}

pub struct EnrollmentModel {
    pub rules: Rules,
}

fn violation(invariant: &'static str, detail: &str) -> Violation {
    Violation::new(invariant, detail)
}

impl State {
    fn live(&self) -> Option<usize> {
        self.certs.iter().position(|c| c.state == CertState::Live)
    }

    /// What the gate learns of the enrollment of `cert`: the Mac reports what it is, the LAN
    /// peer reports whatever lets it through.
    fn claims_dep_supervised(&self, cert: usize) -> bool {
        self.certs[cert].key == 1 || (self.device.supervised && self.device.via_dep)
    }

    /// The holder of the identity can use it: a wiped Mac cannot.
    fn usable(&self, cert: usize) -> bool {
        let c = &self.certs[cert];
        c.state != CertState::Free && (c.key == 1 || !c.lost)
    }
}

impl Model for EnrollmentModel {
    type State = State;
    type Action = Action;

    fn init(&self) -> Vec<State> {
        let mut out = Vec::new();
        for bits in 0..16u8 {
            out.push(State {
                device: Device {
                    supervised: bits & 1 != 0,
                    via_dep: bits & 2 != 0,
                    group: bits & 4 != 0,
                    foreign: bits & 8 != 0,
                },
                abm: Abm::Listed,
                challenges: [Challenge {
                    state: ChallengeState::Free,
                    holder: 0,
                    by_operator: false,
                    cert: 0,
                    consumed_by: 0,
                }; 2],
                certs: [Cert {
                    state: CertState::Free,
                    key: 0,
                    challenge: 0,
                    delivered: false,
                    lost: false,
                }; 2],
                token_for: None,
                nonces: [Nonce {
                    issued: false,
                    cert: 0,
                    used: false,
                    expired: false,
                }; 2],
                pending: None,
                approved: None,
                wiped: false,
            });
        }
        out
    }

    fn actions(&self, s: &State) -> Vec<Action> {
        let mut out = vec![Action::AbmAssign, Action::AbmRemove, Action::Wipe];
        for key in 0..2 {
            out.push(Action::PostEnroll {
                key,
                verified: true,
            });
            out.push(Action::PostEnroll {
                key,
                verified: false,
            });
        }
        if s.pending.is_some() {
            out.push(Action::OperatorConfirm);
            out.push(Action::OperatorReject);
        }
        if s.device.foreign {
            out.push(Action::ForeignCheckin);
        }
        for cert in 0..2 {
            let held = s.certs[cert].state == CertState::Issued
                && s.certs[cert].key == 0
                && !s.challenges[usize::from(s.certs[cert].challenge)].by_operator;
            if held && s.live().is_some() && s.abm == Abm::Removed {
                out.push(Action::OperatorConfirmCert { cert });
            }
        }
        for challenge in 0..2 {
            if s.challenges[challenge].state == ChallengeState::Unused {
                out.push(Action::Expire { challenge });
            }
            // An unused challenge reaches only the party it was issued to (it travels in a profile
            // over TLS); a consumed one is also tried by whoever saw the exchange.
            let ch = &s.challenges[challenge];
            if ch.state != ChallengeState::Free {
                for key in 0..2 {
                    if ch.state == ChallengeState::Unused && key != ch.holder {
                        continue;
                    }
                    for lost in [false, true] {
                        out.push(Action::Scep {
                            challenge,
                            key,
                            lost,
                        });
                    }
                }
            }
        }
        for cert in 0..2 {
            if s.usable(cert) {
                out.push(Action::Authenticate { cert });
                out.push(Action::TokenUpdate { cert });
                out.push(Action::CheckOut { cert });
            }
            out.push(Action::MintNonce { cert });
        }
        for nonce in 0..2 {
            if s.nonces[nonce].issued {
                out.push(Action::ExpireNonce { nonce });
                out.push(Action::Approve { nonce });
            }
        }
        out
    }

    fn step(&self, s: &State, action: &Action) -> Result<State, Violation> {
        let mut n = s.clone();
        match *action {
            Action::AbmAssign => {
                if n.abm == Abm::Listed {
                    n.abm = Abm::Assigned;
                }
            }
            Action::AbmRemove => n.abm = Abm::Removed,
            Action::PostEnroll { key, verified } => self.post_enroll(&mut n, key, verified)?,
            Action::OperatorConfirm => {
                // The operator confirms the Mac in front of them, never the LAN peer.
                if n.pending.take() == Some(0) {
                    self.mint(&mut n, 0, true)?;
                }
            }
            Action::OperatorReject => n.pending = None,
            Action::OperatorConfirmCert { cert } => {
                n.challenges[usize::from(n.certs[cert].challenge)].by_operator = true;
            }
            Action::Expire { challenge } => n.challenges[challenge].state = ChallengeState::Expired,
            Action::Scep {
                challenge,
                key,
                lost,
            } => self.scep(&mut n, challenge, key, lost)?,
            Action::Authenticate { cert } => self.authenticate(&mut n, cert)?,
            Action::TokenUpdate { cert } => self.token_update(&mut n, cert)?,
            Action::CheckOut { cert } => {
                if n.certs[cert].state == CertState::Live {
                    n.certs[cert].state = CertState::CheckedOut;
                    n.token_for = None;
                }
            }
            Action::ForeignCheckin => {
                if !self.rules.refuse_foreign_identity {
                    return Err(violation(
                        FOREIGN_NEVER_ACCEPTED,
                        "a check-in on an identity of the old stack's CA was accepted",
                    ));
                }
            }
            Action::Wipe => {
                n.wiped = true;
                n.device.foreign = false;
                for c in &mut n.certs {
                    c.lost = c.lost || c.key == 0;
                }
            }
            Action::MintNonce { cert } => self.mint_nonce(&mut n, cert),
            Action::ExpireNonce { nonce } => n.nonces[nonce].expired = true,
            Action::Approve { nonce } => self.approve(&mut n, nonce)?,
        }
        Ok(n)
    }

    fn check(&self, s: &State) -> Result<(), Violation> {
        if s.certs
            .iter()
            .filter(|c| c.state == CertState::Live)
            .count()
            > 1
        {
            return Err(violation(
                ONE_LIVE,
                "two certificates of the serial are live",
            ));
        }
        for i in 0..2 {
            let issued = s
                .certs
                .iter()
                .filter(|c| c.state != CertState::Free && c.challenge == i as u8)
                .count();
            if issued > 1 {
                return Err(violation(
                    CHALLENGE_ONCE,
                    &format!("challenge {i} issued {issued} certificates"),
                ));
            }
        }
        if let Some(t) = s.token_for
            && s.certs[usize::from(t)].state != CertState::Live
        {
            return Err(violation(
                TOKEN_IS_LIVE,
                &format!(
                    "the push token is stored for certificate {t}, which is {:?}",
                    s.certs[usize::from(t)].state
                ),
            ));
        }
        Ok(())
    }
}

impl EnrollmentModel {
    fn post_enroll(&self, n: &mut State, key: u8, verified: bool) -> Result<(), Violation> {
        if n.abm != Abm::Assigned && self.rules.challenge_needs_abm {
            return Ok(());
        }
        if !verified && self.rules.unverified_waits_for_operator {
            n.pending = Some(key);
            return Ok(());
        }
        if !verified {
            return Err(violation(
                NO_UNVERIFIED_CHALLENGE,
                "a challenge was issued on a blob that did not verify",
            ));
        }
        self.mint(n, key, false)
    }

    fn mint(&self, n: &mut State, holder: u8, by_operator: bool) -> Result<(), Violation> {
        if n.abm != Abm::Assigned && self.rules.challenge_needs_abm {
            return Ok(());
        }
        if n.abm != Abm::Assigned {
            return Err(violation(
                NO_CHALLENGE_OUTSIDE_ABM,
                &format!("a challenge was issued while the ABM state is {:?}", n.abm),
            ));
        }
        for slot in 0..2 {
            if n.challenges[slot].state != ChallengeState::Free {
                continue;
            }
            n.challenges[slot] = Challenge {
                state: ChallengeState::Unused,
                holder,
                by_operator,
                cert: 0,
                consumed_by: 0,
            };
            break;
        }
        Ok(())
    }

    fn scep(&self, n: &mut State, challenge: usize, key: u8, lost: bool) -> Result<(), Violation> {
        let ch = n.challenges[challenge];
        let issue = match ch.state {
            ChallengeState::Unused => true,
            ChallengeState::Consumed if !self.rules.consume_once => true,
            ChallengeState::Consumed if key == ch.consumed_by => {
                let stored = &mut n.certs[usize::from(ch.cert)];
                if self.rules.same_key_gets_stored_certificate {
                    stored.delivered = stored.delivered || !lost;
                } else if !stored.delivered {
                    return Err(violation(
                        LOST_CERTREP_RECOVERS,
                        "the key that consumed the challenge was refused its certificate after the reply was lost",
                    ));
                }
                false
            }
            _ => false,
        };
        if !issue {
            return Ok(());
        }
        for slot in 0..2 {
            if n.certs[slot].state != CertState::Free {
                continue;
            }
            n.certs[slot] = Cert {
                state: CertState::Issued,
                key,
                challenge: challenge as u8,
                delivered: !lost,
                lost: false,
            };
            let ch = &mut n.challenges[challenge];
            ch.state = ChallengeState::Consumed;
            ch.cert = slot as u8;
            ch.consumed_by = key;
            break;
        }
        Ok(())
    }

    /// A request on a certificate that is not live: refused, or (a broken rule) accepted.
    fn dead(&self, state: CertState) -> Result<(), Violation> {
        if self.rules.refuse_dead_identity {
            return Ok(());
        }
        Err(violation(
            SUPERSEDED_NEVER_ACCEPTED,
            &format!("a request on a {state:?} certificate was accepted"),
        ))
    }

    fn authenticate(&self, n: &mut State, cert: usize) -> Result<(), Violation> {
        match n.certs[cert].state {
            CertState::Issued => {}
            CertState::Live | CertState::Free => return Ok(()),
            dead => return self.dead(dead),
        }
        if let Some(old) = n.live() {
            let confirmed = n.challenges[usize::from(n.certs[cert].challenge)].by_operator;
            if n.abm == Abm::Removed && !confirmed {
                if self.rules.replace_needs_abm_at_authenticate {
                    return Ok(());
                }
                return Err(violation(
                    REPLACE_ONLY_IN_ABM,
                    "a live enrollment was replaced after the ABM removed the serial, with no operator",
                ));
            }
            if self.rules.supersede_old {
                n.certs[old].state = CertState::Replaced;
            }
        }
        n.certs[cert].state = CertState::Live;
        n.token_for = None;
        Ok(())
    }

    fn token_update(&self, n: &mut State, cert: usize) -> Result<(), Violation> {
        match n.certs[cert].state {
            CertState::Live => n.token_for = Some(cert as u8),
            CertState::Issued | CertState::Free => {}
            dead => {
                self.dead(dead)?;
            }
        }
        Ok(())
    }

    fn mint_nonce(&self, n: &mut State, cert: usize) {
        let ready = n.live() == Some(cert) && n.token_for == Some(cert as u8);
        let claims = n.claims_dep_supervised(cert) || !self.rules.require_approval_conditions;
        if ready && claims && !n.nonces[cert].issued {
            n.nonces[cert] = Nonce {
                issued: true,
                cert: cert as u8,
                used: false,
                expired: false,
            };
        }
    }

    fn approve(&self, n: &mut State, nonce: usize) -> Result<(), Violation> {
        let x = n.nonces[nonce];
        let cert = usize::from(x.cert);
        // Only the Mac itself holds the nonce of its own enrollment, and a wiped Mac holds none.
        if x.expired || n.certs[cert].key != 0 || !n.usable(cert) {
            return Ok(());
        }
        let Some(live) = n.live() else {
            return Ok(());
        };
        if x.used && self.rules.nonce_single_use {
            return Ok(());
        }
        if live != cert && self.rules.nonce_bound_to_enrollment {
            return Ok(());
        }
        let d = n.device;
        let conditions = d.supervised & d.via_dep & d.group & (n.abm != Abm::Removed);
        if !conditions & self.rules.require_approval_conditions {
            return Ok(());
        }
        if x.used {
            return Err(violation(
                NONCE_ONCE,
                "a nonce that was used approved again",
            ));
        }
        if live != cert {
            return Err(violation(
                NONCE_BOUND,
                "a nonce issued for a replaced enrollment approved the live one",
            ));
        }
        if !conditions {
            return Err(violation(
                APPROVAL_CONDITIONS,
                &format!("approved with {d:?} and the ABM state {:?}", n.abm),
            ));
        }
        n.nonces[nonce].used = true;
        n.approved = Some(cert as u8);
        Ok(())
    }
}
