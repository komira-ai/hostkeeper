//! The real machine, the kernel's random source, the wall clock, the pause and the program's
//! entry.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use hostkeeper_mdm_step::{Host, RealHost, Trustee};

use crate::{Grant, HttpLoopback, Machine, Net, System, run};

/// The machine itself: a [`RealHost`] with the grants a verb asks for.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealMachine;

impl Machine for RealMachine {
    fn host(&self, grants: &[Grant]) -> Box<dyn Host + '_> {
        Box::new(granted(RealHost::new(), grants))
    }

    fn owner(&self, path: &str) -> io::Result<Option<u32>> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) => Ok(Some(meta.uid())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// `host`, also trusting each grant's owner at and below its directory: by id, or by name.
fn granted(host: RealHost, grants: &[Grant]) -> RealHost {
    grants.iter().fold(host, |host, g| match &g.who {
        Trustee::Uid(uid) => host.trust_below(&g.dir, *uid),
        Trustee::User(user) => host.trust_below_user(&g.dir, user),
    })
}

/// The kernel's random source.
///
/// # Errors
/// The source could not be read.
pub fn system_random(buf: &mut [u8]) -> io::Result<()> {
    getrandom::fill(buf).map_err(io::Error::from)
}

/// Calls `f` with the machine itself: the real host, the network, the services' APIs on
/// loopback, the kernel's random source and the wall clock, read once (it names an uninstall
/// snapshot, `MDM_STATE/removed/<unix seconds>`, and `push-cert` refuses a push certificate
/// that has expired by it).
pub(crate) fn with_machine<T>(f: impl FnOnce(&System<'_>) -> T) -> T {
    let net = Net::new(true);
    let api = HttpLoopback::new();
    let secs = SystemTime::now().duration_since(UNIX_EPOCH);
    let system = System {
        machine: &RealMachine,
        net: &net,
        api: &api,
        random: &system_random,
        now: secs.map_or(0, |d| d.as_secs()),
        pause: &std::thread::sleep,
    };
    f(&system)
}

/// Runs the program: reads the arguments, acts on the machine, exits as documented.
pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    with_machine(|system| {
        ExitCode::from(run(
            &args,
            system,
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        ))
    })
}

#[cfg(test)]
mod tests {
    use hostkeeper_mdm_step::{Host, RealHost, Trustee};

    use super::granted;
    use crate::Grant;

    /// Catches the machine dropping a grant by name or making it a grant by id (the new
    /// service user's state would be refused again, as on the live move): the user of a grant
    /// by name is looked up on the walk to a path below its directory, and nowhere else, and a
    /// grant by id is trusted below its own directory. The lookup program is a path that does
    /// not exist, so the lookup fails to start, naming its command, and no account database
    /// is read.
    #[test]
    fn a_grant_by_name_is_looked_up_below_its_directory_and_one_by_id_is_trusted() {
        let root = std::env::current_exe()
            .expect("the test binary")
            .parent()
            .expect("its directory")
            .to_path_buf();
        let missing = root.join("no-such-lookup-program");
        let missing = missing.to_str().expect("a UTF-8 path");
        let grants = [
            Grant {
                dir: "/svc".to_owned(),
                who: Trustee::User("svc".to_owned()),
            },
            Grant {
                dir: "/op".to_owned(),
                who: Trustee::Uid(7),
            },
        ];
        let host = granted(
            RealHost::trusting(&root, &[]).looking_up_users_with(missing),
            &grants,
        );
        let err = host.way("/svc/f").unwrap_err().to_string();
        assert!(err.starts_with(&format!("{missing} passwd svc: ")), "{err}");
        let op = host
            .way("/op/f")
            .unwrap()
            .expect("the real host judges owners");
        assert_eq!(op.trusted, [Trustee::Uid(7)]);
        let other = host
            .way("/other/f")
            .unwrap()
            .expect("the real host judges owners");
        assert_eq!(other.trusted, []);
    }
}
