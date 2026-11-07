//! The scripts hkctl emits hold no construct newer than bash 3.2 (the bash macOS ships), no
//! export and no here-document or here-string, for the real protos and the fixture's.

use hostkeeper_hkctl::scan::{newer_than_bash_3_2, secret_hazards};

/// The fixture's command table, which `build.rs` generated from tests/fixtures.
#[expect(
    dead_code,
    reason = "the generated file also holds the local-service and reserved-flag lists"
)]
mod fixture {
    use hostkeeper_proto::ctl::{Command, EnumValue, Field, FieldKind, Input, Label};

    include!(concat!(env!("OUT_DIR"), "/fixture_commands.rs"));
}

/// Both scripts.
fn scripts() -> [String; 2] {
    [
        hostkeeper_hkctl::script(hostkeeper_proto::ctl::COMMANDS, "1.2.3+0123456789ab")
            .expect("the real protos make a script"),
        hostkeeper_hkctl::script(fixture::COMMANDS, "1.2.3+0123456789ab")
            .expect("the fixture protos make a script"),
    ]
}

/// Catches a construct of bash 4 or 5 in the runtime or a generated part (an associative array,
/// `mapfile`, `${x,,}`, `;&`, `|&`, ...), which would fail on a Mac's bash 3.2; and the scan
/// itself passing everything: the same script with one such line added is refused, by line.
#[test]
fn the_emitted_scripts_hold_no_construct_newer_than_bash_3_2() {
    for script in scripts() {
        assert_eq!(newer_than_bash_3_2(&script), []);
        let lines = script.lines().count();
        let worse = script.replacen("_hk_main \"$@\"", "declare -A _hk_seen\n_hk_main \"$@\"", 1);
        let found = newer_than_bash_3_2(&worse);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, lines);
    }
}

/// Catches an export (a variable a child inherits) or a here-document or here-string (which bash
/// may write to a temporary file) in the script: none is there, and one added is refused.
#[test]
fn the_emitted_scripts_export_nothing_and_write_no_here_document() {
    for script in scripts() {
        assert_eq!(secret_hazards(&script), []);
        for added in ["export _hk_token", "read -r x <<<\"$_hk_token\""] {
            let worse = script.replacen("_hk_main \"$@\"", &format!("{added}\n_hk_main \"$@\""), 1);
            assert_eq!(secret_hazards(&worse).len(), 1, "{added}");
        }
    }
}
