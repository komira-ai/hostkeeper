use super::*;
use crate::release::doc_tests::{DOC, documented_names};

const BASELINE: &str = include_str!("../../../tests/release/asset_names.baseline");

/// What stands for the version in the baseline.
const PLACEHOLDER: &str = "<version>";

/// The asset names a release once held and no longer does, `<version>` standing for X.Y.Z, each
/// with why it left. A name leaves the list of `asset_names` only by being added here (and
/// marked `withdrawn: ` in the baseline) in the same pull request: consumers fetch
/// assets by name, so a name is withdrawn only when nothing fetches it.
const WITHDRAWN: [(&str, &str); 38] = [
    ("hostkeeper-<version>-1-aarch64.pkg.tar.zst", ARCH_WITHDRAWN),
    (
        "hostkeeper-<version>-1-aarch64.pkg.tar.zst.sigstore.json",
        ARCH_WITHDRAWN,
    ),
    ("hostkeeper-<version>-1-x86_64.pkg.tar.zst", ARCH_WITHDRAWN),
    (
        "hostkeeper-<version>-1-x86_64.pkg.tar.zst.sigstore.json",
        ARCH_WITHDRAWN,
    ),
    (
        "hello-<version>-fail-ready-linux-arm64.tar.zst",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hello-<version>-fail-ready-linux-arm64.tar.zst.sha256",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hello-<version>-fail-ready-linux-arm64.tar.zst.sigstore.json",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hello-<version>-fail-ready-linux-x86_64.tar.zst",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hello-<version>-fail-ready-linux-x86_64.tar.zst.sha256",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hello-<version>-fail-ready-linux-x86_64.tar.zst.sigstore.json",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hello-<version>-fail-ready-macos-arm64.tar.zst",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hello-<version>-fail-ready-macos-arm64.tar.zst.sha256",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hello-<version>-fail-ready-macos-arm64.tar.zst.sigstore.json",
        ARCHIVE_WITHDRAWN,
    ),
    ("hello-<version>-linux-arm64.tar.zst", ARCHIVE_WITHDRAWN),
    (
        "hello-<version>-linux-arm64.tar.zst.sha256",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hello-<version>-linux-arm64.tar.zst.sigstore.json",
        ARCHIVE_WITHDRAWN,
    ),
    ("hello-<version>-linux-x86_64.tar.zst", ARCHIVE_WITHDRAWN),
    (
        "hello-<version>-linux-x86_64.tar.zst.sha256",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hello-<version>-linux-x86_64.tar.zst.sigstore.json",
        ARCHIVE_WITHDRAWN,
    ),
    ("hello-<version>-macos-arm64.tar.zst", ARCHIVE_WITHDRAWN),
    (
        "hello-<version>-macos-arm64.tar.zst.sha256",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hello-<version>-macos-arm64.tar.zst.sigstore.json",
        ARCHIVE_WITHDRAWN,
    ),
    ("hello-fail-ready-index.json", INDEX_WITHDRAWN),
    ("hello-index.json", INDEX_WITHDRAWN),
    (
        "hostkeeper-<version>-linux-arm64.tar.zst",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hostkeeper-<version>-linux-arm64.tar.zst.sha256",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hostkeeper-<version>-linux-arm64.tar.zst.sigstore.json",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hostkeeper-<version>-linux-x86_64.tar.zst",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hostkeeper-<version>-linux-x86_64.tar.zst.sha256",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hostkeeper-<version>-linux-x86_64.tar.zst.sigstore.json",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hostkeeper-<version>-macos-arm64.tar.zst",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hostkeeper-<version>-macos-arm64.tar.zst.sha256",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "hostkeeper-<version>-macos-arm64.tar.zst.sigstore.json",
        ARCHIVE_WITHDRAWN,
    ),
    ("index.json", INDEX_WITHDRAWN),
    ("mdm-<version>-linux-x86_64.tar.zst", ARCHIVE_WITHDRAWN),
    (
        "mdm-<version>-linux-x86_64.tar.zst.sha256",
        ARCHIVE_WITHDRAWN,
    ),
    (
        "mdm-<version>-linux-x86_64.tar.zst.sigstore.json",
        ARCHIVE_WITHDRAWN,
    ),
    ("mdm-index.json", INDEX_WITHDRAWN),
];

