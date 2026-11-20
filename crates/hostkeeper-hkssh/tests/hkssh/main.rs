//! `hkssh` against a fake coordinator: a loopback gRPC server with TLS, a CA made at run time,
//! and a double for the OpenSSH programs. Real `ssh-keygen` and `ssh-agent` are used only where
//! a test says so (`programs.rs`).

mod commands;
mod fake;
mod programs;
mod proxy;
mod stamp;
mod state;
mod usage;
