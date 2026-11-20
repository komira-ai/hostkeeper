//! The pin of a service (docs/design/hostkeeper-design.md, Q14 and Q16): the file
//! `pins/<service>` beside the state directory, `/var/lib/updater/pins/<service>` with the default
//! `--state-dir`, which names the one version of the service the host keeps whatever its age, and
//! holds the coordinator's orders for the service while it stands.
//!
//! The file holds the version and nothing else, with one newline after it or none. Root writes it
//! with `hostkeeper pin` or `hostkeeper activate --pin`, and removes it with `hostkeeper unpin`
//! ([`crate::local`]); the daemon removes it in one case only, a coordinator `force` that names
//! the version it pins, once that force has run ([`gate`], [`cleared`]). Every reader [`look`]s
//! at it the same way:
//!
//! - no file: no pin;
//! - a link, anything but a regular file, a file owned by an account other than root (or the
//!   account the reader runs as), or a file its group or others may write: not a pin, since
//!   someone other than root may have put it there. The service is not held, and the daemon says
//!   so in the alert [`ALERT_NOT_A_PIN`];
//! - root's regular file that holds a version: the pin;
//! - root's regular file that cannot be read or holds anything else, or a name that cannot be
//!   looked at: unreadable. It may name a version to keep, so the service is held (the deferral
//!   `pin_unreadable`) and the prune ([`crate::prune`]) deletes nothing.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::Verdict;

use crate::executor::{Ctx, Reply};
use crate::files;
use crate::refusal::Refusal;
use crate::verb::Verb;
use crate::verbs::force::SELF_SERVICE;

/// The directory of the pins, beside the state directory.
pub const PINS_DIR: &str = "pins";

/// The alert of a file at a pin's path that is not a pin.
pub const ALERT_NOT_A_PIN: &str = "not_a_pin";

/// The alert of a pin a coordinator `force` to its version ran past and could not remove.
pub const ALERT_PIN_NOT_CLEARED: &str = "pin_not_cleared";

/// The seconds an order held by a pin is deferred for: the coordinator asks again after its own
/// backoff, which is at least this.
pub const PINNED_RETRY_SECS: u64 = 60;

/// The most bytes of a pin that are read: more than the longest version and its newline.
const MAX_PIN_BYTES: u64 = 1024;

/// What is at the path of a service's pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Look {
    /// No file: the service follows the coordinator.
    Absent,
    /// Root's pin, naming this version.
    Pinned(Version),
    /// A file that is not a pin, for this reason: the service follows the coordinator.
    NotAPin(String),
    /// Root's file, or a name that cannot be looked at, whose version cannot be read, for this
    /// reason: the service is held.
    Unreadable(String),
}

/// The path of the pin of `service`, for the state directory `state_dir`: `pins/<service>`
/// beside it.
pub fn path(state_dir: &Path, service: &ServiceName) -> PathBuf {
    state_dir.with_file_name(PINS_DIR).join(service.as_str())
}

/// What is at `path`, a pin only when root, or the account this process runs as, owns it.
pub fn look(path: &Path) -> Look {
    look_as(path, &[0, rustix::process::geteuid().as_raw()])
}

/// What is at `path`, a pin only when one of the accounts `trusted` owns it.
pub fn look_as(path: &Path, trusted: &[u32]) -> Look {
    looked(path, trusted).unwrap_or_else(|look| look)
}

/// [`look_as`], with every answer but a pin as the error.
fn looked(path: &Path, trusted: &[u32]) -> Result<Look, Look> {
    let shown = path.display();
    let unreadable =
        |error: io::Error| Look::Unreadable(format!("the pin {shown} cannot be read: {error}"));
    let not_a_pin = |why: String| Look::NotAPin(format!("the file {shown} is not a pin: {why}"));
    // O_NOFOLLOW: a link is never followed; O_NONBLOCK: a pipe put there is never waited on.
    let opened = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path);
    let file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Look::Absent),
        // Not opened: a link, a socket, a file this account may not read, or a name that cannot
        // be looked at. One that is not root's is no pin; any other may be root's pin.
        Err(error) => {
            let why = fs::symlink_metadata(path)
                .ok()
                .and_then(|meta| untrusted(&meta, trusted));
            return Err(why.map_or_else(|| unreadable(error), not_a_pin));
        }
    };
    let meta = file.metadata().map_err(unreadable)?;
    if let Some(why) = untrusted(&meta, trusted) {
        return Err(not_a_pin(why));
    }
    let mut bytes = Vec::new();
    file.take(MAX_PIN_BYTES)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    let text = String::from_utf8(bytes).unwrap_or_default();
    let version = text.strip_suffix('\n').unwrap_or(&text);
    Version::new(version)
        .map(Look::Pinned)
        .map_err(|_| Look::Unreadable(format!("the pin {shown} does not hold a version")))
}