/// Why the Arch Linux package and its bundle left the release.
const ARCH_WITHDRAWN: &str = "the Arch package is withdrawn (no daemon ever listed the arch installer, docs/design/packages-repositories.md section 0)";

/// Why the archive packages (`.tar.zst`), their digest files and their bundles left the release.
const ARCHIVE_WITHDRAWN: &str = "hostkeeper's own archive package is withdrawn: a host installs the .deb from the project's repository and the release sets native entries alone (docs/design/packages-native.md, item A28)";

/// Why the release indexes left the release.
const INDEX_WITHDRAWN: &str = "the release indexes are withdrawn with the archive packages they listed and the release channel that served them (docs/design/packages-native.md, item A28)";

/// What marks a withdrawn name in the baseline.
const WITHDRAWN_MARK: &str = "withdrawn: ";

fn commit(text: &str) -> Commit {
    text.parse().expect("a commit")
}

fn version(text: &str) -> Version {
    text.parse().expect("a version")
}

/// The names of the baseline, with `version` in place of the placeholder, sorted.
fn baseline_for(version: &Version) -> Vec<String> {
    let mut names: Vec<String> = BASELINE
        .lines()
        .filter(|l| !l.starts_with('#') & !l.trim().is_empty() & !l.starts_with(WITHDRAWN_MARK))
        .map(|l| l.replace(PLACEHOLDER, &version.to_string()))
        .collect();
    names.sort();
    names
}

/// The names the baseline marks withdrawn, with `version` in place of the placeholder, sorted.
fn withdrawn_in_baseline(version: &Version) -> Vec<String> {
    let mut names: Vec<String> = BASELINE
        .lines()
        .filter_map(|l| l.strip_prefix(WITHDRAWN_MARK))
        .map(|l| l.replace(PLACEHOLDER, &version.to_string()))
        .collect();
    names.sort();
    names
}

/// The names of [`WITHDRAWN`], with `version` in place of the placeholder, sorted.
fn withdrawn_for(version: &Version) -> Vec<String> {
    let mut names: Vec<String> = WITHDRAWN
        .iter()
        .map(|(n, _)| n.replace(PLACEHOLDER, &version.to_string()))
        .collect();
    names.sort();
    names
}

/// The names of `baseline` that `current` does not hold.
fn missing_from(baseline: &[String], current: &[String]) -> Vec<String> {
    baseline
        .iter()
        .filter(|n| !current.contains(n))
        .cloned()
        .collect()
}

#[test]
fn asset_names_are_the_documented_contract() {
    // Catches: a name added, removed or renamed in `asset_names` without the baseline file and
    // the list of every name in docs/deploy/verify-a-release.md changing with it, or either of
    // those two changing alone, or a platform added or dropped.
    let v = version("7.8.9");
    let names = asset_names(&v, &Platform::RELEASED);
    assert_eq!(names, baseline_for(&v));
    assert_eq!(names.len(), 58);
    assert_eq!(documented_names(DOC, &v), Ok(names.clone()));
    // The document changed alone: a name dropped, or one renamed.
    let image = "hostkeeper-coordinator-<version>-<linux-platform>.image.tar\n";
    for to in [
        "",
        "hostkeeper-coordinator-<version>-<linux-platform>.oci.tar\n",
    ] {
        assert_eq!(DOC.matches(image).count(), 1);
        let doc = DOC.replacen(image, to, 1);
        assert_ne!(documented_names(&doc, &v), Ok(names.clone()), "{to:?}");
    }
}

