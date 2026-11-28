//! How full the host's local filesystems are, as the heartbeat's host health carries it
//! (`HostHealth.filesystems`, docs/design/host-health-alerts.md sections 3.1 and 5.1): for each,
//! where it is mounted, the bytes an unprivileged writer has and those it may still write, as
//! `statvfs` gives them (so the percent used is `df`'s: the blocks reserved for root are in
//! neither). The daemon only reports; the coordinator judges a nearly full filesystem
//! (`storage_failing`, never a hardware record, never a cordon).
//!
//! **Which filesystems.** The design names none, so the daemon reports those that hold its own
//! state, the hosts' packages and the jobs' data, wherever a host mounts them, by reporting every
//! one that could:
//!
//! - **Linux** ([`linux_mounts`], from [`MOUNTINFO`]): every filesystem of a local block device
//!   (its source a `/dev/` path, but no `loop`, `ram`, `zram`, `nbd` or `sr` device: a snap or
//!   image, a RAM disk, a network block device, a CD) mounted read-write, once per source device
//!   (a bind mount of one already listed, or another btrfs subvolume of it, is not listed again).
//!   No `tmpfs`, `overlay`, network or pseudo filesystem: none of them is a disk to fill, and
//!   `statvfs` of a network filesystem can hang.
//! - **macOS**: the data volume alone ([`DATA_VOLUME`], where the daemon's state, the packages
//!   and the jobs' data are; `/` is the sealed system volume, read-only). The volumes under
//!   `/Volumes` are not read: they are as often a network share, which can hang `statvfs`, or a
//!   disk image as an external disk, and nothing tells them apart without a program.
//!
//! The root filesystem first, then the others by mount point; at most [`MAX_FILESYSTEMS`], each
//! mount point cut to [`MAX_MOUNT`] bytes. A filesystem `statvfs` cannot read, one it says is
//! read-only and one of no size are left out, and so take no place of the eight. Nothing is
//! written, and no program is run: `statvfs` of a local filesystem answers from the kernel's own
//! counters.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use hostkeeper_launchd::PlatformKind;
use hostkeeper_proto::health::{MAX_FILESYSTEMS, MAX_MOUNT};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;

use crate::health_state::cut;

/// The kernel's table of this process's mounts, under the host's root.
pub const MOUNTINFO: &str = "proc/self/mountinfo";

/// A Mac's data volume.
pub const DATA_VOLUME: &str = "/System/Volumes/Data";

/// The block devices, by the start of their name under `/dev/`, whose filesystems are not
/// reported.
const NOT_DISKS: [&str; 5] = ["loop", "ram", "zram", "nbd", "sr"];

/// What `statvfs` says of one filesystem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    /// The bytes an unprivileged writer has: its blocks less those only root may write (its free
    /// blocks less its available ones), times its fragment size.
    pub size_bytes: u64,
    /// Its blocks available to an unprivileged writer times its fragment size.
    pub available_bytes: u64,
    /// Whether it is mounted read-only.
    pub read_only: bool,
}

/// The block counts of one filesystem, as `statvfs` gives them, each in fragments of
/// `fragment_bytes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blocks {
    /// All its blocks (`f_blocks`).
    pub total: u64,
    /// Its free blocks, those reserved for root among them (`f_bfree`).
    pub free: u64,
    /// Its free blocks an unprivileged writer may write (`f_bavail`).
    pub available: u64,
    /// The bytes of a block (`f_frsize`).
    pub fragment_bytes: u64,
}

impl Usage {
    /// The usage of a filesystem of `blocks`, read-only when `read_only`: its size is its blocks
    /// less those reserved for root (its free blocks less its available ones), so that the
    /// percent used of its available bytes is `df`'s. Each count saturates: a filesystem that says
    /// more available than free blocks has none reserved, and no product passes `u64::MAX`.
    #[must_use]
    pub fn of(blocks: Blocks, read_only: bool) -> Usage {
        let reserved = blocks.free.saturating_sub(blocks.available);
        Usage {
            size_bytes: blocks
                .total
                .saturating_sub(reserved)
                .saturating_mul(blocks.fragment_bytes),
            available_bytes: blocks.available.saturating_mul(blocks.fragment_bytes),
            read_only,
        }
    }
}

