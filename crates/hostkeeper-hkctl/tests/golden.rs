//! The script emitted from the fixture protos, byte for byte against `tests/golden/fixture.hkctl`.
//! A change to the runtime or the emitter is a change to the golden file in the same pull
//! request; when they differ, the test writes the current script beside its test binary (under
//! the target directory) and says where, so it can be copied over.

use std::path::Path;

/// The fixture's command table, which `build.rs` generated from tests/fixtures.
#[expect(
    dead_code,
    reason = "the generated file also holds the local-service and reserved-flag lists"
)]
mod fixture {
    use hostkeeper_proto::ctl::{Command, EnumValue, Field, FieldKind, Input, Label};

    include!(concat!(env!("OUT_DIR"), "/fixture_commands.rs"));
}

const GOLDEN: &str = include_str!("golden/fixture.hkctl");

/// Catches any change to the emitted script that its golden file does not record: a flag spec,
/// a help text, the order of the methods, the runtime, the version line.
#[test]
fn the_script_of_the_fixture_protos_equals_its_golden_file() {
    let current = hostkeeper_hkctl::script(fixture::COMMANDS, "1.2.3+0123456789ab")
        .expect("the fixture protos make a script");
    if current != GOLDEN {
        let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("fixture.hkctl");
        std::fs::write(&out, &current).expect("write the current script");
        panic!(
            "tests/golden/fixture.hkctl is not the script the fixture protos make; the current \
             script is in {}",
            out.display()
        );
    }
    // The golden file holds the fixture's methods, the new one among them, and not the host API.
    for line in [
        "\t'widgets brand-new-method')",
        "\t\t_hk_path='/hostkeeper.fixture.v1.Widgets/BrandNewMethod'",
        "\t\t\t'password|password|password|s|string|one|'",
        "\t\t\t'colour|colour|colour|f|enum|one|red=COLOUR_RED,dark_blue=COLOUR_DARK_BLUE'",
        "_hk_version='hkctl 1.2.3+0123456789ab'",
        "_hk_main \"$@\"",
    ] {
        assert!(GOLDEN.lines().any(|l| l == line), "{line}");
    }
    assert!(!GOLDEN.contains("hostkeeper.agent.v9"));
}
