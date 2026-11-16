//! The book in memory, for tests that do not need a database; the Postgres book passes the same
//! tests (`ssh_book_tests.rs`, `store/postgres/tests/ssh_book.rs`).
//!
//! It is built over the memory ledger ([`ssh_ledger::Memory`]): it reads the certificates and
//! revocations from it, takes its clock, and appends its lines to its chain, a refused write
//! of which changes nothing here either.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use super::{
    ADDRESS_CHANGED, Book, BookError, Cause, Closing, HOSTKEY_CHANGED, HOSTKEY_PINNED, HostKeys,
    OpenRefused, Opened, Opening, PinRefused, Polled, TUNNEL_CLOSE, TUNNEL_OPEN, address_detail,
    close_detail, keys_detail, open_detail, same_set,
};
use crate::ssh_ledger::{self, Line};

/// The `by` of the lines the coordinator itself writes.
const COORDINATOR: &str = "coordinator";

struct Host {
    address: String,
    seen_unix: u64,
    reported: Vec<String>,
}

struct Tunnel {
    operator: String,
    host_id: String,
    serial: u64,
    instance: String,
    opened_unix: u64,
    last_seen_unix: u64,
}

#[derive(Default)]
struct State {
    hosts: BTreeMap<String, Host>,
    pins: BTreeMap<String, Vec<String>>,
    tunnels: BTreeMap<u64, Tunnel>,
    last_id: u64,
}

/// The memory book: its rows, over a memory ledger.
pub(crate) struct Memory {
    ledger: Arc<ssh_ledger::Memory>,
    state: Mutex<State>,
}

