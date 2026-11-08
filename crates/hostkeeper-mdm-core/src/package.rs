//! The `mdm` service package (docs/design/two-programs.md section 6.2): the names and texts
//! the package builder (`cargo xtask release package --service mdm`) and the package's own
//! program (`bin/mdm`, crate `hostkeeper-mdm`) share, so the two cannot disagree.
//!
//! - **The layout is the package's.** A package's units are files of the package, the same
//!   bytes on every host, so the settings that name units, paths and loopback ports are fixed
//!   by the package ([`FIXED`]): the service user `mdm`, `/var/lib/mdm`, `/etc/mdm`, the
//!   binaries of the package's `current` version, the pkg directory `/var/lib/mdm-pkg`, the
//!   unit prefix `mdm`, the slice `mdm.slice` and the ports 9004, 9005 and 9001. The settings
//!   file on the host may leave these keys out or set them to the package's values;
//!   [`settings`] refuses any other value. Every other key is the site's, as for the kit.
//! - **The units** ([`UNITS`]) are the kit's services with those values in place, compiled in
//!   from `package/units/`. The daemon links every one into `/etc/systemd/system` from the
//!   package's `current` version, and starts and observes the [`RUN`] units. A package unit
//!   is a `.service`, `.socket` or `.timer` (the manifest loader's kinds), so the kit's slice
//!   and its path unit are not package units: the slice exists because the services name it,
//!   with its caps in a drop-in, and the certificate job restarts the proxy itself after each
//!   run, which the path unit did on a change of the certificate.
//! - **The site's values reach the units through drop-ins** ([`drop_ins`]): one
//!   `/etc/systemd/system/<unit>.d/settings.conf` for each unit that needs a site value (the
//!   slice's caps, the proxy's LAN, NanoDEP's token name for depsyncer, the ACME settings for
//!   the certificate job). The hook's `prepare` writes them, with the Caddyfile, from the
//!   settings.
//! - **The manifest** ([`manifest`]): the hook is `bin/mdm update-hook`, the params name the
//!   settings file ([`SETTINGS_FILE`]) and the hook's record directory ([`RECORD_DIR`]), and
//!   the health URL is NanoHUB's `/version` on loopback.

use crate::acme::Acme;
use crate::config::{KEYS, MdmConfig};
use crate::pins::Tool;
use crate::render::Unit;
use crate::settings::{Settings, SettingsError};

/// The service's name: its package, the prefix of its units, its user.
pub const SERVICE: &str = "mdm";

/// The only platform the package is built for: the pinned tools are linux-amd64 builds.
pub const PLATFORM: &str = "linux-x86_64";

/// Where the daemon links the package's version that runs.
pub const CURRENT: &str = "/var/lib/hostkeeper/packages/mdm/current";

/// The binaries of the package, through [`CURRENT`]: `MDM_BIN` of the package.
pub const BIN: &str = "/var/lib/hostkeeper/packages/mdm/current/bin";

/// The service user.
pub const USER: &str = "mdm";

/// `MDM_STATE` of the package.
pub const STATE: &str = "/var/lib/mdm";

/// `MDM_ETC` of the package.
pub const ETC: &str = "/etc/mdm";

/// The slice of the services.
pub const SLICE: &str = "mdm.slice";

/// The settings file the manifest's params name: root's, on the MDM host.
pub const SETTINGS_FILE: &str = "/etc/mdm/settings";

/// The hook's record directory, which the manifest's params name.
pub const RECORD_DIR: &str = "/var/lib/updater/hooks/mdm";

/// NanoHUB's loopback port, which the health URL names.
pub const NANOHUB_PORT: u16 = 9004;

/// The settings the package fixes, with their values: a host's settings file may leave each
/// out or give it this value.
pub const FIXED: [(&str, &str); 10] = [
    ("MDM_USER", USER),
    ("MDM_STATE", STATE),
    ("MDM_ETC", ETC),
    ("MDM_BIN", BIN),
    ("MDM_PKG_DIR", "/var/lib/mdm-pkg"),
    ("MDM_UNIT_PREFIX", SERVICE),
    ("MDM_SLICE", SLICE),
    ("MDM_NANOHUB_PORT", "9004"),
    ("MDM_SCEP_PORT", "9005"),
    ("MDM_DEP_PORT", "9001"),
];