#[test]
fn asset_name_list_only_grows_but_for_the_withdrawn_names() {
    // Catches: a released name that the current list no longer holds, unless it is withdrawn
    // with its reason; a withdrawn name shipped again, or withdrawn in the baseline and not in
    // WITHDRAWN (or the other way round); adding a name is allowed.
    let v = version("0.1.4");
    let current = asset_names(&v, &Platform::RELEASED);
    assert_eq!(
        missing_from(&baseline_for(&v), &current),
        Vec::<String>::new()
    );
    let withdrawn = withdrawn_for(&v);
    assert_eq!(withdrawn_in_baseline(&v), withdrawn);
    assert!(
        withdrawn.iter().all(|n| !current.contains(n)),
        "{withdrawn:?}"
    );
    assert!(WITHDRAWN.iter().all(|(_, why)| !why.is_empty()));
    // A name whose removal is not in WITHDRAWN is reported: the Arch package of the baseline
    // before its withdrawal, against the current list.
    let mut before = baseline_for(&v);
    before.extend(withdrawn.iter().cloned());
    let unexplained: Vec<String> = missing_from(&before, &current)
        .into_iter()
        .filter(|n| !withdrawn.contains(n))
        .collect();
    assert_eq!(unexplained, Vec::<String>::new());
    assert_eq!(missing_from(&before, &current), withdrawn);
    // The check itself: a removed name and a renamed one are both reported, an added one is not.
    let mut changed = current.clone();
    changed.retain(|n| n != SUMS);
    changed.push("hostkeeper-0.1.4-linux-riscv64.tar.gz".to_owned());
    let renamed = "hostkeeper_0.1.4_amd64.deb";
    changed.retain(|n| n != renamed);
    changed.push("hostkeeper-0.1.4-amd64.deb".to_owned());
    assert_eq!(
        missing_from(&baseline_for(&v), &changed),
        vec![SUMS.to_owned(), renamed.to_owned()]
    );
}

#[test]
fn tag_and_version_string_formats_are_pinned() {
    // Catches: a tag that is not `v<X.Y.Z>`, a `--version` text other than
    // `hostkeeper X.Y.Z+<12 hex>` (`hostkeeper-coordinator ...`, `hostkeeper-mdm ...` and
    // `mdm ...`, the mdm package's program, for the other shipped binaries), a shipped binary dropped, or a change of the signer identity.
    let v = version("0.1.27");
    let c = commit("0123456789abcdef0123456789abcdef01234567");
    assert_eq!(v.tag(), "v0.1.27");
    assert_eq!(Version::of_tag("v0.1.27"), Some(v));
    assert_eq!(stamp_string(&v, &c), "hostkeeper 0.1.27+0123456789ab");
    assert_eq!(
        STAMPED.map(|p| stamp_of(p, &v, &c)),
        [
            "hostkeeper 0.1.27+0123456789ab",
            "hostkeeper-coordinator 0.1.27+0123456789ab",
            "hostkeeper-mdm 0.1.27+0123456789ab",
            "mdm 0.1.27+0123456789ab",
        ]
    );
    assert_eq!(RELEASE_COMMIT, "RELEASE_COMMIT");
    assert_eq!(
        SIGNER_WORKFLOW,
        "komira-ai/hostkeeper/.github/workflows/artifacts.yml"
    );
    assert_eq!(SIGNER_REF, "refs/heads/main");
    assert_eq!(REPO, "komira-ai/hostkeeper");
    assert_eq!(
        format!("{REPO}/.github/workflows/{ARTIFACTS_WORKFLOW}"),
        SIGNER_WORKFLOW
    );
}

#[test]
fn a_tag_names_a_version_only_in_the_exact_form() {
    // Catches: a tag with a prefix other than `v`, a leading zero, a suffix or a part too many
    // read as a release version.
    for tag in [
        "0.1.2",
        "v0.1",
        "v0.1.2.3",
        "v01.1.2",
        "V0.1.2",
        "v0.1.2-rc1",
        "v",
    ] {
        assert_eq!(Version::of_tag(tag), None, "{tag}");
    }
    assert_eq!(Version::of_tag("v0.0.0"), Some(version("0.0.0")));
}

