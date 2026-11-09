//! `target.json`: the sealed copy of the entry a version of a repository installer was staged
//! for, and the manifest the daemon reads from it (packages-lifecycle.md section 6.1,
//! packages-repositories.md section 4.2).
//!
//! A repository version's directory holds no `manifest.toml`: what hostkeeper knows of the
//! service is the entry's manifest, which the order carried and the receipt check judged
//! ([`crate::entry::check`]). The stage writes the entry into the version directory as proto3
//! canonical JSON, before the seal, so the seal and every check of the tree cover it, and from
//! then on the daemon reads the version from that copy, never from a later order: a rollback, a
//! resume at boot, the observer and the uninstall read the sealed `target.json`.
//!
//! [`manifest`] is the one reader of a version directory's manifest: `target.json` when it is
//! there, `manifest.toml` ([`Manifest::load`]) otherwise, so a `hostkeeper-archive` version reads
//! exactly as before. The manifest of a repository version names no unit files and places no
//! file (the package installs its own), and its hook is the package's program at the fixed path
//! `tree/usr/lib/<package>/hostkeeper-hook` when the package has one; without one the service
//! gets the built-in steps (packages-lifecycle.md section 6.3, [`crate::builtin`]).

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::num::NonZeroU64;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use hostkeeper_config::manifest::{
    Expected, Hook, HostkeeperVersion, Manifest, ManifestRefusal, Timeouts,
};
use hostkeeper_install::repository::TREE;
use hostkeeper_install::repository::tree::hook_path;
use hostkeeper_proto::FILE_DESCRIPTOR_SET;
use hostkeeper_proto::hostkeeper::release::v1::{Param, Target, param};
use prost::Message;
use prost_reflect::{DescriptorPool, DynamicMessage, SerializeOptions};

/// The name of the copy of the entry in a repository version's directory.
pub const TARGET_FILE: &str = "target.json";

/// The full name of the entry's message.
const TARGET_MESSAGE: &str = "hostkeeper.release.v1.Target";

/// The most bytes of `target.json` read: an entry is far smaller (its manifest has at most 64
/// units and 64 params).
pub const MAX_TARGET_BYTES: u64 = 1024 * 1024;

/// Why `target.json` could not be written or read.
#[derive(Debug, thiserror::Error)]
pub enum TargetError {
    /// The file could not be read or written.
    #[error("{TARGET_FILE}: {0}")]
    Io(#[from] io::Error),
    /// The file is not the JSON of an entry, or is larger than [`MAX_TARGET_BYTES`].
    #[error("{TARGET_FILE} is not an entry: {0}")]
    Invalid(String),
}

/// The entry's message descriptor, from the compiled descriptor set.
fn descriptor() -> Option<prost_reflect::MessageDescriptor> {
    DescriptorPool::decode(FILE_DESCRIPTOR_SET)
        .ok()?
        .get_message_by_name(TARGET_MESSAGE)
}

/// `entry` as proto3 canonical JSON. The encoding of a decoded entry cannot fail: an `Err`
/// says the compiled descriptor set has no entry message.
pub fn to_json(entry: &Target) -> Result<Vec<u8>, TargetError> {
    let encoded = || {
        let message =
            DynamicMessage::decode(descriptor()?, entry.encode_to_vec().as_slice()).ok()?;
        let mut writer = serde_json::Serializer::new(Vec::new());
        message
            .serialize_with_options(&mut writer, &SerializeOptions::new())
            .ok()?;
        Some(writer.into_inner())
    };
    encoded().ok_or(TargetError::Invalid(NO_DESCRIPTOR.to_owned()))
}

/// What [`to_json`] and [`from_json`] say when the compiled descriptor set has no entry message.
const NO_DESCRIPTOR: &str = "the compiled descriptor set has no entry message";

/// The text of `error`.
fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// The entry `bytes` hold, as [`to_json`] writes it.
pub fn from_json(bytes: &[u8]) -> Result<Target, TargetError> {
    let descriptor = descriptor().ok_or(TargetError::Invalid(NO_DESCRIPTOR.to_owned()))?;
    let mut reader = serde_json::Deserializer::from_slice(bytes);
    DynamicMessage::deserialize(descriptor, &mut reader)
        .map_err(text)
        .and_then(|message| message.transcode_to::<Target>().map_err(text))
        .map_err(TargetError::Invalid)
}

/// Writes `entry` as `target.json` in the staging directory `stage`, mode 0444, durably.
pub fn write(stage: &Path, entry: &Target) -> Result<(), TargetError> {
    let json = to_json(entry)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o444)
        .open(stage.join(TARGET_FILE))?;
    file.write_all(&json)?;
    file.set_permissions(fs::Permissions::from_mode(0o444))?;
    file.sync_all()?;
    Ok(())
}

