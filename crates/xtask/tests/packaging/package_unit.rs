//! The unit hostkeeper's own package carries (`packaging/linux/package/hostkeeper.service`,
//! `units/hostkeeper.service` in the package) is the unit the deb and the rpm install, line for
//! line, but for the binary it starts: the package's, through the package layout's `current`
//! link. So every rule the deb's unit is held to holds for the package's too.

use crate::{UNIT, altered, read, refuses};

/// The package's unit, from `packaging/linux`.
const PACKAGE_UNIT: &str = "package/hostkeeper.service";

/// The binary the deb's unit starts.
const INSTALLED_BINARY: &str = "ExecStart=/usr/bin/hostkeeper ";

/// The binary the package's unit starts: `bin/hostkeeper` of the package, through `current`.
const PACKAGE_BINARY: &str =
    "ExecStart=/var/lib/hostkeeper/packages/hostkeeper/current/bin/hostkeeper ";

/// The lines of a unit that are not comments.
fn settings(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect()
}

/// Passes when `package` is `deb` with its binary replaced by the package's, comments aside.
fn check_package_unit(deb: &str, package: &str) -> Result<(), String> {
    let starts = |text: &str, binary: &str| text.lines().filter(|l| l.starts_with(binary)).count();
    if starts(deb, INSTALLED_BINARY) != 1 {
        return Err(format!(
            "the deb's unit has no one `{INSTALLED_BINARY}` line"
        ));
    }
    let want = deb.replace(INSTALLED_BINARY, PACKAGE_BINARY);
    let (want, got) = (settings(&want), settings(package));
    match want.iter().zip(&got).position(|(w, g)| w != g) {
        Some(at) => Err(format!(
            "the package's unit differs from the deb's at its setting {}: `{}`, not `{}`",
            at + 1,
            got[at],
            want[at]
        )),
        None if want.len() != got.len() => Err(format!(
            "the package's unit has {} settings, the deb's {}",
            got.len(),
            want.len()
        )),
        None => Ok(()),
    }
}

/// Catches the package's unit drifting from the deb's (a setting changed in one alone), or
/// starting the binary the deb installs instead of the package's own.
#[test]
fn the_package_unit_is_the_deb_unit_but_for_the_binary_it_starts() {
    let deb = read(UNIT);
    let package = read(PACKAGE_UNIT);
    assert_eq!(check_package_unit(&deb, &package), Ok(()));
    let check = |text: &str| check_package_unit(&deb, text);
    refuses(
        check,
        &[
            (
                altered(&package, PACKAGE_BINARY, INSTALLED_BINARY),
                "differs from the deb's at its setting",
            ),
            (
                altered(&package, "Restart=always\n", "Restart=on-failure\n"),
                "`Restart=on-failure`, not `Restart=always`",
            ),
            (
                altered(&package, "WatchdogSec=60\n", "WatchdogSec=45\n"),
                "`WatchdogSec=45`, not `WatchdogSec=60`",
            ),
            (
                altered(
                    &package,
                    "StartLimitIntervalSec=0\n",
                    "StartLimitIntervalSec=300\n",
                ),
                "`StartLimitIntervalSec=300`, not `StartLimitIntervalSec=0`",
            ),
            (
                altered(&package, "WantedBy=multi-user.target\n", ""),
                "the package's unit has",
            ),
        ],
    );
    // The deb's unit changed alone is refused too, and one without its binary line.
    let changed = altered(&deb, "TimeoutStartSec=infinity\n", "TimeoutStartSec=90\n");
    assert!(
        check_package_unit(&changed, &package)
            .is_err_and(|e| e.contains("`TimeoutStartSec=infinity`, not `TimeoutStartSec=90`"))
    );
    let no_binary = altered(
        &deb,
        INSTALLED_BINARY,
        "ExecStart=/usr/local/bin/hostkeeper ",
    );
    assert_eq!(
        check_package_unit(&no_binary, &package),
        Err(format!(
            "the deb's unit has no one `{INSTALLED_BINARY}` line"
        ))
    );
}
