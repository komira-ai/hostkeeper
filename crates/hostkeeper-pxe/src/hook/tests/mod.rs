//! The tests of the hook steps: each step run through [`run`], over a scratch boot directory, a
//! fake `systemctl` that records what it is asked and a fake fetch that seals a directory, so
//! the exit status, standard error and the JSON report of each outcome are read as the updater
//! reads them.

pub(crate) mod kit;

mod ports;
mod prepare;
mod refusals;
mod steps;
