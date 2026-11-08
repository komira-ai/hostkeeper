//! The package scripts as pacman runs them for the Arch package: nfpm inlines each script whole
//! into the package's `.INSTALL` as the body of `post_install` and `pre_remove`, which pacman
//! calls with one argument, `<pkgver>-<pkgrel>`.

use crate::package::{DAEMON_RELOAD, Scenario, TMPFILES_CREATE, check_runs};
use crate::{POSTINSTALL, PREREMOVE, altered, read, refuses};

/// The post-install script as pacman runs it: nfpm inlines the script as the body of
/// `post_install`, which pacman calls with the new version, `<pkgver>-<pkgrel>`, alone. No `case`
/// arm matches that, so the directories are made and the unit is never enabled.
const PACMAN_POSTINSTALL_RUNS: [Scenario; 3] = [
    Scenario {
        name: "pacman install",
        args: &["1.2.3-1"],
        failing: None,
        ran: &[&TMPFILES_CREATE, &DAEMON_RELOAD],
        status: 0,
    },
    Scenario {
        name: "pacman install of another release",
        args: &["1.2.4-2"],
        failing: None,
        ran: &[&TMPFILES_CREATE, &DAEMON_RELOAD],
        status: 0,
    },
    Scenario {
        name: "pacman install without a running systemd",
        args: &["1.2.3-1"],
        failing: Some(&DAEMON_RELOAD),
        ran: &[&TMPFILES_CREATE, &DAEMON_RELOAD],
        status: 0,
    },
];

/// The pre-remove script as pacman runs it: `pre_remove` is called with the old version,
/// `<pkgver>-<pkgrel>`, alone; no `case` arm matches it, so nothing runs.
const PACMAN_PREREMOVE_RUNS: [Scenario; 1] = [Scenario {
    name: "pacman removal",
    args: &["1.2.3-1"],
    failing: None,
    ran: &[],
    status: 0,
}];

/// Catches a package script that, run as pacman runs it (one argument, `<pkgver>-<pkgrel>`),
/// enables the unit, disables it on removal, or leaves out the directories and the reload: the
/// Arch package makes the directories and reloads systemd, never enables the unit, and its
/// removal runs nothing.
#[test]
fn package_scripts_run_under_pacman_make_the_directories_and_never_enable() {
    let post = read(POSTINSTALL);
    let pre = read(PREREMOVE);
    assert_eq!(check_runs(&post, &PACMAN_POSTINSTALL_RUNS), Ok(()));
    assert_eq!(check_runs(&pre, &PACMAN_PREREMOVE_RUNS), Ok(()));
    let always_enable = altered(
        &post,
        "case \"$1:$2\" in\n  configure: | 1:) systemctl enable hostkeeper.service ;;\nesac\n",
        "systemctl enable hostkeeper.service\n",
    );
    let post_cases = [
        (always_enable, "pacman install: the script ran"),
        (
            altered(&post, "systemd-tmpfiles --create hostkeeper.conf\n", ""),
            "pacman install: the script ran",
        ),
        (
            altered(&post, "systemctl daemon-reload || true\n", ""),
            "pacman install: the script ran",
        ),
        (
            altered(
                &post,
                "systemctl daemon-reload || true",
                "systemctl daemon-reload",
            ),
            "pacman install without a running systemd: the script ran",
        ),
    ];
    refuses(
        |t: &str| check_runs(t, &PACMAN_POSTINSTALL_RUNS),
        &post_cases,
    );
    let always_disable = altered(
        &pre,
        "case \"$1\" in\n  remove | 0) systemctl disable hostkeeper.service || true ;;\nesac\n",
        "systemctl disable hostkeeper.service || true\n",
    );
    refuses(
        |t: &str| check_runs(t, &PACMAN_PREREMOVE_RUNS),
        &[(always_disable, "pacman removal: the script ran")],
    );
}
