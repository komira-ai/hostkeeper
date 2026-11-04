//! The load rules of design section 4.7 on the package's files, units, health URL and params.
//! Each case builds a version directory from the design's tierstore example and breaks one thing.

mod support;

use std::fs;
use std::os::unix::fs::symlink;

use hostkeeper_config::manifest::ManifestRefusal;
use hostkeeper_config::names::UnitError;
use support::fifo;
use support::package::{EXAMPLE, Package, invalid_key, with};

/// Catches a `[[files]]` source that is not in the package.
#[test]
fn files_source_missing_is_refused() {
    let package = Package::new("source-missing");
    fs::remove_file(package.dir().join("files/tierstore.conf")).unwrap();
    let refusal = package.load().expect_err("refused");
    assert!(
        matches!(&refusal, ManifestRefusal::FileSource { entry, .. } if entry == "files/tierstore.conf"),
        "{refusal}"
    );
    assert!(
        refusal.to_string().contains("not in the package"),
        "{refusal}"
    );
    for source in [
        "bin/tierstore",
        "files",
        "../files/x",
        "files/../bin/tierstore",
        "/etc/x",
    ] {
        let text = with("\"files/tierstore.conf\"", &format!("\"{source}\""));
        assert!(
            matches!(package.refusal(&text), ManifestRefusal::FileSource { .. }),
            "{source}"
        );
    }
    package.scratch.dir("files/conf.d", 0o555);
    let dir = with("\"files/tierstore.conf\"", "\"files/conf.d\"");
    assert!(package.load_text(&dir).is_ok(), "a directory is a source");
    fifo(&package.dir().join("files/pipe"));
    let pipe = with("\"files/tierstore.conf\"", "\"files/pipe\"");
    let refusal = package.refusal(&pipe);
    assert!(
        refusal
            .to_string()
            .contains("not a regular file or a directory"),
        "{refusal}"
    );
}

/// Catches a `[[files]]` source that is a link, to a file out of the tree or through a linked
/// directory.
#[test]
fn files_source_symlink_out_of_tree_is_refused() {
    let package = Package::new("source-link");
    let outside = package.scratch.file("outside/secret", "x", 0o444);
    let file = package.dir().join("files/tierstore.conf");
    fs::remove_file(&file).unwrap();
    symlink(&outside, &file).unwrap();
    let refusal = package.load().expect_err("refused");
    assert!(refusal.to_string().contains("link"), "{refusal}");
    assert!(matches!(refusal, ManifestRefusal::FileSource { .. }));
    fs::remove_file(&file).unwrap();
    symlink(
        package.dir().join("outside"),
        package.dir().join("files/linked"),
    )
    .unwrap();
    let through = with("\"files/tierstore.conf\"", "\"files/linked/secret\"");
    assert!(matches!(
        package.refusal(&through),
        ManifestRefusal::FileSource { .. }
    ));
}

/// The path guards, matched on whole components: each refused prefix, a relative path and `..`
/// are refused; paths elsewhere (design Q11 option (b)), including `/usr2/x`, are accepted; and
/// two entries may not share a path.
#[test]
fn files_path_guards() {
    let package = Package::new("paths");
    let line = "path   = \"/etc/tierstore/tierstore.conf\"";
    for (path, prefix) in [
        ("/etc/hostkeeper/x", "/etc/hostkeeper"),
        ("/etc/hostkeeper", "/etc/hostkeeper"),
        ("/etc/updater", "/etc/updater"),
        ("/var/lib/hostkeeper", "/var/lib/hostkeeper"),
        ("/var/lib/updater/x", "/var/lib/updater"),
        ("/etc/systemd/system/x.service", "/etc/systemd/system"),
        ("/usr/bin/x", "/usr"),
        ("/boot/x", "/boot"),
        ("/var/lib/x", "/var/lib"),
    ] {
        let refusal = package.refusal(&with(line, &format!("path = \"{path}\"")));
        assert_eq!(
            refusal,
            ManifestRefusal::ReservedPath {
                path: path.to_owned(),
                prefix,
            },
            "{path}"
        );
    }
    for path in [
        "etc/tierstore/x",
        "/etc/../usr/x",
        "/etc/tierstore/",
        "/etc/a b",
        "",
    ] {
        let refusal = package.refusal(&with(line, &format!("path = \"{path}\"")));
        assert!(
            matches!(&refusal, ManifestRefusal::FilePath { path: found, .. } if found == path),
            "{path}: {refusal:?}"
        );
    }
    for path in [
        "/etc/tierstore/tierstore.conf",
        "/opt/tierstore/x",
        "/usr2/x",
        "/var/libx/y",
        "/etc/systemd/user/x",
    ] {
        let text = with(line, &format!("path = \"{path}\""));
        let manifest = package
            .load_text(&text)
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(manifest.files[0].path, path);
    }
    let twice = format!(
        "{}\n[[files]]\nsource = \"files/tierstore.conf\"\npath = \"/etc/tierstore/tierstore.conf\"\n",
        EXAMPLE.replace("[params]\npromotion_bound_secs = 900\n", "")
    );
    assert_eq!(
        package.refusal(&twice),
        ManifestRefusal::DuplicatePath {
            path: "/etc/tierstore/tierstore.conf".to_owned()
        }
    );
    let not_tables = EXAMPLE.replace("[[files]]\nsource = \"files/tierstore.conf\"\npath   = \"/etc/tierstore/tierstore.conf\"\n", "")
        .replace("schema         = 1\n", "schema = 1\nfiles = [1]\n");
    assert_eq!(invalid_key(package.refusal(&not_tables)), "files");
    let not_array = not_tables.replace("files = [1]", "files = 1");
    assert_eq!(invalid_key(package.refusal(&not_array)), "files");
}

