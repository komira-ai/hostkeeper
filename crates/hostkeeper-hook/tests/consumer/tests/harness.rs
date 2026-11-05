//! What a service repository writes: a fake of its service and one call to the harness. The
//! service here is the example hook's blue/green pair, and the hook is the example binary of
//! hostkeeper-hook, whose path the building test passes in as `SAMPLE_HOOK`.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;

use hostkeeper_hook::conformance::{FakeService, Fault, HookCommand, Sandbox, check};
use hostkeeper_hook::{Params, Scalar, Step};

struct SampleFake;

impl FakeService for SampleFake {
    fn init(&mut self, sandbox: &Sandbox) -> io::Result<()> {
        fs::write(sandbox.artifact().join("VERSION"), "v2\n")?;
        std::os::unix::fs::symlink("v1", sandbox.world().join("current"))?;
        fs::create_dir_all(sandbox.root().join("faults"))
    }

    fn env(&self, sandbox: &Sandbox) -> Vec<(OsString, OsString)> {
        vec![
            ("EXAMPLE_WORLD".into(), sandbox.world().into()),
            ("EXAMPLE_RECORD".into(), sandbox.record().into()),
            ("EXAMPLE_OUTSIDE".into(), sandbox.outside().into()),
            (
                "EXAMPLE_FAULTS".into(),
                sandbox.root().join("faults").into(),
            ),
        ]
    }

    fn params(&self) -> Params {
        let mut params = Params::default();
        params.insert("promotion_bound_secs", Scalar::Integer(900));
        params
    }

    fn inject(&mut self, sandbox: &Sandbox, step: Step, fault: Fault) -> io::Result<bool> {
        let word = match fault {
            Fault::Fail => "fail",
            Fault::Defer => "defer",
            Fault::Halt => "halt",
        };
        fs::write(sandbox.root().join("faults").join(step.name()), word)?;
        Ok(true)
    }

    fn residue(&self, sandbox: &Sandbox) -> io::Result<Vec<String>> {
        Ok(["next", "cordon"]
            .into_iter()
            .filter(|name| sandbox.world().join(name).exists())
            .map(str::to_owned)
            .collect())
    }
}

/// Catches a harness that cannot be driven from a crate outside the workspace: a fake written
/// against the public API only, and the example hook, pass every check.
#[test]
fn the_example_hook_passes_the_harness_from_outside_the_workspace() {
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("conformance");
    if scratch.exists() {
        fs::remove_dir_all(&scratch).expect("remove the old scratch directory");
    }
    fs::create_dir_all(&scratch).expect("create the scratch directory");
    let hook = HookCommand::new(Path::new(env!("SAMPLE_HOOK"))).with_hook_args(&["update-hook"]);
    let summary = check(&hook, &mut SampleFake, &scratch).unwrap_or_else(|e| panic!("{e}"));
    assert!(summary.kill_points > 0);
}
