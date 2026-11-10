//! The check that systemd loads a package's own unit (docs/design/packages-native.md, section
//! 10.6): after a native version is installed over an archive version, each unit of the archive's
//! that the package also installs must be loaded from the package's file, not from a file or a
//! link an earlier release left in `/etc/systemd/system/`, which would hide it.
//!
//! `systemctl show --property=LoadState,FragmentPath -- <unit>` prints one `Key=Value` line for
//! each. The unit is the package's when `LoadState` is `loaded` and `FragmentPath` is the unit's
//! file in one of the package's unit directories ([`UNIT_DIRS`]); anything else is
//! [`Reason::UnitNotLoaded`], naming what systemd loads instead. A unit the package does not
//! install, and a template (`name@.service`, which has no `LoadState` of its own), is not checked.

use std::path::Path;

use super::outcome::{Failure, Reason};
use super::tools::{Run, Tool, Tools};
use super::tree::{UNIT_DIRS, unit_file};

/// The properties the check reads.
pub const SHOW_PROPERTIES: &str = "--property=LoadState,FragmentPath";

/// What systemd says of a unit: its `LoadState` and `FragmentPath`, empty when not printed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnitLoad {
    pub load_state: String,
    pub fragment_path: String,
}

/// Reads the output of `systemctl show` [`SHOW_PROPERTIES`]: a line it does not know is skipped.
pub fn parse_show(stdout: &str) -> UnitLoad {
    let mut load = UnitLoad::default();
    for line in stdout.lines() {
        match line.split_once('=') {
            Some(("LoadState", value)) => value.clone_into(&mut load.load_state),
            Some(("FragmentPath", value)) => value.clone_into(&mut load.fragment_path),
            _ => {}
        }
    }
    load
}

/// Whether `load` names `unit`'s file in one of [`UNIT_DIRS`].
fn package_file(unit: &str, load: &UnitLoad) -> bool {
    UNIT_DIRS
        .iter()
        .any(|dir| load.fragment_path == format!("/{dir}/{unit}"))
}

/// Why `load` is not `unit` loaded from the package's file: the package's file that systemd
/// does not load, or what systemd loads instead of it; `None` when it is the package's unit.
fn not_loaded(unit: &str, load: &UnitLoad) -> Option<String> {
    let loaded = load.load_state == "loaded";
    match (package_file(unit, load), loaded) {
        (true, true) => None,
        (true, false) => Some(format!(
            "systemd does not load the package's unit {} (LoadState={})",
            load.fragment_path, load.load_state
        )),
        (false, _) => {
            let from = match load.fragment_path.as_str() {
                "" => "no file",
                path => path,
            };
            Some(format!(
                "systemd loads {unit} from {from} (LoadState={}), not from the package's unit in \
                 /{}/: a unit left in /etc/systemd/system/ hides it",
                load.load_state, UNIT_DIRS[0]
            ))
        }
    }
}

/// Checks each of `units` that the package's extracted `tree` installs, but a template: systemd
/// loads it from the package's file ([the module](self)). A `systemctl` that cannot be run, or
/// that exits other than 0, is [`Reason::IoError`]: nothing was learnt of the unit.
pub fn check_units(
    tools: &dyn Tools,
    tree: &Path,
    units: &[String],
    bound_secs: u64,
) -> Result<(), Failure> {
    let checked = units
        .iter()
        .filter(|unit| !unit.contains("@.") & unit_file(tree, unit).is_some());
    for unit in checked {
        let args = vec![
            "show".to_owned(),
            SHOW_PROPERTIES.to_owned(),
            "--".to_owned(),
            unit.clone(),
        ];
        let output = tools
            .run(&Run::offline(Tool::Systemctl, args, bound_secs))
            .map_err(|e| Failure::new(Reason::IoError, format!("systemctl show {unit}: {e}")))?;
        if !output.succeeded() {
            return Err(Failure::new(
                Reason::IoError,
                format!("systemctl show {unit} failed: {}", output.text().trim()),
            ));
        }
        let load = parse_show(&String::from_utf8_lossy(&output.stdout));
        if let Some(why) = not_loaded(unit, &load) {
            return Err(Failure::new(Reason::UnitNotLoaded, why));
        }
    }
    Ok(())
}
