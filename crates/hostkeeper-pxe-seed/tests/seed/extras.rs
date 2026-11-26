//! The seed of a node with extra partitions and kernel parameters (issue
//! komira-ai/hostkeeper#496; tierstore's fetch-cache-serve.md sections 11 and 21.1, and 5.2
//! item 4): each against its golden by exact equality, and read back as YAML, so the storage
//! config the installer reads, the GRUB defaults file the first boot writes and the `runcmd`
//! that applies it are asserted as values, not only as bytes. Without either input the seed is
//! the bytes it was before them.

use hostkeeper_pxe_seed::inputs::{InstallDisk, LinuxPlatform};
use hostkeeper_pxe_seed::kernel::KERNEL_PARAMETERS_FILE;

use super::support::{self, Build, NODE_PARTITIONS};
use super::{files_and_runcmd, scalars};

/// The items of the storage config of `user_data`, in order, each as its `(key, value)` pairs
/// below the item (`match.ssd` for a key of its `match`), read back as YAML.
fn config_items(user_data: &str) -> Vec<Vec<(String, String)>> {
    let mut items: Vec<Vec<(String, String)>> = Vec::new();
    for (path, value) in scalars(user_data) {
        let Some(rest) = path.strip_prefix(".autoinstall.storage.config[") else {
            continue;
        };
        let (index, key) = rest.split_once("].").expect("an item's key");
        let index: usize = index.parse().expect("an index");
        if items.len() == index {
            items.push(Vec::new());
        }
        items[index].push((key.to_owned(), value));
    }
    items
}

