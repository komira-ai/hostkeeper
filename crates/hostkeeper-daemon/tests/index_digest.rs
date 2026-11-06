//! The index digest of an order: decoded like the package digest, and handed to the package
//! store with the version it names, for the store to check the release index against.

mod support;

use hostkeeper_daemon::verb::DIGEST_RULE;
use hostkeeper_daemon::{Refusal, Reply};
use hostkeeper_proto::hostkeeper::agent::v1::{Order, order};
use support::force::WEB;
use support::{Rig, d, digest, force, force_rollback, install, update};

/// An index digest unlike the package digest the tests' orders name.
fn index() -> String {
    digest("cd")
}

/// `order` with the index digest `text` in its verb.
fn indexed(mut order: Order, text: &str) -> Order {
    let slot = match order.verb.as_mut().expect("a verb") {
        order::Verb::Update(a) => &mut a.index_digest,
        order::Verb::Install(a) => &mut a.index_digest,
        order::Verb::Force(a) => &mut a.index_digest,
        order::Verb::ForceRollback(a) => &mut a.index_digest,
        other => panic!("a verb with no index digest: {other:?}"),
    };
    *slot = text.to_owned();
    order
}

/// Catches an index digest that is not decoded or not checked for its form: a digest of 63
/// characters, one in upper case and one that is not hex, in an `update`, an `install`, a
/// `force` and a `force_rollback`, is refused `invalid_order` naming `<verb>.index_digest`, and
/// the package store is never asked.
#[test]
fn a_malformed_index_digest_is_an_invalid_order() {
    let rig = Rig::new("index-digest-malformed");
    let daemon = rig.daemon();
    for bad in [
        index()[1..].to_owned(),
        index().to_uppercase(),
        "x".repeat(64),
    ] {
        let rows = [
            ("update", update(WEB, "2.0.0", &d(), 1)),
            ("install", install(WEB, "2.0.0", &d(), 1)),
            ("force", force(WEB, "2.0.0", &d(), 1, 1)),
            ("force_rollback", force_rollback(WEB, "1.0.0", &d(), 1)),
        ];
        for (verb, order) in rows {
            assert_eq!(
                daemon.order(&indexed(order, &bad)),
                Reply::Refused {
                    refusal: Refusal::InvalidOrder,
                    detail: format!("the order's {verb}.index_digest {DIGEST_RULE}"),
                },
                "{verb} {bad}"
            );
        }
    }
    assert!(rig.store.asked().is_empty());
}

/// Catches an index digest that stops at the decoder: an `update`, an `install` and a `force`
/// that carry one hand it to the package store with the order's package, and an order that
/// carries none (a coordinator that does not send it) hands over none.
#[test]
fn the_index_digest_of_an_order_reaches_the_package_store() {
    let rig = Rig::new("index-digest-store");
    let daemon = rig.daemon();
    rig.running(WEB, support::BASE);
    rig.stage(WEB, "2.0.0");
    rig.stage(WEB, "3.0.0");
    rig.stage(WEB, "4.0.0");
    rig.stage(WEB, "5.0.0");
    rig.stage("api", "1.0.0");
    let named = Some(index());
    let orders = [
        (update(WEB, "2.0.0", &d(), 1), None),
        (
            indexed(update(WEB, "3.0.0", &d(), 2), &index()),
            named.clone(),
        ),
        (
            indexed(force(WEB, "4.0.0", &d(), 3, 0), &index()),
            named.clone(),
        ),
        (force(WEB, "5.0.0", &d(), 4, 0), None),
        (indexed(install("api", "1.0.0", &d(), 1), &index()), named),
    ];
    for (order, expected) in orders {
        let before = rig.store.index_digests().len();
        let reply = daemon.order(&order);
        let seen = rig.store.index_digests();
        assert_eq!(seen[before..], [expected], "{order:?} gave {reply:?}");
    }
}
