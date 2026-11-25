//! The methods of a generated service that are written by hand: `hk xcode verify` is a method of
//! the service `xcode`, which the table of the protos gives only `list` and `remove`. A hand
//! method is found by the service's command word and its own, listed after the generated
//! methods in `hk <service> --help` and in `docs/cli.md`, and run by its function in place of a
//! gateway call. A service that the table does not have has none.

use crate::args::Globals;
use crate::{Flow, World};

/// What a hand method runs: the words after its name, and the global flags given before them.
type Run = fn(&mut World<'_>, Globals, &[Vec<u8>]) -> Flow<()>;

/// A flag of a hand method, for `docs/cli.md`.
pub(crate) struct Flag {
    pub(crate) name: &'static str,
    pub(crate) value: &'static str,
    pub(crate) help: &'static str,
}

/// A hand-written method of a generated service.
pub(crate) struct Method {
    /// The command word of the service, as the table has it (`xcode`).
    pub(crate) service: &'static str,
    /// The command word of the method.
    pub(crate) method: &'static str,
    /// What it does, in the one paragraph `--help` and `docs/cli.md` show.
    pub(crate) doc: &'static str,
    /// Its own flags, at least one (the global flags are not listed here).
    pub(crate) flags: &'static [Flag],
    pub(crate) run: Run,
}

/// Every hand method.
pub(crate) const METHODS: &[Method] = &[Method {
    service: "xcode",
    method: "verify",
    doc: "Checks an Xcode archive on the operator's own Mac, macOS only, and writes the record of \
          what it found to ~/.hostkeeper/xcode/<sha256>.json, mode 0600, and to stdout: the \
          programs, the archive's kind, Apple's signature and published root, a damaged copy and a \
          file that is no archive, the archive's table of contents, and the expansion with the \
          version and build read from the app. Not a gateway call: no coordinator is asked. \
          Exits 0 when the verdict is pass and 1 otherwise; `hk xcode verify --help` says each \
          check.",
    flags: &[Flag {
        name: "--apple-root",
        value: "FILE",
        help: "Apple's root certificate (DER or PEM), in place of fetching it over HTTPS from \
               Apple's certificate authority page.",
    }],
    run: crate::xcode_verify::xcode_verify,
}];

/// The hand methods of the service whose command word is `service`.
pub(crate) fn of(service: &str) -> impl Iterator<Item = &'static Method> {
    METHODS.iter().filter(move |m| m.service == service)
}

/// The hand method `method` of `service`.
pub(crate) fn find(service: &[u8], method: &[u8]) -> Option<&'static Method> {
    METHODS
        .iter()
        .find(|m| m.service.as_bytes() == service && m.method.as_bytes() == method)
}
