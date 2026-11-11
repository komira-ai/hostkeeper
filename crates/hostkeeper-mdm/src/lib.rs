//! `hostkeeper-mdm`: the MDM kit's binary, which applies [`hostkeeper_mdm_core`]'s rendered
//! output to the Linux host of the MDM server stack (docs/design/hostkeeper-mdm.md, tasks M4 and
//! M5), and `mdm`, the program of the `mdm` service package that replaces the kit
//! (docs/design/two-programs.md section 6.2): its update hook ([`hook`]) and the same
//! `push-cert` and `dep` verbs on the package's layout ([`program`]). Both programs are built
//! from this library; the verbs below are `hostkeeper-mdm`'s.
//!
//! The stack is NanoHUB, a SCEP server, NanoDEP and Caddy as capped systemd services on one
//! Linux host, which enrolls the farm's Macs. This crate links no other crate of the workspace
//! but `hostkeeper-mdm-core`, the pure half (settings, units, profiles and pins),
//! `hostkeeper-mdm-step`, the host-step layer, and `hostkeeper-hook`, the hook contract
//! (crates/xtask/tests/layering keeps it so). Every
//! verb checks the host read only, makes a plan of the steps that would change it, prints the
//! plan by default, and carries it out with `--apply`. It is the port of komira-ai/kbf's
//! `mdm-host`, with the fixes of komira-ai/kbf#2248 that the design gives task M4, and of the
//! `push-cert` and `dep` verbs of kbf-deploy's `mdm` module (`MIGRATION.md` lists them with
//! their tests).
//!
//! # Verbs
//!
//! * `hostkeeper-mdm fetch --dir DIR [--only TOOL]... [--apply]`: the pinned upstream archives,
//!   downloaded over HTTPS as your own user into `DIR` (refused at uid 0: it does not drop
//!   privileges; refused inside a git work tree, links resolved first), each checked against its
//!   sha256 pin. A download that is not the pin is kept as `<file>.rejected` and fails the
//!   run. Nothing downloaded is run.
//! * `hostkeeper-mdm install --settings FILE --bin-dir DIR [--apply]`: as root, on an x86-64
//!   host (the pins are linux-amd64 builds), the user, the directories, the binaries unpacked
//!   from the checked archives, the secrets (made once from 32 random bytes, never printed), the
//!   SCEP CA (kept when its depot holds a whole CA, refused when it holds part of one), the
//!   slice, the service units and the Caddyfile, and the services started. The proxy waits for
//!   the TLS certificate. A service whose inputs changed is restarted on this run or, if the run
//!   stops first, on the next one: a marker file per such service under
//!   `/run/<MDM_UNIT_PREFIX>/restart/` is made before the run's first change and removed
//!   after the restart (`/run` is a tmpfs; a reboot restarts every service anyway).
//! * `hostkeeper-mdm install --settings FILE --uninstall [--apply]`: stops and disables the
//!   units, stops the certificate and reload jobs, and moves their files to
//!   `MDM_STATE/removed/<unix seconds>/` (a later snapshot in the same second takes the next free
//!   name). The state, the TLS certificate, the secrets, the binaries and the user stay.
//! * `hostkeeper-mdm tls --settings FILE [--apply]`: as root, the TLS certificate from an ACME
//!   CA over DNS-01 by the pinned lego, with the DNS provider's credentials read by the
//!   certificate unit from the root-only `MDM_ETC/dns.env` (never argv), its renewal timer, and
//!   the proxy started once the certificate is there.
//! * `hostkeeper-mdm verify --settings FILE [--ignore LINE]...`: read only. Health checks (units
//!   active, listeners, certificate lifetime, modes), then one line per installed file
//!   compared with `hostkeeper-mdm-core`'s render of the same settings. Before the checks it
//!   waits, a minute at most, for the services to be active and listening, so it can run
//!   straight after `install`, `tls` or `migrate`.
//!
//! * `hostkeeper-mdm push-cert --settings FILE (--cert PEM --key PEM | --profile-only [--topic
//!   TOPIC]) [--apply]`: as root, the APNs push certificate checked (an MDM push certificate,
//!   unexpired by the injected clock, with its own unencrypted key, the key file 0600, neither
//!   file inside a git work tree) and given to NanoHUB's API on loopback unless NanoHUB has it
//!   already, then the enrollment profile written to `MDM_STATE/www/enroll.mobileconfig`, mode
//!   0640, root's and the service group's, which the proxy serves at `/enroll`. The SCEP
//!   challenge in it is never printed.
//! * `hostkeeper-mdm dep (get-cert --out FILE | upload-token --token FILE | define-profile |
//!   assign --serial SERIAL...) --settings FILE [--apply]`: as root, NanoDEP linked to Apple
//!   Business Manager through depserver's API on loopback: its token certificate, the server
//!   token, the ADE profile (defined once per change, then depsyncer started) and its assignment
//!   to devices already in ABM (exit 1 unless Apple assigns every one).
//! * `hostkeeper-mdm migrate --from OLD --settings NEW --bin-dir DIR --profile-id ID
//!   [--scep-ca-cn CN] [--acme-resolvers R] [--acme-propagation-wait W] [--apply]`:
//!   as root, the old kit's live stack moved to this kit's names while no Mac is enrolled: the
//!   new settings written from the old ones, NanoHUB's push certificate and NanoDEP's ABM token
//!   copied from the old state with the DNS provider's `dns.env`, this kit's files installed (a
//!   fresh SCEP CA under its final name included), the old units stopped and disabled, and this
//!   kit's services started. The old state and etc directories are never changed.
//!
//! * `hostkeeper-mdm --version`: `hostkeeper-mdm X.Y.Z+<first 12 hex digits of the commit>`
//!   for a release build (the binary of the release asset
//!   `hostkeeper-mdm-X.Y.Z-<linux platform>.tar.gz`), `hostkeeper-mdm X.Y.Z` for any other
//!   (`stamp`).
//!
//! A call to NanoHUB's or NanoDEP's API (`api`) is printed as a step is, with its method, URL and
//! body size, and made only with `--apply`.
//!
//! # Rules every verb keeps
//!
//! * A dry run is the default and changes nothing; `--apply` acts. A rerun finds nothing to
//!   do.
//! * `install` and `tls` read root-only files, so they refuse to run as another user, even for
//!   a dry run; `verify` runs as anyone, says it is not root, and fails the checks of the files
//!   it cannot see.
//! * Files are made without following links: a link in a directory the service user owns is
//!   refused, never chmodded, chowned or written through. The directories on the way to a path
//!   must be root's, or the operator's on the way to `--bin-dir`, or the service user's below
//!   `MDM_STATE` (granted by name, so also when the verb's own plan makes that user). A plan is
//!   checked whole against these owners before its first step, for a dry run as for an apply.
//! * A plan names paths, sizes and modes, never contents (its `Debug` form neither). No secret
//!   is put on a command line or printed; the SCEP challenge and API keys are written straight
//!   to 0600 root files.
//! * Exit 0 done (or nothing to do), 1 a step, a download or a check failed, or the host could
//!   not be read (`FAILED: ...` on stderr; a link or an untrusted owner met on the way to a path
//!   the verb reads is such a failure), 2 refused (`REFUSED: ...` on stderr). `dep assign` also
//!   exits 1 when Apple did not assign every device.
//! * Settings come from the file `--settings` names, never from the environment.
//! * No API key, token, private key or challenge is put on a command line or printed: an API key
//!   travels in a call's Basic authentication header only.