fn item(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// The paths of the storage section of `user_data` that are not of its `config`.
fn storage_beside_config(user_data: &str) -> Vec<String> {
    scalars(user_data)
        .into_iter()
        .map(|(path, _)| path)
        .filter(|path| path.starts_with(".autoinstall.storage."))
        .filter(|path| !path.starts_with(".autoinstall.storage.config["))
        .collect()
}

/// tierstore's combined node: the node cache (220 GiB) and the hot tier (300 GiB), each an xfs
/// partition mounted `noatime`, on the default disk. The storage config is, item by item: the
/// smallest SSD wiped with a GPT table; the 1 GiB fat32 EFI system partition GRUB is installed
/// to; the two partitions in the order given, by GPT label, size in bytes and xfs; root, ext4,
/// taking the rest (`-1`), last on the disk; then the mounts, root first, the EFI system
/// partition at `/boot/efi`, and each extra one with `noatime`. The layout `direct` is gone, and
/// nothing else of the seed changes.
///
/// Catches a size in decimal units, a partition left out or put after root (which would leave it
/// no space), root not last or not filling the disk, a mount at the wrong path or without its
/// options, a layout `direct` left beside the config (which the installer refuses), or a seed
/// that differs from its golden in any byte.
#[test]
fn extra_partitions_replace_the_direct_layout_with_a_storage_config() {
    let mut build = Build::new(LinuxPlatform::X86_64);
    build.partitions = support::partitions(&NODE_PARTITIONS);
    let user_data = build.seed().user_data;
    assert_eq!(
        user_data,
        include_str!("../golden/user-data-extra-partitions.yaml")
    );
    assert_eq!(storage_beside_config(&user_data), Vec::<String>::new());
    let partition = |id: &str, number: &str, size: &str, extra: &[(&str, &str)]| {
        let mut list = vec![
            ("type", "partition"),
            ("id", id),
            ("device", "disk-install"),
            ("number", number),
            ("size", size),
        ];
        list.extend_from_slice(extra);
        list.extend([("wipe", "superblock"), ("preserve", "false")]);
        item(&list)
    };
    let format = |id: &str, volume: &str, fstype: &str| {
        item(&[
            ("type", "format"),
            ("id", id),
            ("volume", volume),
            ("fstype", fstype),
            ("preserve", "false"),
        ])
    };
    assert_eq!(
        config_items(&user_data),
        [
            item(&[
                ("type", "disk"),
                ("id", "disk-install"),
                ("match.ssd", "true"),
                ("match.size", "smallest"),
                ("ptable", "gpt"),
                ("wipe", "superblock-recursive"),
                ("preserve", "false"),
            ]),
            partition(
                "part-efi",
                "1",
                "1073741824",
                &[("flag", "boot"), ("grub_device", "true")]
            ),
            format("format-efi", "part-efi", "fat32"),
            partition(
                "part-extra-1",
                "2",
                "236223201280",
                &[("partition_name", "tierstore-cache")]
            ),
            format("format-extra-1", "part-extra-1", "xfs"),
            partition(
                "part-extra-2",
                "3",
                "322122547200",
                &[("partition_name", "tierstore-hot")]
            ),
            format("format-extra-2", "part-extra-2", "xfs"),
            partition("part-root", "4", "-1", &[]),
            format("format-root", "part-root", "ext4"),
            item(&[
                ("type", "mount"),
                ("id", "mount-root"),
                ("device", "format-root"),
                ("path", "/"),
            ]),
            item(&[
                ("type", "mount"),
                ("id", "mount-efi"),
                ("device", "format-efi"),
                ("path", "/boot/efi"),
            ]),
            item(&[
                ("type", "mount"),
                ("id", "mount-extra-1"),
                ("device", "format-extra-1"),
                ("path", "/var/lib/tierstore-cache"),
                ("options", "noatime"),
            ]),
            item(&[
                ("type", "mount"),
                ("id", "mount-extra-2"),
                ("device", "format-extra-2"),
                ("path", "/var/lib/ts-hot"),
                ("options", "noatime"),
            ]),
        ]
    );
    // Apart from the storage section, the seed is the one without extra partitions.
    let rest = |text: &str| -> Vec<(String, String)> {
        scalars(text)
            .into_iter()
            .filter(|(path, _)| !path.starts_with(".autoinstall.storage."))
            .collect()
    };
    assert_eq!(
        rest(&user_data),
        rest(&Build::new(LinuxPlatform::X86_64).seed().user_data)
    );
}

/// The mounts come in their mount points' order, so a mount point below another is mounted after
/// it whatever order the partitions were given in; the partitions stay in the order given, each
/// mount names its own partition's format; a partition given without mount options is mounted
/// without an `options` key, and an ext4 one is formatted ext4.
///
/// Catches a child mount made before its parent (the parent's mount would hide it), mounts
/// paired with the wrong partition once they are sorted, or options invented for a partition
/// that gave none.
#[test]
fn mounts_are_in_mount_point_order_and_options_only_where_given() {
    let mut build = Build::new(LinuxPlatform::Arm64);
    build.partitions =
        support::partitions(&["inner:1GiB:xfs:/srv/a/b:noatime", "outer:2GiB:ext4:/srv/a"]);
    let items = config_items(&build.seed().user_data);
    let field = |index: usize, key: &str| {
        items[index]
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, value)| value.clone())
    };
    let named: Vec<(Option<String>, Option<String>)> = (0..items.len())
        .map(|index| (field(index, "id"), field(index, "partition_name")))
        .filter(|(_, name)| name.is_some())
        .collect();
    assert_eq!(
        named,
        [
            (Some("part-extra-1".to_owned()), Some("inner".to_owned())),
            (Some("part-extra-2".to_owned()), Some("outer".to_owned())),
        ]
    );
    // Items 4 and 6 are the formats of the two partitions: the disk, the EFI system partition
    // and its format come first.
    assert_eq!(field(4, "volume").as_deref(), Some("part-extra-1"));
    assert_eq!(field(4, "fstype").as_deref(), Some("xfs"));
    assert_eq!(field(6, "volume").as_deref(), Some("part-extra-2"));
    assert_eq!(field(6, "fstype").as_deref(), Some("ext4"));
    let mounts: Vec<Vec<(String, String)>> = items
        .iter()
        .filter(|fields| fields.first().is_some_and(|(_, kind)| kind == "mount"))
        .cloned()
        .collect();
    assert_eq!(
        mounts[2..],
        [
            item(&[
                ("type", "mount"),
                ("id", "mount-extra-2"),
                ("device", "format-extra-2"),
                ("path", "/srv/a"),
            ]),
            item(&[
                ("type", "mount"),
                ("id", "mount-extra-1"),
                ("device", "format-extra-1"),
                ("path", "/srv/a/b"),
                ("options", "noatime"),
            ]),
        ]
    );
}