#[test]
fn versions_parse_in_the_exact_form_and_order_by_number() {
    // Catches: a malformed or overflowing version accepted, or versions ordered as text.
    for bad in [
        "",
        "1",
        "1.2",
        "1.2.3.4.5",
        "1.02.3",
        "1.2.x",
        "1..3",
        " 1.2.3",
        "1.2.3\n",
    ] {
        assert_eq!(
            bad.parse::<Version>(),
            Err(BadValue::Version(bad.to_owned()))
        );
    }
    assert!(version("0.1.10") > version("0.1.9"));
    assert!(version("1.0.0") > version("0.99.99"));
    assert_eq!(version("10.20.30").to_string(), "10.20.30");
    assert_eq!(
        "18446744073709551616.0.0".parse::<Version>(),
        Err(BadValue::Version("18446744073709551616.0.0".to_owned()))
    );
}

#[test]
fn the_version_file_is_major_and_minor_with_one_optional_line_break() {
    // Catches: a version file with a patch, a prefix, spaces or extra lines accepted as `X.Y`.
    assert_eq!("0.1".parse::<Base>(), Ok(Base { major: 0, minor: 1 }));
    assert_eq!(
        "2.13\n".parse::<Base>(),
        Ok(Base {
            major: 2,
            minor: 13
        })
    );
    for bad in [
        "0.1\n\n", "0.1.0", "v0.1", " 0.1", "0.1 \n", "", "01.1", "0",
    ] {
        assert_eq!(bad.parse::<Base>(), Err(BadValue::Base(bad.to_owned())));
    }
    let base: Base = "3.4".parse().expect("base");
    assert_eq!(base.with_patch(5), version("3.4.5"));
}

#[test]
fn a_commit_is_forty_lowercase_hex_digits() {
    // Catches: a short, uppercase, non-hex or overlong commit accepted, or a stamp prefix that
    // is not its first 12 digits.
    let full = "abcdef0123456789abcdef0123456789abcdef01";
    assert_eq!(commit(full).as_str(), full);
    assert_eq!(commit(full).short(), "abcdef012345");
    assert_eq!(commit(full).to_string(), full);
    for bad in [
        "abcdef012345",
        "ABCDEF0123456789ABCDEF0123456789ABCDEF01",
        "abcdef0123456789abcdef0123456789abcdef0g",
        "abcdef0123456789abcdef0123456789abcdef012",
    ] {
        assert_eq!(bad.parse::<Commit>(), Err(BadValue::Commit(bad.to_owned())));
    }
}

#[test]
fn a_repository_is_owner_slash_name() {
    // Catches: a repository without an owner, a name, or with a part too many accepted.
    let repo: Repo = "komira-ai/hostkeeper".parse().expect("repo");
    assert_eq!(repo.as_str(), "komira-ai/hostkeeper");
    assert_eq!(repo.to_string(), "komira-ai/hostkeeper");
    for bad in [
        "hostkeeper",
        "/hostkeeper",
        "komira-ai/",
        "a/b/c",
        "a b/c",
        "",
    ] {
        assert_eq!(bad.parse::<Repo>(), Err(BadValue::Repo(bad.to_owned())));
    }
}

#[test]
fn the_attested_and_summed_names_partition_the_assets() {
    // Catches: a bundle listed in the sums, an attested asset without its bundle, or the sums
    // listing themselves.
    let v = version("0.1.3");
    let attested = attested_names(&v);
    let summed = summed_names(&v);
    assert_eq!(attested.len(), 25);
    assert_eq!(summed.len(), 32);
    // The boot sets of network install are summed and attested, with a bundle of their own, and
    // so is the command line, one script for every platform.
    assert!(summed.contains(&NETBOOT.to_owned()) & attested.contains(&NETBOOT.to_owned()));
    assert_eq!(hkctl_name(&v), "hkctl-0.1.3");
    assert!(summed.contains(&hkctl_name(&v)) & attested.contains(&hkctl_name(&v)));
    assert!(attested.iter().all(|a| summed.contains(a)));
    assert!(!summed.iter().any(|n| is_bundle(n) | (n == SUMS)));
    // The mdm package is attested for linux-x86_64 and is no asset for any other platform.
    assert!(attested.contains(&"mdm_0.1.3_amd64.deb".to_owned()));
    assert!(summed.contains(&"mdm_0.1.3_amd64.deb".to_owned()));
    for platform in [Platform::LinuxArm64, Platform::MacosArm64] {
        let name = Package::Mdm.native_name(&v, platform);
        assert_eq!(name, None);
    }
    assert!(attested.contains(&installer_name(&v)));
    assert_eq!(bundle_of("a.deb"), "a.deb.sigstore.json");
}

