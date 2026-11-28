//! How full the host's filesystems are (`hostkeeper_daemon::filesystems`), as each heartbeat's
//! health carries it: the filesystems chosen from a Linux mount table, and a Mac's data volume,
//! the root first and at most 8, read-only and unreadable ones left out, and the keeper sending
//! them in every heartbeat and leaving them out, with the memory devices, of a health that would
//! pass its 4096 bytes. Every file is under a scratch root; `statvfs` is faked but for the tests
//! of `statvfs` itself and of `read`, which read the scratch directory's own filesystem.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_daemon::filesystems::{
    DATA_VOLUME, MOUNTINFO, Usage, linux_mounts, read, statvfs, usage,
};
use hostkeeper_daemon::hardware_sources::SourceRead;
use hostkeeper_daemon::host_health::{BootFacts, HealthKeeper};
use hostkeeper_daemon::shutdown_probe::{GoingDown, ShutdownProbe};
use hostkeeper_launchd::PlatformKind;
use hostkeeper_link::HostHealthSource;
use hostkeeper_link::testing::RecordingLog;
use hostkeeper_proto::health::{MAX_FILESYSTEMS, MAX_HEALTH_BYTES, check_health};
use hostkeeper_proto::hostkeeper::agent::v1 as pb;
use prost::Message as _;
use support::Scratch;

/// A build host's mount table: the root and the EFI partition of one NVMe disk, the jobs' data
/// on LVM, the root's device bound again under `/srv`, a btrfs disk with two subvolumes mounted
/// (each its own anonymous device number), a snap, a read-only disk, a disk whose
/// mount point has a space, a CD, a RAM disk, zram, a network block device, a network filesystem,
/// and the kernel's own filesystems; one line cut short and one with no source.
const BUILD_HOST: &str = "\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw,errors=remount-ro
23 22 0:21 / /proc rw,nosuid,nodev,noexec,relatime shared:12 - proc proc rw
24 22 0:22 / /sys rw,nosuid,nodev,noexec,relatime shared:2 - sysfs sysfs rw
25 22 0:24 / /run rw,nosuid,nodev,noexec,relatime shared:5 - tmpfs tmpfs rw,size=3267480k
30 22 259:1 / /boot/efi rw,relatime shared:30 - vfat /dev/nvme0n1p1 rw,fmask=0077
31 22 253:0 / /var/lib/js-agent rw,noatime shared:31 - xfs /dev/mapper/vg-jobs rw,attr2
32 22 259:2 /srv/data /srv/bind rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
43 22 0:60 /@data /data rw,relatime shared:43 - btrfs /dev/sdd1 rw,subvol=/@data
44 22 0:61 /@cache /data/cache rw,relatime shared:44 - btrfs /dev/sdd1 rw,subvol=/@cache
33 22 7:0 / /snap/core22/1380 ro,nodev,relatime shared:33 - squashfs /dev/loop0 ro
34 22 8:17 / /mnt/archive ro,relatime shared:34 - ext4 /dev/sdb1 ro
35 22 8:33 / /mnt/My\\040Disk rw,relatime shared:35 - ext4 /dev/sdc1 rw
36 22 11:0 / /media/cd ro,relatime shared:36 - iso9660 /dev/sr0 ro
37 22 1:0 / /mnt/ram rw,relatime shared:37 - ext2 /dev/ram0 rw
38 22 252:0 / /mnt/zram rw,relatime shared:38 - ext4 /dev/zram0 rw
39 22 43:0 / /mnt/nbd rw,relatime shared:39 - ext4 /dev/nbd0 rw
40 22 0:50 / /mnt/nfs rw,relatime shared:40 - nfs4 server:/export rw
41 22 0:51 / /mnt/cut
42 22 0:52 / /mnt/nosource rw - ext4
";

