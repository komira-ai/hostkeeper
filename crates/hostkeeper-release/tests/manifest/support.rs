//! Valid releases to break one rule at a time: a manifest and an entry for every installer, each
//! passing every check, so a test changes one field and sees exactly that field refused.

use std::collections::BTreeMap;

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::ServiceName;
use hostkeeper_config::platform::Platform;
use hostkeeper_proto::hostkeeper::release::v1::{
    Blob, Container, Hook, InstallerBounds, Location, Manifest, Param, Place, Port, StepBounds,
    Target, TargetList, Volume, param,
};
use hostkeeper_release::Installer;
use prost::Message as _;

/// The service every fixture is of.
pub fn web() -> ServiceName {
    ServiceName::new("web").expect("a service name")
}

/// The platform each fixture is built for: macOS for `macos-pkg`, whose units are launchd
/// labels, Linux for the others.
pub fn platform_of(installer: Installer) -> Platform {
    match installer {
        Installer::MacosPkg => Platform::MacosArm64,
        _ => Platform::LinuxX86_64,
    }
}

/// The platform of the Linux fixtures.
pub const LINUX: Platform = Platform::LinuxX86_64;

/// A version.
pub fn version(text: &str) -> HostkeeperVersion {
    HostkeeperVersion::parse(text).expect("a version")
}

/// A digest of 64 lower-case hex characters.
pub fn digest(c: char) -> String {
    c.to_string().repeat(64)
}

/// A hook with a program and every bound set, the deadline exactly `verify` plus `rollback`.
pub fn hook() -> Hook {
    Hook {
        command: vec!["bin/web".to_owned(), "update-hook".to_owned()],
        boot: Some("restore".to_owned()),
        timeout_secs: Some(StepBounds {
            prepare: 0,
            cutover: 10,
            verify: 20,
            rollback: 30,
            cleanup: 40,
            uninstall: Some(5),
        }),
        verify_deadline_secs: 50,
        promotion_ceiling_secs: Some(3600),
        cleanup_retries: Some(3),
    }
}

pub fn bounds() -> InstallerBounds {
    InstallerBounds {
        stage_secs: 60,
        activate_secs: 120,
        remove_secs: 3600,
    }
}

pub fn text(value: &str) -> Param {
    Param {
        value: Some(param::Value::Text(value.to_owned())),
    }
}

pub fn place(kind: &str, source: &str, path: &str) -> Place {
    Place {
        kind: kind.to_owned(),
        source: source.to_owned(),
        path: path.to_owned(),
    }
}

pub fn container() -> Container {
    Container {
        unit: "web-api.service".to_owned(),
        args: vec!["--port".to_owned(), "8080".to_owned()],
        ports: vec![Port {
            host: 8080,
            container: 8080,
            loopback_only: true,
        }],
        volumes: vec![Volume {
            host_path: "/var/lib/web/data".to_owned(),
            container_path: "/data".to_owned(),
            read_only: false,
        }],
    }
}

/// The hook of a native entry: the package's program at its fixed path, so no command, and a
/// boot step.
pub fn native_hook() -> Hook {
    Hook {
        command: Vec::new(),
        ..hook()
    }
}

/// The fields every fixture manifest shares: the hook's bounds, the health URL and the params.
fn base_manifest() -> Manifest {
    let mut manifest = Manifest {
        min_hostkeeper: "0.4.0".to_owned(),
        ..Manifest::default()
    };
    manifest.hook = Some(hook());
    manifest.installer_bounds = Some(bounds());
    manifest.health_url = Some("http://127.0.0.1:8080/healthz".to_owned());
    manifest.params = BTreeMap::from([
        ("listen".to_owned(), text("8080")),
        (
            "workers".to_owned(),
            Param {
                value: Some(param::Value::Integer(4)),
            },
        ),
        (
            "ratio".to_owned(),
            Param {
                value: Some(param::Value::Float(0.5)),
            },
        ),
        (
            "debug".to_owned(),
            Param {
                value: Some(param::Value::Flag(false)),
            },
        ),
    ]);
    manifest
}

/// A manifest `installer` takes, with every field it allows set.
pub fn manifest(installer: Installer) -> Manifest {
    let mut manifest = base_manifest();
    match installer {
        Installer::MacosPkg => {
            manifest.hook = Some(native_hook());
            manifest.package = Some("dev.web.server".to_owned());
            manifest.run = vec!["web".to_owned(), "web.worker".to_owned()];
        }
        Installer::Apt => {
            manifest.hook = Some(native_hook());
            manifest.package = Some("web".to_owned());
            manifest.run = vec!["web.service".to_owned(), "web-cache.timer".to_owned()];
        }
    }
    manifest
}

/// A manifest of the shared rules alone ([`hostkeeper_release::manifest::check`]), with the
/// fields only a withdrawn installer used set: a hook with a program, `place` entries and no
/// package. No installer of this crate takes it, and an entry of an unlisted installer carries
/// it.
pub fn placed_manifest() -> Manifest {
    Manifest {
        run: vec!["web.service".to_owned()],
        place: vec![
            place("unit", "units/web.service", "web.service"),
            place("unit", "units/web@.service", "web@.service"),
            place("file", "files/web.conf", "/etc/web/web.conf"),
            place("file", "files/share", "/opt/web/share"),
        ],
        ..base_manifest()
    }
}

