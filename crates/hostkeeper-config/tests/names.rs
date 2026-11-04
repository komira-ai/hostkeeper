//! The grammars of names and paths. The path cases are those of `hostkeeper-rights` before the
//! move; the service-name cases are the wider grammar of design section 11.8.

use hostkeeper_config::names::{
    MAX_PATH, NameError, PathError, ServiceName, UnitError, UnitName, Version, check_path,
    check_relative, is_service_name, is_version, is_within,
};

/// Catches a service name outside design 11.8: `A-Z`, `a-z`, `0-9`, `_` and `-`, 1 to 64 bytes.
/// Upper case and `_` are new: the grammar before allowed lowercase, digits and `-` only.
#[test]
fn service_names() {
    for good in [
        "a",
        "tierstore",
        "js-agent",
        "k9",
        "Tier",
        "a_b",
        "A-Z_0",
        &"a".repeat(64),
    ] {
        assert!(is_service_name(good), "{good}");
        assert_eq!(ServiceName::new(good).unwrap().as_str(), good);
    }
    for bad in ["", "a b", "a.b", "a/b", "a@b", "é", &"a".repeat(65)] {
        assert!(!is_service_name(bad), "{bad}");
        assert_eq!(
            ServiceName::new(bad),
            Err(NameError::Service(bad.to_owned()))
        );
    }
    assert_eq!(ServiceName::new("web").unwrap().to_string(), "web");
}

/// Catches a version outside design 11.8: 1 to 128 bytes of `A-Za-z0-9._+-`, no leading dot.
#[test]
fn versions() {
    for good in [
        "1",
        "1.5.0",
        "1.5.0-rc.1+build_7",
        "v2",
        "0.",
        &"9".repeat(128),
    ] {
        assert!(is_version(good), "{good}");
        assert_eq!(Version::new(good).unwrap().as_str(), good);
    }
    for bad in ["", ".1", "..", "1 0", "1/0", "1:0", "1~0", &"9".repeat(129)] {
        assert!(!is_version(bad), "{bad}");
        assert_eq!(Version::new(bad), Err(NameError::Version(bad.to_owned())));
    }
    assert_eq!(Version::new("1.5.0").unwrap().to_string(), "1.5.0");
    assert!(
        NameError::Version("x".into())
            .to_string()
            .contains("version")
    );
    assert!(
        NameError::Service("x".into())
            .to_string()
            .contains("service name")
    );
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
        ("tierstore.target", UnitError::NotAUnit),
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
    ];
    assert!(all.iter().all(|text| !text.is_empty()));
}