/// Proves the filesystems chosen from a mount table: each read-write filesystem of a local block
/// device, LVM's included, once per source device (a btrfs disk's second subvolume not again), in
/// the order of the table, a mount point's escape read
/// back.
///
/// Catches: a snap, a RAM disk, zram, a network block device, a CD, a read-only disk, a network or
/// pseudo filesystem reported (a squashfs is always 100% full: a false critical on every host with a
/// snap), a bind mount or a btrfs disk reported twice, or a line that does not parse failing the read.
#[test]
fn the_local_read_write_disks_of_the_mount_table_are_chosen() {
    assert_eq!(
        linux_mounts(BUILD_HOST),
        [
            "/",
            "/boot/efi",
            "/var/lib/js-agent",
            "/data",
            "/mnt/My Disk"
        ]
    );
    assert_eq!(linux_mounts(""), Vec::<String>::new());
}

/// Proves a mount point's escapes are read as the kernel writes them: an octal byte of three
/// digits (a space, a tab, a backslash), and anything else, a backslash of fewer or other digits
/// or of a value past a byte, as it is.
#[test]
fn a_mount_points_escapes_are_read_back() {
    let line = |mount: &str| format!("1 0 8:1 / {mount} rw - ext4 /dev/sda1 rw\n");
    let cases = [
        ("/a\\040b", "/a b"),
        ("/a\\011b", "/a\tb"),
        ("/a\\134b", "/a\\b"),
        ("/a\\777b", "/a\\777b"),
        ("/a\\08b", "/a\\08b"),
        ("/a\\04", "/a\\04"),
        ("/a\\", "/a\\"),
    ];
    for (written, read) in cases {
        assert_eq!(linux_mounts(&line(written)), [read], "{written}");
    }
}

/// A fake `statvfs`: each mount point under the root, by its path, and what it says of it.
fn fake(root: &Path, known: &[(&str, Usage)]) -> impl Fn(&Path) -> Option<Usage> + use<> {
    let known: Vec<(PathBuf, Usage)> = known
        .iter()
        .map(|(mount, usage)| (root.join(mount.trim_start_matches('/')), *usage))
        .collect();
    move |path| {
        known
            .iter()
            .find(|(known, _)| known == path)
            .map(|(_, usage)| *usage)
    }
}

/// A filesystem of `size` bytes with `available` of them free, read-write.
fn rw(size: u64, available: u64) -> Usage {
    Usage {
        size_bytes: size,
        available_bytes: available,
        read_only: false,
    }
}

/// Proves the root filesystem is sent first and the others by mount point, each with its size
/// and its available bytes as `statvfs` gives them, and that one `statvfs` cannot read, one it
/// says is read-only and one of no size are left out.
#[test]
fn the_root_is_first_and_the_unreadable_read_only_and_empty_are_left_out() {
    let root = Path::new("/host");
    let stat = fake(
        root,
        &[
            ("/", rw(100, 3)),
            ("/boot/efi", rw(512, 500)),
            ("/var/lib/js-agent", rw(1 << 40, 1 << 39)),
            (
                "/mnt/ro",
                Usage {
                    read_only: true,
                    ..rw(10, 0)
                },
            ),
            ("/mnt/empty", rw(0, 0)),
        ],
    );
    let mounts = [
        "/var/lib/js-agent",
        "/mnt/gone",
        "/mnt/ro",
        "/boot/efi",
        "/mnt/empty",
        "/",
    ]
    .map(str::to_owned)
    .to_vec();
    let fs = |mount: &str, size: u64, available: u64| pb::FilesystemUse {
        mount: mount.to_owned(),
        size_bytes: size,
        available_bytes: available,
    };
    assert_eq!(
        usage(root, mounts, &stat),
        [
            fs("/", 100, 3),
            fs("/boot/efi", 512, 500),
            fs("/var/lib/js-agent", 1 << 40, 1 << 39),
        ]
    );
    assert_eq!(usage(root, Vec::new(), &stat), Vec::new());
}

