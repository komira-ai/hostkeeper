use std::os::unix::fs::PermissionsExt as _;

use hostkeeper_proto::ctl::{COMMANDS, Field, FieldKind, Input, Label};

use super::*;
use crate::release::fakes::{nth_commit, scratch};
use crate::release::ports::Adapters as _;
use crate::release::run_with;
use crate::release::tests::{connects, empty, text, verb};

/// A commit, 40 hex digits.
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn version() -> Version {
    "0.1.70".parse().expect("a version")
}

#[test]
fn hkctl_writes_the_script_of_the_protos_stamped_with_the_version_and_commit() {
    // Catches: an asset under another name, not executable, stamped with another version or
    // commit, or other than the emitter's script of the protos' table; and a directory that does
    // not exist yet refused.
    let dir = scratch("hkctl-write");
    let out = dir.join("dist/new");
    let commit = nth_commit(0xabc);
    let said = write(COMMANDS, &version(), &commit, &out).expect("written");
    let path = out.join("hkctl-0.1.70");
    assert_eq!(
        said,
        [format!("{}: hkctl 0.1.70+000000000000", path.display())]
    );
    let text = std::fs::read_to_string(&path).expect("the asset");
    assert_eq!(
        text,
        hostkeeper_hkctl::script(COMMANDS, "0.1.70+000000000000").expect("a script")
    );
    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    assert_eq!(problems(&text, &version()), Vec::<String>::new());
    // The stamp is the commit's first 12 digits.
    let commit: Commit = "0123456789abcdef0123456789abcdef01234567"
        .parse()
        .expect("a commit");
    write(COMMANDS, &version(), &commit, &out).expect("written");
    let text = std::fs::read_to_string(&path).expect("the asset");
    assert!(text.contains("\n_hk_version='hkctl 0.1.70+0123456789ab'\n"));
}

#[test]
fn hkctl_refuses_a_table_it_cannot_write_and_a_directory_it_cannot_make() {
    // Catches: a table whose field takes one of hkctl's own flags written anyway, and a write
    // failure passing as success.
    static FIELDS: [Field; 1] = [Field {
        name: "response_file",
        flag: "response-file",
        kind: FieldKind::String,
        type_name: "",
        label: Label::Singular,
        input: Input::Flag,
        secret: false,
        enum_values: &[],
        doc: "",
    }];
    let clash = [Command {
        package: "x",
        service: "Things",
        method: "Do",
        path: "/x.Things/Do",
        service_command: "things",
        method_command: "do",
        service_doc: "",
        doc: "",
        request: "x.R",
        response: "x.R",
        fields: &FIELDS,
    }];
    let dir = scratch("hkctl-refused");
    assert_eq!(
        write(&clash, &version(), &nth_commit(1), &dir),
        Err(Error::Failed(
            "hkctl things do: the flag --response-file is one of hkctl's own".to_owned()
        ))
    );
    std::fs::write(dir.join("file"), b"").expect("a file");
    assert!(matches!(
        write(COMMANDS, &version(), &nth_commit(1), &dir.join("file")),
        Err(Error::Io { .. })
    ));
    std::fs::create_dir(dir.join("hkctl-0.1.70")).expect("a directory in the way");
    assert!(matches!(
        write(COMMANDS, &version(), &nth_commit(1), &dir),
        Err(Error::Io { .. })
    ));
}

#[test]
fn an_asset_that_is_not_the_releases_script_is_named() {
    // Catches: `release check` passing an asset with no bash shebang, with no version line or
    // two, or with the version line of another release or a stamp that is not 12 hex digits.
    let good = "#!/usr/bin/env bash\n# x\n_hk_version='hkctl 0.1.70+0123456789ab'\n";
    assert_eq!(problems(good, &version()), Vec::<String>::new());
    let shebang = "hkctl-0.1.70 does not start with `#!/usr/bin/env bash`";
    let line =
        "hkctl-0.1.70 does not hold exactly one version line `_hk_version='hkctl 0.1.70+<12 hex>'`";
    for (text, want) in [
        (
            "#!/bin/sh\n_hk_version='hkctl 0.1.70+0123456789ab'\n",
            vec![shebang],
        ),
        ("", vec![shebang, line]),
        ("#!/usr/bin/env bash\n", vec![line]),
        (
            "#!/usr/bin/env bash\n_hk_version='hkctl 0.1.70+0123456789ab'\n_hk_version='hkctl 0.1.70+0123456789ab'\n",
            vec![line],
        ),
        (
            "#!/usr/bin/env bash\n_hk_version='hkctl 0.1.71+0123456789ab'\n",
            vec![line],
        ),
        (
            "#!/usr/bin/env bash\n_hk_version='hkctl 0.1.70+0123456789AB'\n",
            vec![line],
        ),
        (
            "#!/usr/bin/env bash\n_hk_version='hkctl 0.1.70+0123456789a'\n",
            vec![line],
        ),
        (
            "#!/usr/bin/env bash\n_hk_version='hkctl 0.1.70+0123456789ab\n",
            vec![line],
        ),
    ] {
        assert_eq!(problems(text, &version()), want, "{text:?}");
    }
}

#[test]
fn hkctl_asks_for_no_adapters_and_runs_with_the_ports_lent_back() {
    // Catches: `hkctl` building the real adapters (git and gh), which it does not need, or
    // `run_with` failing it instead of handing it back to `run`; and the asset written under
    // another name or stamp.
    let d = scratch("release-hkctl");
    let (out, connected) = connects(verb(&[
        "hkctl",
        "--version",
        "0.1.9",
        "--commit",
        COMMIT,
        "--out",
        text(&d),
    ]));
    assert_eq!(
        out,
        Ok(vec![format!(
            "{}: hkctl 0.1.9+{}",
            d.join("hkctl-0.1.9").display(),
            &COMMIT[..12]
        )])
    );
    assert!(!connected);
    let fakes = empty();
    let ports = fakes.ports();
    let blocked = verb(&[
        "hkctl",
        "--version",
        "0.1.9",
        "--commit",
        COMMIT,
        "--out",
        text(&d.join("hkctl-0.1.9")),
    ]);
    assert!(matches!(run_with(blocked, &ports), Err(Error::Io { .. })));
}