/// Why a file with `meta` is not a pin, or `None` when it may be one.
fn untrusted(meta: &Metadata, trusted: &[u32]) -> Option<String> {
    if !meta.file_type().is_file() {
        return Some("it is not a regular file (a link, a directory or a device)".to_owned());
    }
    if !trusted.contains(&meta.uid()) {
        return Some(format!(
            "it is owned by the account {}, not root",
            meta.uid()
        ));
    }
    (meta.mode() & 0o022 != 0).then(|| "its group or others may write it".to_owned())
}

/// Pins `service` at `version` for the state directory `state_dir`: the file written whole, mode
/// 0600, in the pins directory made mode 0700, both fsynced.
pub fn write(state_dir: &Path, service: &ServiceName, version: &Version) -> io::Result<()> {
    let dir = state_dir.with_file_name(PINS_DIR);
    files::make_private(&dir)?;
    files::write_atomic(
        &dir,
        service.as_str(),
        format!("{version}\n").as_bytes(),
        &|_| false,
    )
}

/// Removes whatever is at the pin's path of `service`, a link or a file that is not a pin
/// included, and fsyncs the pins directory: `true` when something was there.
pub fn remove(state_dir: &Path, service: &ServiceName) -> io::Result<bool> {
    let file = path(state_dir, service);
    match fs::remove_file(&file) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
        Ok(()) => {
            File::open(state_dir.with_file_name(PINS_DIR))?.sync_all()?;
            Ok(true)
        }
    }
}

/// The pin's answer to an order the executor is about to run, before any lock: `Err` with the
/// reply that holds it, or `Ok` to run it, naming the service whose pin the order clears once it
/// has run ([`cleared`]).
///
/// Only the orders that change what a service runs are held: `update`, `force_rollback`,
/// `install`, `uninstall` and `force`, for any service but hostkeeper itself (whose update is its
/// own path, which no pin holds). While the service is pinned they are deferred with the reason
/// `pinned`, all but a `force` that names the pinned version: that order runs, and clears the pin
/// once it has run to a verdict of its own, so the operator who forces the version the host holds
/// hands the service back to the coordinator. While the pin is unreadable they are deferred with
/// the reason `pin_unreadable`. A file that is not a pin holds nothing and is said in the alert
/// [`ALERT_NOT_A_PIN`].
pub fn gate<'v>(ctx: &Ctx, verb: &'v Verb) -> Result<Option<&'v ServiceName>, Reply> {
    let (service, forced) = match verb {
        Verb::Update(order) | Verb::ForceRollback(order) | Verb::Install(order) => {
            (&order.service, None)
        }
        Verb::Uninstall(order) => (&order.service, None),
        Verb::Force(order) => (&order.package.service, Some(&order.package.version)),
        _ => return Ok(None),
    };
    if service.as_str() == SELF_SERVICE {
        return Ok(None);
    }
    match look(&path(&ctx.dirs.state, service)) {
        Look::Absent => Ok(None),
        Look::NotAPin(why) => {
            ctx.alert(ALERT_NOT_A_PIN, &format!("service={service}: {why}"));
            Ok(None)
        }
        Look::Unreadable(_) => Err(deferred(Refusal::PinUnreadable)),
        Look::Pinned(pinned) if forced == Some(&pinned) => Ok(Some(service)),
        Look::Pinned(_) => Err(deferred(Refusal::Pinned)),
    }
}

/// After the `force` that [`gate`] let through for `service`, the version it pins: the pin
/// removed when the force ran to a verdict of its own, not when it was refused, stopped without
/// a verdict, or answered from the record as a resend. A pin that cannot be removed stays, and is
/// the alert [`ALERT_PIN_NOT_CLEARED`]: the service stays held until `hostkeeper unpin`.
pub fn cleared(ctx: &Ctx, service: Option<&ServiceName>, reply: &Reply) {
    let ran = matches!(reply, Reply::Finished { resent: false, .. });
    if let Some(service) = service.filter(|_| ran)
        && let Err(error) = remove(&ctx.dirs.state, service)
    {
        ctx.alert(
            ALERT_PIN_NOT_CLEARED,
            &format!("service={service}: the force to its pinned version ran: {error}"),
        );
    }
}

/// An order deferred by the pin, for the reason `why`.
fn deferred(why: Refusal) -> Reply {
    Reply::Finished {
        verdict: Verdict::Deferred {
            retry_after_secs: Some(PINNED_RETRY_SECS),
        },
        reason: Some(why.token().to_owned()),
        resent: false,
    }
}
