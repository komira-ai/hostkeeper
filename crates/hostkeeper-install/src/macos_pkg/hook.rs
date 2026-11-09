//! The hook at its fixed path (packages-native.md section 10.2): a `.pkg` that has a hand-over
//! carries its hook program at `/Library/Application Support/<service>/hostkeeper-hook`, and one
//! without gets the built-in steps; whether it has one is the file's presence, checked at
//! `stage` (step 5 of section 10.2's checks): when present, a regular file its owner may run,
//! reached through no link; and `hook.boot` set with no hook is refused.

use std::collections::BTreeMap;

use crate::host::{Kind, Meta};

/// The hook's name, in the service's directory.
pub const HOOK_FILE: &str = "hostkeeper-hook";

/// The logical path of `service`'s hook.
pub fn hook_path(service: &str) -> String {
    format!("{}/{HOOK_FILE}", super::payload::service_dir(service))
}

/// What the hook check found wrong; each is `manifest_invalid`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HookError {
    #[error("the hook {0} is reached through the link {1}")]
    ThroughLink(String, String),
    #[error("the hook {0} is a link")]
    Link(String),
    #[error("the hook {0} is a directory")]
    Directory(String),
    #[error("the hook {0} is not a regular file")]
    NotFile(String),
    #[error("the hook {0} is not executable (mode {1:o})")]
    NotExecutable(String, u32),
    #[error("the entry's hook.boot is set, but the package has no hook at {0}")]
    BootWithoutHook(String),
}

/// Checks the hook of `service` in the walked `payload`; `boot` is whether the entry's manifest
/// sets `hook.boot`. `Ok(true)` when the package has a hook, `Ok(false)` when it has none.
pub fn check(
    payload: &BTreeMap<String, Meta>,
    service: &str,
    boot: bool,
) -> Result<bool, HookError> {
    let path = hook_path(service);
    let mut parent = path.as_str();
    while let Some((above, _)) = parent.rsplit_once('/') {
        if payload
            .get(above)
            .is_some_and(|meta| meta.kind == Kind::Link)
        {
            return Err(HookError::ThroughLink(path.clone(), above.to_owned()));
        }
        parent = above;
    }
    match payload.get(&path) {
        None if boot => Err(HookError::BootWithoutHook(path)),
        None => Ok(false),
        Some(meta) => match meta.kind {
            Kind::Link => Err(HookError::Link(path)),
            Kind::Dir => Err(HookError::Directory(path)),
            Kind::Other => Err(HookError::NotFile(path)),
            Kind::File if meta.mode & 0o100 == 0 => {
                Err(HookError::NotExecutable(path, meta.mode & 0o7777))
            }
            Kind::File => Ok(true),
        },
    }
}

#[cfg(test)]
#[path = "hook_tests.rs"]
mod tests;
