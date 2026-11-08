//! Valid releases to break one rule at a time: a manifest and an entry for every installer, each
//! passing every check, so a test changes one field and sees exactly that field refused.

use std::collections::BTreeMap;

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::ServiceName;
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

/// A manifest `installer` takes, with every field it allows set.
pub fn manifest(installer: Installer) -> Manifest {
    let mut manifest = Manifest {
        min_hostkeeper: "0.4.0".to_owned(),
        ..Manifest::default()
    };
    if installer == Installer::HostkeeperArchive {
        return manifest;
    }
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
    match installer {
        Installer::MacosPkg => {
            manifest.package = Some("dev.web.server".to_owned());
            manifest.run = vec!["web".to_owned(), "web.worker".to_owned()];
        }
        Installer::Tarball => {
            manifest.run = vec!["web.service".to_owned()];
            manifest.place = vec![
                place("unit", "units/web.service", "web.service"),
                place("unit", "units/web@.service", "web@.service"),
                place("file", "files/web.conf", "/etc/web/web.conf"),
                place("file", "files/share", "/opt/web/share"),
            ];
        }
        Installer::OciImage => {
            manifest.hook = Some(Hook {
                command: Vec::new(),
                boot: None,
                ..hook()
            });
            manifest.run = vec!["web-api.service".to_owned()];
            manifest.containers = vec![container()];
        }
        _ => {
            manifest.package = Some("web".to_owned());
            manifest.run = vec!["web.service".to_owned(), "web-cache.timer".to_owned()];
        }
    }
    manifest
}

pub fn location(store: &str, locator: &str) -> Location {
    Location {
        store: store.to_owned(),
        locator: locator.to_owned(),
    }
}

/// An entry of `installer` with its manifest.
pub fn entry(installer: Installer) -> Target {
    Target {
        installer: installer.as_str().to_owned(),
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
        manifest: Some(manifest(installer)),
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
