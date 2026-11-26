//! `--extra-partition`: each part of the grammar at its bounds, every refusal by its exact
//! message, and the checks of a run's partitions as a whole (count, GPT label and mount point
//! given twice).

use hostkeeper_pxe_seed::partitions::{
    ExtraPartition, ExtraPartitionError, ExtraPartitions, ExtraPartitionsError, Filesystem,
    MAX_EXTRA_PARTITIONS,
};

fn parse(text: &str) -> Result<ExtraPartition, ExtraPartitionError> {
    text.parse()
}

/// The fields of a partition, as its accessors give them.
fn fields(text: &str) -> (String, u64, Filesystem, String, Option<String>) {
    let p = parse(text).expect(text);
    (
        p.label().to_owned(),
        p.size_bytes(),
        p.filesystem(),
        p.mount_point().to_owned(),
        p.mount_options().map(str::to_owned),
    )
}

const GIB: u64 = 1 << 30;
const MIB: u64 = 1 << 20;

/// Catches a field read into another one, a size in decimal units or off by a unit, options
/// lost or invented, or a file system written as the other.
#[test]
fn each_field_of_an_extra_partition_is_read() {
    assert_eq!(
        fields("tierstore-cache:220GiB:xfs:/var/lib/tierstore-cache:noatime"),
        (
            "tierstore-cache".to_owned(),
            220 * GIB,
            Filesystem::Xfs,
            "/var/lib/tierstore-cache".to_owned(),
            Some("noatime".to_owned())
        )
    );
    assert_eq!(
        fields("data_1.b:512MiB:ext4:/srv/data"),
        (
            "data_1.b".to_owned(),
            512 * MIB,
            Filesystem::Ext4,
            "/srv/data".to_owned(),
            None
        )
    );
    assert_eq!(
        fields("x:1MiB:xfs:/x:noatime,logbsize=256k,nodiscard").4,
        Some("noatime,logbsize=256k,nodiscard".to_owned())
    );
    assert_eq!(
        (Filesystem::Xfs.as_str(), Filesystem::Ext4.as_str()),
        ("xfs", "ext4")
    );
}

/// The bounds that are accepted: a 36-character label, the smallest size, the largest size whose
/// bytes fit in 64 bits, a mount point of 255 bytes, options of 255 bytes, and `/boot` as a prefix
/// of a longer component.
///
/// Catches a limit one short of its bound, and a `/boot` check that refuses `/bootstrap`.
#[test]
fn the_bounds_of_each_field_are_accepted() {
    let label = "a".repeat(36);
    assert_eq!(fields(&format!("{label}:1MiB:xfs:/a")).0, label);
    assert_eq!(fields("a:1MiB:xfs:/a").1, MIB);
    assert_eq!(fields("a:1GiB:xfs:/a").1, GIB);
    // 2^34 - 1 GiB is 2^64 - 2^30 bytes, the most GiB that fit.
    assert_eq!(fields("a:17179869183GiB:xfs:/a").1, 17_179_869_183 * GIB);
    let mount = format!("/{}", "m".repeat(254));
    assert_eq!(fields(&format!("a:1MiB:xfs:{mount}")).3, mount);
    let options = "o".repeat(255);
    assert_eq!(fields(&format!("a:1MiB:xfs:/a:{options}")).4, Some(options));
    assert_eq!(fields("a:1MiB:xfs:/bootstrap").3, "/bootstrap");
    assert_eq!(fields("a:1MiB:xfs:/srv/boot").3, "/srv/boot");
}