#[test]
fn there_are_exactly_three_platforms_and_a_release_ships_all_three() {
    // Catches: a fourth platform, a name read loosely, or a release that leaves one out (macOS
    // dropped from the release while its assets are still built).
    assert_eq!(
        Platform::RELEASED.map(Platform::name),
        ["linux-x86_64", "linux-arm64", "macos-arm64"]
    );
    assert_eq!(
        Platform::RELEASED,
        [
            Platform::LinuxX86_64,
            Platform::LinuxArm64,
            Platform::MacosArm64
        ]
    );
}

#[test]
fn a_macos_platform_has_a_binary_a_package_an_installer_and_an_sbom_but_no_linux_assets() {
    // Catches: a deb, an rpm, a coordinator, an MDM kit binary, an mdm package or an image named
    // for macOS (the MDM kit and the mdm package are Linux-only), the archive packages or release
    // indexes back in the list, or its installer pkg or hello's `.pkg` missing or not attested.
    let v = version("0.2.1");
    assert_eq!(
        asset_names(&v, &[Platform::MacosArm64]),
        [
            "SHA256SUMS",
            "hello-0.2.1-macos-arm64.pkg",
            "hello-0.2.1-macos-arm64.pkg.sigstore.json",
            "hkctl-0.2.1",
            "hkctl-0.2.1.sigstore.json",
            "hostkeeper-0.2.1-macos-arm64.cdx.json",
            "hostkeeper-0.2.1-macos-arm64.pkg",
            "hostkeeper-0.2.1-macos-arm64.pkg.sigstore.json",
            "hostkeeper-0.2.1-macos-arm64.tar.gz",
            "hostkeeper-0.2.1-macos-arm64.tar.gz.sigstore.json",
            "netboot.json",
            "netboot.json.sigstore.json",
        ]
    );
}

#[test]
fn the_native_packages_are_hostkeepers_and_hellos_on_every_platform_and_mdms_on_linux_x86_64_each_attested()
 {
    // Catches: a native package named other than the one `release deb`, `release macos-pkg` and
    // nfpm write, one shipped for a platform its package is not built for (mdm on arm64 or
    // macOS), hostkeeper's own native packages named other than the daemon's `.deb` and the
    // installer the release ships (its entry would pin a file the release does not have), hello's
    // failing build given a native name (its version is not X.Y.Z, which the shared repository's
    // index requires), a native package without its bundle or with a `.sha256` it does not have,
    // and an asset listed twice.
    let v = version("0.2.1");
    let names = |package: Package| -> Vec<Option<String>> {
        Platform::RELEASED
            .iter()
            .map(|p| package.native_name(&v, *p))
            .collect()
    };
    assert_eq!(
        names(Package::Hello),
        [
            Some("hello_0.2.1_amd64.deb".to_owned()),
            Some("hello_0.2.1_arm64.deb".to_owned()),
            Some("hello-0.2.1-macos-arm64.pkg".to_owned()),
        ]
    );
    assert_eq!(
        names(Package::Mdm),
        [Some("mdm_0.2.1_amd64.deb".to_owned()), None, None]
    );
    assert_eq!(
        names(Package::Hostkeeper),
        [
            Some("hostkeeper_0.2.1_amd64.deb".to_owned()),
            Some("hostkeeper_0.2.1_arm64.deb".to_owned()),
            Some(installer_name(&v)),
        ]
    );
    assert_eq!(installer_name(&v), "hostkeeper-0.2.1-macos-arm64.pkg");
    assert_eq!(names(Package::HelloFailReady), [None, None, None]);
    let all = asset_names(&v, &Platform::RELEASED);
    let mut once = all.clone();
    once.dedup();
    assert_eq!(once, all, "every asset is listed once");
    let attested = attested_names(&v);
    for native in [
        "hostkeeper_0.2.1_amd64.deb",
        "hostkeeper_0.2.1_arm64.deb",
        "hostkeeper-0.2.1-macos-arm64.pkg",
        "hello_0.2.1_amd64.deb",
        "hello_0.2.1_arm64.deb",
        "hello-0.2.1-macos-arm64.pkg",
        "mdm_0.2.1_amd64.deb",
    ] {
        assert!(attested.contains(&native.to_owned()), "{native}");
        assert!(summed_names(&v).contains(&native.to_owned()), "{native}");
        assert!(!all.contains(&format!("{native}.sha256")), "{native}");
    }
}

