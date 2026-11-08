//! The `mdm` package's shared texts: the settings it fixes, the drop-ins of the example site,
//! the manifest, and its units held to the kit's templates.

mod common;

use common::{example, read, site_addresses};
use hostkeeper_mdm_core::acme::Acme;
use hostkeeper_mdm_core::config::MdmConfig;
use hostkeeper_mdm_core::package::{
    self, FIXED, HEALTH_URL, NANOHUB_PORT, PackageError, RUN, UNITS, drop_ins, manifest, settings,
};
use hostkeeper_mdm_core::render::{render, render_tls, strip_comments};
use hostkeeper_mdm_core::settings::SettingsError;

/// The example with every key the package fixes left out.
fn package_example() -> String {
    example()
        .lines()
        .filter(|l| !FIXED.iter().any(|(k, _)| l.starts_with(&format!("{k}="))))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// The example read as the package reads it.
fn package_config() -> (MdmConfig, Acme) {
    let text = settings(&package_example()).expect("the package example fits");
    (
        MdmConfig::parse(&text).expect("parses"),
        Acme::parse(&text).expect("parses"),
    )
}

/// Catches a fixed key that is not filled in, a site's own key that is changed, or a value
/// the parser cannot read: a file that leaves the fixed keys out reads with every one of them
/// at the package's value, and every other setting as the file gives it.
#[test]
fn a_settings_file_without_the_fixed_keys_reads_with_the_packages_values() {
    let (c, acme) = package_config();
    assert_eq!(c.user(), "mdm");
    assert_eq!(c.state().as_str(), "/var/lib/mdm");
    assert_eq!(c.etc().as_str(), "/etc/mdm");
    assert_eq!(
        c.bin().as_str(),
        "/var/lib/hostkeeper/packages/mdm/current/bin"
    );
    assert_eq!(c.pkg_dir().as_str(), "/var/lib/mdm-pkg");
    assert_eq!((c.unit_prefix(), c.slice()), ("mdm", "mdm.slice"));
    assert_eq!(
        (c.nanohub_port(), c.scep_port(), c.dep_port()),
        (9004, 9005, 9001)
    );
    assert_eq!(c.dns_name(), "mdm.example.com");
    assert_eq!(c.dep_name(), "example");
    assert_eq!(acme.email, "admin@example.com");
}

/// Catches the overlay giving a fixed key twice, refusing the package's own value, or taking
/// another value: a file that sets each fixed key to the package's value (quoted, too) reads
/// as one that leaves it out; one that sets any of them to another value is refused naming the
/// key, both values and what to do; a file that is not a settings file is refused as such; a
/// file without a final line break still reads.
#[test]
fn a_fixed_key_may_be_given_only_with_the_packages_value() {
    let mut same = package_example();
    for (key, value) in FIXED {
        same.push_str(&format!("{key}=\"{value}\"\n"));
    }
    assert_eq!(
        MdmConfig::parse(&settings(&same).unwrap()).unwrap(),
        package_config().0
    );
    for (key, fixed) in FIXED {
        let text = format!("{}{key}=/elsewhere\n", package_example());
        assert_eq!(
            settings(&text),
            Err(PackageError::Fixed {
                key,
                given: "/elsewhere".to_owned(),
                fixed,
            }),
            "{key}"
        );
    }
    assert_eq!(
        settings("MDM_STATE=/var/lib/mdm\nMDM_STATE=/var/lib/mdm\n")
            .unwrap_err()
            .to_string(),
        "the settings file: line 2: MDM_STATE is already set on line 1"
    );
    assert_eq!(
        settings("NOT_A_KEY=1\n"),
        Err(PackageError::Settings(SettingsError::Unknown {
            line: 1,
            key: "NOT_A_KEY".to_owned()
        }))
    );
    let unterminated = package_example().trim_end().to_owned();
    assert_eq!(
        MdmConfig::parse(&settings(&unterminated).unwrap()).unwrap(),
        package_config().0
    );
    assert_eq!(
        settings("").unwrap(),
        FIXED
            .iter()
            .map(|(k, v)| format!("{k}={v}\n"))
            .collect::<String>()
    );
}

/// Catches a drop-in that does not carry the site's value, carries it under another name than
/// the unit reads, or a change in their wording: the four drop-ins of the example site are
/// their committed bytes.
#[test]
fn the_drop_ins_of_the_example_site_are_the_golden_bytes() {
    let (c, acme) = package_config();
    let got: Vec<(String, String)> = drop_ins(&c, &acme)
        .into_iter()
        .map(|u| (u.name, u.text))
        .collect();
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "mdm.slice.d/settings.conf",
            "mdm-proxy.service.d/settings.conf",
            "mdm-depsyncer.service.d/settings.conf",
            "mdm-cert.service.d/settings.conf",
        ]
    );
    for (name, text) in got {
        let file = name.replace(".d/settings.conf", ".conf");
        assert_eq!(
            text,
            site_addresses(&read(&format!("golden/package/{file}"))),
            "{name}"
        );
    }
}

