//! The grammars of names and paths. The path cases are those imported from kbf's `update-rights`
//! (MIGRATION.md); the service-name and version cases are the rules of design section 11.8.

use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::{
    MAX_NAME, MAX_PATH, MAX_UNIT_NAME, NameError, PathError, SERVICE_NAME_RULE, ServiceName,
    UnitError, UnitName, VERSION_RULE, Version, check_path, check_relative, is_service_name,
    is_version, is_within,
};

/// Strings both rules of design 11.8 accept: a service name and a version alike.
fn accepted_by_both() -> Vec<String> {
    [
        "tierstore",
        "Tierstore_2",
        "kbf-proxy",
        "0.3.2",
        "v1.2",
        "1.0.0-rc.1",
        "_a",
        "a",
        "a.",
    ]
    .into_iter()
    .map(String::from)
    .chain([
        "a".repeat(MAX_NAME),
        format!("1.{}", "0".repeat(MAX_NAME - 2)),
    ])
    .collect()
}

/// Strings both rules refuse: the empty string, too long, a leading `-` or `.`, a path
/// separator, white space, a control character, a non-ASCII letter and the characters bash gives
/// a meaning.
fn refused_by_both() -> Vec<String> {
    [
        "",
        "-x",
        ".hidden",
        ".",
        "..",
        "a/b",
        "a b",
        "1.0~rc1",
        "a$b",
        "a*b",
        "a?b",
        "a\nb",
        "a\tb",
        "a\0b",
        "caf\u{e9}",
        "\u{e9}",
        "a\"b",
        "a'b",
        "a:b",
        "a@b",
        "~a",
    ]
    .into_iter()
    .map(String::from)
    .chain([
        "a".repeat(MAX_NAME + 1),
        format!("+{}", "a".repeat(MAX_NAME)),
    ])
    .collect()
}

/// Strings with a `+`: a version, never a service name (a `+` cannot be in a systemd unit name).
const PLUS: [&str; 4] = ["1.4.0+g1a2b3c", "a+b", "+a", "a+"];

/// Catches a service name or a version outside design 11.8: the POSIX portable filename
/// characters, 1 to 255 bytes, not beginning with `-` or `.`, and `+` in a version only. The
/// checks and the constructors answer each string alike.
#[test]
fn service_names_and_versions_keep_their_rules() {
    assert_eq!(MAX_NAME, 255);
    for good in accepted_by_both() {
        assert!(is_service_name(&good), "{good:?}");
        assert!(is_version(&good), "{good:?}");
        assert_eq!(ServiceName::new(&good).unwrap().as_str(), good);
        assert_eq!(Version::new(&good).unwrap().as_str(), good);
    }
    for bad in refused_by_both() {
        assert!(!is_service_name(&bad), "{bad:?}");
        assert!(!is_version(&bad), "{bad:?}");
        assert_eq!(ServiceName::new(&bad), Err(NameError::Service(bad.clone())));
        assert_eq!(Version::new(&bad), Err(NameError::Version(bad.clone())));
    }
    for plus in PLUS {
        assert!(!is_service_name(plus), "{plus:?}");
        assert_eq!(
            ServiceName::new(plus),
            Err(NameError::Service(plus.to_owned()))
        );
        assert!(is_version(plus), "{plus:?}");
        assert_eq!(Version::new(plus).unwrap().as_str(), plus);
    }
}

/// Catches a refusal that does not state its rule, and a name that does not print as itself.
#[test]
fn a_refused_name_states_the_rule() {
    assert_eq!(
        ServiceName::new("a+b").unwrap_err().to_string(),
        "\"a+b\" is not a service name: 1 to 255 bytes of A-Z, a-z, 0-9, `.`, `_` and `-`, \
         not beginning with `-` or `.`"
    );
    assert_eq!(
        Version::new("-1").unwrap_err().to_string(),
        "\"-1\" is not a version: 1 to 255 bytes of A-Z, a-z, 0-9, `.`, `_`, `+` and `-`, \
         not beginning with `-` or `.`"
    );
    assert_eq!(ServiceName::new("web").unwrap().to_string(), "web");
    assert_eq!(Version::new("1.5.0").unwrap().to_string(), "1.5.0");
    assert!(SERVICE_NAME_RULE.contains(&format!("1 to {MAX_NAME} bytes")));
    assert!(VERSION_RULE.contains(&format!("1 to {MAX_NAME} bytes")));
}

