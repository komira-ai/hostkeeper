//! Behaviour tests of the bootstrap, against fakes of the network, of `pkgutil`, `installer` and `launchctl`,
//! and of the pause; every file is in a scratch directory.

mod cli;
mod files;
mod job;
mod package;
mod run;
mod settings;
mod signer;
mod support;
mod update_account;
