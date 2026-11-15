//! The flags the coordinator without its MDM no longer reads: the `--mdm-*` flags, the
//! `--mac-serial-allowlist` and the `--proxy-cache-dir`. The release before this one still ran the
//! MDM and its service passes them, and its store still holds the MDM's sealed secrets, so this
//! release, which deploys over that service's arguments, accepts each flag, with the value shape it
//! had, and ignores it: a start that gives one logs a line naming it (`retired, ignored`). They are
//! not in the help. A later release removes them, and then a start that gives one is a usage error.

use std::ffi::OsString;

use clap::Args;

/// The retired flags, none of which is read.
#[derive(Clone, Debug, Default, Args)]
pub struct RetiredFlags {
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_listen: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_command_timeout_s: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_queue_sweep_s: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_pkg_dir: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_api_url: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_api_key_file: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_webhook_listen: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_profile_identifier: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_daemon_config: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_stores: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_macos_pkg: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_pkg_allowlist: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_host: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_baseline_file: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_baseline_check_s: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mdm_secrets_key_file: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    mac_serial_allowlist: Option<OsString>,
    #[arg(long, hide = true, value_name = "VALUE")]
    proxy_cache_dir: Option<OsString>,
    #[arg(long, hide = true)]
    mdm_listen_wildcard: bool,
}

impl RetiredFlags {
    /// The names, with their `--`, of the retired flags that were given.
    #[must_use]
    pub fn given(&self) -> Vec<&'static str> {
        let held = [
            ("--mdm-listen", self.mdm_listen.is_some()),
            (
                "--mdm-command-timeout-s",
                self.mdm_command_timeout_s.is_some(),
            ),
            ("--mdm-queue-sweep-s", self.mdm_queue_sweep_s.is_some()),
            ("--mdm-pkg-dir", self.mdm_pkg_dir.is_some()),
            ("--mdm-api-url", self.mdm_api_url.is_some()),
            ("--mdm-api-key-file", self.mdm_api_key_file.is_some()),
            ("--mdm-webhook-listen", self.mdm_webhook_listen.is_some()),
            (
                "--mdm-profile-identifier",
                self.mdm_profile_identifier.is_some(),
            ),
            ("--mdm-daemon-config", self.mdm_daemon_config.is_some()),
            ("--mdm-stores", self.mdm_stores.is_some()),
            ("--mdm-macos-pkg", self.mdm_macos_pkg.is_some()),
            ("--mdm-pkg-allowlist", self.mdm_pkg_allowlist.is_some()),
            ("--mdm-host", self.mdm_host.is_some()),
            ("--mdm-baseline-file", self.mdm_baseline_file.is_some()),
            (
                "--mdm-baseline-check-s",
                self.mdm_baseline_check_s.is_some(),
            ),
            (
                "--mdm-secrets-key-file",
                self.mdm_secrets_key_file.is_some(),
            ),
            (
                "--mac-serial-allowlist",
                self.mac_serial_allowlist.is_some(),
            ),
            ("--proxy-cache-dir", self.proxy_cache_dir.is_some()),
            ("--mdm-listen-wildcard", self.mdm_listen_wildcard),
        ];
        held.into_iter()
            .filter_map(|(name, given)| given.then_some(name))
            .collect()
    }
}