/// The entry the version directory `dir` was staged for; `None` when it has no `target.json`
/// (a `hostkeeper-archive` version).
pub fn read(dir: &Path) -> Result<Option<Target>, TargetError> {
    let file = match File::open(dir.join(TARGET_FILE)) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        opened => opened?,
    };
    let mut bytes = Vec::new();
    file.take(MAX_TARGET_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_TARGET_BYTES {
        return Err(TargetError::Invalid(format!(
            "it is larger than {MAX_TARGET_BYTES} bytes"
        )));
    }
    from_json(&bytes).map(Some)
}

/// The manifest of the version directory `dir`, read for `expected` by a daemon of version
/// `own`: from `target.json` when the directory has one ([`of_entry`]), from `manifest.toml`
/// otherwise ([`Manifest::load`]).
pub fn manifest(
    dir: &Path,
    expected: &Expected,
    own: &HostkeeperVersion,
) -> Result<Manifest, ManifestRefusal> {
    let unreadable = |e: TargetError| ManifestRefusal::Unreadable { why: e.to_string() };
    match read(dir).map_err(unreadable)? {
        Some(entry) => of_entry(dir, &entry, expected, own),
        None => Manifest::load(dir, expected, own),
    }
}

/// The manifest of `entry`, staged in `dir`, for `expected` and a daemon of version `own`. The
/// entry passed the receipt check of its installer when it was staged; what is checked here is
/// what the manifest's own loader checks of every manifest: the bounds above 0, the params
/// scalars, and `min_hostkeeper` against `own`, last.
pub fn of_entry(
    dir: &Path,
    entry: &Target,
    expected: &Expected,
    own: &HostkeeperVersion,
) -> Result<Manifest, ManifestRefusal> {
    let invalid = |key: &str, why: &str| ManifestRefusal::Invalid {
        key: key.to_owned(),
        why: why.to_owned(),
    };
    let manifest = entry
        .manifest
        .as_ref()
        .ok_or_else(|| invalid("manifest", "is missing from the entry"))?;
    let hook = manifest.hook.clone().unwrap_or_default();
    let bounds = hook.timeout_secs.unwrap_or_default();
    let above_0 = |key: &str, secs: u64| {
        NonZeroU64::new(secs).ok_or_else(|| invalid(&format!("hook.timeout_secs.{key}"), "is 0"))
    };
    let timeout_secs = Timeouts {
        prepare: NonZeroU64::new(bounds.prepare),
        cutover: above_0("cutover", bounds.cutover)?,
        verify: above_0("verify", bounds.verify)?,
        rollback: above_0("rollback", bounds.rollback)?,
        cleanup: above_0("cleanup", bounds.cleanup)?,
        uninstall: bounds.uninstall.and_then(NonZeroU64::new),
    };
    // The package's own hook, at its fixed path in the sealed tree; none: the built-in steps.
    let program = manifest
        .package
        .as_deref()
        .map(|package| format!("{TREE}/{}", hook_path(package)))
        .filter(|program| fs::symlink_metadata(dir.join(program)).is_ok_and(|m| m.is_file()));
    let params = manifest
        .params
        .iter()
        .map(|(key, param)| scalar(param).map(|value| (key.clone(), value)))
        .collect::<Option<toml::Table>>()
        .ok_or_else(|| invalid("params", "holds a param with no value"))?;
    let min_hostkeeper = HostkeeperVersion::parse(&manifest.min_hostkeeper)
        .map_err(|why| invalid("min_hostkeeper", &why.to_string()))?;
    if min_hostkeeper > *own {
        return Err(ManifestRefusal::DaemonTooOld {
            needs: min_hostkeeper,
            running: *own,
        });
    }
    Ok(Manifest {
        name: expected.name.clone(),
        version: expected.version.clone(),
        platform: expected.platform,
        min_hostkeeper,
        hook: Hook {
            command: program.into_iter().collect(),
            boot: hook.boot,
            verify_deadline_secs: hook.verify_deadline_secs,
            promotion_ceiling_secs: hook.promotion_ceiling_secs.and_then(NonZeroU64::new),
            cleanup_retries: hook.cleanup_retries.unwrap_or(DEFAULT_CLEANUP_RETRIES),
            timeout_secs,
        },
        unit_files: BTreeSet::new(),
        run: manifest.run.clone(),
        health_url: manifest.health_url.clone(),
        files: Vec::new(),
        params,
    })
}

/// How often `cleanup` is tried again when the entry's hook does not say (`Hook.cleanup_retries`
/// absent).
pub const DEFAULT_CLEANUP_RETRIES: u32 = 3;

/// A param of the entry as the TOML scalar the hook's params file holds; `None` for one with no
/// value.
fn scalar(param: &Param) -> Option<toml::Value> {
    Some(match param.value.as_ref()? {
        param::Value::Text(text) => toml::Value::String(text.clone()),
        param::Value::Integer(n) => toml::Value::Integer(*n),
        param::Value::Float(x) => toml::Value::Float(*x),
        param::Value::Flag(flag) => toml::Value::Boolean(*flag),
    })
}

#[cfg(test)]
#[path = "target_tests.rs"]
mod tests;