/// Catches a path that systemd would read as something else: a `-`, `+` or `!` prefix, a
/// relative path, white space, a specifier, an escape, a `..`.
#[test]
fn paths_refused_and_accepted() {
    for good in [
        "/",
        "/a",
        "/usr/sbin/nft",
        "/var/lib/x-y_z.d/a@b+c",
        "/a/b.c",
    ] {
        assert_eq!(check_path(good), Ok(()), "{good}");
    }
    assert_eq!(check_path("-/a"), Err(PathError::SystemdPrefix('-')));
    assert_eq!(check_path("+/a"), Err(PathError::SystemdPrefix('+')));
    assert_eq!(check_path("!/a"), Err(PathError::SystemdPrefix('!')));
    assert_eq!(check_path("a/b"), Err(PathError::NotAbsolute));
    assert_eq!(check_path(""), Err(PathError::NotAbsolute));
    assert_eq!(check_path("./a"), Err(PathError::NotAbsolute));
    assert_eq!(check_path("/a b"), Err(PathError::BadChar(' ')));
    assert_eq!(check_path("/a%i"), Err(PathError::BadChar('%')));
    assert_eq!(check_path("/a\\x20"), Err(PathError::BadChar('\\')));
    assert_eq!(check_path("/a:b"), Err(PathError::BadChar(':')));
    assert_eq!(check_path("/a\n"), Err(PathError::BadChar('\n')));
    assert_eq!(check_path("/é"), Err(PathError::BadChar('é')));
    assert_eq!(check_path("/a//b"), Err(PathError::EmptyComponent));
    assert_eq!(check_path("/a/"), Err(PathError::EmptyComponent));
    assert_eq!(check_path("//a"), Err(PathError::EmptyComponent));
    assert_eq!(check_path("/a/../b"), Err(PathError::DotComponent));
    assert_eq!(check_path("/.."), Err(PathError::DotComponent));
    assert_eq!(check_path("/a/./b"), Err(PathError::DotComponent));
    let long = format!("/{}", "a".repeat(MAX_PATH));
    assert_eq!(check_path(&long), Err(PathError::TooLong));
    assert_eq!(check_path(&long[..MAX_PATH]), Ok(()));
}

/// Catches a path within a package that escapes it or is empty.
#[test]
fn relative_paths() {
    assert_eq!(check_relative("tierstore"), Ok(()));
    assert_eq!(check_relative("bin/tierstore"), Ok(()));
    assert_eq!(check_relative("-x"), Ok(()));
    assert_eq!(check_relative(""), Err(PathError::Empty));
    assert_eq!(check_relative("../x"), Err(PathError::DotComponent));
    assert_eq!(check_relative("a/../x"), Err(PathError::DotComponent));
    assert_eq!(check_relative("/x"), Err(PathError::EmptyComponent));
    assert_eq!(check_relative("x/"), Err(PathError::EmptyComponent));
    assert_eq!(check_relative("a b"), Err(PathError::BadChar(' ')));
}

/// Catches a prefix compared on characters rather than whole components: `/usr2/x` is not below
/// `/usr`.
#[test]
fn within_is_by_whole_components() {
    assert!(is_within("/usr", "/usr"));
    assert!(is_within("/usr/bin/x", "/usr"));
    assert!(is_within("/usr/bin", "/usr/"));
    assert!(!is_within("/usr2/x", "/usr"));
    assert!(!is_within("/us", "/usr"));
    assert!(!is_within("/var/libx", "/var/lib"));
}

/// Catches a unit that is not the service's own: a system unit, a unit of another service whose
/// name merely begins the same, a bad instance, a kind a package may not carry; and a template
/// accepted where only a startable unit may be.
#[test]
fn units_belong_to_the_service() {
    for good in [
        "tierstore.service",
        "tierstore@9301.service",
        "tierstore-extra.service",
        "tierstore-extra@a_b.c-d.service",
        "tierstore-X.service",
        "tierstore.socket",
        "tierstore-gc.timer",
    ] {
        assert!(UnitName::of("tierstore", good, false).is_ok(), "{good}");
    }
    for (unit, error) in [
        ("tierstore", UnitError::NotAUnit),
        ("tierstore.mount", UnitError::NotAUnit),
        ("sshd.service", UnitError::NotTheServices),
        ("tierstorex.service", UnitError::NotTheServices),
        ("tier.service", UnitError::NotTheServices),
        ("tierstore_x.service", UnitError::NotTheServices),
        ("tierstore-a b.service", UnitError::NotTheServices),
        ("xtierstore.service", UnitError::NotTheServices),
        (".service", UnitError::NotTheServices),
        ("tierstore@a b.service", UnitError::BadInstance),
        ("tierstore@a/b.service", UnitError::BadInstance),
        ("tierstore@%i.service", UnitError::BadInstance),
        ("tierstore@.service", UnitError::BadInstance),
    ] {
        assert_eq!(UnitName::of("tierstore", unit, false), Err(error), "{unit}");
    }
    // Built from its parts: the name written whole reads as a host name.
    let target = ["tierstore", "target"].join(".");
    assert_eq!(
        UnitName::of("tierstore", &target, false),
        Err(UnitError::NotAUnit)
    );
    let template = UnitName::of("tierstore", "tierstore@.service", true).unwrap();
    assert!(template.is_template());
    assert_eq!(template.template(), None);
    let long = format!("tierstore@{}.service", "a".repeat(65));
    assert_eq!(
        UnitName::of("tierstore", &long, true),
        Err(UnitError::BadInstance)
    );
    let longest = format!("tierstore@{}.service", "a".repeat(64));
    assert!(UnitName::of("tierstore", &longest, false).is_ok());
}

