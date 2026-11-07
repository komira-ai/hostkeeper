//! The `Distribution` layer of the product archive: its files, its component and its elements.

use super::*;

#[test]
fn a_product_archive_without_its_distribution_or_component_or_with_another_entry_is_refused() {
    // Catches: a product archive that is a bare component pkg (the layer productbuild adds
    // missing), one without its component, one with a stray file or directory beside them, or a
    // Distribution that is a directory or a link.
    let expanded = Expanded::good("macos-pkg-product-distribution-missing");
    std::fs::remove_file(expanded.product().join(DISTRIBUTION)).expect("remove");
    assert_eq!(expanded.problems(), ["missing Distribution"]);
    let expanded = Expanded::good("macos-pkg-product-component-missing");
    std::fs::remove_dir_all(expanded.pkg()).expect("remove");
    assert_eq!(expanded.problems(), ["missing hostkeeper.pkg"]);
    let only = "an expanded product archive of hostkeeper holds only the file Distribution and \
                the directory hostkeeper.pkg";
    let expanded = Expanded::good("macos-pkg-product-extra");
    std::fs::write(expanded.product().join("Resources"), b"").expect("a stray file");
    std::fs::create_dir(expanded.product().join("Scripts")).expect("a stray directory");
    assert_eq!(
        expanded.problems(),
        [format!("Resources: {only}"), format!("Scripts: {only}")]
    );
    let expanded = Expanded::good("macos-pkg-product-bare-component");
    let bare = Expanded::good("macos-pkg-product-bare-component-source");
    let product = expanded.product();
    std::fs::remove_file(product.join(DISTRIBUTION)).expect("remove");
    std::fs::remove_dir_all(expanded.pkg()).expect("remove");
    for name in ["Bom", "PackageInfo", "Payload"] {
        std::fs::copy(bare.pkg().join(name), product.join(name)).expect("copy");
    }
    std::fs::create_dir(product.join("Scripts")).expect("a directory");
    assert_eq!(
        expanded.problems(),
        [
            format!("Bom: {only}"),
            format!("PackageInfo: {only}"),
            format!("Payload: {only}"),
            format!("Scripts: {only}"),
            "missing Distribution".to_owned(),
            "missing hostkeeper.pkg".to_owned(),
        ]
    );
    // A Distribution that is a directory, and a component that is a link to a good one, are not
    // the file and the directory.
    let expanded = Expanded::good("macos-pkg-product-wrong-kinds");
    let elsewhere = Expanded::good("macos-pkg-product-wrong-kinds-source");
    std::fs::remove_file(expanded.product().join(DISTRIBUTION)).expect("remove");
    std::fs::create_dir(expanded.product().join(DISTRIBUTION)).expect("a directory");
    std::fs::remove_dir_all(expanded.pkg()).expect("remove");
    std::os::unix::fs::symlink(elsewhere.pkg(), expanded.pkg()).expect("a link");
    assert_eq!(
        expanded.problems(),
        [
            format!("Distribution: {only}"),
            format!("hostkeeper.pkg: {only}"),
            "missing Distribution".to_owned(),
            "missing hostkeeper.pkg".to_owned(),
        ]
    );
}

#[test]
fn a_component_that_is_wrong_inside_is_refused_through_the_product_archive() {
    // Catches: a check that stops at the outer layer and never reads the component's parts.
    let expanded = Expanded::good("macos-pkg-product-inner");
    std::fs::remove_file(expanded.pkg().join("Bom")).expect("remove");
    assert_eq!(expanded.problems(), ["missing Bom"]);
}

/// The problems of the `Distribution` `text` of the pkg of 0.1.5.
fn problems_of(text: &str) -> Vec<String> {
    let expanded = Expanded::good("macos-pkg-distribution");
    expanded.write_distribution(text);
    match expanded.check() {
        Ok(_) => Vec::new(),
        Err(Error::Refused(lines)) => lines,
        other => panic!("expected a pass or a refusal, got {other:?}"),
    }
}

impl Expanded {
    /// Replaces the `Distribution` of the product archive.
    fn write_distribution(&self, text: &str) {
        std::fs::write(self.product().join(DISTRIBUTION), text).expect("the distribution");
    }
}

#[test]
fn the_distribution_productbuild_writes_for_the_component_passes() {
    // Catches: a Distribution check that refuses the file productbuild synthesizes, or one that
    // needs more of it than the identifier, the version and the component's reference.
    assert_eq!(problems_of(&distribution_text()), Vec::<String>::new());
    // The reference may be written with its text on its own lines.
    let spaced = distribution_with(
        &COMPONENT_REF
            .replace(">#", ">\n  #")
            .replace("</pkg", "\n</pkg"),
        "",
    );
    assert_eq!(problems_of(&spaced), Vec::<String>::new());
}

#[test]
fn a_distribution_that_names_another_component_id_is_refused() {
    // Catches: a product that installs a second component, or one under another identifier,
    // which the pkg's allowlist by digest would still admit.
    let id = "dev.hostkeeper.hostkeeper";
    let second = format!("<pkg-ref id=\"dev.hostkeeper.other\"/>\n    {COMPONENT_REF}");
    assert_eq!(
        problems_of(&distribution_with(&second, "")),
        [format!(
            "Distribution: a <pkg-ref> has the id Some(\"dev.hostkeeper.other\"), not \
             {id:?}: the product holds only the one component"
        )]
    );
    let none = distribution_with("<pkg-ref/>\n    ", "").replace(COMPONENT_REF, "");
    assert_eq!(
        problems_of(&none),
        [
            format!(
                "Distribution: a <pkg-ref> has the id None, not {id:?}: the product holds only \
                 the one component"
            ),
            "Distribution has 0 <pkg-ref> elements that refer to #hostkeeper.pkg, not one"
                .to_owned(),
        ]
    );
}