/// Catches a unit file of another service: every name in `units/` begins with the service's.
#[test]
fn unit_file_not_starting_with_service_name_is_refused() {
    let package = Package::new("unit-file");
    package.scratch.file("units/sshd.service", "", 0o444);
    assert_eq!(
        package.load().expect_err("refused"),
        ManifestRefusal::UnitFile {
            file: "sshd.service".to_owned(),
            why: UnitError::NotTheServices,
        }
    );
    fs::remove_file(package.dir().join("units/sshd.service")).unwrap();
    package.scratch.file("units/tierstore.conf", "", 0o444);
    assert!(matches!(
        package.load(),
        Err(ManifestRefusal::UnitFile {
            why: UnitError::NotAUnit,
            ..
        })
    ));
    fs::remove_file(package.dir().join("units/tierstore.conf")).unwrap();
    fs::remove_dir_all(package.dir().join("units")).unwrap();
    package.scratch.file("units", "not a directory", 0o444);
    assert!(matches!(
        package.load(),
        Err(ManifestRefusal::Unreadable { .. })
    ));
}

/// Catches the prefix rule matching characters rather than the name: `tier.service` is not
/// `tierstore`'s, `tierstore-cache.service` is.
#[test]
fn unit_named_for_another_service_with_same_prefix_chars_is_refused() {
    let package = Package::new("unit-prefix");
    package
        .scratch
        .file("units/tierstore-cache.service", "", 0o444);
    package.scratch.file("units/tierstore-gc.timer", "", 0o444);
    let manifest = package
        .load()
        .expect("tierstore-cache.service is tierstore's");
    assert!(manifest.unit_files.contains("tierstore-cache.service"));
    package.scratch.file("units/tier.service", "", 0o444);
    assert_eq!(
        package.load().expect_err("refused"),
        ManifestRefusal::UnitFile {
            file: "tier.service".to_owned(),
            why: UnitError::NotTheServices,
        }
    );
}

/// Catches a run entry that names no unit of the package: an instance of a linked template and
/// a linked unit are accepted; an unlinked name, an instance with no template and a template
/// itself are refused.
#[test]
fn run_entry_must_be_a_linked_unit_or_an_instance_of_a_linked_template() {
    let package = Package::new("run");
    package
        .scratch
        .file("units/tierstore-cache.service", "", 0o444);
    let line = "run = [\"tierstore@9301.service\", \"tierstore@9302.service\"]";
    let plain = with(
        line,
        "run = [\"tierstore@9301.service\", \"tierstore-cache.service\"]",
    )
    .replace(
        "http://127.0.0.1:{port}/healthz",
        "http://127.0.0.1:9000/healthz",
    );
    assert_eq!(
        package.load_text(&plain).unwrap().run,
        ["tierstore@9301.service", "tierstore-cache.service"]
    );
    for unit in [
        "tierstore-other.service",
        "tierstore-cache@1.service",
        "tierstore@.service",
        "sshd.service",
        "tierstore@9301.socket",
    ] {
        let text = with(line, &format!("run = [\"{unit}\"]"));
        let refusal = package.refusal(&text);
        assert!(
            matches!(&refusal, ManifestRefusal::RunUnit { unit: found, .. } if found == unit),
            "{unit}: {refusal:?}"
        );
        assert!(refusal.to_string().contains(unit));
    }
    assert_eq!(
        invalid_key(package.refusal(&with(line, "run = \"tierstore@9301.service\""))),
        "units.run"
    );
    let empty = with(line, "run = []");
    assert!(package.load_text(&empty).unwrap().run.is_empty());
}

