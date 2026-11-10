//! A repository package's unit files in its extracted tree (packages-native.md section 10.2), and
//! what a `run` unit's `[Install]` section asks to be enabled by. hostkeeper holds no rule on
//! what else a package installs.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// The directories a package's unit files may be in, relative to the root.
pub const UNIT_DIRS: [&str; 2] = ["usr/lib/systemd/system", "lib/systemd/system"];

/// The unit file of `unit` in `tree`, relative to it: a regular file of one of [`UNIT_DIRS`].
pub fn unit_file(tree: &Path, unit: &str) -> Option<String> {
    UNIT_DIRS
        .iter()
        .map(|dir| format!("{dir}/{unit}"))
        .find(|rel| {
            fs::symlink_metadata(tree.join(rel)).is_ok_and(|meta| meta.file_type().is_file())
        })
}

/// The links that enable a unit: one per target of `WantedBy=` (`<target>.wants/`) and
/// `RequiredBy=` (`<target>.requires/`) in its `[Install]` section, as
/// `<target>.<wants|requires>/<unit>` relative to the service manager's directory. An empty
/// assignment drops the targets before it, as systemd reads it. A target that is not a unit's
/// name yields no link.
pub fn install_links(unit: &str, text: &str) -> BTreeSet<String> {
    let mut wants: Vec<String> = Vec::new();
    let mut requires: Vec<String> = Vec::new();
    let mut in_install = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_install = line == "[Install]";
            continue;
        }
        let Some((key, value)) = line.split_once('=').filter(|_| in_install) else {
            continue;
        };
        let list = match key.trim() {
            "WantedBy" => &mut wants,
            "RequiredBy" => &mut requires,
            _ => continue,
        };
        match value.trim() {
            "" => list.clear(),
            value => list.extend(value.split_whitespace().map(str::to_owned)),
        }
    }
    let mut links = BTreeSet::new();
    for (list, kind) in [(wants, "wants"), (requires, "requires")] {
        for target in list.into_iter().filter(|target| is_unit_name(target)) {
            links.insert(format!("{target}.{kind}/{unit}"));
        }
    }
    links
}

/// Whether `name` is a unit's name: `[A-Za-z0-9:_.@-]` with a dot, at most 255 bytes.
fn is_unit_name(name: &str) -> bool {
    (1..=255).contains(&name.len())
        & name.contains('.')
        & name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() | b":_.@-".contains(&b))
}
