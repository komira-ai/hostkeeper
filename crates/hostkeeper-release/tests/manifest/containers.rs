//! A manifest's containers: their units, ports and volumes.

use hostkeeper_config::names::{PathError, UnitError};
use hostkeeper_proto::hostkeeper::release::v1::{Container, Manifest, Port, Volume};
use hostkeeper_release::manifest::{self, ManifestError};

use crate::support::{LINUX, container, container_manifest, web};

fn with(containers: Vec<Container>, run: &[&str]) -> Manifest {
    Manifest {
        containers,
        run: run.iter().map(|unit| (*unit).to_owned()).collect(),
        ..container_manifest()
    }
}

fn check_one(container: Container) -> Result<(), ManifestError> {
    let run = [container.unit.clone()];
    let run: Vec<&str> = run.iter().map(String::as_str).collect();
    manifest::check(&web(), LINUX, &with(vec![container], &run))
}

fn unit(unit: &str) -> Container {
    Container {
        unit: unit.to_owned(),
        ..container()
    }
}

/// Catches: a container unit of another service, without a suffix, of a kind that is not a
/// service, a template, or not in `run` passing; two containers of one unit passing.
#[test]
fn a_container_unit_is_a_service_of_the_service_in_run_and_no_other_containers() {
    assert_eq!(check_one(unit("web.service")), Ok(()));
    assert_eq!(check_one(unit("web-api@1.service")), Ok(()));
    for (bad, why) in [
        ("api.service", UnitError::NotTheServices),
        ("web-api", UnitError::NotAUnit),
        ("web@.service", UnitError::BadInstance),
        ("web@a b.service", UnitError::BadInstance),
    ] {
        // Not in run either: the unit's own rule is the answer.
        let got = manifest::check(&web(), LINUX, &with(vec![unit(bad)], &["web.service"]));
        assert_eq!(
            got,
            Err(ManifestError::ContainerUnit {
                unit: bad.to_owned(),
                why
            }),
            "{bad}"
        );
    }
    assert_eq!(
        check_one(unit("web-api.socket")),
        Err(ManifestError::ContainerNotService(
            "web-api.socket".to_owned()
        ))
    );
    assert_eq!(
        manifest::check(
            &web(),
            LINUX,
            &with(vec![unit("web-api.service")], &["web.service"])
        ),
        Err(ManifestError::ContainerNotRun("web-api.service".to_owned()))
    );
    assert_eq!(
        manifest::check(
            &web(),
            LINUX,
            &with(
                vec![unit("web-api.service"), unit("web-api.service")],
                &["web-api.service"]
            )
        ),
        Err(ManifestError::DuplicateContainer(
            "web-api.service".to_owned()
        ))
    );
    for (err, text) in [
        (
            ManifestError::ContainerNotService("web.socket".to_owned()),
            "the container unit \"web.socket\" is not a .service",
        ),
        (
            ManifestError::ContainerNotRun("web.service".to_owned()),
            "the container unit \"web.service\" is not in run",
        ),
        (
            ManifestError::DuplicateContainer("web.service".to_owned()),
            "two containers have the unit \"web.service\"",
        ),
        (
            ManifestError::ContainerUnit {
                unit: "api.service".to_owned(),
                why: UnitError::NotTheServices,
            },
            "the container unit \"api.service\" does not begin with the service name, alone or followed by `-`",
        ),
    ] {
        assert_eq!(err.to_string(), text);
    }
}

fn port(host: u32, inside: u32) -> Container {
    Container {
        ports: vec![Port {
            host,
            container: inside,
            loopback_only: false,
        }],
        ..container()
    }
}

/// Catches: a privileged or out-of-range host port passing, a container port of 0 or out of
/// range passing, or a port at either end of its range refused.
#[test]
fn a_host_port_is_1024_to_65535_and_a_container_port_1_to_65535() {
    for host in [1024, 65535] {
        assert_eq!(check_one(port(host, 80)), Ok(()), "{host}");
    }
    for host in [0, 80, 1023, 65536] {
        assert_eq!(
            check_one(port(host, 80)),
            Err(ManifestError::HostPort {
                unit: "web-api.service".to_owned(),
                port: host
            }),
            "{host}"
        );
    }
    for inside in [1, 65535] {
        assert_eq!(check_one(port(8080, inside)), Ok(()), "{inside}");
    }
    for inside in [0, 65536] {
        assert_eq!(
            check_one(port(8080, inside)),
            Err(ManifestError::ContainerPort {
                unit: "web-api.service".to_owned(),
                port: inside
            }),
            "{inside}"
        );
    }
    assert_eq!(
        check_one(port(1023, 80)).expect_err("refused").to_string(),
        "the container \"web-api.service\" publishes the host port 1023, not 1024 to 65535"
    );
    assert_eq!(
        check_one(port(8080, 0)).expect_err("refused").to_string(),
        "the container \"web-api.service\" publishes the container port 0, not 1 to 65535"
    );
}

fn volume(host_path: &str, container_path: &str) -> Container {
    Container {
        volumes: vec![Volume {
            host_path: host_path.to_owned(),
            container_path: container_path.to_owned(),
            read_only: true,
        }],
        ..container()
    }
}

/// Catches: a volume outside the service's data directory, beside it (`/var/lib/webx`), or
/// with `..` passing; a relative container path passing; the data directory itself refused.
#[test]
fn a_volume_host_path_is_under_the_services_data_directory() {
    for good in ["/var/lib/web", "/var/lib/web/data", "/var/lib/web/a/b"] {
        assert_eq!(check_one(volume(good, "/data")), Ok(()), "{good}");
    }
    for outside in ["/var/lib/api/data", "/var/lib/webx", "/etc/web", "/"] {
        assert_eq!(
            check_one(volume(outside, "/data")),
            Err(ManifestError::VolumeOutside {
                path: outside.to_owned(),
                dir: "/var/lib/web".to_owned()
            }),
            "{outside}"
        );
    }
    for (bad, why) in [
        ("/var/lib/web/../api", PathError::DotComponent),
        ("/var/lib/web/./data", PathError::DotComponent),
        ("var/lib/web", PathError::NotAbsolute),
    ] {
        assert_eq!(
            check_one(volume(bad, "/data")),
            Err(ManifestError::VolumeHostPath {
                path: bad.to_owned(),
                why
            }),
            "{bad}"
        );
    }
    assert_eq!(
        check_one(volume("/var/lib/web", "data")),
        Err(ManifestError::VolumeContainerPath {
            path: "data".to_owned(),
            why: PathError::NotAbsolute
        })
    );
    assert_eq!(
        check_one(volume("/var/lib/api", "/data"))
            .expect_err("refused")
            .to_string(),
        "the volume host path \"/var/lib/api\" is not /var/lib/web or below it"
    );
    assert_eq!(
        check_one(volume("/var/lib/web/../a", "/d"))
            .expect_err("refused")
            .to_string(),
        "the volume host path \"/var/lib/web/../a\" has a `.` or `..` component"
    );
    assert_eq!(
        check_one(volume("/var/lib/web", "d"))
            .expect_err("refused")
            .to_string(),
        "the volume container path \"d\" is not absolute"
    );
}