#[test]
fn a_linux_platform_ships_the_mdm_binary_attested_with_its_sbom() {
    // Catches: the MDM kit's binary missing from a Linux platform, named other than
    // `hostkeeper-mdm-<v>-<p>.tar.gz`, shipped without its bundle (so neither verified offline
    // nor held to the signer), or without its SBOM, or an SBOM given a bundle of its own.
    let v = version("0.2.1");
    for platform in [Platform::LinuxX86_64, Platform::LinuxArm64] {
        let p = platform.name();
        let names = asset_names(&v, &[platform]);
        let mdm: Vec<&str> = names
            .iter()
            .map(String::as_str)
            .filter(|n| n.starts_with("hostkeeper-mdm-"))
            .collect();
        assert_eq!(
            mdm,
            [
                format!("hostkeeper-mdm-0.2.1-{p}.cdx.json"),
                format!("hostkeeper-mdm-0.2.1-{p}.tar.gz"),
                format!("hostkeeper-mdm-0.2.1-{p}.tar.gz.sigstore.json"),
            ],
            "{p}"
        );
    }
    let attested = attested_names(&v);
    let summed = summed_names(&v);
    for p in ["linux-x86_64", "linux-arm64"] {
        let tarball = format!("hostkeeper-mdm-0.2.1-{p}.tar.gz");
        let sbom = format!("hostkeeper-mdm-0.2.1-{p}.cdx.json");
        assert!(attested.contains(&tarball), "{tarball} is attested");
        assert!(!attested.contains(&sbom), "{sbom} has no bundle");
        assert!(summed.contains(&tarball) & summed.contains(&sbom), "{p}");
    }
    assert!(
        !attested
            .iter()
            .any(|n| n.starts_with("hostkeeper-mdm-0.2.1-macos")),
        "{attested:?}"
    );
}

#[test]
fn no_platform_ships_the_withdrawn_arch_package() {
    // Catches: the Arch package (or its bundle) shipped again on any platform after its
    // withdrawal, attested or summed.
    let v = version("0.2.1");
    for platform in Platform::RELEASED {
        let names = asset_names(&v, &[platform]);
        assert!(
            !names.iter().any(|n| n.contains(".pkg.tar.zst")),
            "{platform:?}: {names:?}"
        );
    }
    assert!(
        !attested_names(&v)
            .iter()
            .any(|n| n.contains(".pkg.tar.zst"))
    );
    assert!(!summed_names(&v).iter().any(|n| n.contains(".pkg.tar.zst")));
}

#[test]
fn the_services_and_packages_name_their_services_and_versions() {
    // Catches: a hello package named as a hostkeeper one, a failing build whose version is not
    // the documented one, or a default service other than hostkeeper (which every existing
    // invocation relies on).
    let v = version("1.2.3");
    assert_eq!(Service::default(), Service::Hostkeeper);
    assert_eq!(Service::Hostkeeper.name(), "hostkeeper");
    assert_eq!(Service::Hello.name(), "hello");
    assert_eq!(Service::Mdm.name(), "mdm");
    assert_eq!(
        Package::ALL,
        [
            Package::Hostkeeper,
            Package::Hello,
            Package::HelloFailReady,
            Package::Mdm
        ]
    );
    assert_eq!(
        Package::ALL.map(|p| p.service().name()),
        ["hostkeeper", "hello", "hello", "mdm"]
    );
    assert_eq!(
        Package::ALL.map(|p| p.version_text(&v)),
        ["1.2.3", "1.2.3", "1.2.3-fail-ready", "1.2.3"]
    );
}

