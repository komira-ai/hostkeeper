# Testing a service's update hook with the conformance harness

A service that ships an update hook can prove, in its own tests, that the hook keeps the hook
contract. The harness is part of the `hostkeeper-hook` crate, behind the feature `conformance`. It
starts your real hook as the updater would, against a fake of your service that you write, and
refuses the hook when it breaks the contract. The contract itself is in
[crates/hostkeeper-hook/README.md](../crates/hostkeeper-hook/README.md).

## Use it from your repository

Add the crate as a git dev-dependency, pinned to the hostkeeper release you build against. The
`tag` is `v` followed by that release's version:

```toml
[dev-dependencies]
hostkeeper-hook = { git = "https://github.com/komira-ai/hostkeeper", tag = "v0.1.0", features = ["conformance"] }
```

The crate depends on no other crate of the hostkeeper workspace, so nothing else is needed.

Then write one test. It builds your hook (a binary of your own crate, or any executable), gives
the harness its path and the arguments that come before the step, and passes a `FakeService` of
your own:

```rust
use std::path::Path;
use hostkeeper_hook::conformance::{HookCommand, check};

#[test]
fn the_hook_keeps_the_contract() {
    let hook = HookCommand::new(Path::new(env!("CARGO_BIN_EXE_my-service")))
        .with_hook_args(&["update-hook"]);
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("conformance");
    // The harness reuses what it finds there: start each run from an empty directory.
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch).expect("remove the last run's sandboxes");
    }
    std::fs::create_dir_all(&scratch).expect("create the scratch directory");
    let summary = check(&hook, &mut MyServiceFake::default(), &scratch).unwrap_or_else(|e| panic!("{e}"));
    assert!(summary.kill_points > 0);
}
```

`hook_args` is `["update-hook"]` by default; pass `&[]` for an executable that takes the step
first. The directory given to `check` is yours: the harness makes one sandbox under it for each
scenario and does not remove them, so start each run from an empty one.

A `FakeService` is the world your hook acts on, in a directory:

```rust
impl FakeService for MyServiceFake {
    // Stage the release in the sandbox's artifact directory; set the world's starting state.
    fn init(&mut self, sandbox: &Sandbox) -> io::Result<()> { /* ... */ }
    // The environment of the hook: how it finds the sandbox's directories. Nothing else is inherited.
    fn env(&self, sandbox: &Sandbox) -> Vec<(OsString, OsString)> { /* ... */ }
    // The table that the hook sees as its `[params]`.
    fn params(&self) -> Params { /* ... */ }
    // Make `step` end as the fault says (fail, defer or halt); Ok(false) if your service cannot.
    fn inject(&mut self, sandbox: &Sandbox, step: Step, fault: Fault) -> io::Result<bool> { /* ... */ }
    // What only the update needed and is still there (a staged copy, a cordon).
    fn residue(&self, sandbox: &Sandbox) -> io::Result<Vec<String>> { /* ... */ }
}
```

Everything your hook changes in the service must land in the sandbox's world directory, and each
change must pass a kill point (`hostkeeper_hook::conformance::gate::point`) first, so that the
harness can stop the hook before it. The module documentation of `hostkeeper_hook::conformance`
and the small example in `crates/hostkeeper-hook/tests/support/mod.rs` show a complete fake.

## What the harness checks

1. Every run ends with an exit code of the contract (0, 1, 75 or 76), never by a signal, and
   writes the JSON last line, strictly read; a 75 carries `retry_after_secs`.
2. Each step of a successful update, and `rollback`, run twice in a row: the second run ends the
   same way and leaves the world as the first left it.
3. No run changes the staged release or the service's own data directory.
4. After `cutover`, a `rollback` that says it was reversible leaves the world as it was before
   `prepare`, with nothing of the update left behind; `cleanup` after a rollback changes nothing.
5. The hook is stopped at every kill point its fake reaches, and each step is run again after it
   finished; running on from there reaches the world an uninterrupted run reaches. The hook's
   record is compared as well as the world.
6. The updater's table of next steps is driven through success, and through a failure, a deferral
   and a halt at each step; a deferral leaves the world as it was.
7. A params file with an `updater.*` key the hook does not know is no reason to refuse.
8. With an exclusive lock held on the hook's record directory, every step refuses (exit 1) and
   changes nothing.

Each check also runs as a first install (no previous release or version). A hook may declare up to
three safety behaviours that the checks refuse by default, with `Conformance::with_allowances`;
they are described in the README of the crate.

## What it does not check

The harness judges what a hook does to its sandbox world and its record, from outside. It does not
run the real updater, systemd, or anything as root. It does not fetch or verify a package, and it
does not apply the timeouts a manifest sets: it waits for no time, only for a child to exit or to
announce that it is parked at a kill point. It does not run your real service. Whether your fake
matches your real service is for your own tests to settle; the harness can only be as faithful as
the fake it is given.

## How this repository keeps the line working

`crates/hostkeeper-hook/tests/consumer` is a small crate that is not a member of the hostkeeper
workspace and has exactly the dependency line above. The test
`a_consumer_crate_outside_the_workspace_compiles_against_the_harness` checks that the line in this
page, the line in that crate and this release's version agree, copies the crate to a scratch
directory, and builds and runs it offline with the line's source (`git` and `tag`) replaced by a path to
the checkout and its features as written. Its test runs the
harness against the example hook of `hostkeeper-hook`.
