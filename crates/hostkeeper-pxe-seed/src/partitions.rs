//! The partitions a seed makes on the install disk beside root (`--extra-partition`, repeatable),
//! each checked once when it is made, so the seed cannot fail: a node's own file systems, such as
//! tierstore's cache, which is an xfs partition of the node's SSD and not a loop image on root.
//!
//! One value is `<GPT label>:<size>:<file system>:<mount point>[:<mount options>]`, such as
//! `tierstore-cache:220GiB:xfs:/var/lib/tierstore-cache:noatime`:
//!
//! - the GPT partition name, 1 to 36 of `A-Za-z0-9._-` starting with a letter or a digit;
//! - the size, `<n>MiB` or `<n>GiB` with `n` from 1 and no leading 0;
//! - the file system, `xfs` or `ext4`;
//! - the mount point, an absolute path of components of `A-Za-z0-9._-` (none `.` or `..`), at
//!   most 255 bytes, not `/` and not `/boot` or below it, which the layout's own partitions use;
//! - the mount options, when given, a comma-separated list of `A-Za-z0-9._=-` words, at most 255
//!   bytes; without them the mount takes the installer's default.
//!
//! [`ExtraPartitions`] holds the values of one run, in the order given, which is their order on
//! the disk: at most [`MAX_EXTRA_PARTITIONS`], no label and no mount point twice.

use std::str::FromStr;

/// The most extra partitions one seed makes.
pub const MAX_EXTRA_PARTITIONS: usize = 16;

/// The longest GPT partition name, in characters: GPT's own limit.
pub const MAX_PARTITION_LABEL: usize = 36;

/// The longest mount point, in bytes.
pub const MAX_MOUNT_POINT: usize = 255;

/// The longest list of mount options, in bytes.
pub const MAX_MOUNT_OPTIONS: usize = 255;

/// Bytes in a MiB.
const MIB: u64 = 1 << 20;

/// Bytes in a GiB.
const GIB: u64 = 1 << 30;

/// The file system of an extra partition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filesystem {
    /// `xfs`.
    Xfs,
    /// `ext4`.
    Ext4,
}

impl Filesystem {
    /// The spelling of the flag and of the installer's `fstype`: `xfs` or `ext4`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Filesystem::Xfs => "xfs",
            Filesystem::Ext4 => "ext4",
        }
    }
}

/// Why an `--extra-partition` value was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExtraPartitionError {
    #[error(
        "--extra-partition is not <GPT label>:<size>:<file system>:<mount point>[:<mount options>]"
    )]
    Form,
    #[error(
        "--extra-partition's GPT label is not 1 to 36 of A-Za-z0-9._-, starting with a letter or a digit"
    )]
    Label,
    #[error("--extra-partition's size is not <n>MiB or <n>GiB, with n from 1 and no leading 0")]
    Size,
    #[error("--extra-partition's file system is not xfs or ext4")]
    Filesystem,
    #[error(
        "--extra-partition's mount point is not an absolute path of at most 255 bytes, other than /, whose components are A-Za-z0-9._- and none . or .."
    )]
    MountPoint,
    #[error(
        "--extra-partition's mount point {0} is /boot or below it, which the layout's own partitions use"
    )]
    Boot(String),
    #[error(
        "--extra-partition's mount options are not a comma-separated list of A-Za-z0-9._=- words of at most 255 bytes"
    )]
    Options,
}

/// One partition of the install disk beside root, as [`ExtraPartition::from_str`] reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtraPartition {
    label: String,
    size_bytes: u64,
    filesystem: Filesystem,
    mount_point: String,
    mount_options: Option<String>,
}

impl ExtraPartition {
    /// The GPT partition name.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The size, in bytes: a whole number of MiB.
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// The file system.
    pub fn filesystem(&self) -> Filesystem {
        self.filesystem
    }

    /// The mount point.
    pub fn mount_point(&self) -> &str {
        &self.mount_point
    }

    /// The mount options, when given.
    pub fn mount_options(&self) -> Option<&str> {
        self.mount_options.as_deref()
    }
}

/// Whether `text` is a GPT label this crate writes.
fn is_label(text: &str) -> bool {
    let word = |b: u8| b.is_ascii_alphanumeric() | matches!(b, b'.' | b'_' | b'-');
    (1..=MAX_PARTITION_LABEL).contains(&text.len())
        & text.bytes().all(word)
        & text
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
}

/// The bytes of a size `<n>MiB` or `<n>GiB`; `None` for any other text, and for one whose bytes
/// do not fit in 64 bits.
fn size_bytes(text: &str) -> Option<u64> {
    let (number, unit) = match (text.strip_suffix("MiB"), text.strip_suffix("GiB")) {
        (Some(number), _) => (number, MIB),
        (_, Some(number)) => (number, GIB),
        (None, None) => return None,
    };
    let digits = !number.is_empty() & number.bytes().all(|b| b.is_ascii_digit());
    if !digits | number.starts_with('0') {
        return None;
    }
    number.parse::<u64>().ok()?.checked_mul(unit)
}

