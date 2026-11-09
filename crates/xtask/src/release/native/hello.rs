//! What hello's native packages hold. The service has two slots, `a` and `b`, one at a time on
//! port 9310, each running only while the hook's marker `/var/lib/updater/hooks/hello/slots/<slot>` exists; the failing build
//! starts the program with `--fail-ready`. What differs is where the files are:
//!
//! - **The `.deb`** installs the program at `/usr/lib/hello/hello` (a program no one types), the
//!   hook beside it, and one unit per slot, `hello@a.service` and `hello@b.service`, each a plain
//!   file in `/usr/lib/systemd/system/` (the run units are files of the package, not instances
//!   of a template the package would install without running it).
//! - **The `.pkg`** installs the program at `/Library/Application Support/hello/bin/hello`, the
//!   hook at `/Library/Application Support/hello/hostkeeper-hook`, and one launchd job per slot,
//!   `/Library/LaunchDaemons/hello.a.plist` and `hello.b.plist`, rendered by
//!   `hostkeeper_launchd::service_plist` with `Disabled` true added, so launchd loads neither at
//!   boot until hostkeeper enables it.
//!
//! The program paths are the ones hello's hook looks for in a staged tree
//! (`hello::hook::DEB_PROGRAM`, `PKG_PROGRAM`; this module's tests hold them equal).

use std::ffi::OsString;
use std::path::PathBuf;

use hostkeeper_launchd::{Label, ServiceJob, service_plist};

use super::{FILE_MODE, HOOK, PROGRAM_MODE, SYSTEMD_UNITS, hook_wrapper};
use crate::release::Error;

/// The service's name: its package, its program, its units and its jobs.
pub(crate) const NAME: &str = "hello";

/// The port the serving slot listens on.
pub(crate) const PORT: u16 = 9310;

/// The two slots an update alternates between.
pub(crate) const SLOTS: [&str; 2] = ["a", "b"];

/// The hook's record directory; a slot runs while `<RECORD_DIR>/slots/<slot>` exists.
pub(crate) const RECORD_DIR: &str = "/var/lib/updater/hooks/hello";

/// Where a launchd job's output goes.
const LOG_DIR: &str = "/var/log";

/// Where the `.deb` installs the program.
pub(crate) const DEB_PROGRAM: &str = "/usr/lib/hello/hello";

/// Where the `.pkg` installs the service's directory, the program and the hook.
pub(crate) const PKG_DIR: &str = "/Library/Application Support/hello";
pub(crate) const PKG_PROGRAM: &str = "/Library/Application Support/hello/bin/hello";

/// Where the `.pkg` installs its jobs' plists.
pub(crate) const LAUNCH_DAEMONS: &str = "/Library/LaunchDaemons";

/// One file of a native package: its absolute installed path, its mode and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct File {
    pub(crate) path: String,
    pub(crate) mode: u32,
    pub(crate) data: Vec<u8>,
}

impl File {
    fn new(path: &str, mode: u32, data: impl Into<Vec<u8>>) -> File {
        File {
            path: path.to_owned(),
            mode,
            data: data.into(),
        }
    }
}

/// The program's arguments: its port, and `--fail-ready` for the failing build.
fn args(fail_ready: bool) -> Vec<String> {
    let mut args = vec!["--port".to_owned(), PORT.to_string()];
    if fail_ready {
        args.push("--fail-ready".to_owned());
    }
    args
}

/// The unit of `slot` in the `.deb`: the slot's marker as its condition, the installed program
/// as its command.
pub(crate) fn unit(slot: &str, fail_ready: bool) -> String {
    let args = args(fail_ready).join(" ");
    format!(
        "[Unit]\n\
         Description={NAME}, the example service of hostkeeper (slot {slot})\n\
         ConditionPathExists={RECORD_DIR}/slots/{slot}\n\
         \n\
         [Service]\n\
         ExecStart={DEB_PROGRAM} {args}\n\
         DynamicUser=yes\n\
         Restart=on-failure\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    )
}

/// The files of hello's `.deb` with the program `binary`, in path order.
pub(crate) fn deb_files(binary: Vec<u8>, fail_ready: bool) -> Vec<File> {
    let mut files = vec![
        File::new(DEB_PROGRAM, PROGRAM_MODE, binary),
        File::new(
            &format!("/usr/lib/{NAME}/{HOOK}"),
            PROGRAM_MODE,
            hook_wrapper(NAME),
        ),
    ];
    files.extend(SLOTS.map(|slot| {
        File::new(
            &format!("{SYSTEMD_UNITS}/{NAME}@{slot}.service"),
            FILE_MODE,
            unit(slot, fail_ready),
        )
    }));
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// The key and value the `.pkg`'s plists carry beside what `service_plist` writes: launchd
/// leaves a job whose plist says `Disabled` unloaded at boot until an override enables it.
const DISABLED: &str = "\t<key>Disabled</key>\n\t<true/>\n";

/// The plist of the job of `slot` in the `.pkg`: `service_plist`'s text with [`DISABLED`] as the
/// first entry of its dictionary.
pub(crate) fn plist(slot: &str, fail_ready: bool) -> Result<String, Error> {
    let unwritable = Error::Failed(format!(
        "{LAUNCH_DAEMONS}/{NAME}.{slot}.plist: {NAME}.{slot} is not a launchd label, or its job \
         is not plist text"
    ));
    let job = |label| ServiceJob {
        label,
        program: PathBuf::from(PKG_PROGRAM),
        args: args(fail_ready).into_iter().map(OsString::from).collect(),
        run_while: PathBuf::from(format!("{RECORD_DIR}/slots/{slot}")),
        stdout: PathBuf::from(format!("{LOG_DIR}/{NAME}.{slot}.out.log")),
        stderr: PathBuf::from(format!("{LOG_DIR}/{NAME}.{slot}.err.log")),
    };
    let text = Label::parse(&format!("{NAME}.{slot}"))
        .ok()
        .and_then(|label| service_plist(&job(label)).ok())
        .ok_or(unwritable)?;
    let open = "<dict>\n";
    Ok(text.replacen(open, &format!("{open}{DISABLED}"), 1))
}

/// The files of hello's `.pkg` with the program `binary`, in path order.
pub(crate) fn pkg_files(binary: Vec<u8>, fail_ready: bool) -> Result<Vec<File>, Error> {
    let mut files = vec![
        File::new(PKG_PROGRAM, PROGRAM_MODE, binary),
        File::new(
            &format!("{PKG_DIR}/{HOOK}"),
            PROGRAM_MODE,
            hook_wrapper(&format!("bin/{NAME}")),
        ),
    ];
    for slot in SLOTS {
        files.push(File::new(
            &format!("{LAUNCH_DAEMONS}/{NAME}.{slot}.plist"),
            FILE_MODE,
            plist(slot, fail_ready)?,
        ));
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

#[cfg(test)]
mod tests;
