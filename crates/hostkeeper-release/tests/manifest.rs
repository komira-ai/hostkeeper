//! The checks of `hostkeeper-release` (docs/design/packages.md sections 3.4, 4.4 and 7.1, and
//! packages-repositories.md section 2.2; packages-tests.md, A1 and A16): one case per rule, each under and over every limit, on releases
//! that pass every other rule (`support`).
//!
//! One test binary, so every arm of a function is covered by the same binary.

#[path = "manifest/support.rs"]
mod support;

#[path = "manifest/containers.rs"]
mod containers;
#[path = "manifest/grammar.rs"]
mod grammar;
#[path = "manifest/hook.rs"]
mod hook;
#[path = "manifest/installers.rs"]
mod installers;
#[path = "manifest/paths.rs"]
mod paths;
#[path = "manifest/receipt.rs"]
mod receipt;
#[path = "manifest/repository.rs"]
mod repository;
#[path = "manifest/shape.rs"]
mod shape;
#[path = "manifest/staged.rs"]
mod staged;
#[path = "manifest/targets.rs"]
mod targets;