/// A manifest of the shared rules alone with a container and no hook program, as a withdrawn
/// image installer's was.
pub fn container_manifest() -> Manifest {
    Manifest {
        hook: Some(Hook {
            command: Vec::new(),
            boot: None,
            ..hook()
        }),
        run: vec!["web-api.service".to_owned()],
        containers: vec![container()],
        ..base_manifest()
    }
}

/// A manifest holding `min_hostkeeper` alone, as an archive entry's was.
pub fn bare_manifest() -> Manifest {
    Manifest {
        min_hostkeeper: "0.4.0".to_owned(),
        ..Manifest::default()
    }
}

pub fn location(store: &str, locator: &str) -> Location {
    Location {
        store: store.to_owned(),
        locator: locator.to_owned(),
    }
}

/// An artifact entry named `installer`, which may be none of this crate's, with `manifest`, its
/// artifact's locations and digest, and an attestation.
pub fn attested_artifact(installer: &str, manifest: Manifest) -> Target {
    Target {
        installer: installer.to_owned(),
        locations: vec![
            location("lan", "releases/web/1.6.0/web.bin"),
            location("github", "example-org/web/releases/download/v1.6.0/web.bin"),
        ],
        digest: digest('a'),
        bytes: 48_812_032,
        attestation: Some(Blob {
            digest: digest('b'),
            bytes: 7311,
            locations: vec![location(
                "github",
                "example-org/web/releases/download/v1.6.0/web.bin.sigstore.json",
            )],
        }),
        manifest: Some(manifest),
        ..Target::default()
    }
}

/// An entry of `installer` with its manifest: a repository entry ([`repository_entry`]) for a
/// backend of the repository installer, an artifact entry for any other, with an attestation
/// but for `macos-pkg`, whose entry carries none.
pub fn entry(installer: Installer) -> Target {
    if installer.is_repository() {
        return Target {
            installer: installer.as_str().to_owned(),
            ..repository_entry()
        };
    }
    let attested = attested_artifact(installer.as_str(), manifest(installer));
    if installer == Installer::MacosPkg {
        return Target {
            attestation: None,
            ..attested
        };
    }
    attested
}

/// An artifact entry of a withdrawn installer's name (`tarball`, `hostkeeper-archive`, `deb`),
/// with a manifest of the shared rules alone ([`placed_manifest`]).
pub fn unlisted(name: &str) -> Target {
    attested_artifact(name, placed_manifest())
}

/// A repository entry as CI sends it (packages-repositories.md section 1): `apt`, the
/// repository, the package version, the pin, and a manifest naming the package; no location, no
/// size and no attestation.
pub fn repository_entry() -> Target {
    Target {
        installer: Installer::Apt.as_str().to_owned(),
        digest: digest('d'),
        manifest: Some(manifest(Installer::Apt)),
        repository: Some("komira-testing".to_owned()),
        package_version: Some("1:1.6.0-1".to_owned()),
        ..Target::default()
    }
}

/// An artifact entry of the installer `name`, which may be one no daemon lists (a withdrawn
/// `deb` or `rpm`, or any other of the grammar), with a manifest naming a package and a hook
/// program, as such an entry had before the installers were withdrawn.
pub fn named(name: &str) -> Target {
    attested_artifact(
        name,
        Manifest {
            hook: Some(hook()),
            ..manifest(Installer::Apt)
        },
    )
}

/// A list of the artifact entries of the installers `names` ([`named`]), in that order.
pub fn named_list(names: &[&str]) -> TargetList {
    TargetList {
        entries: names.iter().map(|name| named(name)).collect(),
    }
}

/// A list of the entries of `installers`, in that order.
pub fn list(installers: &[Installer]) -> TargetList {
    TargetList {
        entries: installers.iter().map(|i| entry(*i)).collect(),
    }
}

/// Targets keyed by platform spelling.
pub fn targets(lists: &[(&str, TargetList)]) -> BTreeMap<String, TargetList> {
    lists
        .iter()
        .map(|(key, list)| ((*key).to_owned(), list.clone()))
        .collect()
}

/// Pads the text param `pad` of `manifest` so the manifest encodes to exactly `bytes` bytes.
pub fn pad_manifest_to(manifest: &mut Manifest, bytes: usize) {
    manifest.params.insert("pad".to_owned(), text(""));
    let mut fill = bytes.saturating_sub(manifest.encoded_len());
    // A longer string can take a longer length prefix; settle on the exact size.
    loop {
        manifest
            .params
            .insert("pad".to_owned(), text(&"x".repeat(fill)));
        let now = manifest.encoded_len();
        if now == bytes {
            return;
        }
        assert!(now > bytes, "a pad only grows the encoding");
        fill -= now - bytes;
    }
}
