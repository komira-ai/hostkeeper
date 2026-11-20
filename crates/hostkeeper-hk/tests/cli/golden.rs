//! The help text of `hk` for the fixture's command table, which is the user-visible part of the
//! command line: the version line, the rules, the global flags, the exit codes, the services,
//! a service's methods and a method's flags, in every kind of field. Each is compared with its
//! file under tests/golden; on a difference the current text is written to the target
//! directory's `tmp` and its path said, so it can be copied over.

use std::path::Path;

use crate::rig::{Rig, STAMP, args};

/// Proves: what `hk --help` and each level of help prints for the fixture protos, and the help of
/// `hk xcode-push`, is exactly the text in the golden files, so a change of a help text, a flag
/// rule, an exit code or a field's kind is a change of a file the review sees.
#[test]
fn the_help_of_the_fixture_protos_equals_its_golden_files() {
    let rig = Rig::new("golden");
    let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let cases: [(&str, &[&str]); 8] = [
        ("top.txt", &["--help"]),
        ("service_widgets.txt", &["widgets", "--help"]),
        ("service_gadgets.txt", &["gadgets", "--help"]),
        ("method_widgets_make.txt", &["widgets", "make", "--help"]),
        ("method_widgets_login.txt", &["widgets", "login", "--help"]),
        ("method_gadgets_tune.txt", &["gadgets", "tune", "--help"]),
        ("ssh.txt", &["ssh-proxy", "--help"]),
        ("xcode_push.txt", &["xcode-push", "--help"]),
    ];
    let mut stale = Vec::new();
    for (file, words) in cases {
        let ran = rig.fx(&args(words));
        assert_eq!((ran.code, ran.stderr.as_str()), (0, ""), "{words:?}");
        let written = std::fs::read_to_string(golden.join(file)).unwrap_or_default();
        if written != ran.stdout {
            let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join(file);
            std::fs::write(&out, &ran.stdout).expect("write the current text");
            stale.push(format!("{file}: {}", out.display()));
        }
    }
    assert!(
        stale.is_empty(),
        "stale golden files, current text in: {stale:#?}"
    );
    // The golden top help carries the stamp the tests run with, and no host API package.
    let top = std::fs::read_to_string(golden.join("top.txt")).expect("the top help");
    assert!(top.starts_with(&format!("hk {STAMP}: ")));
    assert!(!top.contains("beats"));
}
