//! The programs of the Mac and the system they run on (MP-X1): `sw_vers` and `uname -sm`; for each
//! program the Mac names, whether its path is a file; `xcodes version`; the first lines of
//! `xip --help`; and which of the programs of Apple's other archive formats exist.

use std::path::Path;

use super::run::tool;
use crate::xcode_push::Mac;
use crate::xcode_record::{MacOs, Program, Programs};

/// The lines of `xip --help` that are recorded.
const HELP_LINES: usize = 20;

/// The macOS the checks run on.
pub(super) fn macos(mac: &Mac) -> MacOs {
    MacOs {
        sw_vers: tool(&mac.sw_vers, &[], None),
        uname: tool(&mac.uname, &[&"-sm"], None),
    }
}

/// `name` at `path`, and whether the path is a file.
fn program(name: &'static str, path: &Path) -> Program {
    Program {
        name,
        path: path.display().to_string(),
        file: path.is_file(),
    }
}

/// The programs: the failure names the first required one whose path is not a file.
pub(super) fn check(mac: &Mac) -> (Programs, Result<(), String>) {
    let required = vec![
        program("pkgutil", &mac.pkgutil),
        program("xip", &mac.xip),
        program("plutil", &mac.plutil),
        program("codesign", &mac.codesign),
        program("spctl", &mac.spctl),
        program("xar", &mac.xar),
        program("sw_vers", &mac.sw_vers),
        program("uname", &mac.uname),
        program("cp", &mac.cp),
        program("df", &mac.df),
    ];
    let missing = required.iter().find(|p| !p.file).map_or(Ok(()), |p| {
        Err(format!("programs: {} is not a file at {}", p.name, p.path))
    });
    let help = tool(&mac.xip, &[&"--help"], None);
    let xip_help: Vec<&str> = help
        .stdout
        .lines()
        .chain(help.stderr.lines())
        .take(HELP_LINES)
        .collect();
    let programs = Programs {
        required,
        optional: vec![
            program("archiveutil", &mac.archiveutil),
            program("aa", &mac.aa),
            program("aea", &mac.aea),
        ],
        xcodes: tool(&mac.xcodes, &[&"version"], None),
        xip_help: xip_help.join("\n"),
    };
    (programs, missing)
}
