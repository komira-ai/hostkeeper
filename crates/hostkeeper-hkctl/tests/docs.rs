//! `docs/cli.md` against what the command table makes now. The doc must equal the generated
//! text; when it does not, the test writes the current text to `cli.md` beside its test binary
//! (under the target directory) and says where, so it can be copied over.

use std::path::Path;

use hostkeeper_hkctl::markdown;
use hostkeeper_proto::ctl::COMMANDS;

/// The fixture's command table, which `build.rs` generated from tests/fixtures.
#[expect(
    dead_code,
    reason = "the generated file also holds the local-service and reserved-flag lists"
)]
mod fixture {
    use hostkeeper_proto::ctl::{Command, EnumValue, Field, FieldKind, Input, Label};

    include!(concat!(env!("OUT_DIR"), "/fixture_commands.rs"));
}

/// Catches `docs/cli.md` left behind when a proto, a flag rule, a global flag or an exit code
/// changes.
#[test]
fn docs_cli_md_is_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/cli.md");
    let written = std::fs::read_to_string(path).unwrap_or_default();
    let current = markdown(COMMANDS);
    if written != current {
        let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli.md");
        std::fs::write(&out, &current).expect("write the current doc");
        panic!(
            "docs/cli.md is not what the protos make; the current text is in {}",
            out.display()
        );
    }
    // The doc names every command hkctl has, and none of the host API.
    for command in COMMANDS.iter().filter(|c| hostkeeper_hkctl::is_served(c)) {
        assert!(current.contains(&format!(
            "#### `hkctl {} {}`",
            command.service_command, command.method_command
        )));
    }
    assert!(!current.contains("hkctl coordinator heartbeat"));
    assert!(current.contains(
        "| 3 | the coordinator refused the caller: UNAUTHENTICATED or PERMISSION_DENIED | PERMISSION_DENIED, UNAUTHENTICATED |"
    ));
    assert!(current.contains("| `--timeout-secs` | SECONDS | 30 |"));
    // A proto change is a doc change.
    let fixture = markdown(fixture::COMMANDS);
    assert_ne!(fixture, current);
    assert!(fixture.contains("#### `hkctl widgets brand-new-method`"));
    assert!(fixture.contains("| `--password-file` | PATH | Read the secret `password` from this file; one trailing newline is dropped. The password. |"));
    assert!(fixture.contains("| `--password-stdin` |  | Read the secret `password` from stdin. |"));
    assert!(fixture.contains("Through `--request` only: `recovery_codes` (repeated string)."));
    assert!(!fixture.contains("beats"));
}
