//! The hello package's manifest and units: one [`Description`] of the service, rendered for the
//! kind of unit a platform runs, so no platform has code of its own. The Linux platforms differ
//! only in the package's file name; macOS is the launchd kind.
//!
//! - The systemd kind (Linux): the one template `units/hello@.service`, started as the instances
//!   `hello@a.service` and `hello@b.service` of the two slots. A slot's unit has
//!   `ConditionPathExists=<record dir>/slots/%i`, so a slot whose marker the update hook has not
//!   written never starts, at boot or by `Restart=`.
//! - The launchd kind (macOS): a plist per slot, `units/hello.a.plist` and `units/hello.b.plist`,
//!   rendered by `hostkeeper_launchd::service_plist`: the job runs while the slot's marker file
//!   exists.
//!
//! A package holds one kind of unit, and the manifest's `[params]` says which (`units`), with the
//! port the slots serve on and the hook's record directory, for the hook to read. The text is
//! committed as goldens under `examples/hello/package/` and compared by this module's tests.

use std::ffi::OsString;
use std::path::PathBuf;

use hostkeeper_launchd::{Label, ServiceJob, service_plist};

use super::{Contents, Unit, binary_path};
use crate::release::Error;
use crate::release::contract::Platform;

/// The oldest hostkeeper that can run the package: the daemon of the first release.
const MIN_HOSTKEEPER: &str = "0.1.0";

/// Everything the package says about the service, in one place.
pub(super) struct Description {
    /// The service's name: its package, binary, units and labels.
    name: &'static str,
    /// The port the serving slot listens on (one slot serves at a time, so the port is shared).
    port: u16,
    /// The two slots an update alternates between.
    slots: [&'static str; 2],
    /// The hook's record directory; a slot runs while `<record_dir>/slots/<slot>` exists.
    record_dir: &'static str,
    /// Where a launchd job's output goes (launchd does not create it).
    log_dir: &'static str,
}

/// The hello service.
pub(super) const HELLO: Description = Description {
    name: "hello",
    port: 9310,
    slots: ["a", "b"],
    record_dir: "/var/lib/updater/hooks/hello",
    log_dir: "/var/log",
};

/// What a platform runs its units as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Systemd,
    Launchd,
}

impl Kind {
    fn of(platform: Platform) -> Kind {
        match platform {
            Platform::LinuxX86_64 | Platform::LinuxArm64 => Kind::Systemd,
            Platform::MacosArm64 => Kind::Launchd,
        }
    }

    /// The value of the manifest's `units` param: what the hook starts and stops slots with.
    fn word(self) -> &'static str {
        match self {
            Kind::Systemd => "systemd",
            Kind::Launchd => "launchd",
        }
    }
}

/// The manifest and units of the hello package of `version` (the text of the version the
/// manifest names, which the failing build has a suffix on) for `platform`; `fail_ready` starts
/// the service with `--fail-ready`.
pub(super) fn contents(
    platform: Platform,
    version: &str,
    fail_ready: bool,
) -> Result<Contents, Error> {
    HELLO.contents(Kind::of(platform), platform, version, fail_ready)
}

impl Description {
    /// The service's arguments: its port, and `--fail-ready` when asked.
    fn args(&self, fail_ready: bool) -> Vec<String> {
        let mut args = vec!["--port".to_owned(), self.port.to_string()];
        if fail_ready {
            args.push("--fail-ready".to_owned());
        }
        args
    }

    /// The units the manifest's `[units] run` names, one per slot.
    fn run(&self, kind: Kind) -> Vec<String> {
        self.slots
            .iter()
            .map(|slot| match kind {
                Kind::Systemd => format!("{}@{slot}.service", self.name),
                Kind::Launchd => format!("{}.{slot}", self.name),
            })
            .collect()
    }

    fn contents(
        &self,
        kind: Kind,
        platform: Platform,
        version: &str,
        fail_ready: bool,
    ) -> Result<Contents, Error> {
        let units = match kind {
            Kind::Systemd => vec![Unit {
                file: format!("{}@.service", self.name),
                origin: format!("units/{}@.service", self.name),
                data: self.service_unit(fail_ready).into_bytes(),
            }],
            Kind::Launchd => self
                .slots
                .iter()
                .map(|slot| {
                    let file = format!("{}.{slot}.plist", self.name);
                    Ok(Unit {
                        data: self.plist(slot, fail_ready, &file)?.into_bytes(),
                        origin: format!("units/{file}"),
                        file,
                    })
                })
                .collect::<Result<Vec<_>, Error>>()?,
        };
        Ok(Contents {
            manifest: self.manifest(kind, platform, version),
            units,
            trusted_root: None,
        })
    }

    /// `manifest.toml`: the platform, the hook, the units, the health URL and the params the hook
    /// reads.
    fn manifest(&self, kind: Kind, platform: Platform, version: &str) -> String {
        let platform = platform.name();
        let name = self.name;
        let port = self.port;
        let run = self
            .run(kind)
            .iter()
            .map(|unit| format!("\"{unit}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let units = kind.word();
        let record_dir = self.record_dir;
        format!(
            "schema = 1\n\
             name = \"{name}\"\n\
             version = \"{version}\"\n\
             platform = \"{platform}\"\n\
             min_hostkeeper = \"{MIN_HOSTKEEPER}\"\n\
             \n\
             [hook]\n\
             command = [\"bin/{name}\", \"update-hook\"]\n\
             verify_deadline_secs = 120\n\
             \n\
             [hook.timeout_secs]\n\
             prepare = 30\n\
             cutover = 60\n\
             verify = 30\n\
             rollback = 60\n\
             cleanup = 30\n\
             uninstall = 60\n\
             \n\
             [units]\n\
             run = [{run}]\n\
             \n\
             [health]\n\
             url = \"http://127.0.0.1:{port}/readyz\"\n\
             \n\
             [params]\n\
             units = \"{units}\"\n\
             port = {port}\n\
             record_dir = \"{record_dir}\"\n"
        )
    }

    /// `units/hello@.service`, the template of both slots.
    fn service_unit(&self, fail_ready: bool) -> String {
        let name = self.name;
        let record_dir = self.record_dir;
        let program = binary_path(name);
        let args = self.args(fail_ready).join(" ");
        format!(
            "[Unit]\n\
             Description={name}, the example service of hostkeeper (slot %i)\n\
             ConditionPathExists={record_dir}/slots/%i\n\
             \n\
             [Service]\n\
             ExecStart={program} {args}\n\
             DynamicUser=yes\n\
             Restart=on-failure\n\
             \n\
             [Install]\n\
             WantedBy=multi-user.target\n"
        )
    }

    /// `units/hello.<slot>.plist`, the job of one slot; `file` names it in an error.
    fn plist(&self, slot: &str, fail_ready: bool, file: &str) -> Result<String, Error> {
        let name = self.name;
        let failed = |why: String| Error::Failed(format!("units/{file}: {why}"));
        let label = Label::parse(&format!("{name}.{slot}")).map_err(|e| failed(e.to_string()))?;
        let job = ServiceJob {
            label,
            program: PathBuf::from(binary_path(name)),
            args: self
                .args(fail_ready)
                .into_iter()
                .map(OsString::from)
                .collect(),
            run_while: PathBuf::from(format!("{}/slots/{slot}", self.record_dir)),
            stdout: PathBuf::from(format!("{}/{name}.{slot}.out.log", self.log_dir)),
            stderr: PathBuf::from(format!("{}/{name}.{slot}.err.log", self.log_dir)),
        };
        service_plist(&job).map_err(|e| failed(e.to_string()))
    }
}

#[cfg(test)]
mod tests;
