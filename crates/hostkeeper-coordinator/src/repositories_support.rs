//! Repositories for the tests: entries that pass [`registry::check`], with keyrings built by
//! hand (one public key packet of a version 4 key each, whose fingerprint the keyring says).

use std::collections::BTreeMap;

use hostkeeper_proto::hostkeeper::release::v1::{
    Hook, InstallerBounds, Manifest, Repository, StepBounds, Target, TargetList,
};
use hostkeeper_release::registry;

/// A keyring of one public key whose body is made from `seed`.
pub(crate) fn keyring(seed: u8) -> Vec<u8> {
    let mut body = vec![4, 0, 0, 0, 1, 22];
    body.extend([seed; 8]);
    let mut packet = vec![0xc0 | 6, u8::try_from(body.len()).expect("short")];
    packet.extend(body);
    packet
}

/// A keyring of one public key, `size` bytes long (at least 12), the new packet format's five-byte
/// length, its body made from `seed`.
pub(crate) fn keyring_of_size(seed: u8, size: usize) -> Vec<u8> {
    let mut body = vec![4, 0, 0, 0, 1, 22];
    body.resize(size - 6, seed);
    let mut packet = vec![0xc0 | 6, 255];
    packet.extend(u32::try_from(body.len()).expect("short").to_be_bytes());
    packet.extend(body);
    packet
}

/// The fingerprint of the one key of `keyring`.
pub(crate) fn fingerprint(keyring: &[u8]) -> String {
    registry::keys(keyring).expect("a keyring")[0]
        .fingerprint
        .clone()
}

/// A repository `name` at `uri` with `suites`, the component `main`, and the key of `seed`.
pub(crate) fn repository(name: &str, uri: &str, suites: &[&str], seed: u8) -> Repository {
    let keyring = keyring(seed);
    Repository {
        name: name.to_owned(),
        kind: registry::KIND_APT.to_owned(),
        uri: uri.to_owned(),
        suites: suites.iter().map(|s| (*s).to_owned()).collect(),
        components: vec!["main".to_owned()],
        fingerprints: vec![fingerprint(&keyring)],
        keyring,
        ..Repository::default()
    }
}

/// The repository `komira`, with the suites `testing` and `stable`, signed by the key of `seed`.
pub(crate) fn komira(seed: u8) -> Repository {
    repository(
        "komira",
        "https://packages.example.com/apt",
        &["testing", "stable"],
        seed,
    )
}

/// The descriptor of the repository `komira` of the tests (docs/deploy/package-repositories.md):
/// the channels `testing` and `stable`, an apt format, and a macOS format whose packages are
/// `hostkeeper` and `hello` (hello's installer is in hostkeeper's directory), made for `keyring`.
pub(crate) fn komira_descriptor(keyring: &[u8]) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "version": 1,
        "name": "komira",
        "channels": ["testing", "stable"],
        "key": {"keyring": "k.gpg", "fingerprints": [fingerprint(keyring)]},
        "formats": {
            "apt": {"path": "apt", "components": ["main"]},
            "macos-pkg": {
                "path": "pkg",
                "platforms": ["macos-arm64"],
                "packages": {"hostkeeper": {"dir": "hostkeeper"}, "hello": {"dir": "hostkeeper"}}
            }
        }
    }))
    .expect("json")
}

/// [`komira`] as `Repositories.Add` keeps it: with the descriptor of [`komira_descriptor`] and
/// its base URL.
pub(crate) fn komira_added(seed: u8) -> Repository {
    let mut entry = komira(seed);
    entry.base_uri = "https://packages.example.com".to_owned();
    entry.descriptor = komira_descriptor(&entry.keyring);
    entry
}

/// A repository entry of `service` that passes every rule of the release: `apt` from the entry
/// `repository` of the registry at `1:1.6.0-1`, pinned by the digest all `a`, its manifest naming
/// the package `service`.
pub(crate) fn repository_entry(service: &str, repository: &str) -> Target {
    Target {
        installer: "apt".to_owned(),
        digest: "a".repeat(64),
        manifest: Some(Manifest {
            package: Some(service.to_owned()),
            hook: Some(Hook {
                timeout_secs: Some(StepBounds {
                    prepare: 0,
                    cutover: 10,
                    verify: 20,
                    rollback: 30,
                    cleanup: 40,
                    uninstall: None,
                }),
                verify_deadline_secs: 50,
                ..Hook::default()
            }),
            run: vec![format!("{service}.service")],
            min_hostkeeper: "0.4.0".to_owned(),
            installer_bounds: Some(InstallerBounds {
                stage_secs: 60,
                activate_secs: 120,
                remove_secs: 300,
            }),
            ..Manifest::default()
        }),
        repository: Some(repository.to_owned()),
        package_version: Some("1:1.6.0-1".to_owned()),
        ..Target::default()
    }
}

/// The named-version entry of a Mac's `.pkg` of `service` (docs/design/packages-repositories.md
/// section 4.7): `macos-pkg` from the channel `channel` at `version`, pinned by the digest all `a`,
/// `bytes` 1000, a manifest that passes every rule of the release for `macos-arm64`.
pub(crate) fn macos_entry(service: &str, channel: &str, version: &str) -> Target {
    let mut entry = repository_entry(service, channel);
    entry.installer = "macos-pkg".to_owned();
    entry.package_version = Some(version.to_owned());
    entry.bytes = 1000;
    let manifest = entry.manifest.as_mut().expect("a manifest");
    manifest.package = Some(format!("dev.{service}.server"));
    manifest.run = vec![service.to_owned()];
    entry
}

/// The targets of `service` with the one list `linux-x86_64`, holding `entries`.
pub(crate) fn targets_of(entries: Vec<Target>) -> BTreeMap<String, TargetList> {
    BTreeMap::from([("linux-x86_64".to_owned(), TargetList { entries })])
}

/// A private repository `name` at `uri` with `suites` (the groups it serves), the key of `seed`,
/// and the credential file at `credential_file`.
pub(crate) fn private(
    name: &str,
    uri: &str,
    suites: &[&str],
    seed: u8,
    credential_file: &str,
) -> Repository {
    Repository {
        access: registry::ACCESS_HOSTS.to_owned(),
        credential_file: credential_file.to_owned(),
        ..repository(name, uri, suites, seed)
    }
}

/// Writes `text` to the file `name` in `dir` with mode 0600 and answers its path.
pub(crate) fn credential_in(dir: &std::path::Path, name: &str, text: &str) -> String {
    use std::os::unix::fs::PermissionsExt as _;
    let path = dir.join(name);
    std::fs::write(&path, text).expect("a credential file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("mode");
    path.to_str().expect("a path").to_owned()
}