/// With extra partitions, the disk of each `--install-disk` form is the config's disk, matched
/// as the layout `direct` matches it: by that field, the smallest of those it matches.
///
/// Catches a config that drops `--install-disk` for the default SSD, which would write another
/// disk than the operator named.
#[test]
fn the_config_disk_is_the_install_disk() {
    for (flag, key, glob) in [
        ("serial=S4EV*", "serial", "S4EV*"),
        ("model=Samsung SSD 980*", "model", "Samsung SSD 980*"),
        ("path=/dev/nvme0n1", "path", "/dev/nvme0n1"),
    ] {
        let mut build = Build::new(LinuxPlatform::X86_64);
        build.disk = flag.parse::<InstallDisk>().expect(flag);
        build.partitions = support::partitions(&NODE_PARTITIONS);
        let disk = config_items(&build.seed().user_data).remove(0);
        let matched = format!("match.{key}");
        assert_eq!(
            disk[..4],
            item(&[
                ("type", "disk"),
                ("id", "disk-install"),
                (matched.as_str(), glob),
                ("match.size", "smallest"),
            ]),
            "{flag}"
        );
    }
}

/// `fuse.enable_uring=1`, the parameter tierstore's FUSE front reads at boot: the seed keeps the
/// layout `direct`, writes the parameter's own GRUB defaults file, root-owned, mode 0644, whose
/// one line adds the word to `GRUB_CMDLINE_LINUX`, and runs `update-grub` once at the first boot;
/// it writes no serial console file, and no getty.
///
/// Catches a parameter written into the serial console's file (which the daemon rewrites to the
/// console's bytes alone), a line that replaces `GRUB_CMDLINE_LINUX` and drops what the image
/// set, a file never applied, or a seed that differs from its golden in any byte.
#[test]
fn a_kernel_parameter_has_its_own_grub_defaults_file_applied_at_first_boot() {
    let mut build = Build::new(LinuxPlatform::X86_64);
    build.parameters = support::parameters(&["fuse.enable_uring=1"]);
    let user_data = build.seed().user_data;
    assert_eq!(
        user_data,
        include_str!("../golden/user-data-kernel-parameters.yaml")
    );
    let (files, runcmd) = files_and_runcmd(&user_data);
    let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/etc/hostkeeper/hostkeeper.toml",
            "/etc/cloud/cloud.cfg.d/99_hostkeeper.cfg",
            KERNEL_PARAMETERS_FILE,
        ]
    );
    assert_eq!(
        files[2].1,
        "GRUB_CMDLINE_LINUX=\"$GRUB_CMDLINE_LINUX fuse.enable_uring=1\"\n"
    );
    assert_eq!(runcmd, [vec!["update-grub"]]);
    let fields: Vec<(String, String)> = scalars(&user_data)
        .into_iter()
        .filter(|(path, _)| path.starts_with(".autoinstall.user-data.write_files[2]."))
        .collect();
    assert_eq!(
        fields,
        super::pairs(&[
            (
                ".autoinstall.user-data.write_files[2].path",
                KERNEL_PARAMETERS_FILE
            ),
            (".autoinstall.user-data.write_files[2].owner", "root:root"),
            (".autoinstall.user-data.write_files[2].permissions", "0644"),
            (
                ".autoinstall.user-data.write_files[2].content",
                "GRUB_CMDLINE_LINUX=\"$GRUB_CMDLINE_LINUX fuse.enable_uring=1\"\n"
            ),
        ])
    );
    assert!(user_data.contains("\n    layout:\n      name: direct\n"));
}