#[test]
fn a_distribution_with_another_component_file_or_a_second_reference_is_refused() {
    // Catches: a Distribution that points at a package the archive does not hold, or at the
    // component twice.
    let other = COMPONENT_REF.replace("#hostkeeper.pkg", "#other.pkg");
    assert_eq!(
        problems_of(&distribution_with(&other, "")),
        [
            "Distribution: a <pkg-ref> refers to \"#other.pkg\", not \"#hostkeeper.pkg\", the \
             component of the product archive"
                .to_owned(),
            "Distribution has 0 <pkg-ref> elements that refer to #hostkeeper.pkg, not one"
                .to_owned(),
        ]
    );
    let twice = format!("{COMPONENT_REF}\n    {COMPONENT_REF}");
    assert_eq!(
        problems_of(&distribution_with(&twice, "")),
        ["Distribution has 2 <pkg-ref> elements that refer to #hostkeeper.pkg, not one"]
    );
}

#[test]
fn a_distribution_of_another_version_or_conclusion_is_refused() {
    // Catches: a product archive of another release, and one that asks the installer to restart,
    // log out or shut down after the install.
    let old = COMPONENT_REF.replace("version=\"0.1.5\"", "version=\"0.1.4\"");
    assert_eq!(
        problems_of(&distribution_with(&old, "")),
        [
            "Distribution: the <pkg-ref> of #hostkeeper.pkg has the version Some(\"0.1.4\"), not \
             \"0.1.5\""
        ]
    );
    let bare = COMPONENT_REF.replace(" version=\"0.1.5\"", "");
    assert_eq!(
        problems_of(&distribution_with(&bare, "")),
        ["Distribution: the <pkg-ref> of #hostkeeper.pkg has the version None, not \"0.1.5\""]
    );
    let restart = COMPONENT_REF.replace("onConclusion=\"none\"", "onConclusion=\"RequireRestart\"");
    assert_eq!(
        problems_of(&distribution_with(&restart, "")),
        ["Distribution: a <pkg-ref> has onConclusion \"RequireRestart\", not \"none\""]
    );
    // A reference without onConclusion is the installer's default, none.
    let default = COMPONENT_REF.replace(" onConclusion=\"none\"", "");
    assert_eq!(
        problems_of(&distribution_with(&default, "")),
        Vec::<String>::new()
    );
}

#[test]
fn a_distribution_without_its_product_or_with_another_product_is_refused() {
    // Catches: a Distribution with no product element, or one of another identifier or version,
    // which the installer would show and record as another product.
    let good = distribution_text();
    let product = "<product id=\"dev.hostkeeper.hostkeeper\" version=\"0.1.5\"/>";
    assert!(good.contains(product));
    assert_eq!(
        problems_of(&good.replace(product, "")),
        ["Distribution has no <product> element"]
    );
    assert_eq!(
        problems_of(&good.replace(
            product,
            "<product id=\"dev.hostkeeper.other\" version=\"0.1.4\"/>"
        )),
        [
            "Distribution: the <product> id is Some(\"dev.hostkeeper.other\"), not \
             \"dev.hostkeeper.hostkeeper\"",
            "Distribution: the <product> version is Some(\"0.1.4\"), not \"0.1.5\"",
        ]
    );
    assert_eq!(
        problems_of(&good.replace(product, "<product/>")),
        [
            "Distribution: the <product> id is None, not \"dev.hostkeeper.hostkeeper\"",
            "Distribution: the <product> version is None, not \"0.1.5\"",
        ]
    );
    // An element that only starts with the name is another element.
    assert_eq!(
        problems_of(&good.replace(product, "<products/>")),
        ["Distribution has no <product> element"]
    );
}

#[test]
fn a_distribution_with_installer_code_or_a_check_or_no_root_is_refused() {
    // Catches: installer JavaScript (a script element, an installation or volume check) that
    // would run on the Mac at install, and a file that is not a Distribution.
    let good = distribution_text();
    for forbidden in ["script", "installation-check", "volume-check"] {
        let with = distribution_with(COMPONENT_REF, &format!("<{forbidden}>x</{forbidden}>"));
        assert_eq!(
            problems_of(&with),
            [format!(
                "Distribution has a <{forbidden}> element: no installer JavaScript or install \
                 check runs"
            )]
        );
    }
    assert_eq!(
        problems_of(&good.replace("installer-gui-script", "installer")),
        ["Distribution has no <installer-gui-script> element"]
    );
}

#[test]
fn a_distribution_with_an_unreadable_start_tag_is_refused() {
    // Catches: a start tag read loosely, so that a malformed pkg-ref or product passes.
    let broken = distribution_text().replace(COMPONENT_REF, "<pkg-ref id=dev>#hostkeeper.pkg");
    let problems = problems_of(&broken);
    assert_eq!(
        problems,
        ["Distribution has an unreadable <pkg-ref> element"]
    );
    let broken = distribution_text().replace("<product id=", "<product id=x ");
    assert_eq!(
        problems_of(&broken),
        ["Distribution has an unreadable <product> element"]
    );
}

#[test]
fn a_reference_text_that_never_ends_is_read_to_the_end_of_the_file() {
    // Catches: a scan that panics or drops a reference whose text has no tag after it.
    let ends = "<installer-gui-script><product id=\"dev.hostkeeper.hostkeeper\" \
                version=\"0.1.5\"/><pkg-ref id=\"dev.hostkeeper.hostkeeper\" \
                version=\"0.1.5\">#hostkeeper.pkg";
    assert_eq!(problems_of(ends), Vec::<String>::new());
}
