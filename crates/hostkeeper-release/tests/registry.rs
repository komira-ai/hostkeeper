//! The rules of `hostkeeper_release::registry` (packages-repositories.md sections 3.1 and 3.2):
//! what makes a repository valid, the files a host holds for a set of them, their digest, and
//! the fingerprints of an OpenPGP keyring.
//!
//! One test binary, so every arm of a function is covered by the same binary.

#[path = "registry/support.rs"]
mod support;

#[path = "registry/descriptor.rs"]
mod descriptor;
#[path = "registry/keys.rs"]
mod keys;
#[path = "registry/private.rs"]
mod private;
#[path = "registry/real.rs"]
mod real;
#[path = "registry/render.rs"]
mod render;
#[path = "registry/rules.rs"]
mod rules;