#![deny(clippy::disallowed_methods, clippy::disallowed_types)]

pub mod api;
pub mod archive;
mod cli;
pub mod dep;
mod error;
pub mod fetch;
pub mod hook;
pub mod install;
pub mod migrate;
pub mod net;
pub mod program;
pub mod push_cert;
mod repo;
pub mod secret;
mod stamp;
mod system;
pub mod tls;
pub mod verify;

use std::io;
use std::time::Duration;

use hostkeeper_mdm_step::{Host, Trustee};

pub use api::{HttpLoopback, Loopback};
pub use cli::{run, run_with};
pub use error::MdmError;
pub use net::{Download, DownloadError, Net};
pub use system::{RealMachine, main, system_random};

/// What a verb needs from the outside: the host, the network, the services' APIs on loopback,
/// a random source and the time.
pub struct Env<'a> {
    /// The host.
    pub host: &'a dyn Host,
    /// Downloads.
    pub net: &'a dyn Download,
    /// Calls to NanoHUB's and NanoDEP's APIs on `127.0.0.1`.
    pub api: &'a dyn Loopback,
    /// Fills a buffer with random bytes.
    pub random: &'a dyn Fn(&mut [u8]) -> io::Result<()>,
    /// Unix seconds now, read once when the program starts (the injected clock): it names an
    /// uninstall snapshot, and `push-cert` refuses a push certificate that has expired by it.
    pub now: u64,
}

/// A directory whose owner is trusted on the way to it and to every path below it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    /// The directory.
    pub dir: String,
    /// The owner trusted: by id (the operator's directory, owned now), or by name (a service
    /// user, which the verb's own plan may make before it writes below its state).
    pub who: Trustee,
}

/// Where a verb's [`Host`] comes from: the machine, trusting root and the effective user, and
/// the owners of the directories a verb grants.
pub trait Machine {
    /// The host, also trusting each grant's owner for the paths at and below its directory.
    fn host(&self, grants: &[Grant]) -> Box<dyn Host + '_>;

    /// The owner of the directory at `path`; `None` when nothing is there. Only whom
    /// [`Machine::host`] is to trust is decided on it: every read still walks the path itself.
    ///
    /// # Errors
    /// The system's error, other than "not found".
    fn owner(&self, path: &str) -> io::Result<Option<u32>>;
}

/// What [`run`] acts on: the machine, the network, the services' APIs, a random source and the
/// time.
pub struct System<'a> {
    /// The machine.
    pub machine: &'a dyn Machine,
    /// Downloads.
    pub net: &'a dyn Download,
    /// Calls to the services' APIs on loopback.
    pub api: &'a dyn Loopback,
    /// Fills a buffer with random bytes.
    pub random: &'a dyn Fn(&mut [u8]) -> io::Result<()>,
    /// Unix seconds now.
    pub now: u64,
    /// Waits the time it is given: `verify` pauses between two looks for the services'
    /// listeners. The real one sleeps; a test's returns at once.
    pub pause: &'a dyn Fn(Duration),
}