/// Catches a variable a unit reads that no drop-in sets, or one a drop-in sets that no unit
/// reads: the `${...}` words of the units' command lines are exactly the `Environment=`
/// names of their drop-ins.
#[test]
fn every_variable_a_unit_reads_is_set_by_its_drop_in() {
    let (c, acme) = package_config();
    let drops = drop_ins(&c, &acme);
    for unit in UNITS {
        let mut read: Vec<&str> = unit
            .text
            .split("${")
            .skip(1)
            .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
            .collect();
        read.sort_unstable();
        let set: Vec<&str> = drops
            .iter()
            .filter(|d| d.name == format!("{}.d/settings.conf", unit.name))
            .flat_map(|d| d.text.lines())
            .filter_map(|l| l.strip_prefix("Environment="))
            .filter_map(|l| l.split_once('=').map(|(name, _)| name))
            .collect();
        let mut set = set;
        set.sort_unstable();
        assert_eq!(read, set, "{}", unit.name);
    }
}

/// Catches the manifest losing a field the daemon needs or the hook reads, or one of the
/// shared names drifting from it: the manifest of a version is its committed bytes, its run
/// units are files of the package, and its health URL names NanoHUB's fixed port.
#[test]
fn the_manifest_is_the_golden_bytes_and_names_the_packages_units() {
    assert_eq!(manifest("1.2.3"), read("golden/package/manifest.toml"));
    for unit in RUN {
        assert!(UNITS.iter().any(|u| u.name == unit), "{unit}");
    }
    assert!(HEALTH_URL.contains(&format!(":{NANOHUB_PORT}/")));
    assert!(
        FIXED.contains(&("MDM_NANOHUB_PORT", NANOHUB_PORT.to_string().as_str())),
        "the health URL's port is the port the package fixes"
    );
    let mut names: Vec<&str> = UNITS.iter().map(|u| u.name).collect();
    let sorted = {
        let mut s = names.clone();
        s.sort_unstable();
        s
    };
    assert_eq!(names, sorted, "UNITS is in the order of the names");
    names.dedup();
    assert_eq!(names.len(), UNITS.len());
}

/// The lines of `text` without comments.
fn lines(text: &str) -> Vec<String> {
    strip_comments(text).lines().map(str::to_owned).collect()
}

/// Catches a package unit that drifts from the kit's unit it replaces (a hardening line lost,
/// a path or port changed): each package unit, its comments aside, is the kit's template
/// rendered with the package's settings but for the lines listed here, each a difference the
/// package makes on purpose.
#[test]
fn the_package_units_are_the_kits_units_on_the_packages_layout() {
    let (c, acme) = package_config();
    let mut kit = render(&c).expect("renders").units;
    kit.extend(render_tls(&c, &acme).expect("renders"));
    let lan = c.lan().to_string();
    let bin = package::BIN;
    // (unit, the kit's lines the package leaves out, the package's lines the kit has not)
    let differences: [(&str, Vec<String>, Vec<String>); 7] = [
        ("mdm-scep.service", vec![], vec![]),
        ("mdm-nanohub.service", vec![], vec![]),
        ("mdm-depserver.service", vec![], vec![]),
        (
            "mdm-depsyncer.service",
            vec![format!(
                "ExecStart={bin}/depsyncer -storage filekv -storage-dsn /var/lib/mdm/dep -duration 1800 example"
            )],
            vec![format!(
                "ExecStart={bin}/depsyncer -storage filekv -storage-dsn /var/lib/mdm/dep -duration 1800 ${{MDM_DEP_NAME}}"
            )],
        ),
        (
            "mdm-proxy.service",
            vec![format!("IPAddressAllow=localhost {lan}")],
            vec![
                "ConditionPathExistsGlob=/var/lib/mdm/lego/certificates/*.crt".to_owned(),
                "IPAddressAllow=localhost".to_owned(),
            ],
        ),
        (
            "mdm-cert.service",
            vec![
                lines(
                    &kit.iter()
                        .find(|u| u.name == "mdm-cert.service")
                        .unwrap()
                        .text,
                )
                .into_iter()
                .find(|l| l.starts_with("ExecStart="))
                .unwrap(),
            ],
            vec![
                "ConditionPathExists=/etc/mdm/dns.env".to_owned(),
                format!(
                    "ExecStart={bin}/lego run --accept-tos --email ${{MDM_ACME_EMAIL}} --server ${{MDM_ACME_SERVER}} --path /var/lib/mdm/lego --key-type ec256 --dns ${{MDM_ACME_DNS_PROVIDER}} --domains ${{MDM_DNS_NAME}} --dns.resolvers ${{MDM_ACME_RESOLVERS}} --dns.propagation.wait ${{MDM_ACME_PROPAGATION_WAIT}}"
                ),
                "ExecStartPost=+/usr/bin/systemctl restart mdm-proxy.service".to_owned(),
            ],
        ),
        ("mdm-cert.timer", vec![], vec![]),
    ];
    for (name, kit_only, package_only) in differences {
        let ours = UNITS.iter().find(|u| u.name == name).expect(name);
        let theirs = kit.iter().find(|u| u.name == name).expect(name);
        let ours: Vec<String> = lines(ours.text)
            .into_iter()
            .filter(|l| !package_only.contains(l))
            .collect();
        let theirs: Vec<String> = lines(&theirs.text)
            .into_iter()
            .filter(|l| !kit_only.contains(l))
            .collect();
        assert_eq!(ours, theirs, "{name}");
        for line in package_only {
            assert!(
                lines(UNITS.iter().find(|u| u.name == name).unwrap().text).contains(&line),
                "{name}: {line}"
            );
        }
    }
}