/// Whether `text` is a mount point this crate writes, `/boot` and below it aside.
fn is_mount_point(text: &str) -> bool {
    let word = |b: u8| b.is_ascii_alphanumeric() | matches!(b, b'.' | b'_' | b'-');
    let Some(rest) = text.strip_prefix('/') else {
        return false;
    };
    (text.len() <= MAX_MOUNT_POINT)
        & !rest.is_empty()
        & rest
            .split('/')
            .all(|part| !part.is_empty() & (part != ".") & (part != "..") & part.bytes().all(word))
}

/// Whether `text` is a list of mount options this crate writes.
fn is_mount_options(text: &str) -> bool {
    let word = |b: u8| b.is_ascii_alphanumeric() | matches!(b, b'.' | b'_' | b'=' | b'-');
    (text.len() <= MAX_MOUNT_OPTIONS)
        & text
            .split(',')
            .all(|option| !option.is_empty() & option.bytes().all(word))
}

impl FromStr for ExtraPartition {
    type Err = ExtraPartitionError;

    /// The value of `--extra-partition`, as the module says.
    fn from_str(text: &str) -> Result<ExtraPartition, ExtraPartitionError> {
        let fields: Vec<&str> = text.split(':').collect();
        let (label, size, filesystem, mount_point, options) = match fields[..] {
            [label, size, filesystem, mount_point] => (label, size, filesystem, mount_point, None),
            [label, size, filesystem, mount_point, options] => {
                (label, size, filesystem, mount_point, Some(options))
            }
            _ => return Err(ExtraPartitionError::Form),
        };
        if !is_label(label) {
            return Err(ExtraPartitionError::Label);
        }
        let size_bytes = size_bytes(size).ok_or(ExtraPartitionError::Size)?;
        let filesystem = match filesystem {
            "xfs" => Filesystem::Xfs,
            "ext4" => Filesystem::Ext4,
            _ => return Err(ExtraPartitionError::Filesystem),
        };
        if !is_mount_point(mount_point) {
            return Err(ExtraPartitionError::MountPoint);
        }
        if (mount_point == "/boot") | mount_point.starts_with("/boot/") {
            return Err(ExtraPartitionError::Boot(mount_point.to_owned()));
        }
        if !options.is_none_or(is_mount_options) {
            return Err(ExtraPartitionError::Options);
        }
        Ok(ExtraPartition {
            label: label.to_owned(),
            size_bytes,
            filesystem,
            mount_point: mount_point.to_owned(),
            mount_options: options.map(str::to_owned),
        })
    }
}

/// Why the extra partitions of a run were refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExtraPartitionsError {
    /// More than [`MAX_EXTRA_PARTITIONS`].
    #[error("--extra-partition is given {0} times, over {MAX_EXTRA_PARTITIONS}")]
    TooMany(usize),
    /// Two partitions of one GPT label.
    #[error("--extra-partition's GPT label {0} is given twice")]
    DuplicateLabel(String),
    /// Two partitions of one mount point.
    #[error("--extra-partition's mount point {0} is given twice")]
    DuplicateMountPoint(String),
}

/// The extra partitions of a run, in the order given: none by default, and then the seed's
/// layout is the installer's `direct`, as without this input.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ExtraPartitions(Vec<ExtraPartition>);

impl ExtraPartitions {
    /// `partitions`, in their order, once there are at most [`MAX_EXTRA_PARTITIONS`] and no two
    /// share a GPT label or a mount point.
    pub fn new(partitions: Vec<ExtraPartition>) -> Result<ExtraPartitions, ExtraPartitionsError> {
        if partitions.len() > MAX_EXTRA_PARTITIONS {
            return Err(ExtraPartitionsError::TooMany(partitions.len()));
        }
        for (index, partition) in partitions.iter().enumerate() {
            let earlier = &partitions[..index];
            if earlier.iter().any(|e| e.label == partition.label) {
                return Err(ExtraPartitionsError::DuplicateLabel(
                    partition.label.clone(),
                ));
            }
            if earlier
                .iter()
                .any(|e| e.mount_point == partition.mount_point)
            {
                return Err(ExtraPartitionsError::DuplicateMountPoint(
                    partition.mount_point.clone(),
                ));
            }
        }
        Ok(ExtraPartitions(partitions))
    }

    /// The partitions, in their order on the disk.
    pub fn partitions(&self) -> &[ExtraPartition] {
        &self.0
    }
}
