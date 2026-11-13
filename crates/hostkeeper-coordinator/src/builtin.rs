//! The built-in repository `komira` (docs/design/packages-repositories.md, section 3.2): the
//! organisation's own repository, `https://packages.komira.ai`, known to every coordinator from
//! its code, so that a fresh coordinator needs no repository setup and the packages published
//! there (hostkeeper, tierstore, js-agent, kbf, hello) are added by name.
//!
//! It is an entry of the registry like any other, at generation 0, which [`Registry::get`],
//! [`Registry::listed`] and [`Registry::entries_of`] answer for while no entry of its name was
//! put. It is in no file and no row. An operator's `Put` of the name `komira` replaces it (a key
//! rotation, or another base), and removing that entry brings the built-in back.
//!
//! Its signing key is pinned here: the keyring is the file packages.komira.ai serves
//! (`builtin/komira-archive-keyring.gpg`, compiled in) and [`PINNED`] are the fingerprints of its
//! keys, the root of trust of every host's apt for this repository. A test judges the entry by the
//! registry's own rules, so a keyring that does not hold the pinned fingerprints fails the build.
//! Rotating the key means a new keyring and pin in a coordinator release
//! (docs/operations/repository-keys.md), or a `Put` over it.
//!
//! [`Registry::get`]: crate::repositories::Registry::get
//! [`Registry::listed`]: crate::repositories::Registry::listed
//! [`Registry::entries_of`]: crate::repositories::Registry::entries_of

use std::sync::LazyLock;

use hostkeeper_proto::hostkeeper::release::v1::Repository;
use hostkeeper_release::registry::KIND_APT;

use crate::repositories::Kept;

/// The name of the built-in repository.
pub(crate) const NAME: &str = "komira";

/// Its base URL.
pub(crate) const BASE_URI: &str = "https://packages.komira.ai";

/// The fingerprints of its keys, pinned: the primary key and its signing subkey.
pub(crate) const PINNED: [&str; 2] = [
    "CB09CF205649494A92DFC22C70D1F2A25906CB22",
    "F767235D83B0410CFAF4779E8738AD1BE061EEDD",
];

/// The keyring packages.komira.ai serves at `apt/komira-archive-keyring.gpg`.
const KEYRING: &[u8] = include_bytes!("../builtin/komira-archive-keyring.gpg");

/// The descriptor the repository serves at `/hostkeeper-repository.json`
/// (docs/deploy/package-repositories.md): the layout the built-in entry is made from, and the
/// macOS directories of the packages published there (hello's installer is in hostkeeper's).
const DESCRIPTOR: &str = r#"{
  "version": 1,
  "name": "komira",
  "description": "komira-ai packages",
  "channels": ["testing", "stable"],
  "key": {
    "keyring": "apt/komira-archive-keyring.gpg",
    "fingerprints": [
      "CB09CF205649494A92DFC22C70D1F2A25906CB22",
      "F767235D83B0410CFAF4779E8738AD1BE061EEDD"
    ]
  },
  "formats": {
    "apt": { "path": "apt", "components": ["main"], "architectures": ["amd64", "arm64"] },
    "rpm": { "path": "rpm", "architectures": ["x86_64", "aarch64"] },
    "macos-pkg": {
      "path": "pkg",
      "platforms": ["macos-arm64"],
      "packages": { "hostkeeper": { "dir": "hostkeeper" }, "hello": { "dir": "hostkeeper" } }
    }
  }
}
"#;

static KOMIRA: LazyLock<Kept> = LazyLock::new(|| Kept {
    repository: Repository {
        name: NAME.to_owned(),
        kind: KIND_APT.to_owned(),
        uri: format!("{BASE_URI}/apt"),
        suites: vec!["testing".to_owned(), "stable".to_owned()],
        components: vec!["main".to_owned()],
        fingerprints: PINNED.iter().map(|print| (*print).to_owned()).collect(),
        keyring: KEYRING.to_vec(),
        descriptor: DESCRIPTOR.as_bytes().to_vec(),
        base_uri: BASE_URI.to_owned(),
        ..Repository::default()
    },
    generation: 0,
});

/// The built-in repository `komira`, at generation 0.
pub(crate) fn komira() -> &'static Kept {
    &KOMIRA
}

/// Whether `fingerprints` are exactly the pinned ones, in any order.
pub(crate) fn is_pinned(fingerprints: &[String]) -> bool {
    fingerprints.len() == PINNED.len()
        && PINNED
            .iter()
            .all(|pin| fingerprints.iter().any(|f| f == pin))
}
