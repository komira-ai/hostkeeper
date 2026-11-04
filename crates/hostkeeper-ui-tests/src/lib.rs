//! The checks of the operator web app under `ui/` at the repository root. Tests only: no crate
//! depends on this one and nothing of it is shipped.
//!
//! The app is static files (HTML, CSS and JavaScript modules) that call the coordinator's
//! HTTP/JSON gateway with the operator's token. This crate holds what its tests apply to them:
//!
//! - [`UI_FILES`]: every file of the app, read at build time with `include_str!`, so no test
//!   needs a path or a working directory;
//! - [`call_sites`] and [`check_calls`]: the operator API methods the app's pages call, read
//!   from the source, and the check that each is a method of the protobuf descriptors;
//! - [`listed_calls`]: `ui/calls.json`, the list of those methods with the pages that call them;
//! - [`scan`]: what the sources must never contain (token storage, logging, a URL of another
//!   origin, an inline script or handler, markup built from text).
//!
//! The tests under `tests/` also run the app's data layer and pages in a JavaScript engine
//! against a fake gateway.

mod calls;
mod lexis;
pub mod scan;
mod sources;

pub use calls::{
    CallSite, ListError, Listed, ScanError, UnknownCall, call_sites, check_calls,
    descriptor_methods, listed_calls,
};
pub use sources::{UI_FILES, UiFile};
