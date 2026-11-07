//! The checks of the operator web app under `ui/` at the repository root. Tests only: no crate
//! depends on this one and nothing of it is shipped.
//!
//! The app is static files (HTML, CSS and JavaScript modules) that call the coordinator's
//! HTTP/JSON gateway with the operator's token. This crate holds what its tests apply to them:
//!
//! - [`UI_FILES`]: every file of the app, read at build time with `include_str!`, so no test
//!   needs a path or a working directory;
//! - [`client_gen`]: the generator of the app's API client, `ui/gen/client.js`, and its
//!   TypeScript declarations, `ui/gen/client.d.ts`, from the command table and the descriptors
//!   of `hostkeeper-proto`;
//! - [`call_sites`] and [`check_calls`]: the client's functions the app's pages call, read
//!   from the source, and the check that each method is one of the protobuf descriptors;
//! - [`derived_calls`], [`calls_json`] and [`listed_calls`]: `ui/calls.json`, the list of those
//!   functions with their methods and the pages that call them, derived from the source;
//! - [`scan`]: what the sources must never contain (token storage, logging, a URL of another
//!   origin, an inline script or handler, markup built from text).
//!
//! The tests under `tests/` also run the app's data layer and pages in a JavaScript engine
//! against a fake gateway, and send each call of `ui/calls.json` to the real coordinator, built
//! from its flags and called through its router in process, with an operator's token
//! (`tests/real_router.rs`).

mod calls;
pub mod client_gen;
mod lexis;
pub mod scan;
mod sources;

pub use calls::{
    CallSite, DeriveError, ListError, Listed, ScanError, UnknownCall, call_sites, calls_json,
    check_calls, derived_calls, descriptor_methods, listed_calls,
};
pub use sources::{UI_FILES, UiFile};
