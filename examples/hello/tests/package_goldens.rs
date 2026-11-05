//! The committed package goldens against the program they carry: the manifest loads with the
//! daemon's own loader, its params are what the hook reads, and the unit files, the plists and
//! the health URL name the port, the record directory and the units the hook and the server use.

use std::fs;
use std::path::PathBuf;

use hello::cli::{Command, parse};
use hello::hook::{Settings, Units};
use hello::record::Slot;
use hostkeeper_config::manifest::{Expected, HostkeeperVersion, Manifest};
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_hook::{Params, Scalar};

const DAEMON: HostkeeperVersion = HostkeeperVersion {
    major: 0,
    minor: 1,
    patch: 0,
};

fn dir(platform: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("package")
        .join(platform)
}

fn load(platform: &str) -> Manifest {
    let expected = Expected {
        name: ServiceName::new("hello").expect("a service name"),
        version: Version::new("1.2.3").expect("a version"),
    };
    Manifest::load(&dir(platform), &expected, &DAEMON)
        .unwrap_or_else(|why| panic!("the {platform} manifest is refused: {why}"))
}

fn read(platform: &str, file: &str) -> String {
    fs::read_to_string(dir(platform).join(file)).expect("a golden")
}

/// The params as the daemon hands them to a step: the manifest's `[params]` written as the
/// params file, and read back by the hook's own reader into its settings.
fn settings(manifest: &Manifest) -> Settings {
    let mut params = Params::default();
    for (key, value) in &manifest.params {
        let scalar = match (value.as_str(), value.as_integer()) {
            (Some(text), _) => Scalar::Text(text.to_owned()),
            (_, Some(number)) => Scalar::Integer(number),
            _ => panic!("{key} is a scalar of another kind"),
        };
        params.insert(key, scalar);
    }
    params.insert("updater.target_version", Scalar::Text("1.2.3".to_owned()));
    let table = hello::params::parse(&params.to_toml()).expect("the hook reads the params file");
    Settings::from_table(&table).unwrap_or_else(|why| panic!("{}", why.report.line()))
}

/// Catches a manifest the daemon would refuse, or one whose hook command is not the program with
/// the `update-hook` word, or whose run units are not the two slots the hook starts.
#[test]
fn the_manifests_load_and_name_the_hook_and_the_two_slots() {
    for (platform, run) in [
        ("linux", ["hello@a.service", "hello@b.service"]),
        ("macos", ["hello.a", "hello.b"]),
    ] {
        let manifest = load(platform);
        assert_eq!(
            manifest.hook.command,
            ["bin/hello", "update-hook"],
            "{platform}"
        );
        assert_eq!(manifest.run, run, "{platform}");
        assert_eq!(
            manifest.health_url.as_deref(),
            Some("http://127.0.0.1:9310/readyz"),
            "{platform}"
        );
        assert!(
            manifest.hook.timeout_secs.uninstall.is_some(),
            "{platform}: uninstall is bounded"
        );
        assert!(
            manifest.hook.timeout_secs.prepare.is_some(),
            "{platform}: prepare is bounded"
        );
    }
}

/// Catches the manifest's params not being what the hook reads (a key renamed on one side), and
/// the unit names of the slots in the manifest differing from the ones the hook runs.
#[test]
fn the_params_are_what_the_hook_reads() {
    for (platform, units) in [("linux", Units::Systemd), ("macos", Units::Launchd)] {
        let manifest = load(platform);
        let settings = settings(&manifest);
        assert_eq!(settings.units, units, "{platform}");
        assert_eq!(settings.port, 9310, "{platform}");
        assert_eq!(
            settings.record_dir,
            PathBuf::from("/var/lib/updater/hooks/hello")
        );
        for slot in Slot::ALL {
            let unit = match units {
                Units::Systemd => format!("hello@{}.service", slot.name()),
                Units::Launchd => format!("hello.{}", slot.name()),
            };
            assert!(manifest.run.contains(&unit), "{platform}: {unit}");
        }
    }
}

/// Catches the manifest's command not reaching the hook: the program is run as
/// `bin/hello update-hook <step>`.
#[test]
fn the_manifest_command_followed_by_a_step_is_the_hook() {
    let manifest = load("linux");
    for step in hello::cli::STEPS {
        let mut args: Vec<std::ffi::OsString> =
            manifest.hook.command[1..].iter().map(Into::into).collect();
        args.push(step.into());
        assert_eq!(parse(&args), Ok(Command::Hook(vec![step.into()])), "{step}");
    }
}

/// Catches a unit file whose port, record directory or marker path differs from the manifest's,
/// so that the hook would start a slot that never starts, or verify the wrong port.
#[test]
fn the_unit_file_agrees_with_the_manifest_and_the_hook() {
    let settings = settings(&load("linux"));
    let unit = read("linux", "units/hello@.service");
    let record = settings.record_dir.display();
    assert!(
        unit.contains(&format!("\nConditionPathExists={record}/slots/%i\n")),
        "a slot starts only while its marker exists, where the hook puts it:\n{unit}"
    );
    assert!(
        unit.contains(&format!(
            "\nExecStart=/var/lib/hostkeeper/packages/hello/current/bin/hello --port {}\n",
            settings.port
        )),
        "{unit}"
    );
    assert!(!unit.contains("--fail-ready"), "the default build is ready");
}

/// Catches a plist whose label, port or marker path differs from what the hook runs with
/// `launchctl`, or that starts at load.
#[test]
fn the_plists_agree_with_the_manifest_and_the_hook() {
    let settings = settings(&load("macos"));
    let record = settings.record_dir.display();
    for slot in Slot::ALL {
        let name = slot.name();
        let plist = read("macos", &format!("units/hello.{name}.plist"));
        for want in [
            format!("<key>Label</key>\n\t<string>hello.{name}</string>"),
            format!(
                "<string>--port</string>\n\t\t<string>{}</string>",
                settings.port
            ),
            format!("<key>{record}/slots/{name}</key>\n\t\t\t<true/>"),
            "<key>RunAtLoad</key>\n\t<false/>".to_owned(),
        ] {
            assert!(
                plist.contains(&want),
                "hello.{name}.plist lacks {want:?}:\n{plist}"
            );
        }
        assert!(!plist.contains("--fail-ready"));
    }
}

/// Catches a package that does not carry the program `prepare` looks for, or carries it
/// writable.
#[test]
fn the_package_carries_the_program_the_hook_checks() {
    for platform in ["linux", "macos"] {
        let entries = read(platform, "entries.txt");
        let lines: Vec<&str> = entries.lines().collect();
        assert!(lines.contains(&"0555 bin/hello"), "{platform}: {lines:?}");
        assert!(
            lines.contains(&"0444 manifest.toml"),
            "{platform}: {lines:?}"
        );
    }
}