/// Proves nine filesystems send eight, the root and the first seven others by mount point, and
/// that a filesystem left out takes none of the eight places; a mount point over 32 bytes is cut
/// to 32 on a character's boundary.
///
/// Catches: a health over its bound of 8 filesystems, which the coordinator refuses whole.
#[test]
fn nine_filesystems_send_eight_and_a_long_mount_point_is_cut() {
    let root = Path::new("/");
    let mut known: Vec<(String, Usage)> = (1..=8)
        .map(|n| (format!("/srv/disk{n}"), rw(100, 50)))
        .collect();
    known.push(("/".to_owned(), rw(100, 50)));
    known.push((
        "/mnt/ro".to_owned(),
        Usage {
            read_only: true,
            ..rw(1, 1)
        },
    ));
    let pairs: Vec<(&str, Usage)> = known.iter().map(|(m, u)| (m.as_str(), *u)).collect();
    let stat = fake(root, &pairs);
    let mounts: Vec<String> = known.iter().map(|(m, _)| m.clone()).collect();
    assert_eq!(mounts.len(), 10);
    let sent: Vec<String> = usage(root, mounts, &stat)
        .into_iter()
        .map(|f| f.mount)
        .collect();
    assert_eq!(sent.len(), MAX_FILESYSTEMS);
    assert_eq!(
        sent,
        [
            "/",
            "/srv/disk1",
            "/srv/disk2",
            "/srv/disk3",
            "/srv/disk4",
            "/srv/disk5",
            "/srv/disk6",
            "/srv/disk7"
        ]
    );

    let long = format!("/{}é", "d".repeat(30));
    assert_eq!(long.len(), 33);
    let stat = fake(root, &[(long.as_str(), rw(1, 1))]);
    let cut = usage(root, vec![long.clone()], &stat);
    assert_eq!(cut[0].mount, long[..31]);
}

/// Proves `statvfs` of a directory gives its filesystem's size and available bytes, read-write,
/// and of a path that is not there gives none.
#[test]
fn statvfs_reads_a_filesystem_and_not_a_missing_path() {
    let dir = Scratch::new("filesystems-statvfs");
    let usage = statvfs(dir.path()).expect("the scratch directory's filesystem");
    assert!(usage.size_bytes > 0, "{usage:?}");
    assert!(usage.available_bytes <= usage.size_bytes, "{usage:?}");
    assert!(!usage.read_only);
    assert_eq!(statvfs(&dir.path().join("missing")), None);
}

/// Writes `text` to `rel` under `root`, making its directories.
fn file(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
    fs::write(path, text).expect("write");
}

/// Proves a Linux host's filesystems are read from its mount table under its root and by
/// `statvfs`: a table naming the root of the scratch root gives that filesystem, as `statvfs`
/// reads it; no table gives none.
#[test]
fn a_linux_hosts_filesystems_are_read_from_its_root() {
    let host = Scratch::new("filesystems-linux");
    assert_eq!(read(PlatformKind::Linux, host.path()), Vec::new());
    file(
        host.path(),
        MOUNTINFO,
        "22 1 259:2 / / rw,relatime - ext4 /dev/nvme0n1p2 rw\n",
    );
    for kind in [PlatformKind::Linux, PlatformKind::Openrc] {
        let read = read(kind, host.path());
        let [root] = read.as_slice() else {
            panic!("the root alone: {read:?}");
        };
        let usage = statvfs(host.path()).expect("statvfs");
        assert_eq!(root.mount, "/");
        assert_eq!(root.size_bytes, usage.size_bytes);
    }
}

