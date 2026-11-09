//! The hook of hello's native packages, run for real by `/bin/sh` as the daemon runs it: from a
//! tree laid out in a scratch directory as `<version dir>/tree/<path>`, with the program of that
//! same tree (`tests/bin/exec_fixture.rs`, which prints the path it was started by and each
//! argument) at the path the package installs it. The hook runs the program of its own tree,
//! not the installed one, with `update-hook` and every argument it was given, untouched.

#[path = "../src/testdir.rs"]
mod testdir;

use std::path::Path;
use std::process::Command;

use testdir::TestDir;

/// The program the hook runs: it prints how it was started.
const FIXTURE: &str = env!("CARGO_BIN_EXE_xtask-exec-fixture");

#[test]
fn the_hook_runs_its_own_trees_program_with_update_hook_and_its_arguments() {
    // Catches: a hook that runs the installed program (the old version, before activate), one
    // that loses, splits or reorders an argument (a path with a space), or one that drops the
    // verb.
    let hooks = xtask::test_support::hello_hooks();
    assert_eq!(hooks.len(), 2, "{hooks:?}");
    for (n, (hook, text, program)) in hooks.into_iter().enumerate() {
        let scratch = TestDir::under(
            Path::new(env!("CARGO_TARGET_TMPDIR")),
            &format!("native-hook-{n}"),
        );
        let tree = scratch.join("version dir").join("tree");
        let hook_at = tree.join(hook.trim_start_matches('/'));
        let program_at = tree.join(program.trim_start_matches('/'));
        for path in [&hook_at, &program_at] {
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("its directory");
        }
        std::fs::write(&hook_at, text).expect("write the hook");
        std::fs::copy(FIXTURE, &program_at).expect("copy the program");
        let version_dir = scratch.join("version dir").display().to_string();
        let out = Command::new("/bin/sh")
            .arg(&hook_at)
            .args(["prepare", &version_dir, "--params", "/p q.toml"])
            .output()
            .expect("run the hook");
        let lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            lines,
            [
                program_at.display().to_string(),
                "update-hook".to_owned(),
                "prepare".to_owned(),
                version_dir,
                "--params".to_owned(),
                "/p q.toml".to_owned(),
            ],
            "{hook}"
        );
    }
}
