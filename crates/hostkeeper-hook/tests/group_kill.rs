//! The harness stops a parked step as a whole: every process of its group, not only the hook
//! it started.

use std::fs::{self, File};

use hostkeeper_hook::conformance::Conformance;

mod support;
use support::{ExampleFake, Scratch, example};

/// Catches a harness that kills only the leader of a parked step's process group. The example
/// hook run with `--behaviour=descendant` starts, before its first kill point in `prepare`, a
/// child process in its own group that takes an exclusive lock on a file beside the sandboxes,
/// writes its pid and parks; the harness then stops the hook at each kill point of `prepare`
/// (`crash_resume` returns only if every kill point parked and the runs resumed to the world of
/// an uninterrupted run, so each leader was killed there), and the next stop's child needs the
/// lock the previous child held. After the harness returns, this test takes the same lock,
/// blocking: that succeeds only once every descendant is dead. A harness that killed only the
/// leaders leaves the test blocked on the lock (or the harness blocked on the next child's
/// lock) until the runner's slow-test guard ends it; the test asserts no time.
#[test]
fn a_kill_point_stops_the_whole_process_group() {
    let scratch = Scratch::new("group-kill");
    let hook = example(&["update-hook", "--behaviour=descendant"]);
    let mut fake = ExampleFake::default();
    let points = Conformance::new(&hook, &mut fake, scratch.path())
        .crash_resume()
        .expect("the hook resumes from every kill point");
    assert!(points > 0, "no kill point was reached");

    let pid = fs::read_to_string(scratch.path().join("descendant.pid"))
        .expect("a descendant started and wrote its pid");
    assert!(
        pid.parse::<u32>().is_ok_and(|pid| pid > 0),
        "not a pid: {pid:?}"
    );
    let lock = File::open(scratch.path().join("descendant.lock")).expect("the lock file");
    lock.lock()
        .expect("the lock is free once every descendant is dead");
}
