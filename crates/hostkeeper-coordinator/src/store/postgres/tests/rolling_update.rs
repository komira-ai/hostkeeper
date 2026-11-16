//! A start-first rolling update of the coordinator, with the fleet heartbeating through it: the
//! old coordinator (A) serves alone, the new one (B) starts on the same database beside it, the
//! hosts' beats reach either while both run, and then A stops and B serves alone. Whichever
//! coordinator a beat reaches, every order the coordinators gave reaches its host: an update whose
//! answer was lost with A is told again by B, and an operator's abort queued through A is sent by
//! B, once.
//!
//! No test sleeps or measures time. A round is one heartbeat period of every host, the time both
//! coordinators read is moved on by the test, and which coordinator hears a beat is the test's
//! schedule ([`Route`]), not a race.

use std::collections::{BTreeMap, BTreeSet};

use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_wire::wire::{Heartbeat, ServiceReply};

use super::TestDatabase;
use super::coordinator::DAEMON;
use super::heartbeat_rows::pair;
use crate::marks_support::{DB, NEW_DAEMON, WEB, ask_typed, beat_of, operator, runs};
use crate::orders::Order;

/// The fleet: six hosts, each running `web` and `db`.
const HOSTS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];

/// The heartbeat period of a host, in seconds: one round.
const PERIOD: u64 = 15;

/// The rounds A serves alone, then the rounds both serve: B serves alone from `OVERLAP_ENDS` on.
const B_STARTS: u64 = 2;
const OVERLAP_ENDS: u64 = 6;

/// More rounds than the rollout needs: a host still behind after them missed an order.
const ROUNDS: u64 = 120;

/// A host as its daemon behaves: it beats the versions it runs, and runs the version an update
/// told it from its next beat on. It keeps every order it was told.
#[derive(Default)]
struct Host {
    running: BTreeMap<String, String>,
    told: Vec<Told>,
}

/// An order as a host was told it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Told {
    Update {
        service: String,
        version: String,
        order_try: Option<u64>,
    },
    Abort {
        service: String,
        order_id: String,
    },
}

impl Host {
    fn new() -> Self {
        Self {
            running: [WEB, DB]
                .map(|service| (service.to_owned(), "1.0".to_owned()))
                .into(),
            told: Vec::new(),
        }
    }

    fn beat(&self) -> Heartbeat {
        beat_of(
            NEW_DAEMON,
            self.running
                .iter()
                .map(|(service, version)| runs(service, version))
                .collect(),
        )
    }

    /// Takes in the answer to a beat: an update is run from the next beat on.
    fn hear(&mut self, reply: &ServiceReply, order: Option<&Order>) {
        let told = match (reply, order) {
            (_, Some(Order::Abort { service, order_id })) => Told::Abort {
                service: service.clone(),
                order_id: order_id.clone(),
            },
            (
                _,
                Some(Order::Update {
                    service,
                    version,
                    order_try,
                    ..
                }),
            ) => Told::Update {
                service: service.clone(),
                version: version.to_string(),
                order_try: Some(*order_try),
            },
            (
                ServiceReply::Update {
                    service: Some(service),
                    version,
                },
                _,
            ) => Told::Update {
                service: service.clone(),
                version: version.to_string(),
                order_try: None,
            },
            (ServiceReply::Nothing | ServiceReply::Update { service: None, .. }, other) => {
                assert!(
                    other.is_none(),
                    "an order the host does not know: {other:?}"
                );
                return;
            }
        };
        if let Told::Update {
            service, version, ..
        } = &told
        {
            self.running.insert(service.clone(), version.clone());
        }
        self.told.push(told);
    }

    fn at_2(&self) -> bool {
        self.running.values().all(|version| version == "2.0")
    }
}

/// Which coordinator hears a host's beat in a round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    A,
    B,
}

/// The schedule of the update: A alone, then both (each host alternating between them, half the
/// fleet on each in every round), then B alone.
fn route(round: u64, host: usize) -> Route {
    if round < B_STARTS {
        Route::A
    } else if round < OVERLAP_ENDS {
        let host = u64::try_from(host).expect("a small index");
        if (round + host).is_multiple_of(2) {
            Route::A
        } else {
            Route::B
        }
    } else {
        Route::B
    }
}

/// The order rows: host, service, target and try.
fn order_rows(db: &TestDatabase) -> BTreeSet<(String, String, String, String)> {
    db.rows("SELECT host_id, service, target, try::text FROM fleet_orders")
        .into_iter()
        .map(|row| {
            let mut cells = row.into_iter().map(|cell| cell.expect("a cell"));
            let mut next = || cells.next().expect("four cells");
            (next(), next(), next(), next())
        })
        .collect()
}

/// The host the operator aborts through A in the last round both serve: in that round its beat
/// reaches B, and A stops before it is heard again, so only B can send the abort.
const ABORTED: &str = "h3";