/// Proves a Mac's filesystems are its data volume, read under its root, whatever `/Volumes`
/// holds; with no data volume, none.
#[test]
fn a_macs_filesystem_is_its_data_volume() {
    let host = Scratch::new("filesystems-mac");
    let root = host.path();
    assert_eq!(
        read(PlatformKind::Macos, root),
        Vec::new(),
        "no data volume"
    );
    fs::create_dir_all(root.join(DATA_VOLUME.trim_start_matches('/'))).expect("mkdir");
    fs::create_dir_all(root.join("Volumes/Share")).expect("mkdir");
    let read = read(PlatformKind::Macos, root);
    let mounts: Vec<&str> = read.iter().map(|f| f.mount.as_str()).collect();
    assert_eq!(mounts, [DATA_VOLUME]);
}

/// A probe that never judges the system going down.
struct Running;

impl ShutdownProbe for Running {
    fn going_down(&self) -> GoingDown {
        GoingDown::No("running".to_owned())
    }
}

/// A keeper over `state`, reading the filesystems of a Linux host under `root`.
fn keeper(state: &Scratch, root: &Path) -> HealthKeeper {
    HealthKeeper::open(
        state.path(),
        BootFacts {
            boot_id: Some("b".repeat(64)),
            boot_unix: 1_795_000_000,
            machine_id: "m-1".to_owned(),
        },
        Box::new(Running),
        Arc::new(RecordingLog::default()),
    )
    .reading_filesystems(PlatformKind::Linux, root)
}

/// Proves every heartbeat's health carries the filesystems as read then, within the health's
/// bounds; and that a health that would pass 4096 bytes leaves them out, with the memory devices,
/// before it sends fewer records, and has them again once it fits.
///
/// Catches: the filesystems read once at the start (a disk filling later never reported), or a
/// health over its bound, which the coordinator refuses whole.
#[test]
fn every_heartbeat_carries_the_filesystems_unless_the_health_is_full() {
    let state = Scratch::new("filesystems-keeper-state");
    let host = Scratch::new("filesystems-keeper-host");
    let keeper = keeper(&state, host.path());
    assert_eq!(
        keeper.health().filesystems,
        Vec::new(),
        "no mount table yet"
    );
    file(
        host.path(),
        MOUNTINFO,
        "22 1 259:2 / / rw,relatime - ext4 /dev/nvme0n1p2 rw\n",
    );
    let first = keeper.health();
    assert_eq!(check_health(&first), Ok(()));
    assert_eq!(
        first
            .filesystems
            .iter()
            .map(|f| f.mount.as_str())
            .collect::<Vec<_>>(),
        ["/"]
    );

    for n in 0..12 {
        let name = format!("source{n:0>10}");
        keeper.read(
            &name,
            SourceRead {
                status: pb::SourceStatus::Unreadable,
                reason: "r".repeat(64),
                mark: None,
                events: Vec::new(),
            },
            1_795_662_000,
        );
    }
    let full = |n: u64| pb::HardwareEvent {
        event_id: format!("{n:0>64}"),
        time_unix: n,
        severity: pb::HardwareSeverity::Warning.into(),
        subject: "s".repeat(32),
        source: "s".repeat(16),
        code: "c".repeat(32),
        message: "m".repeat(120),
        raw: "r".repeat(256),
        about: "a".repeat(64),
        ..pb::HardwareEvent::default()
    };
    keeper.record("sel", "", (1..=6).map(full).collect(), 1_795_662_000);
    let crowded = keeper.health();
    assert_eq!(check_health(&crowded), Ok(()));
    assert!(crowded.encoded_len() <= MAX_HEALTH_BYTES);
    assert_eq!(crowded.filesystems, Vec::new(), "left out with the devices");
    assert_eq!(crowded.sources.len(), 12, "the states kept");
    assert!(!crowded.events.is_empty() && crowded.events.len() < 6);

    let ids: Vec<String> = (1..=6).map(|n| format!("{n:0>64}")).collect();
    keeper.written(&ids);
    let again = keeper.health();
    assert_eq!(again.events, Vec::new());
    assert_eq!(again.filesystems.len(), 1, "back once the health fits");
}