/// Every refusal, by its exact message: a value with too few or too many fields; a label that is
/// empty, too long, starts with a dot or a dash, or holds a space or a slash; a size of 0, with a
/// leading 0, in decimal units, with no unit, with no number, signed, fractional, spaced, or
/// whose bytes overflow 64 bits (in the multiplication or in the number itself); a file system
/// other than xfs and ext4, by case too; a mount point that is `/`, relative, with an empty, `.`
/// or `..` component, a trailing slash, a space, or over 255 bytes; `/boot` and below it; and
/// options that are empty, have an empty item, hold a space, or are over 255 bytes.
///
/// Catches a value that would put a partition where the installer cannot make it, a mount over
/// the EFI system partition, or an option that breaks the installed system's fstab.
#[test]
fn each_refusal_says_which_field_is_wrong() {
    let form =
        "--extra-partition is not <GPT label>:<size>:<file system>:<mount point>[:<mount options>]";
    let label = "--extra-partition's GPT label is not 1 to 36 of A-Za-z0-9._-, starting with a letter or a digit";
    let size = "--extra-partition's size is not <n>MiB or <n>GiB, with n from 1 and no leading 0";
    let filesystem = "--extra-partition's file system is not xfs or ext4";
    let mount = "--extra-partition's mount point is not an absolute path of at most 255 bytes, other than /, whose components are A-Za-z0-9._- and none . or ..";
    let options = "--extra-partition's mount options are not a comma-separated list of A-Za-z0-9._=- words of at most 255 bytes";
    let long_label = format!("{}:1MiB:xfs:/a", "a".repeat(37));
    let long_mount = format!("a:1MiB:xfs:/{}", "m".repeat(255));
    let long_options = format!("a:1MiB:xfs:/a:{}", "o".repeat(256));
    let cases: Vec<(&str, &str)> = vec![
        ("", form),
        ("a:1MiB:xfs", form),
        ("a:1MiB:xfs:/a:noatime:x", form),
        (":1MiB:xfs:/a", label),
        (&long_label, label),
        (".a:1MiB:xfs:/a", label),
        ("-a:1MiB:xfs:/a", label),
        ("a b:1MiB:xfs:/a", label),
        ("a/b:1MiB:xfs:/a", label),
        ("a:0GiB:xfs:/a", size),
        ("a:0MiB:xfs:/a", size),
        ("a:01GiB:xfs:/a", size),
        ("a:1GB:xfs:/a", size),
        ("a:1G:xfs:/a", size),
        ("a:1:xfs:/a", size),
        ("a:GiB:xfs:/a", size),
        ("a:-1GiB:xfs:/a", size),
        ("a:+1GiB:xfs:/a", size),
        ("a:1.5GiB:xfs:/a", size),
        ("a:1 GiB:xfs:/a", size),
        ("a:17179869184GiB:xfs:/a", size),
        ("a:99999999999999999999MiB:xfs:/a", size),
        ("a:1GiB:btrfs:/a", filesystem),
        ("a:1GiB:XFS:/a", filesystem),
        ("a:1GiB::/a", filesystem),
        ("a:1GiB:xfs:/", mount),
        ("a:1GiB:xfs:", mount),
        ("a:1GiB:xfs:srv/a", mount),
        ("a:1GiB:xfs://a", mount),
        ("a:1GiB:xfs:/a//b", mount),
        ("a:1GiB:xfs:/a/", mount),
        ("a:1GiB:xfs:/a/./b", mount),
        ("a:1GiB:xfs:/a/../b", mount),
        ("a:1GiB:xfs:/..", mount),
        ("a:1GiB:xfs:/a b", mount),
        (&long_mount, mount),
        ("a:1GiB:xfs:/a:", options),
        ("a:1GiB:xfs:/a:noatime,,nodiscard", options),
        ("a:1GiB:xfs:/a:,noatime", options),
        ("a:1GiB:xfs:/a:noatime,", options),
        ("a:1GiB:xfs:/a:no atime", options),
        ("a:1GiB:xfs:/a:context=\"x\"", options),
        (&long_options, options),
    ];
    for (text, why) in cases {
        let refused = parse(text).expect_err(text);
        assert_eq!(refused.to_string(), why, "{text}");
    }
    for at in ["/boot", "/boot/efi", "/boot/x/y"] {
        assert_eq!(
            parse(&format!("a:1GiB:xfs:{at}")),
            Err(ExtraPartitionError::Boot(at.to_owned())),
            "{at}"
        );
        assert_eq!(
            ExtraPartitionError::Boot(at.to_owned()).to_string(),
            format!(
                "--extra-partition's mount point {at} is /boot or below it, which the layout's own partitions use"
            )
        );
    }
}

fn many(count: usize) -> Vec<ExtraPartition> {
    (0..count)
        .map(|n| parse(&format!("p{n}:1GiB:xfs:/p{n}")).expect("a partition"))
        .collect()
}

/// A run's partitions keep their order, none and up to the limit are accepted, and over the
/// limit, a GPT label given twice and a mount point given twice are refused with their exact
/// messages.
///
/// Catches partitions reordered on the disk, a limit one off, and two partitions the installer
/// would refuse (one label) or mount over each other (one mount point).
#[test]
fn a_runs_partitions_are_checked_as_a_whole() {
    assert_eq!(
        ExtraPartitions::new(Vec::new()),
        Ok(ExtraPartitions::default())
    );
    assert!(ExtraPartitions::default().partitions().is_empty());
    let most = ExtraPartitions::new(many(MAX_EXTRA_PARTITIONS)).expect("the most");
    let labels: Vec<&str> = most
        .partitions()
        .iter()
        .map(ExtraPartition::label)
        .collect();
    let want: Vec<String> = (0..MAX_EXTRA_PARTITIONS).map(|n| format!("p{n}")).collect();
    assert_eq!(labels, want);
    let refused = ExtraPartitions::new(many(MAX_EXTRA_PARTITIONS + 1)).expect_err("too many");
    assert_eq!(refused, ExtraPartitionsError::TooMany(17));
    assert_eq!(
        refused.to_string(),
        "--extra-partition is given 17 times, over 16"
    );

    let label = ExtraPartitions::new(vec![
        parse("cache:1GiB:xfs:/a").expect("a"),
        parse("hot:1GiB:xfs:/b").expect("b"),
        parse("cache:1GiB:xfs:/c").expect("c"),
    ])
    .expect_err("one label twice");
    assert_eq!(
        label.to_string(),
        "--extra-partition's GPT label cache is given twice"
    );
    let mount = ExtraPartitions::new(vec![
        parse("a:1GiB:xfs:/srv/a").expect("a"),
        parse("b:1GiB:ext4:/srv/a").expect("b"),
    ])
    .expect_err("one mount point twice");
    assert_eq!(
        mount.to_string(),
        "--extra-partition's mount point /srv/a is given twice"
    );
}
