//! The halt generations of a reply, each handed to the held-halt store with the one the reply
//! before named, which judges them.

use hostkeeper_link::{Feature, HeldHalt, Reply};

use crate::{Rig, recorded, service};

fn halts(generations: &[(&str, u64)], features: &[Feature]) -> Reply {
    Reply {
        features: features.to_vec(),
        halt_generations: generations
            .iter()
            .map(|(name, generation)| (service(name), *generation))
            .collect(),
        ..Reply::default()
    }
}

/// Catches a loop that judges a generation itself against a value it kept (so the store, which
/// alone knows whether the service holds a halt, never hears an equal or older one), one that
/// hands a generation over without `halt-v1`, and one that hands over the wrong generation
/// before: every generation of a `halt-v1` reply reaches the store in order, with the one the
/// `halt-v1` reply before named (none for the first, 0 for a service it did not name), and none
/// of a reply without it; a service that holds no halt keeps none.
#[test]
fn every_halt_generation_of_a_reply_goes_to_the_store_and_none_without_halt_v1() {
    let rig = Rig::new("halts-heard");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    for generation in [5, 3, 7] {
        rig.world
            .link
            .answer(Ok(halts(&[("x", generation)], &[Feature::HaltV1])));
    }
    rig.world.link.answer(Ok(halts(&[("x", 20)], &[])));
    rig.world
        .link
        .answer(Ok(halts(&[("x", 8), ("y", 1)], &[Feature::HaltV1])));
    for _ in 0..5 {
        heartbeater.beat_once();
    }
    let heard = [
        ("x", None, 5),
        ("x", Some(5), 3),
        ("x", Some(3), 7),
        ("x", Some(7), 8),
        ("y", Some(0), 1),
    ]
    .map(|(name, before, generation)| (name.to_owned(), before, generation));
    assert_eq!(rig.world.halts.heard(), heard);
    assert_eq!(rig.world.halts.held(&service("x")), None);
}

/// Catches a halt held under no generation lifted by the first one a reply names when no reply
/// came before it, or one held under a generation lifted by the same one again: the first
/// generation since the start holds it, the same again leaves it, and only a greater one lifts
/// it.
#[test]
fn a_halt_is_held_under_the_first_generation_heard_and_lifted_only_by_a_greater_one() {
    let rig = Rig::new("halts-held");
    rig.install(vec![recorded("x", "1.0")]);
    let x = service("x");
    let unheld = HeldHalt {
        reason: None,
        generation: 0,
        written_unix: None,
    };
    rig.world.halts.halt(&x, unheld);
    let mut heartbeater = rig.heartbeater();
    for generation in [4, 4] {
        rig.world
            .link
            .answer(Ok(halts(&[("x", generation)], &[Feature::HaltV1])));
        heartbeater.beat_once();
        assert_eq!(rig.world.halts.held(&x), Some(4));
    }
    rig.world
        .link
        .answer(Ok(halts(&[("x", 5)], &[Feature::HaltV1])));
    heartbeater.beat_once();
    assert_eq!(rig.world.halts.held(&x), None);
}

/// Catches the first clear of a service lost on a host whose halt came while no reply named the
/// service (its generation was 0, which a reply leaves out): the halt is held under 0, and the
/// reply naming generation 1 lifts it.
#[test]
fn a_halt_before_any_clear_is_lifted_by_the_first_clear() {
    let rig = Rig::new("halts-first-clear");
    rig.install(vec![recorded("x", "1.0")]);
    let x = service("x");
    let mut heartbeater = rig.heartbeater();
    rig.world.link.answer(Ok(halts(&[], &[Feature::HaltV1])));
    heartbeater.beat_once();
    let unheld = HeldHalt {
        reason: None,
        generation: 0,
        written_unix: None,
    };
    rig.world.halts.halt(&x, unheld);
    rig.world
        .link
        .answer(Ok(halts(&[("x", 1)], &[Feature::HaltV1])));
    heartbeater.beat_once();
    assert_eq!(rig.world.halts.heard(), [("x".to_owned(), Some(0), 1)]);
    assert_eq!(rig.world.halts.held(&x), None);
    heartbeater.beat_once();
    assert_eq!(rig.world.halts.heard(), [("x".to_owned(), Some(0), 1)]);
}

/// Catches a halt generation the store does not take dropped silently, or not tried again: the
/// line says so, and the next reply that carries it hands it over again.
#[test]
fn a_halt_generation_the_store_does_not_take_is_logged_and_handed_over_again() {
    let rig = Rig::new("halts-unwritable");
    rig.install(vec![recorded("x", "1.0")]);
    let mut heartbeater = rig.heartbeater();
    rig.world.halts.fail(true);
    rig.world
        .link
        .answer(Ok(halts(&[("x", 2)], &[Feature::HaltV1])));
    heartbeater.beat_once();
    assert_eq!(
        rig.world.log.lines(),
        ["error: halt generation 2 of x is not taken: permission denied"]
    );
    rig.world.halts.fail(false);
    rig.world
        .link
        .answer(Ok(halts(&[("x", 2)], &[Feature::HaltV1])));
    heartbeater.beat_once();
    assert_eq!(
        rig.world.halts.heard(),
        [("x".to_owned(), None, 2), ("x".to_owned(), Some(2), 2)]
    );
}