/// `statvfs` of `path`, as [`Usage::of`] counts it; none when it fails.
#[must_use]
pub fn statvfs(path: &Path) -> Option<Usage> {
    let stat = rustix::fs::statvfs(path).ok()?;
    Some(Usage::of(
        Blocks {
            total: stat.f_blocks,
            free: stat.f_bfree,
            available: stat.f_bavail,
            fragment_bytes: stat.f_frsize,
        },
        stat.f_flag.contains(rustix::fs::StatVfsMountFlags::RDONLY),
    ))
}

/// The mount points of `mountinfo` (the text of [`MOUNTINFO`]) the daemon reports (the module's
/// doc), each source device once, in the order of the table. A line that does not parse is passed over.
#[must_use]
pub fn linux_mounts(mountinfo: &str) -> Vec<String> {
    let mut devices = BTreeSet::new();
    let mut mounts = Vec::new();
    for line in mountinfo.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let left: Vec<&str> = left.split(' ').collect();
        let (Some(mount), Some(options)) = (left.get(4), left.get(5)) else {
            continue;
        };
        let Some(source) = right.split(' ').nth(1) else {
            continue;
        };
        let Some(name) = source.strip_prefix("/dev/") else {
            continue;
        };
        if NOT_DISKS.iter().any(|prefix| name.starts_with(prefix))
            || options.split(',').any(|option| option == "ro")
            || !devices.insert(source)
        {
            continue;
        }
        mounts.push(unescape(mount));
    }
    mounts
}

/// A mount point of the mount table with its escapes (`\040` for a space, `\011` a tab, `\012` a
/// newline, `\134` a backslash: an octal byte) read back.
fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes
            .get(i + 1..i + 4)
            .filter(|digits| bytes[i] == b'\\' && digits.iter().all(|d| (b'0'..=b'7').contains(d)))
            .and_then(|digits| {
                let value = digits
                    .iter()
                    .fold(0u32, |value, digit| value * 8 + u32::from(digit - b'0'));
                u8::try_from(value).ok()
            });
        match octal {
            Some(byte) => {
                out.push(byte);
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// How full each of `mounts` is under the host's root `root`, by `stat`: the root filesystem
/// first, then the others by mount point, those `stat` cannot read, read-only or of no size left
/// out, at most [`MAX_FILESYSTEMS`], each mount point cut to [`MAX_MOUNT`] bytes.
#[must_use]
pub fn usage(
    root: &Path,
    mut mounts: Vec<String>,
    stat: &dyn Fn(&Path) -> Option<Usage>,
) -> Vec<pb::FilesystemUse> {
    mounts.sort_by(|a, b| (a != "/", a).cmp(&(b != "/", b)));
    mounts
        .into_iter()
        .filter_map(|mount| {
            let usage = stat(&root.join(mount.trim_start_matches('/')))?;
            (!usage.read_only && usage.size_bytes > 0).then(|| pb::FilesystemUse {
                mount: cut(&mount, MAX_MOUNT),
                size_bytes: usage.size_bytes,
                available_bytes: usage.available_bytes,
            })
        })
        .take(MAX_FILESYSTEMS)
        .collect()
}

/// How full the filesystems of a host of `kind` whose root is `root` are (the module's doc), by
/// [`statvfs`]; none when its mounts cannot be read.
#[must_use]
pub fn read(kind: PlatformKind, root: &Path) -> Vec<pb::FilesystemUse> {
    let mounts = match kind {
        PlatformKind::Linux | PlatformKind::Openrc => fs::read_to_string(root.join(MOUNTINFO))
            .map(|text| linux_mounts(&text))
            .unwrap_or_default(),
        PlatformKind::Macos => vec![DATA_VOLUME.to_owned()],
    };
    usage(root, mounts, &statvfs)
}