/// Catches a unit name systemd cannot load passing for a service whose name keeps its rule: a
/// unit name is at most 255 bytes, `.service` included, so a service name of 247 bytes still has
/// its `<service>.service` and one of 248 bytes, or 255, is refused with its own error.
#[test]
fn a_unit_name_longer_than_systemd_takes_is_refused() {
    assert_eq!(MAX_UNIT_NAME, 255);
    let fits = "a".repeat(247);
    let unit = format!("{fits}.service");
    assert_eq!(unit.len(), 255);
    assert!(UnitName::of(&fits, &unit, false).is_ok());
    for service in ["a".repeat(248), "a".repeat(MAX_NAME)] {
        assert!(is_service_name(&service));
        let unit = format!("{service}.service");
        assert_eq!(
            UnitName::of(&service, &unit, false),
            Err(UnitError::TooLong),
            "{}",
            unit.len()
        );
    }
    let instance = format!("tierstore-{}@1.service", "a".repeat(236));
    assert_eq!(instance.len(), 256);
    assert_eq!(
        UnitName::of("tierstore", &instance, false),
        Err(UnitError::TooLong)
    );
    assert_eq!(
        UnitError::TooLong.to_string(),
        "is longer than 255 bytes, the most systemd takes for a unit name"
    );
}

/// Catches an instance read as a port when it is not one, and the template of an instance
/// named wrongly.
#[test]
fn instances_templates_and_ports() {
    let instance = UnitName::of("tierstore", "tierstore@9301.service", false).unwrap();
    assert_eq!(
        instance,
        UnitName {
            base: "tierstore",
            instance: Some("9301"),
            suffix: ".service"
        }
    );
    assert!(!instance.is_template());
    assert_eq!(instance.template().as_deref(), Some("tierstore@.service"));
    assert_eq!(instance.port(), Some(9301));
    let socket = UnitName::of("tierstore", "tierstore-a@x.socket", false).unwrap();
    assert_eq!(socket.template().as_deref(), Some("tierstore-a@.socket"));
    assert_eq!(socket.port(), None);
    let plain = UnitName::of("tierstore", "tierstore.service", false).unwrap();
    assert_eq!((plain.template(), plain.port()), (None, None));
    for not_a_port in ["0", "65536", "99999999", "1.2", "-1"] {
        let unit = format!("tierstore@{not_a_port}.service");
        let name = UnitName::of("tierstore", &unit, false).unwrap();
        assert_eq!(name.port(), None, "{unit}");
    }
    let top = UnitName::of("tierstore", "tierstore@65535.service", false).unwrap();
    assert_eq!(top.port(), Some(65535));
}

/// Every refusal says why, in words.
#[test]
fn errors_read_as_sentences() {
    let all = [
        PathError::SystemdPrefix('-').to_string(),
        PathError::NotAbsolute.to_string(),
        PathError::TooLong.to_string(),
        PathError::BadChar('%').to_string(),
        PathError::EmptyComponent.to_string(),
        PathError::DotComponent.to_string(),
        PathError::Empty.to_string(),
        UnitError::NotAUnit.to_string(),
        UnitError::NotTheServices.to_string(),
        UnitError::BadInstance.to_string(),
        UnitError::TooLong.to_string(),
    ];
    assert!(all.iter().all(|text| !text.is_empty()));
}

/// Catches a version of hostkeeper that the heartbeat names in another spelling, or that the
/// version grammar would refuse: the conversion keeps `major.minor.patch` as written, the
/// largest numbers included.
#[test]
fn a_hostkeeper_version_is_the_version_of_its_own_text() {
    for own in [
        HostkeeperVersion {
            major: 0,
            minor: 1,
            patch: 0,
        },
        HostkeeperVersion {
            major: u64::MAX,
            minor: u64::MAX,
            patch: u64::MAX,
        },
    ] {
        let version = Version::from(own);
        assert_eq!(version.as_str(), own.to_string());
        assert_eq!(Version::new(version.as_str()), Ok(version));
    }
}