/// Catches a `{port}` URL with a run unit that has no port: refused, naming the unit.
#[test]
fn port_url_with_non_instance_run_unit_is_refused() {
    let package = Package::new("port-plain");
    package
        .scratch
        .file("units/tierstore-cache.service", "", 0o444);
    package
        .scratch
        .file("units/tierstore-a@.service", "", 0o444);
    let line = "run = [\"tierstore@9301.service\", \"tierstore@9302.service\"]";
    for unit in [
        "tierstore-cache.service",
        "tierstore-a@main.service",
        "tierstore@0.service",
    ] {
        let text = with(
            line,
            &format!("run = [\"tierstore@9301.service\", \"{unit}\"]"),
        );
        assert_eq!(
            package.refusal(&text),
            ManifestRefusal::PortWithoutInstance {
                unit: unit.to_owned()
            },
            "{unit}"
        );
    }
}

/// The accepted side: `{port}` with numeric instances, and a URL without `{port}` with plain
/// units; and what a health URL may not be (design 5.8: `http` to a loopback literal).
#[test]
fn port_url_with_numeric_instances_is_accepted() {
    let package = Package::new("port-ok");
    assert!(package.load().is_ok());
    package.scratch.file("units/tierstore.service", "", 0o444);
    let line = "run = [\"tierstore@9301.service\", \"tierstore@9302.service\"]";
    for url in [
        "http://127.0.0.1/healthz",
        "http://127.0.0.1:8080/healthz",
        "http://[::1]:8080",
    ] {
        let text = with(line, "run = [\"tierstore.service\"]")
            .replace("http://127.0.0.1:{port}/healthz", url);
        let manifest = package
            .load_text(&text)
            .unwrap_or_else(|e| panic!("{url}: {e}"));
        assert_eq!(manifest.health_url.as_deref(), Some(url));
    }
    let v6 = with("http://127.0.0.1:{port}/healthz", "http://[::1]:{port}/");
    assert!(package.load_text(&v6).is_ok());
    for url in [
        "https://127.0.0.1:{port}/healthz",
        "http://localhost:{port}/healthz",
        "http://127.0.0.2:{port}/healthz",
        "http://[::2]:{port}/",
        "http://10.0.0.1:{port}/healthz",
        "http://[2001:db8::1]:{port}/",
        "http://[::1:{port}/",
        "http://127.0.0.1:{port}/{port}",
        "http://127.0.0.1:{port}/a?b",
        "127.0.0.1:{port}",
    ] {
        let text = with("http://127.0.0.1:{port}/healthz", url);
        assert_eq!(invalid_key(package.refusal(&text)), "health.url", "{url}");
    }
    let missing = with("url = \"http://127.0.0.1:{port}/healthz\"", "");
    assert_eq!(invalid_key(package.refusal(&missing)), "health.url");
}

/// Catches a manifest setting a key only the daemon writes, quoted or as a bare dotted key.
#[test]
fn updater_prefixed_param_is_refused() {
    let package = Package::new("params-reserved");
    for (line, key) in [
        (
            "\"updater.staged_release\" = \"/x\"",
            "updater.staged_release",
        ),
        ("updater.anything = 1", "updater"),
        ("updater = 1", "updater"),
    ] {
        let text = with("promotion_bound_secs = 900", line);
        assert_eq!(
            package.refusal(&text),
            ManifestRefusal::ReservedParam {
                key: key.to_owned()
            },
            "{line}"
        );
    }
    let near = with("promotion_bound_secs = 900", "updaters = 1\nmy_updater = 2");
    assert!(package.load_text(&near).is_ok());
}

/// Catches a parameter the hook cannot read as a scalar: arrays, tables and dates are refused;
/// strings, integers, floats and booleans are accepted.
#[test]
fn non_scalar_param_is_refused() {
    let package = Package::new("params-scalar");
    for (line, key) in [
        ("list = [1, 2]", "list"),
        ("table = { a = 1 }", "table"),
        ("nested.a = 1", "nested"),
        ("when = 2026-10-01", "when"),
    ] {
        let text = with("promotion_bound_secs = 900", line);
        assert_eq!(
            package.refusal(&text),
            ManifestRefusal::NonScalarParam {
                key: key.to_owned()
            },
            "{line}"
        );
    }
    let scalars = with(
        "promotion_bound_secs = 900",
        "s = \"x\"\ni = -1\nf = 1.5\nb = false",
    );
    assert_eq!(package.load_text(&scalars).unwrap().params.len(), 4);
    let not_table = with("[params]\npromotion_bound_secs = 900\n", "")
        .replace("schema         = 1\n", "schema = 1\nparams = 1\n");
    assert_eq!(invalid_key(package.refusal(&not_table)), "params");
}