#[test]
fn a_package_is_chosen_by_service_and_fail_ready_and_hostkeeper_has_no_failing_build() {
    // Catches: a failing build of hostkeeper that exists, or `--fail-ready` ignored for hello.
    assert_eq!(
        Package::of(Service::Hostkeeper, false),
        Some(Package::Hostkeeper)
    );
    assert_eq!(Package::of(Service::Hello, false), Some(Package::Hello));
    assert_eq!(
        Package::of(Service::Hello, true),
        Some(Package::HelloFailReady)
    );
    assert_eq!(Package::of(Service::Hostkeeper, true), None);
    assert_eq!(Package::of(Service::Mdm, false), Some(Package::Mdm));
    assert_eq!(Package::of(Service::Mdm, true), None);
}

#[test]
fn the_mdm_package_is_released_for_linux_x86_64_alone_and_the_others_for_every_platform() {
    // Catches: an mdm package or index asked for on linux-arm64 or macOS, whose pinned tools are
    // linux-amd64 builds (a release would then be refused for lacking it), or another package
    // losing a platform.
    for package in [Package::Hostkeeper, Package::Hello, Package::HelloFailReady] {
        assert_eq!(package.platforms(), Platform::RELEASED);
    }
    assert_eq!(Package::Mdm.platforms(), [Platform::LinuxX86_64]);
}

#[test]
fn hostkeepers_signer_is_the_contracts() {
    // Catches: a signer of hostkeeper's that drifts from the contract's constants.
    let hostkeeper = Signer::hostkeeper();
    assert_eq!(hostkeeper.repo, REPO);
    assert_eq!(hostkeeper.workflow, SIGNER_WORKFLOW);
    assert_eq!(
        format!("{}/{}", REPO, hostkeeper.signers.workflow),
        SIGNER_WORKFLOW
    );
    assert_eq!(hostkeeper.signers.organisation, "komira-ai");
    assert_eq!(hostkeeper.exact_ref(), Some(SIGNER_REF));
    assert!(hostkeeper.signers.hosted_runner);
}

#[test]
fn a_signer_has_an_exact_ref_only_for_one_ref_that_is_not_a_glob() {
    // Catches: a glob passed to `gh --source-ref`, which matches it literally and refuses every
    // real ref, or one of several refs passed as if it were the only one.
    let with = |refs: &[&str]| {
        let mut signer = Signer::hostkeeper();
        signer.signers.refs = refs.iter().map(|r| (*r).to_owned()).collect();
        signer
    };
    assert_eq!(
        with(&["refs/heads/main"]).exact_ref(),
        Some("refs/heads/main")
    );
    assert_eq!(with(&["refs/tags/*"]).exact_ref(), None);
    let two = with(&["refs/heads/main", "refs/tags/v1"]);
    assert_eq!(two.exact_ref(), None);
    assert_eq!(two.refs_text(), "refs/heads/main or refs/tags/v1");
}

#[test]
fn a_repository_is_two_parts_of_letters_digits_dash_underscore_and_dot() {
    // Catches: a part with another character, an empty part, a part too many, or an owner and a
    // name run together, accepted as a repository.
    for good in ["komira-ai/tierstore", "a.b/c_d", "A9/-._"] {
        assert_eq!(
            good.parse::<Repo>().map(|r| r.to_string()),
            Ok(good.to_owned())
        );
    }
    for bad in [
        "x", "a/b/c", "a/", "/b", "a b/c", "a/b c", "a/b\n", "é/b", "a/b+",
    ] {
        assert_eq!(
            bad.parse::<Repo>(),
            Err(BadValue::Repo(bad.to_owned())),
            "{bad}"
        );
    }
}

#[test]
fn a_release_waits_for_ci_privileged_and_artifacts_only() {
    // Catches: a workflow added to the runs a release waits for, such as net-install.yml, whose
    // emulated install takes hours (it runs on its own triggers and holds no release), or one of
    // the three dropped, so that a red privileged or artifacts run no longer holds a release.
    assert_eq!(
        REQUIRED_WORKFLOWS,
        ["ci.yml", "privileged.yml", "artifacts.yml"]
    );
}