fn aborted() -> usize {
    HOSTS
        .iter()
        .position(|name| *name == ABORTED)
        .expect("a host of the fleet")
}

/// Proves: through a start-first rolling update of the coordinator (A alone, A and B on one
/// database with the hosts' beats reaching either, then A stopped and B alone), no host misses an
/// order. Every update order either coordinator wrote reaches its host and the whole fleet ends on
/// the desired version; the update whose answer A lost is told again with the same try at the
/// host's next beat, which B hears; the operator's abort queued through A while both serve reaches
/// its host once, with A's order id, though only B hears the host after it; and no order or queued
/// row is left when the rollout is over.
///
/// Catches: an order kept in the memory of the coordinator that decided it (lost when A stops, or
/// never known to B), an order a lost answer leaves outstanding but never told again, an abort
/// sent by neither or by both, and a rollout that stalls when its coordinator is replaced.
#[tokio::test]
async fn a_rolling_update_of_the_coordinator_misses_no_order_of_a_heartbeating_fleet() {
    let (db, wall, a, b) = pair("rolling").await;
    let mut a = Some(a);
    let mut hosts: Vec<Host> = HOSTS.iter().map(|_| Host::new()).collect();
    let mut written = BTreeSet::new();
    let mut lost = None;
    let mut abort_id = None;
    for round in 0..ROUNDS {
        let elapsed = round * PERIOD;
        if round == OVERLAP_ENDS {
            // A has drained and stops: its store is closed and nothing of it is left to hear a beat.
            drop(a.take());
        }
        if let Some(old) = &a {
            old.at(&wall, elapsed);
        }
        b.at(&wall, elapsed);
        if round == OVERLAP_ENDS - 1 {
            assert_eq!(route(round, aborted()), Route::B, "B hears {ABORTED} next");
            let old = a.as_ref().expect("A serves while both run");
            let answer = old
                .coordinator()
                .abort(
                    &operator(),
                    proto::AbortRequest {
                        service: WEB.to_owned(),
                        node_id: ABORTED.to_owned(),
                        reason: "the rolling update's abort".to_owned(),
                    },
                )
                .expect("an abort is queued");
            abort_id = Some(format!(
                "abort-{}",
                answer.entry.expect("its audit entry").seq
            ));
        }
        for (index, (name, host)) in HOSTS.iter().zip(hosts.iter_mut()).enumerate() {
            let node = match (route(round, index), &a) {
                (Route::A, Some(old)) => old,
                _ => &b,
            };
            let (reply, order) = ask_typed(&node.rig, name, DAEMON, host.beat());
            // The first update A answers while both serve is lost on the way back: the host never
            // hears it, and A stops before the host asks again.
            if lost.is_none() && round >= B_STARTS && route(round, index) == Route::A && a.is_some()
            {
                let mut deaf = Host::new();
                deaf.hear(&reply, order.as_ref());
                if let Some(told @ Told::Update { .. }) = deaf.told.pop() {
                    lost = Some((index, host.told.len(), told));
                    written.extend(order_rows(&db));
                    continue;
                }
            }
            host.hear(&reply, order.as_ref());
            written.extend(order_rows(&db));
        }
        if a.is_none() && hosts.iter().all(Host::at_2) && order_rows(&db).is_empty() {
            break;
        }
    }

    for (name, host) in HOSTS.iter().zip(&hosts) {
        assert!(
            host.at_2(),
            "{name} runs {:?}: it missed an order",
            host.running
        );
    }
    // Every update order either coordinator wrote reached its host.
    for (host, service, target, _) in &written {
        let index = HOSTS
            .iter()
            .position(|name| name == host)
            .expect("a host of the fleet");
        assert!(
            hosts[index].told.iter().any(|told| matches!(
                told,
                Told::Update { service: s, version, .. } if s == service && version == target
            )),
            "{host} was never told the order of {service} to {target}"
        );
    }
    // The next word the host whose answer A lost had from the coordinators is the same order, the
    // same try.
    let (index, before, lost) = lost.expect("A answered an update while both served");
    assert_eq!(hosts[index].told.get(before), Some(&lost));
    // The abort reached its host once, with A's id.
    let abort_id = abort_id.expect("the abort was queued");
    let aborts: Vec<&Told> = hosts
        .iter()
        .flat_map(|host| &host.told)
        .filter(|told| matches!(told, Told::Abort { .. }))
        .collect();
    assert_eq!(
        aborts,
        vec![&Told::Abort {
            service: WEB.to_owned(),
            order_id: abort_id,
        }]
    );
    assert!(
        hosts[aborted()]
            .told
            .iter()
            .any(|told| matches!(told, Told::Abort { .. })),
        "the abort went to {ABORTED}"
    );
    assert!(order_rows(&db).is_empty(), "an order is left");
    assert!(
        db.rows("SELECT 1 FROM fleet_operator_orders").is_empty(),
        "a queued operator order is left"
    );
}
