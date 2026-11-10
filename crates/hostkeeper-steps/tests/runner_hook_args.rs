//! The hook command's leading arguments: the runner gives the order's `hook_args` to every step
//! it launches.

mod support;

use std::fs;
use std::sync::{Arc, Mutex};

use hostkeeper_hook::Step;
use support::Rig;

type Seen = Arc<Mutex<Vec<(Step, Vec<String>)>>>;

fn recording(rig: &Rig) -> Seen {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    rig.fake.on_run(move |call| {
        sink.lock()
            .unwrap()
            .push((call.invocation.step, call.hook_args.to_vec()));
    });
    seen
}

/// Every step of an update reaches the launcher with the order's leading arguments.
#[test]
fn every_step_is_launched_with_the_orders_hook_args() {
    let rig = Rig::new("hook-args-steps");
    let seen = recording(&rig);
    let mut order = rig.order();
    order.hook_args = vec!["update-hook".to_owned(), "--fast".to_owned()];
    rig.runner().run(&order).unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.iter().map(|(step, _)| *step).collect::<Vec<_>>(),
        [Step::Prepare, Step::Cutover, Step::Verify, Step::Cleanup]
    );
    assert!(
        seen.iter()
            .all(|(_, args)| args == &["update-hook", "--fast"]),
        "{seen:?}"
    );
}

/// An order with no leading arguments launches every step with none.
#[test]
fn an_order_without_hook_args_launches_steps_with_none() {
    let rig = Rig::new("hook-args-none");
    let seen = recording(&rig);
    rig.runner().run(&rig.order()).unwrap();
    let seen = seen.lock().unwrap();
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|(_, args)| args.is_empty()), "{seen:?}");
}

/// The `rollback` that falls back to the previous release's hook (the new one cannot start), for
/// an order that names no `previous_hook`, is given the order's leading arguments.
#[test]
fn the_rollback_fallback_to_the_previous_release_keeps_the_hook_args() {
    let rig = Rig::new("hook-args-fallback");
    fs::remove_file(rig.new_dir.join("hook")).unwrap();
    let seen = recording(&rig);
    let mut order = rig.order();
    order.hook_args = vec!["update-hook".to_owned()];
    rig.runner().run(&order).unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(*seen, [(Step::Rollback, vec!["update-hook".to_owned()])]);
    let runs = rig.fake.runs();
    let support::Event::Run { executable, .. } = &runs[0] else {
        unreachable!()
    };
    assert_eq!(executable, &rig.old_dir.join("hook"));
}

/// The `rollback` that falls back to the previous release runs that release's own hook, program
/// and leading arguments, when the order names it (`previous_hook`): the new release's hook path
/// need not exist in the old tree.
#[test]
fn the_rollback_fallback_runs_the_previous_releases_own_hook_when_the_order_names_it() {
    let rig = Rig::new("hook-args-previous-hook");
    fs::remove_file(rig.new_dir.join("hook")).unwrap();
    fs::remove_file(rig.old_dir.join("hook")).unwrap();
    fs::write(rig.old_dir.join("old-hook"), "#!hook old\n").unwrap();
    let seen = recording(&rig);
    let mut order = rig.order();
    order.hook_args = vec!["update-hook".to_owned()];
    order.previous_hook = Some(hostkeeper_steps::PreviousHook {
        binary: "old-hook".into(),
        args: vec!["hook".to_owned(), "--old".to_owned()],
    });
    rig.runner().run(&order).unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(
        *seen,
        [(Step::Rollback, vec!["hook".to_owned(), "--old".to_owned()])]
    );
    let runs = rig.fake.runs();
    let support::Event::Run { executable, .. } = &runs[0] else {
        unreachable!()
    };
    assert_eq!(executable, &rig.old_dir.join("old-hook"));
}
