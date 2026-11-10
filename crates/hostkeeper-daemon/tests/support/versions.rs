//! Version directories as the repository installer leaves them, for the tests that need a service
//! installed without ordering it: `<packages-dir>/<service>/<version>/` with its `target.json`
//! (the entry it was staged for), a `tree/` and, for a package with a hook program, the hook at
//! its fixed path `tree/usr/lib/<package>/hostkeeper-hook`; sealed with [`Versions::seal`].
//!
//! The entry is built from the manifest text the daemon's test doubles have always carried
//! ([`hostkeeper_daemon::testing::manifest`]): its `min_hostkeeper`, hook bounds, `[units] run`,
//! `[health] url` and `[params]`, so that a test that wrote a manifest keeps writing it. A hook
//! program exists when the manifest has a `[hook] command`. The version is an `apt` one: its
//! installer is [`INSTALLER`].

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_proto::hostkeeper::release::v1::{
    Hook, InstallerBounds, Manifest, Param, StepBounds, Target, param,
};
use hostkeeper_steps::{Seal, tree_digest};

/// Where a package with a hook program installs it, relative to the tree: the program the
/// entry's `hook.command` names by the absolute path `/` followed by this.
pub fn hook_path(package: &str) -> String {
    format!("usr/lib/{package}/hostkeeper-hook")
}

/// The installer the versions are staged for.
pub const INSTALLER: &str = "apt";

/// Native version directories under a packages directory.
pub struct Versions {
    root: PathBuf,
}

