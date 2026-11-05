//! The TLS configuration of a server listener whose certificate, key and client CA are files
//! that change while the process runs.
//!
//! [`TlsSource`] holds the `rustls` server configuration the next handshake uses and
//! re-reads the files when the caller says so ([`TlsSource::reload`], which the caller
//! calls on an interval of its own). The caller builds a `tokio_rustls::TlsAcceptor` from
//! [`TlsSource::config`] for each connection it accepts.
//!
//! - A connection already established keeps the configuration it was accepted under,
//!   including the trust roots it was verified against. A new handshake uses whatever
//!   configuration is current when it starts.
//! - A change is applied only if the new files make a working configuration: every file
//!   reads, the chain and the key parse, the key matches the certificate, the client CA
//!   holds at least one certificate, and the leaf has not expired. Otherwise the
//!   configuration in use stays, and the failure is logged and counted
//!   ([`render_metrics`]). A pair renewed in two writes is a mismatch between them; the
//!   next look finds the pair whole and applies it.
//! - The first load must succeed, as it always has: a server does not start on a bad
//!   file.

mod config;
mod source;

pub use config::{Alpn, TlsError, TlsFiles};
pub use source::{Outcome, Reloads, TlsSource, render_metrics};
