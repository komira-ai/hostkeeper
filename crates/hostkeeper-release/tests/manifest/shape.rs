//! A manifest's size, its counts, `min_hostkeeper`, its params and its health URL: the rules
//! every manifest keeps (`manifest::check`).

use hostkeeper_config::manifest::HealthUrlError;
use hostkeeper_proto::hostkeeper::release::v1::{Container, Manifest, Param};
use hostkeeper_release::manifest::{self, ManifestError};
use hostkeeper_release::{
    Installer, MAX_CONTAINERS, MAX_MANIFEST_BYTES, MAX_PARAMS, MAX_PLACE, MAX_RUN,
};
use prost::Message as _;

use crate::support::{container, manifest, pad_manifest_to, place, text, web};

fn check(manifest: &Manifest) -> Result<(), ManifestError> {
    manifest::check(&web(), manifest)
}

/// Catches: a rule that refuses a well-formed manifest of some installer.
#[test]
fn the_manifest_of_every_installer_passes_its_checks() {
    for installer in Installer::ALL {
        let manifest = manifest(installer);
        assert_eq!(check(&manifest), Ok(()), "{installer}");
        assert_eq!(
            manifest::check_for(&web(), installer, &manifest),
            Ok(()),
            "{installer}"
        );
    }
}

/// Catches: a size limit off by one, or measured on something other than the encoding.
#[test]
fn a_manifest_of_16_kib_passes_and_one_byte_more_is_refused() {
    let mut at_limit = manifest(Installer::Tarball);
    pad_manifest_to(&mut at_limit, MAX_MANIFEST_BYTES);
    assert_eq!(at_limit.encoded_len(), 16 * 1024);
    assert_eq!(check(&at_limit), Ok(()));

    let mut over = manifest(Installer::Tarball);
    pad_manifest_to(&mut over, MAX_MANIFEST_BYTES + 1);
    assert_eq!(check(&over), Err(ManifestError::TooLarge { bytes: 16_385 }));
    assert_eq!(
        check(&over).expect_err("refused").to_string(),
        "the manifest is 16385 bytes encoded, more than 16384"
    );
}

fn runs(count: usize) -> Vec<String> {
    (0..count).map(|i| format!("web-{i}.service")).collect()
}

/// Catches: a count limit off by one, or not applied to one of the four fields.
#[test]
fn each_count_limit_takes_its_limit_and_refuses_one_more() {
    let mut run = manifest(Installer::Tarball);
    run.run = runs(MAX_RUN);
    assert_eq!(check(&run), Ok(()));
    run.run = runs(MAX_RUN + 1);
    assert_eq!(
        check(&run),
        Err(ManifestError::TooMany {
            field: "run",
            count: 65,
            limit: 64
        })
    );
    assert_eq!(
        check(&run).expect_err("refused").to_string(),
        "the manifest has 65 run entries, more than 64"
    );

    let mut places = manifest(Installer::Tarball);
    let files = |count: usize| {
        (0..count)
            .map(|i| place("file", &format!("f/{i}"), &format!("/opt/web/{i}")))
            .collect::<Vec<_>>()
    };
    places.place = files(MAX_PLACE);
    assert_eq!(check(&places), Ok(()));
    places.place = files(MAX_PLACE + 1);
    assert_eq!(
        check(&places),
        Err(ManifestError::TooMany {
            field: "place",
            count: 257,
            limit: 256
        })
    );

    let mut params = manifest(Installer::Tarball);
    params.params = (0..MAX_PARAMS)
        .map(|i| (format!("p{i}"), text("v")))
        .collect();
    assert_eq!(check(&params), Ok(()));
    params.params.insert("one-more".to_owned(), text("v"));
    assert_eq!(
        check(&params),
        Err(ManifestError::TooMany {
            field: "params",
            count: 65,
            limit: 64
        })
    );

    let mut image = manifest(Installer::OciImage);
    image.run = (0..MAX_CONTAINERS)
        .map(|i| format!("web-c{i}.service"))
        .collect();
    image.containers = image
        .run
        .iter()
        .map(|unit| Container {
            unit: unit.clone(),
            ..container()
        })
        .collect();
    assert_eq!(check(&image), Ok(()));
    image.containers.push(Container {
        unit: "web-c32.service".to_owned(),
        ..container()
    });
    assert_eq!(
        check(&image),
        Err(ManifestError::TooMany {
            field: "containers",
            count: 33,
            limit: 32
        })
    );
}

