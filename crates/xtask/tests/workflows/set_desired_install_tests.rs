//! The rule of [`set_desired`](super::set_desired) on its install step: one command, a `tar` that
//! unpacks the verified tarball's `hk`.

use super::set_desired_tests::{CLIENT, assert_client, real};

/// The install step of [`CLIENT`], whole.
const INSTALL: &str = "      - id: install\n        run: tar -xzf hk-1-linux-x86_64.tar.gz -C \"$RUNNER_TEMP/hk-release\" hk\n";

/// Catches the lint passing an install step that runs a second command (a `mkdir` before the
/// `tar`, a command chained with `&&` or `;`), runs another program, or unpacks another member;
/// and catches it refusing the real workflow and [`CLIENT`], whose install is one `tar`.
#[test]
fn the_install_step_is_one_tar_of_hk() {
    assert_client(&real(), &[]);
    assert!(CLIENT.contains(INSTALL), "CLIENT holds the install step");
    assert_client(CLIENT, &[]);
    let want = ["step 6 (`install`) is not one command, a `tar` that unpacks `hk`"];
    for run in [
        "|\n          mkdir \"$RUNNER_TEMP/hk-release\"\n          tar -xzf hk-1-linux-x86_64.tar.gz -C \"$RUNNER_TEMP/hk-release\" hk",
        "mkdir -p x && tar -xzf hk-1-linux-x86_64.tar.gz -C x hk",
        "tar -xzf hk-1-linux-x86_64.tar.gz -C x hk; chmod 0755 x/hk",
        "install -D --mode 0755 hk-1 \"$RUNNER_TEMP/hk-release/hk\"",
        "tar -xzf hk-1-linux-x86_64.tar.gz -C x hkctl",
        "",
    ] {
        let edited = CLIENT.replace(
            INSTALL,
            &format!("      - id: install\n        run: {run}\n"),
        );
        assert_client(&edited, &want);
    }
}
