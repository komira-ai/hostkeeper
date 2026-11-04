//! The halt generations of a reply, raised in the held-halt store.

use hostkeeper_link::{Feature, HeldHalts, Reply};

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

/// Catches a halt lifted by a generation equal to or older than the held one, a generation not
/// written to the store, one written without `halt-v1`, and a loop that compares with a value it
/// kept in memory instead of the store (which the executor and `clear_halt` also write).
#[test]
fn a_halt_generation_in_the_reply_is_persisted_and_only_a_newer_one_lifts_it() {
    let rig = Rig::new("halts-newer");
    rig.install(vec![recorded("x", "1.0")]);
    let x = service("x");
    rig.world.halts.set(&x, 5);
    let mut heartbeater = rig.heartbeater();
    for generation in [5, 3, 7] {
        rig.world
            .link
            .answer(Ok(halts(&[("x", generation)], &[Feature::HaltV1])));
    }
    for _ in 0..3 {
        heartbeater.beat_once();
    }
    assert_eq!(rig.world.halts.raises(), [("x".to_owned(), 7)]);

    // The store moves on without the loop: the loop compares with the store.
    rig.world.halts.set(&x, 9);
    for generation in [8, 10] {
        rig.world
            .link
            .answer(Ok(halts(&[("x", generation)], &[Feature::HaltV1])));
    }
    // Without `halt-v1` nothing is written, however new.
    rig.world.link.answer(Ok(halts(&[("x", 20)], &[])));
    for _ in 0..3 {
        heartbeater.beat_once();
    }
    assert_eq!(
        rig.world.halts.raises(),
        [("x".to_owned(), 7), ("x".to_owned(), 10)]
    );
    assert_eq!(rig.world.halts.held(&x), 10);
}

/// Catches a halt generation that is not written silently, or not tried again: the line says
/// so, and the next reply that carries it writes it.
#[test]
fn a_halt_generation_that_cannot_be_written_is_logged_and_written_from_the_next_reply() {
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
        ["error: halt generation 2 of x is not written: permission denied"]
    );
    rig.world.halts.fail(false);
    rig.world
        .link
        .answer(Ok(halts(&[("x", 2)], &[Feature::HaltV1])));
    heartbeater.beat_once();
    assert_eq!(
        rig.world.halts.raises(),
        [("x".to_owned(), 2), ("x".to_owned(), 2)]
    );
    assert_eq!(rig.world.halts.held(&service("x")), 2);
}