/// Catches: a `min_hostkeeper` that is not a version passing, or one that is passing as absent.
#[test]
fn a_min_hostkeeper_that_is_not_major_minor_patch_is_refused() {
    for bad in ["", "1.2", "1.2.3-rc1", "01.2.3", "v1.2.3"] {
        let mut m = manifest(Installer::HostkeeperArchive);
        m.min_hostkeeper = bad.to_owned();
        let err = check(&m).expect_err(bad);
        assert!(matches!(err, ManifestError::MinHostkeeper(_)), "{bad}");
        assert_eq!(
            err.to_string(),
            format!(
                "min_hostkeeper {bad:?} is not major.minor.patch: three whole numbers without leading zeros or a suffix"
            )
        );
    }
    let m = manifest(Installer::HostkeeperArchive);
    assert_eq!(
        manifest::min_hostkeeper(&m).expect("a version").to_string(),
        "0.4.0"
    );
}

/// Catches: a key of the daemon's namespace passing, a key merely beginning with the word
/// refused, or a param with no value passing.
#[test]
fn a_param_in_the_updater_namespace_or_without_a_value_is_refused() {
    for key in ["updater", "updater.staged_release", "updater."] {
        let mut m = manifest(Installer::Tarball);
        m.params.insert(key.to_owned(), text("x"));
        assert_eq!(
            check(&m),
            Err(ManifestError::ReservedParam(key.to_owned())),
            "{key}"
        );
    }
    let mut beside = manifest(Installer::Tarball);
    beside.params.insert("updaters".to_owned(), text("x"));
    beside.params.insert("my.updater".to_owned(), text("x"));
    assert_eq!(check(&beside), Ok(()));

    let mut empty = manifest(Installer::Tarball);
    empty
        .params
        .insert("unset".to_owned(), Param { value: None });
    assert_eq!(
        check(&empty),
        Err(ManifestError::EmptyParam("unset".to_owned()))
    );
    assert_eq!(
        check(&empty).expect_err("refused").to_string(),
        "the params key \"unset\" holds no value: a string, an integer, a float or a boolean"
    );
    let mut reserved = manifest(Installer::Tarball);
    reserved.params.insert("updater".to_owned(), text("x"));
    assert_eq!(
        check(&reserved).expect_err("refused").to_string(),
        "the params key \"updater\" is in the namespace `updater.`, which only the daemon writes"
    );
}

fn with_url(url: &str, run: &[&str]) -> Manifest {
    let mut m = manifest(Installer::Tarball);
    m.health_url = Some(url.to_owned());
    m.run = run.iter().map(|unit| (*unit).to_owned()).collect();
    m
}

/// Catches: a health URL to another host, by another scheme, or not a URL, passing; a loopback
/// spelling refused; a `{port}` passing with a run unit that has no port.
#[test]
fn a_health_url_must_be_http_to_a_loopback_literal() {
    for url in [
        "http://127.0.0.1:8080/healthz",
        "http://[::1]:8080/healthz",
        "http://[0:0:0:0:0:0:0:1]/",
        "http://127.0.0.1:{port}/healthz",
    ] {
        assert_eq!(
            check(&with_url(url, &["web@8080.service"])),
            Ok(()),
            "{url}"
        );
    }
    for url in [
        "http://localhost:8080/",
        "http://192.0.2.10:8080/",
        "https://127.0.0.1:8080/",
        "http://127.0.0.2/",
    ] {
        assert_eq!(
            check(&with_url(url, &["web.service"])),
            Err(ManifestError::HealthUrl {
                url: url.to_owned(),
                why: HealthUrlError::NotLoopback
            }),
            "{url}"
        );
    }
    let err = check(&with_url("127.0.0.1:8080", &[])).expect_err("refused");
    assert_eq!(
        err.to_string(),
        "health_url \"127.0.0.1:8080\" has no `://`"
    );
    assert_eq!(
        check(&with_url("http://localhost/", &[]))
            .expect_err("refused")
            .to_string(),
        "health_url \"http://localhost/\" is not an http:// URL to a loopback address (127.0.0.1 or [::1])"
    );

    let placeholder = "http://127.0.0.1:{port}/healthz";
    assert_eq!(
        check(&with_url(placeholder, &["web@9301.service", "web.service"])),
        Err(ManifestError::HealthUrl {
            url: placeholder.to_owned(),
            why: HealthUrlError::PortWithoutInstance {
                unit: "web.service".to_owned()
            }
        })
    );
    assert_eq!(check(&with_url(placeholder, &[])), Ok(()));
}