/// The pinned tools whose binaries the package carries under `bin/`, beside `bin/mdm`: the
/// servers and lego. MicroMDM's `mdmctl` is pinned for the old kit's `migrate` only.
pub const TOOLS: [Tool; 5] = [
    Tool::Nanohub,
    Tool::Nanodep,
    Tool::Scepserver,
    Tool::Caddy,
    Tool::Lego,
];

/// A unit file of the package: its name under `units/` and its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackageUnit {
    /// The file name.
    pub name: &'static str,
    /// The text.
    pub text: &'static str,
}

/// The SCEP server.
pub const SCEP: &str = "mdm-scep.service";
/// NanoHUB.
pub const NANOHUB: &str = "mdm-nanohub.service";
/// NanoDEP's depserver.
pub const DEPSERVER: &str = "mdm-depserver.service";
/// NanoDEP's depsyncer.
pub const DEPSYNCER: &str = "mdm-depsyncer.service";
/// The HTTPS proxy.
pub const PROXY: &str = "mdm-proxy.service";
/// The certificate job.
pub const CERT_SERVICE: &str = "mdm-cert.service";
/// The certificate job's timer.
pub const CERT_TIMER: &str = "mdm-cert.timer";

/// Every unit file of the package, in the order of their names.
pub const UNITS: [PackageUnit; 7] = [
    PackageUnit {
        name: CERT_SERVICE,
        text: include_str!("../package/units/mdm-cert.service"),
    },
    PackageUnit {
        name: CERT_TIMER,
        text: include_str!("../package/units/mdm-cert.timer"),
    },
    PackageUnit {
        name: DEPSERVER,
        text: include_str!("../package/units/mdm-depserver.service"),
    },
    PackageUnit {
        name: DEPSYNCER,
        text: include_str!("../package/units/mdm-depsyncer.service"),
    },
    PackageUnit {
        name: NANOHUB,
        text: include_str!("../package/units/mdm-nanohub.service"),
    },
    PackageUnit {
        name: PROXY,
        text: include_str!("../package/units/mdm-proxy.service"),
    },
    PackageUnit {
        name: SCEP,
        text: include_str!("../package/units/mdm-scep.service"),
    },
];

/// The manifest's `[units] run`: the services that serve as soon as the package is installed,
/// which the daemon enables, starts at its own start and observes (design section 5.8). The
/// proxy waits for the TLS certificate, the timer for the DNS credentials and depsyncer for
/// Apple's token, all steps the operator takes after the install, so none of them is a run unit
/// (a run unit that is not active holds the host's rollout): the hook enables the proxy and the
/// timer, and `mdm dep define-profile` depsyncer.
pub const RUN: [&str; 3] = [SCEP, NANOHUB, DEPSERVER];

/// The units the hook enables itself, beside [`RUN`].
pub const HOOK_ENABLED: [&str; 2] = [PROXY, CERT_TIMER];

/// The manifest's health URL: NanoHUB's version route, on loopback.
pub const HEALTH_URL: &str = "http://127.0.0.1:9004/version";

/// The file name of every drop-in the hook writes, in `<unit>.d/`.
pub const DROP_IN: &str = "settings.conf";

/// The oldest daemon the package asks for: the floor of hostkeeper's own package, which every
/// host of the farm runs or is newer than.
pub const MIN_HOSTKEEPER: &str = "0.1.53";

/// Why a host's settings file does not fit the package.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PackageError {
    /// The file is not a settings file.
    #[error("the settings file: {0}")]
    Settings(#[from] SettingsError),
    /// The file sets a key the package fixes to another value.
    #[error(
        "{key} is {given:?}, but the mdm package fixes it to {fixed:?}: leave it out or set that value"
    )]
    Fixed {
        /// The key.
        key: &'static str,
        /// The value the file gives.
        given: String,
        /// The package's value.
        fixed: &'static str,
    },
}

/// The settings file `text` with the package's [`FIXED`] keys it leaves out added, refused when
/// it gives one of them another value. The result is what [`MdmConfig::parse`] and
/// [`Acme::parse`] read.
///
/// # Errors
/// [`PackageError::Settings`] when `text` is not a settings file; [`PackageError::Fixed`] for
/// the first fixed key it sets to another value.
pub fn settings(text: &str) -> Result<String, PackageError> {
    let given = Settings::parse(text, KEYS)?;
    let mut out = text.to_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    for (key, fixed) in FIXED {
        match given.get(key) {
            None => out.push_str(&format!("{key}={fixed}\n")),
            Some(value) if value == fixed => {}
            Some(value) => {
                return Err(PackageError::Fixed {
                    key,
                    given: value.to_owned(),
                    fixed,
                });
            }
        }
    }
    Ok(out)
}

