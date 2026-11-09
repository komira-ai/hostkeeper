//! The rule that the attest job of `artifacts.yml` attests, and copies the bundle of, every Linux
//! package (the deb and the rpm of each Linux platform): the file passes as it
//! is, and each package's missing attestation or missing bundle copy is refused with its message.

use super::lint::lint;
use super::release::LINUX_ARCHES;
use super::{with_in, workflows};

/// The repository's artifacts.yml.
fn artifacts_yml() -> String {
    workflows()
        .into_iter()
        .find(|(file, _)| file == "artifacts.yml")
        .map(|(_, text)| text)
        .expect("artifacts.yml")
}

/// One Linux package: the id of its attest step, its `subject-path` file as the workflow spells
/// it, its name as a problem spells it and its name as the bundle copy spells it.
struct LinuxPackage {
    id: String,
    spelled: String,
    name: String,
    copied: String,
}

/// The deb and the rpm of both Linux platforms.
fn linux_packages() -> Vec<LinuxPackage> {
    let version = "${{ needs.version.outputs.version }}";
    let mut all = Vec::new();
    for (platform, (deb, rpm)) in ["linux-x86_64", "linux-arm64"]
        .into_iter()
        .zip(LINUX_ARCHES)
    {
        let kinds = [
            (
                "deb",
                format!("hostkeeper_{version}_{deb}.deb"),
                format!("hostkeeper_<version>_{deb}.deb"),
                format!("hostkeeper_${{V}}_{deb}.deb"),
            ),
            (
                "rpm",
                format!("hostkeeper-{version}-1.{rpm}.rpm"),
                format!("hostkeeper-<version>-1.{rpm}.rpm"),
                format!("hostkeeper-${{V}}-1.{rpm}.rpm"),
            ),
        ];
        for (kind, spelled, name, copied) in kinds {
            all.push(LinuxPackage {
                id: format!("{kind}-{platform}"),
                spelled,
                name,
                copied,
            });
        }
    }
    all
}

#[test]
fn artifacts_yml_attests_every_linux_package_and_copies_its_bundle() {
    // Catches: the rule refusing the committed file, or the file lacking a package's attestation.
    let text = artifacts_yml();
    assert_eq!(lint("artifacts.yml", &text), Vec::<String>::new());
    let packages = linux_packages();
    assert_eq!(packages.len(), 4);
    for p in packages {
        let subject = format!("          subject-path: dist/{}\n", p.spelled);
        assert_eq!(text.matches(&subject).count(), 1, "{}", p.id);
        let copy = format!(
            "        run: cp \"$BUNDLE\" \"dist/{}.sigstore.json\"\n",
            p.copied
        );
        assert_eq!(text.matches(&copy).count(), 1, "{}", p.id);
    }
}

#[test]
fn a_linux_package_without_its_attestation_is_refused() {
    // Catches: a release whose deb or rpm is published without a provenance
    // attestation (a host trusts only attested packages).
    let text = artifacts_yml();
    for p in linux_packages() {
        let subject = format!("          subject-path: dist/{}\n", p.spelled);
        let without = with_in(&text, &subject, "          subject-path: dist/other\n");
        let want = format!(
            "artifacts.yml: job attest does not attest {} (a provenance step with an id and the subject-path dist/{})",
            p.name, p.spelled
        );
        let found = lint("artifacts.yml", &without);
        assert!(found.contains(&want), "{}: {want:?} not in {found:?}", p.id);
    }
}

#[test]
fn a_linux_package_without_its_bundle_copy_is_refused() {
    // Catches: a release whose deb or rpm is attested but whose bundle is not put
    // beside it under its name, so the release lacks the `.sigstore.json` asset.
    let text = artifacts_yml();
    for p in linux_packages() {
        let copy = format!(
            "        run: cp \"$BUNDLE\" \"dist/{}.sigstore.json\"\n",
            p.copied
        );
        let without = with_in(&text, &copy, "        run: echo nothing\n");
        let want = format!(
            "artifacts.yml: job attest does not copy the bundle of {} from the step {} to dist/{}.sigstore.json",
            p.name, p.id, p.copied
        );
        let found = lint("artifacts.yml", &without);
        assert!(found.contains(&want), "{}: {want:?} not in {found:?}", p.id);
    }
}
