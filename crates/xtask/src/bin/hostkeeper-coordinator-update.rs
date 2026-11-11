//! `hostkeeper-coordinator-update`: the coordinator's machine updates the coordinator's container
//! from each attested release (docs/deploy/coordinator-update.md). This file only forwards to
//! [`xtask::coordinator_update_main`].

fn main() -> std::process::ExitCode {
    xtask::coordinator_update_main()
}