/// Several parameters are one line, in the order given, read back as YAML the way the shell reads
/// the file: words separated by one space, and characters YAML and the shell treat as special
/// (`#`, `:`, `'`, `;`, `|`, `&`) kept as written.
///
/// Catches words reordered or joined, and a word YAML reads as a comment or a mapping.
#[test]
fn several_kernel_parameters_are_one_line_in_their_order() {
    let mut build = Build::new(LinuxPlatform::Arm64);
    build.parameters = support::parameters(&["fuse.enable_uring=1", "quiet", "x=a#b:c'd;e|f&g"]);
    let (files, _) = files_and_runcmd(&build.seed().user_data);
    assert_eq!(
        files.last(),
        Some(&(
            KERNEL_PARAMETERS_FILE.to_owned(),
            "GRUB_CMDLINE_LINUX=\"$GRUB_CMDLINE_LINUX fuse.enable_uring=1 quiet x=a#b:c'd;e|f&g\"\n"
                .to_owned()
        ))
    );
}

/// The combined node as a farm installs it: the `--install-disk` of its serial, the serial
/// console `ttyS1`, both partitions and `fuse.enable_uring=1`. The parameter's file comes after
/// the console's two, and one `update-grub` applies both before the getty is reloaded and
/// enabled.
///
/// Catches a second `update-grub`, a console's `runcmd` lost when a parameter is given, the
/// console's file holding the parameter, or a seed that differs from its golden in any byte.
#[test]
fn a_node_with_a_console_partitions_and_a_parameter_is_its_golden() {
    let mut build = Build::new(LinuxPlatform::X86_64);
    build.disk = "serial=S4EV*".parse().expect("a disk");
    build.console = Some("ttyS1".parse().expect("a serial port"));
    build.partitions = support::partitions(&NODE_PARTITIONS);
    build.parameters = support::parameters(&["fuse.enable_uring=1"]);
    let user_data = build.seed().user_data;
    assert_eq!(user_data, include_str!("../golden/user-data-node.yaml"));
    let (files, runcmd) = files_and_runcmd(&user_data);
    let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/etc/hostkeeper/hostkeeper.toml",
            "/etc/cloud/cloud.cfg.d/99_hostkeeper.cfg",
            "/etc/default/grub.d/99_hostkeeper.cfg",
            "/etc/systemd/system/serial-getty@ttyS1.service.d/99_hostkeeper.conf",
            KERNEL_PARAMETERS_FILE,
        ]
    );
    assert!(!files[2].1.contains("fuse"), "{}", files[2].1);
    assert_eq!(
        runcmd,
        [
            vec!["update-grub"],
            vec!["systemctl", "daemon-reload"],
            vec!["systemctl", "enable", "--now", "serial-getty@ttyS1.service"],
        ]
    );
}

/// No extra partition and no kernel parameter, given as empty lists, is the seed of each
/// platform's golden, with and without a serial console: the layout `direct`, and nothing added.
///
/// Catches an empty list that still writes a storage config, an empty GRUB defaults file, or a
/// `runcmd` in a seed with no console.
#[test]
fn empty_lists_leave_the_seed_as_it_was() {
    for (platform, console, golden) in [
        (
            LinuxPlatform::X86_64,
            None,
            include_str!("../golden/user-data-x86_64.yaml"),
        ),
        (
            LinuxPlatform::Arm64,
            None,
            include_str!("../golden/user-data-arm64.yaml"),
        ),
        (
            LinuxPlatform::X86_64,
            Some("ttyS1"),
            include_str!("../golden/user-data-serial-console.yaml"),
        ),
    ] {
        let mut build = Build::new(platform);
        build.console = console.map(|port| port.parse().expect("a serial port"));
        build.partitions = support::partitions(&[]);
        build.parameters = support::parameters(&[]);
        assert_eq!(build.seed().user_data, golden, "{platform} {console:?}");
    }
}
