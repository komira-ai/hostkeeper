use super::*;
use crate::release::doc_tests::{DOC, documented_names};

const BASELINE: &str = include_str!("../../../tests/release/asset_names.baseline");

/// What stands for the version in the baseline.
const PLACEHOLDER: &str = "<version>";

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
        .filter(|l| !l.starts_with('#') & !l.trim().is_empty())
        .map(|l| l.replace(PLACEHOLDER, &version.to_string()))
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
    assert_eq!(names.len(), 70);
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
fn asset_name_list_only_grows() {
    // Catches: a released name that the current list no longer holds; adding a name is allowed.
    let v = version("0.1.4");
    let current = asset_names(&v, &Platform::RELEASED);
    assert_eq!(
        missing_from(&baseline_for(&v), &current),
        Vec::<String>::new()
    );
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
    // `hostkeeper X.Y.Z+<12 hex>` (`hostkeeper-coordinator ...` and `hostkeeper-mdm ...` for the
    // other two shipped binaries), a shipped binary dropped, or a change of the signer identity.
    let v = version("0.1.27");
    let c = commit("0123456789abcdef0123456789abcdef01234567");
    assert_eq!(v.tag(), "v0.1.27");
    assert_eq!(Version::of_tag("v0.1.27"), Some(v));
    assert_eq!(stamp_string(&v, &c), "hostkeeper 0.1.27+0123456789ab");
    assert_eq!(
        PROGRAMS.map(|p| stamp_of(p, &v, &c)),
        [
            "hostkeeper 0.1.27+0123456789ab",
            "hostkeeper-coordinator 0.1.27+0123456789ab",
            "hostkeeper-mdm 0.1.27+0123456789ab",
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
    assert_eq!(summed.len(), 44);
    for index in [INDEX, HELLO_INDEX, HELLO_FAIL_READY_INDEX] {
        assert!(summed.contains(&index.to_owned()) & !attested.contains(&index.to_owned()));
    }
    // The boot sets of network install are summed and attested, with a bundle of their own, and
    // so is the command line, one script for every platform.
    assert!(summed.contains(&NETBOOT.to_owned()) & attested.contains(&NETBOOT.to_owned()));
    assert_eq!(hkctl_name(&v), "hkctl-0.1.3");
    assert!(summed.contains(&hkctl_name(&v)) & attested.contains(&hkctl_name(&v)));
    assert!(attested.iter().all(|a| summed.contains(a)));
    assert!(!summed.iter().any(|n| is_bundle(n) | (n == SUMS)));
    for package in Package::ALL {
        for platform in Platform::RELEASED {
            assert!(attested.contains(&package.package_name(&v, platform)));
        }
    }
    assert!(attested.contains(&installer_name(&v)));
    assert_eq!(
        Package::Hostkeeper.package_name(&v, Platform::LinuxX86_64),
        "hostkeeper-0.1.3-linux-x86_64.tar.zst"
    );
    assert_eq!(bundle_of("a.deb"), "a.deb.sigstore.json");
    assert_eq!(
        Platform::ALL.map(Platform::name),
        ["linux-arm64", "linux-x86_64", "macos-arm64"]
    );
}

#[test]
fn there_are_exactly_three_platforms_and_a_release_ships_all_three() {
    // Catches: a fourth platform, a name read loosely, or a release that leaves one out (macOS
    // dropped from the release while its assets are still built).
    for platform in Platform::ALL {
        assert_eq!(Platform::of_name(platform.name()), Some(platform));
    }
    for bad in [
        "linux-amd64",
        "Linux-x86_64",
        "macos-x86_64",
        "windows-x86_64",
        "",
    ] {
        assert_eq!(Platform::of_name(bad), None, "{bad}");
    }
    assert_eq!(
        Platform::RELEASED,
        [
            Platform::LinuxX86_64,
            Platform::LinuxArm64,
            Platform::MacosArm64
        ]
    );
    let mut released = Platform::RELEASED.to_vec();
    released.sort_by_key(|p| p.name());
    assert_eq!(released, Platform::ALL, "every platform is released");
}

#[test]
fn a_macos_platform_has_a_binary_a_package_an_installer_and_an_sbom_but_no_linux_assets() {
    // Catches: a deb, an rpm, a coordinator, an MDM kit binary or an image named for macOS
    // (the MDM kit is Linux-only), its packages (the
    // hostkeeper one, the hello one and hello's failing build) without a bundle or digest file,
    // or its installer pkg missing or not attested.
    let v = version("0.2.1");
    assert_eq!(
        asset_names(&v, &[Platform::MacosArm64]),
        [
            "SHA256SUMS",
            "hello-0.2.1-fail-ready-macos-arm64.tar.zst",
            "hello-0.2.1-fail-ready-macos-arm64.tar.zst.sha256",
            "hello-0.2.1-fail-ready-macos-arm64.tar.zst.sigstore.json",
            "hello-0.2.1-macos-arm64.tar.zst",
            "hello-0.2.1-macos-arm64.tar.zst.sha256",
            "hello-0.2.1-macos-arm64.tar.zst.sigstore.json",
            "hello-fail-ready-index.json",
            "hello-index.json",
            "hkctl-0.2.1",
            "hkctl-0.2.1.sigstore.json",
            "hostkeeper-0.2.1-macos-arm64.cdx.json",
            "hostkeeper-0.2.1-macos-arm64.pkg",
            "hostkeeper-0.2.1-macos-arm64.pkg.sigstore.json",
            "hostkeeper-0.2.1-macos-arm64.tar.gz",
            "hostkeeper-0.2.1-macos-arm64.tar.gz.sigstore.json",
            "hostkeeper-0.2.1-macos-arm64.tar.zst",
            "hostkeeper-0.2.1-macos-arm64.tar.zst.sha256",
            "hostkeeper-0.2.1-macos-arm64.tar.zst.sigstore.json",
            "index.json",
            "netboot.json",
            "netboot.json.sigstore.json",
        ]
    );
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
fn the_services_and_packages_name_their_packages_and_indexes() {
    // Catches: a hello package named as a hostkeeper one, a package's index under another's
    // name, a failing build whose version, file or index is not the documented one, or a default
    // service other than hostkeeper (which every existing invocation relies on).
    let v = version("1.2.3");
    assert_eq!(Service::default(), Service::Hostkeeper);
    assert_eq!(Service::Hostkeeper.name(), "hostkeeper");
    assert_eq!(Service::Hello.name(), "hello");
    assert_eq!(
        Package::ALL,
        [Package::Hostkeeper, Package::Hello, Package::HelloFailReady]
    );
    assert_eq!(
        Package::ALL.map(Package::index_name),
        [
            "index.json",
            "hello-index.json",
            "hello-fail-ready-index.json"
        ]
    );
    assert_eq!(
        Package::ALL.map(|p| p.service().name()),
        ["hostkeeper", "hello", "hello"]
    );
    assert_eq!(
        Package::ALL.map(|p| p.version_text(&v)),
        ["1.2.3", "1.2.3", "1.2.3-fail-ready"]
    );
    assert_eq!(
        Package::Hello.package_name(&v, Platform::MacosArm64),
        "hello-1.2.3-macos-arm64.tar.zst"
    );
    assert_eq!(
        Package::HelloFailReady.package_name(&v, Platform::LinuxX86_64),
        "hello-1.2.3-fail-ready-linux-x86_64.tar.zst"
    );
    assert_eq!(
        Package::Hostkeeper.package_name(&v, Platform::LinuxArm64),
        "hostkeeper-1.2.3-linux-arm64.tar.zst"
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
}

#[test]
fn the_signer_of_a_trusted_service_is_its_entrys_workflow_and_hostkeepers_is_the_contracts() {
    // Catches: a service's release checked against another workflow or repository than its
    // trust list entry's, or a signer of hostkeeper's that drifts from the contract's constants.
    let trust = hostkeeper_config::channel_trust::ChannelTrust::parse(
        "[[service]]\nname = \"js-agent\"\nrepository = \"komira-ai/jobscheduler\"\n\
         workflow = \".github/workflows/release.yml\"\nrefs = [\"refs/tags/v*\"]\n\
         hosted_runner = false\n",
    )
    .expect("a trust list");
    let agent = Signer::of(&trust.services()[0]);
    assert_eq!(
        agent,
        Signer {
            repo: "komira-ai/jobscheduler".to_owned(),
            workflow: "komira-ai/jobscheduler/.github/workflows/release.yml".to_owned(),
            signers: TrustedSigners {
                organisation: "komira-ai".to_owned(),
                workflow: ".github/workflows/release.yml".to_owned(),
                refs: vec!["refs/tags/v*".to_owned()],
                hosted_runner: false,
            },
        }
    );
    assert_eq!(agent.exact_ref(), None);
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
fn a_repository_has_the_trust_lists_grammar() {
    // Catches: `--repo`-style values and trust list repositories read by two grammars.
    use hostkeeper_config::channel_trust::Repository;
    for text in [
        "komira-ai/tierstore",
        "a.b/c_d",
        "x",
        "a/b/c",
        "a/",
        "a b/c",
    ] {
        let ours = text.parse::<Repo>().ok().map(|r| r.to_string());
        let theirs = Repository::new(text).map(|r| r.to_string());
        assert_eq!(ours, theirs, "{text}");
    }
}