/// Where systemd reads the drop-ins of `unit` from, under `/etc/systemd/system`: `<unit>.d`.
pub fn drop_in_dir(unit: &str) -> String {
    format!("{unit}.d")
}

/// The drop-ins of the settings `c` (with the package's layout, from [`settings`]) and `acme`,
/// each named `<unit>.d/settings.conf`, in the order the hook writes them: the slice's caps,
/// the proxy's LAN, depsyncer's token name and the certificate job's ACME settings. Every value
/// is one the settings parser has checked to be a word systemd takes as it is: no space, quote,
/// `$`, `%` or line break.
pub fn drop_ins(c: &MdmConfig, acme: &Acme) -> Vec<Unit> {
    let head = "# Written by the mdm package's hook from the settings file; it is written again on\n\
                # every update, so edit the settings, not this file.\n";
    let slice = format!(
        "{head}[Unit]\nDescription=mdm services (capped)\n\n[Slice]\nMemoryMax={}\nMemorySwapMax=0\nCPUQuota={}\nTasksMax={}\n",
        c.memory_max(),
        c.cpu_quota(),
        c.tasks_max()
    );
    let proxy = format!("{head}[Service]\nIPAddressAllow={}\n", c.lan());
    let depsyncer = format!("{head}[Service]\nEnvironment=MDM_DEP_NAME={}\n", c.dep_name());
    let cert = format!(
        "{head}[Service]\nEnvironment=MDM_ACME_EMAIL={}\nEnvironment=MDM_ACME_SERVER={}\nEnvironment=MDM_ACME_DNS_PROVIDER={}\nEnvironment=MDM_DNS_NAME={}\nEnvironment=MDM_ACME_RESOLVERS={}\nEnvironment=MDM_ACME_PROPAGATION_WAIT={}\n",
        acme.email,
        acme.server,
        acme.provider,
        c.dns_name(),
        acme.resolvers_arg(),
        acme.propagation_wait_arg()
    );
    [
        (SLICE, slice),
        (PROXY, proxy),
        (DEPSYNCER, depsyncer),
        (CERT_SERVICE, cert),
    ]
    .into_iter()
    .map(|(unit, text)| Unit {
        name: format!("{}/{DROP_IN}", drop_in_dir(unit)),
        text,
    })
    .collect()
}

/// The hook's step bounds, in seconds, in the order of the manifest.
pub const TIMEOUTS: [(&str, u64); 6] = [
    ("prepare", 300),
    ("cutover", 120),
    ("verify", 60),
    ("rollback", 120),
    ("cleanup", 30),
    ("uninstall", 120),
];

/// How long `verify` may keep deferring (the services coming up) before the update fails.
pub const VERIFY_DEADLINE_SECS: u64 = 600;

/// The package's `manifest.toml` for the version text `version`.
pub fn manifest(version: &str) -> String {
    let timeouts: String = TIMEOUTS
        .iter()
        .map(|(step, secs)| format!("{step} = {secs}\n"))
        .collect();
    let run = RUN
        .iter()
        .map(|unit| format!("\"{unit}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "schema = 1\n\
         name = \"{SERVICE}\"\n\
         version = \"{version}\"\n\
         platform = \"{PLATFORM}\"\n\
         min_hostkeeper = \"{MIN_HOSTKEEPER}\"\n\
         \n\
         [hook]\n\
         command = [\"bin/{SERVICE}\", \"update-hook\"]\n\
         verify_deadline_secs = {VERIFY_DEADLINE_SECS}\n\
         \n\
         [hook.timeout_secs]\n\
         {timeouts}\
         \n\
         [units]\n\
         run = [{run}]\n\
         \n\
         [health]\n\
         url = \"{HEALTH_URL}\"\n\
         \n\
         [params]\n\
         settings = \"{SETTINGS_FILE}\"\n\
         record_dir = \"{RECORD_DIR}\"\n"
    )
}
