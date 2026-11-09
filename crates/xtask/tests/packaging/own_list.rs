//! The list the daemon holds hostkeeper's own `.deb` to (`hostkeeper_install::repository::tree`,
//! packages-native.md section 10.4) against `packaging/linux/nfpm.yaml`'s `contents`.

use std::collections::BTreeSet;

use hostkeeper_install::repository::tree::{OWN_FILES, OWN_LINKS, own_dirs};

use super::config;
use super::package::{package_files, package_links};

fn rooted(path: &str) -> String {
    format!("/{path}")
}

/// The files of the list are the `dst` of each file entry of nfpm.yaml, and its links are the
/// `dst` and `src` of each link entry: a file or link added to the package, or taken from it,
/// moves the list in the same change.
#[test]
fn the_daemons_list_for_its_own_deb_is_nfpm_yamls_contents() {
    let config = config();
    let files: BTreeSet<String> = config
        .files("deb")
        .expect("files")
        .into_iter()
        .map(|file| file.dst)
        .collect();
    let listed: BTreeSet<String> = OWN_FILES.iter().map(|path| rooted(path)).collect();
    assert_eq!(listed, files);
    let links: Vec<(String, String)> = config
        .links("deb")
        .expect("links")
        .into_iter()
        .map(|link| (link.dst, link.target))
        .collect();
    let listed: Vec<(String, String)> = OWN_LINKS
        .iter()
        .map(|(path, target)| (rooted(path), (*target).to_owned()))
        .collect();
    assert_eq!(listed, links);
}

/// The list agrees with the files and links the packaging tests hold the package to, and its
/// directories are those above them.
#[test]
fn the_list_is_the_packaging_tests_files_and_links_and_their_directories() {
    let files: BTreeSet<String> = package_files().into_iter().map(|file| file.dst).collect();
    let listed: BTreeSet<String> = OWN_FILES.iter().map(|path| rooted(path)).collect();
    assert_eq!(listed, files);
    let links: Vec<(String, String)> = package_links()
        .into_iter()
        .map(|link| (link.dst, link.target))
        .collect();
    let listed: Vec<(String, String)> = OWN_LINKS
        .iter()
        .map(|(path, target)| (rooted(path), (*target).to_owned()))
        .collect();
    assert_eq!(listed, links);
    let mut above = BTreeSet::new();
    for path in files.iter().chain(links.iter().map(|(dst, _)| dst)) {
        let mut parent = path.as_str();
        while let Some((dir, _)) = parent.rsplit_once('/').filter(|(dir, _)| !dir.is_empty()) {
            above.insert(dir.trim_start_matches('/').to_owned());
            parent = dir;
        }
    }
    assert_eq!(own_dirs(), above);
}
