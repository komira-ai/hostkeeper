//! The reloadable TLS configuration against real handshakes, driven in-process.
//!
//! Every handshake is a `tokio_rustls` client and acceptor over `tokio::io::duplex`, so no
//! socket is opened and what is checked is what a client sees, not what the source says it
//! holds. The files are in a scratch directory under the target directory.

mod logs;
mod reload;
mod sources;
mod support;
