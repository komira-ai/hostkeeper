//! The repository installer's fake of apt, dpkg and dpkg-deb for the shared rigs of `support`
//! (the self-update rig, [`super::selfupdate::SelfRig`]): the fake `hostkeeper-install`'s tests
//! have (one copy for both crates' tests), loaded here once for every test file, which
//! `support_apt` re-exports, answering every tool run of the daemon's step launcher through the
//! launcher's own call ([`bridge`]).

#[path = "../../../hostkeeper-install/tests/support/apt_fake.rs"]
pub mod apt_fake;

use std::fs;
use std::path::Path;
use std::sync::Arc;

use hostkeeper_daemon::testing::ToolAnswer;
use hostkeeper_install::repository::config::{Repositories, parse};
use hostkeeper_install::repository::{Network, Run, Tool, Tools};
use hostkeeper_proto::hostkeeper::agent::v1::{Order, order::Verb};
use hostkeeper_proto::hostkeeper::release::v1::Target;
use hostkeeper_steps::{LaunchError, ToolCall, ToolExit, ToolNetwork};

pub use apt_fake::{Deb, Events, FakeHost};

/// The repository the host's entry names.
pub const REPO: &str = "komira-stable";
/// Its URI: a directory of the host.
pub const URI: &str = "file:/srv/hostkeeper-apt";

/// The launcher's answer to each tool run: the run as the tools port gave it, answered by `host`.
pub fn bridge(host: Arc<FakeHost>) -> ToolAnswer {
    Arc::new(move |call: &ToolCall<'_>| {
        let tool = Tool::ALL
            .into_iter()
            .find(|tool| call.program == Path::new(tool.path()))
            .ok_or_else(|| LaunchError::NoStart(format!("no tool {}", call.program.display())))?;
        let args = call
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let run = Run {
            tool,
            args,
            network: match call.network {
                ToolNetwork::Offline => Network::Offline,
                ToolNetwork::Fetch => Network::Fetch,
            },
            writable: call
                .writable
                .map(|paths| paths.iter().map(|p| p.to_path_buf()).collect()),
            working_dir: call.working_dir.map(|dir| dir.to_path_buf()),
            env: call
                .env
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            bound_secs: call.timeout_secs,
        };
        host.run(&run)
            .map(|output| ToolExit {
                code: output.code,
                stdout: output.stdout,
                stderr: output.stderr,
                cut: output.cut,
            })
            .map_err(|error| LaunchError::Launcher(error.0))
    })
}

/// The host's repository entries: `main`, at [`URI`], its key in `keys`.
pub fn repositories(keys: &Path) -> Repositories {
    fs::create_dir_all(keys).expect("the keys directory");
    let text = format!(
        "Types: deb\nURIs: {URI}\nSuites: stable\nComponents: main\nSigned-By: {}\n",
        keys.join("main.gpg").display()
    );
    Repositories::new(vec![parse(REPO, &text, keys).expect("a repository entry")])
}

/// `order` with `entry` as its target.
pub fn with_entry(mut order: Order, entry: &Target) -> Order {
    match order.verb.as_mut() {
        Some(Verb::Install(a)) => a.target = Some(entry.clone()),
        Some(Verb::Update(a)) => a.target = Some(entry.clone()),
        Some(Verb::ForceRollback(a)) => a.target = Some(entry.clone()),
        Some(Verb::Force(a)) => a.target = Some(entry.clone()),
        other => panic!("no target to set on {other:?}"),
    }
    order
}

/// Makes every directory under `dir` writable by its owner: a staged version directory is sealed
/// read-only, which root writes through on a host and the test's own user cannot.
pub fn open_up(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = fs::symlink_metadata(dir) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).expect("its mode");
    for entry in fs::read_dir(dir).expect("a directory").flatten() {
        open_up(&entry.path());
    }
}