impl Memory {
    /// An empty book over `ledger`, whose certificates it checks and whose chain it writes to.
    pub(crate) fn over(ledger: Arc<ssh_ledger::Memory>) -> Memory {
        Memory {
            ledger,
            state: Mutex::new(State::default()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The ids of the tunnels open, in order.
    pub(crate) fn tunnel_ids(&self) -> Vec<u64> {
        self.state().tunnels.keys().copied().collect()
    }

    /// The time `host_id` was last heard at.
    pub(crate) fn seen(&self, host_id: &str) -> Option<u64> {
        self.state().hosts.get(host_id).map(|host| host.seen_unix)
    }

    /// Moves the last heartbeat of `host_id` back by `secs` (forward when `back` is false).
    pub(crate) fn shift_host(&self, host_id: &str, secs: u64, back: bool) {
        if let Some(host) = self.state().hosts.get_mut(host_id) {
            host.seen_unix = if back {
                host.seen_unix.saturating_sub(secs)
            } else {
                host.seen_unix + secs
            };
        }
    }

    /// Moves back by `secs` the time tunnel `id` was opened and last refreshed.
    pub(crate) fn age_tunnel(&self, id: u64, secs: u64) {
        if let Some(tunnel) = self.state().tunnels.get_mut(&id) {
            tunnel.opened_unix = tunnel.opened_unix.saturating_sub(secs);
            tunnel.last_seen_unix = tunnel.last_seen_unix.saturating_sub(secs);
        }
    }
}

fn unwritten(error: &ssh_ledger::LedgerError) -> String {
    error.to_string()
}

fn close_line(now: u64, by: &str, host_id: &str, detail: String) -> Line {
    Line {
        at_unix: now,
        by: by.to_owned(),
        action: TUNNEL_CLOSE,
        subject: Some(host_id.to_owned()),
        detail,
    }
}

impl Book for Memory {
    fn heard(&self, host_id: &str, address: &str, reported: &[String]) -> Result<(), BookError> {
        let now = self.ledger.now();
        let mut state = self.state();
        let before = state
            .hosts
            .get(host_id)
            .map(|host| (host.address.clone(), host.reported.clone()));
        let mut lines = Vec::new();
        if let Some((was, _)) = &before
            && was != address
        {
            lines.push(Line {
                at_unix: now,
                by: COORDINATOR.to_owned(),
                action: ADDRESS_CHANGED,
                subject: Some(host_id.to_owned()),
                detail: address_detail(was, address),
            });
        }
        if !reported.is_empty()
            && let Some(pinned) = state.pins.get(host_id)
        {
            let was = before.as_ref().map(|(_, keys)| keys.as_slice());
            if !same_set(pinned, reported) && !same_set(was.unwrap_or_default(), reported) {
                lines.push(Line {
                    at_unix: now,
                    by: COORDINATOR.to_owned(),
                    action: HOSTKEY_CHANGED,
                    subject: Some(host_id.to_owned()),
                    detail: keys_detail(pinned, reported),
                });
            }
        }
        if !lines.is_empty() {
            self.ledger
                .append_lines(lines)
                .map_err(|e| BookError(unwritten(&e)))?;
        }
        let host = state.hosts.entry(host_id.to_owned()).or_insert(Host {
            address: String::new(),
            seen_unix: 0,
            reported: Vec::new(),
        });
        address.clone_into(&mut host.address);
        host.seen_unix = host.seen_unix.max(now);
        if !reported.is_empty() {
            reported.clone_into(&mut host.reported);
        }
        Ok(())
    }

    fn host_keys(&self, host_id: &str) -> Result<HostKeys, BookError> {
        let state = self.state();
        Ok(HostKeys {
            pinned: state.pins.get(host_id).cloned(),
            reported: state
                .hosts
                .get(host_id)
                .map(|host| host.reported.clone())
                .unwrap_or_default(),
        })
    }

    fn pin(&self, by: &str, host_id: &str, fingerprints: &[String]) -> Result<(), PinRefused> {
        let now = self.ledger.now();
        let mut state = self.state();
        let reported = state
            .hosts
            .get(host_id)
            .map(|host| host.reported.clone())
            .unwrap_or_default();
        if reported.is_empty() || !same_set(&reported, fingerprints) {
            return Err(PinRefused::NotReported);
        }
        self.ledger
            .append_line(Line {
                at_unix: now,
                by: by.to_owned(),
                action: HOSTKEY_PINNED,
                subject: Some(host_id.to_owned()),
                detail: keys_detail(fingerprints, &reported),
            })
            .map_err(|e| PinRefused::Unwritten(unwritten(&e)))?;
        state.pins.insert(host_id.to_owned(), fingerprints.to_vec());
        Ok(())
    }

    fn open(&self, opening: &Opening) -> Result<Opened, OpenRefused> {
        let now = self.ledger.now();
        let mut state = self.state();
        let good = self
            .ledger
            .certs()
            .get(&opening.serial)
            .is_some_and(|cert| {
                cert.operator == opening.operator
                    && cert.host_id == opening.host_id
                    && cert.expires_unix > now
            })
            && !self.ledger.revoked().contains_key(&opening.serial);
        if !good {
            return Err(OpenRefused::BadSerial);
        }
        let Some(host) = state.hosts.get(&opening.host_id) else {
            return Err(OpenRefused::NoAddress);
        };
        if host.seen_unix < now.saturating_sub(opening.live_secs) {
            return Err(OpenRefused::HostNotLive);
        }
        let address = host.address.clone();
        let mine = state
            .tunnels
            .values()
            .filter(|tunnel| tunnel.operator == opening.operator);
        let (held, to_host) = mine.fold((0_u64, false), |(held, to_host), tunnel| {
            (held + 1, to_host || tunnel.host_id == opening.host_id)
        });
        if held >= opening.per_operator || to_host {
            return Err(OpenRefused::TooMany);
        }
        let id = state.last_id + 1;
        self.ledger
            .append_line(Line {
                at_unix: now,
                by: opening.operator.clone(),
                action: TUNNEL_OPEN,
                subject: Some(opening.host_id.clone()),
                detail: open_detail(id, opening.serial, &address, &opening.instance),
            })
            .map_err(|e| OpenRefused::Unwritten(unwritten(&e)))?;
        state.last_id = id;
        state.tunnels.insert(
            id,
            Tunnel {
                operator: opening.operator.clone(),
                host_id: opening.host_id.clone(),
                serial: opening.serial,
                instance: opening.instance.clone(),
                opened_unix: now,
                last_seen_unix: now,
            },
        );
        Ok(Opened {
            id,
            address,
            opened_unix: now,
        })
    }

    fn close(&self, closing: &Closing) -> Result<(), BookError> {
        let now = self.ledger.now();
        let mut state = self.state();
        let Some(tunnel) = state.tunnels.get(&closing.id) else {
            return Ok(());
        };
        let line = close_line(
            now,
            &tunnel.operator,
            &tunnel.host_id,
            close_detail(
                closing.id,
                closing.cause,
                Some((closing.bytes_up, closing.bytes_down)),
                now.saturating_sub(tunnel.opened_unix),
            ),
        );
        self.ledger
            .append_line(line)
            .map_err(|e| BookError(unwritten(&e)))?;
        state.tunnels.remove(&closing.id);
        Ok(())
    }

    fn poll(&self, instance: &str, ids: &[u64], stale_secs: u64) -> Result<Polled, BookError> {
        let now = self.ledger.now();
        let mut state = self.state();
        let revoked_serials = self.ledger.revoked();
        let revoked = ids
            .iter()
            .copied()
            .filter(|id| {
                state.tunnels.get(id).is_some_and(|tunnel| {
                    tunnel.instance == instance && revoked_serials.contains_key(&tunnel.serial)
                })
            })
            .collect::<Vec<_>>();
        let lost: Vec<u64> = state
            .tunnels
            .iter()
            .filter(|(_, tunnel)| {
                tunnel.instance != instance
                    && tunnel.last_seen_unix < now.saturating_sub(stale_secs)
            })
            .map(|(id, _)| *id)
            .collect();
        let lines: Vec<Line> = lost
            .iter()
            .filter_map(|id| state.tunnels.get(id).map(|tunnel| (id, tunnel)))
            .map(|(id, tunnel)| {
                close_line(
                    now,
                    &tunnel.operator,
                    &tunnel.host_id,
                    close_detail(
                        *id,
                        Cause::InstanceLost,
                        None,
                        now.saturating_sub(tunnel.opened_unix),
                    ),
                )
            })
            .collect();
        if !lines.is_empty() {
            self.ledger
                .append_lines(lines)
                .map_err(|e| BookError(unwritten(&e)))?;
        }
        for id in &lost {
            state.tunnels.remove(id);
        }
        for id in ids {
            if let Some(tunnel) = state.tunnels.get_mut(id)
                && tunnel.instance == instance
            {
                tunnel.last_seen_unix = now;
            }
        }
        let gone = ids
            .iter()
            .copied()
            .filter(|id| !state.tunnels.contains_key(id))
            .collect();
        Ok(Polled {
            revoked,
            gone,
            reaped: lost.len() as u64,
        })
    }
}
