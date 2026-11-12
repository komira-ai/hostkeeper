//! The repository installer with the `apt` backend, against a fake of the tools port that
//! simulates apt, dpkg and dpkg-deb on a model (packages-tests.md, "The `apt` installer" and
//! "The hook, scripts and paths"; packages-repositories.md section 4, every row of 4.5).
//! The modules are in `apt/`, the fake in `support/apt_fake.rs`.

#[path = "support/apt_fake.rs"]
mod apt_fake;
#[path = "support/mod.rs"]
mod support;

#[path = "apt/activate.rs"]
mod activate;
#[path = "apt/commands.rs"]
mod commands;
#[path = "apt/drift.rs"]
mod drift;
#[path = "apt/harness.rs"]
mod harness;
#[path = "apt/io.rs"]
mod io;
#[path = "apt/loaded.rs"]
mod loaded;
#[path = "apt/lock_wait.rs"]
mod lock_wait;
#[path = "apt/mirror.rs"]
mod mirror;
#[path = "apt/order.rs"]
mod order;
#[path = "apt/policy.rs"]
mod policy;
#[path = "apt/remove.rs"]
mod remove;
#[path = "apt/stage.rs"]
mod stage;
#[path = "apt/tree.rs"]
mod tree;
