//! The flags of the maintenance runs: `--macos-update-via`, the way a Mac's macOS update is
//! applied (docs/design/maintenance-updates.md, section 5.5.2).
//!
//! The design names two: `daemon`, the Mac's own daemon installing the update, and
//! `apple-business`, Apple Business's device management installing it after the drain. Apple
//! Business offers no call that installs an update on one Mac, so the second is not built: this
//! build takes `daemon` alone, and refuses `apple-business` at start, saying why, as it refuses
//! any other value.

use clap::Args;

/// The value of `--macos-update-via` the design names and this build does not take.
pub const APPLE_BUSINESS: &str = "apple-business";

/// The way a Mac's macOS update of a maintenance run is applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MacosUpdateVia {
    /// The Mac's daemon installs the update itself, with its update account.
    #[default]
    Daemon,
}

/// The flags of the maintenance runs.
#[derive(Clone, Debug, Args)]
pub struct MaintenanceFlags {
    /// How a Mac's macOS update is applied in a maintenance run: `daemon` (the default), the
    /// Mac's own daemon installing it. `apple-business` (device management installing it after
    /// the drain) is refused at start: Apple Business offers no call that installs an update on
    /// one Mac, so it is not built. Any other value is refused too.
    #[arg(
        long,
        value_name = "WAY",
        default_value = "daemon",
        value_parser = update_via
    )]
    pub macos_update_via: MacosUpdateVia,
}

/// `text` of `--macos-update-via`: `daemon`, and nothing else.
fn update_via(text: &str) -> Result<MacosUpdateVia, String> {
    match text {
        "daemon" => Ok(MacosUpdateVia::Daemon),
        APPLE_BUSINESS => Err(
            "apple-business is not built: Apple Business offers no call that installs an update \
             on one Mac; only daemon is taken"
                .to_owned(),
        ),
        _ => Err("only daemon is taken".to_owned()),
    }
}

#[cfg(test)]
#[path = "config_maintenance_tests.rs"]
mod tests;