/// `dir` and every directory above it that is not there yet, each mode 0755: the step runner
/// trusts no directory writable by group or others.
fn dir_0755(dir: &Path) {
    if let Some(parent) = dir.parent().filter(|parent| !parent.exists()) {
        dir_0755(parent);
    }
    if !dir.exists() {
        fs::create_dir(dir).expect("a directory");
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("its mode");
}

fn secs(table: &toml::Table, key: &str) -> u64 {
    table
        .get(key)
        .and_then(toml::Value::as_integer)
        .map_or(0, |n| u64::try_from(n).expect("a bound"))
}

/// The entry of `service` at the package version `version`, built from `manifest`, the TOML text
/// of a manifest. `hook` is whether the package has a hook program.
pub fn entry_of(service: &str, version: &str, manifest: &str) -> (Target, bool) {
    let table: toml::Table = manifest.parse().expect("a manifest of TOML");
    let hook = table.get("hook").and_then(toml::Value::as_table);
    let bounds = hook
        .and_then(|hook| hook.get("timeout_secs"))
        .and_then(toml::Value::as_table);
    let has_hook = hook
        .and_then(|hook| hook.get("command"))
        .and_then(toml::Value::as_array)
        .is_some_and(|command| !command.is_empty());
    let step_bounds = bounds.map(|bounds| StepBounds {
        prepare: secs(bounds, "prepare"),
        cutover: secs(bounds, "cutover"),
        verify: secs(bounds, "verify"),
        rollback: secs(bounds, "rollback"),
        cleanup: secs(bounds, "cleanup"),
        uninstall: bounds
            .contains_key("uninstall")
            .then(|| secs(bounds, "uninstall")),
    });
    let run = table
        .get("units")
        .and_then(toml::Value::as_table)
        .and_then(|units| units.get("run"))
        .and_then(toml::Value::as_array)
        .map(|run| {
            run.iter()
                .map(|unit| unit.as_str().expect("a unit").to_owned())
                .collect()
        })
        .unwrap_or_default();
    let params = table
        .get("params")
        .and_then(toml::Value::as_table)
        .map(|params| {
            params
                .iter()
                .map(|(key, value)| {
                    let value = match value {
                        toml::Value::String(text) => param::Value::Text(text.clone()),
                        toml::Value::Integer(n) => param::Value::Integer(*n),
                        toml::Value::Float(x) => param::Value::Float(*x),
                        toml::Value::Boolean(flag) => param::Value::Flag(*flag),
                        other => panic!("a param that is no scalar: {other:?}"),
                    };
                    (key.clone(), Param { value: Some(value) })
                })
                .collect()
        })
        .unwrap_or_default();
    let manifest = Manifest {
        package: Some(service.to_owned()),
        hook: Some(Hook {
            command: if has_hook {
                vec![format!("/{}", hook_path(service))]
            } else {
                Vec::new()
            },
            boot: hook
                .and_then(|hook| hook.get("boot"))
                .and_then(toml::Value::as_str)
                .map(str::to_owned),
            timeout_secs: step_bounds,
            verify_deadline_secs: hook.map_or(0, |hook| secs(hook, "verify_deadline_secs")),
            promotion_ceiling_secs: hook
                .and_then(|hook| hook.get("promotion_ceiling_secs"))
                .and_then(toml::Value::as_integer)
                .map(|n| u64::try_from(n).expect("a ceiling")),
            cleanup_retries: hook
                .and_then(|hook| hook.get("cleanup_retries"))
                .and_then(toml::Value::as_integer)
                .map(|n| u32::try_from(n).expect("retries")),
        }),
        run,
        health_url: table
            .get("health")
            .and_then(toml::Value::as_table)
            .and_then(|health| health.get("url"))
            .and_then(toml::Value::as_str)
            .map(str::to_owned),
        params,
        min_hostkeeper: table
            .get("min_hostkeeper")
            .and_then(toml::Value::as_str)
            .unwrap_or("0.1.0")
            .to_owned(),
        installer_bounds: Some(InstallerBounds {
            stage_secs: 60,
            activate_secs: 60,
            remove_secs: 60,
        }),
        ..Manifest::default()
    };
    let entry = Target {
        installer: INSTALLER.to_owned(),
        digest: crate::support::d(),
        manifest: Some(manifest),
        repository: Some("main".to_owned()),
        package_version: Some(format!("{version}-1")),
        ..Target::default()
    };
    (entry, has_hook)
}

impl Versions {
    /// Version directories under `packages_dir`.
    pub fn new(packages_dir: &Path) -> Arc<Versions> {
        Arc::new(Versions {
            root: packages_dir.to_path_buf(),
        })
    }

    /// The version directory of `service` at `version` for `entry`, with a hook program in its
    /// tree when `hook` is set.
    pub fn add_entry(&self, service: &str, version: &str, entry: &Target, hook: bool) -> PathBuf {
        let dir = self.root.join(service).join(version);
        for made in [
            &self.root,
            &self.root.join(service),
            &dir,
            &dir.join("tree"),
        ] {
            dir_0755(made);
        }
        if hook {
            let package = entry
                .manifest
                .as_ref()
                .and_then(|manifest| manifest.package.as_deref())
                .unwrap_or(service);
            let program = dir.join("tree").join(hook_path(package));
            dir_0755(program.parent().expect("a parent"));
            fs::write(&program, "#!hook\n").expect("the hook");
            fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).expect("its mode");
        }
        hostkeeper_daemon::target::write(&dir, entry).expect("target.json");
        dir
    }

    /// The version directory of `service` at `version` whose entry is built from `manifest`, the
    /// TOML text of a manifest ([`entry_of`]).
    pub fn add(&self, service: &str, version: &str, manifest: &str) -> PathBuf {
        let (entry, hook) = entry_of(service, version, manifest);
        self.add_entry(service, version, &entry, hook)
    }

    /// Writes the seal of `service` at `version` (`<version>.seal` beside it): the tree digest of
    /// its directory as it is now, and `package_digest`.
    pub fn seal(&self, service: &str, version: &str, package_digest: &str) {
        let dir = self.root.join(service);
        let tree = tree_digest(&dir.join(version)).expect("the tree digest");
        let seal = Seal::new(&tree, package_digest).expect("a seal");
        fs::write(dir.join(format!("{version}.seal")), seal.to_json()).expect("the seal");
    }

    /// Points the link `link` (`current`, `previous`) of `service` at `version`.
    pub fn link(&self, service: &str, link: &str, version: &str) {
        let path = self.root.join(service).join(link);
        drop(fs::remove_file(&path));
        std::os::unix::fs::symlink(version, &path).expect("the link");
    }
}
