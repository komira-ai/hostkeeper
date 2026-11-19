//! The console's three files (`docs/design/console-broker.md`, decisions D5, D8 and D9), read at
//! start and again on the tick that looks at the TLS files (`--tls-reload-secs`,
//! [`crate::listen::reload_every`]).
//!
//! The operators file grants access, and needs the BMC credential file beside it (the flags say
//! so): with no operators file, nobody has console access and the other two are not read. The
//! targets file is optional: without it no address is configured, and every BMC is reached at the
//! address its host's daemon reports ([`super::address`]). A file that breaks a rule refuses the start; on a later tick it is logged
//! as a warning, naming its flag, and the one read before stays in use. A token revoked from the
//! tokens file loses console access with it, since every call is made by a valid token's name.

use std::path::PathBuf;
use std::sync::{Arc, PoisonError, RwLock};

use hostkeeper_console::credential::{BmcCredential, CredentialError};
use hostkeeper_console::operators::{Denied, Need, Operators, OperatorsError};
use hostkeeper_console::targets::{Targets, TargetsError};

/// Why a console file is refused. The text is the file's own error, which names its flag and
/// never its path or, for the credential, any of its text.
#[derive(Debug, thiserror::Error)]
pub enum ConsoleFilesError {
    /// `--console-operators-file`.
    #[error(transparent)]
    Operators(#[from] OperatorsError),
    /// `--console-targets-file`.
    #[error(transparent)]
    Targets(#[from] TargetsError),
    /// `--console-bmc-credential-file`.
    #[error(transparent)]
    Credential(#[from] CredentialError),
}

/// Where the three files are: given together, or not at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// `--console-operators-file`.
    pub operators: PathBuf,
    /// `--console-targets-file`, if given.
    pub targets: Option<PathBuf>,
    /// `--console-bmc-credential-file`.
    pub credential: PathBuf,
}

/// What a call may use once its operator is granted access: the targets and the credential as
/// read last.
pub(crate) struct Grant {
    /// The address of each device the targets file names, by MAC; none without the file.
    pub(crate) targets: Arc<Targets>,
    /// The one BMC credential.
    pub(crate) credential: Arc<BmcCredential>,
}

/// The files as read last.
struct Loaded {
    operators: Operators,
    targets: Arc<Targets>,
    credential: Arc<BmcCredential>,
}

/// The console's files, shared by the calls and the reload tick. The default has none: nobody
/// has console access.
#[derive(Clone, Default)]
pub struct ConsoleFiles {
    given: Option<Arc<(Paths, RwLock<Loaded>)>>,
}

impl std::fmt::Debug for ConsoleFiles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsoleFiles")
            .field("paths", &self.given.as_ref().map(|given| &given.0))
            .finish_non_exhaustive()
    }
}

impl ConsoleFiles {
    /// The files at `paths`, each read now.
    ///
    /// # Errors
    /// [`ConsoleFilesError`]: a file is refused.
    pub fn read(paths: Paths) -> Result<Self, ConsoleFilesError> {
        let loaded = Loaded {
            targets: Arc::new(read_targets(paths.targets.as_deref())?),
            credential: Arc::new(BmcCredential::read(&paths.credential)?),
            operators: Operators::read(&paths.operators)?,
        };
        Ok(Self {
            given: Some(Arc::new((paths, RwLock::new(loaded)))),
        })
    }

    /// Reads the files again. Each one read and accepted replaces the one in use; a refused one
    /// is logged and the one in use stays.
    pub fn reload(&self) {
        let Some(given) = &self.given else {
            return;
        };
        let (paths, loaded) = &**given;
        let mut loaded = loaded.write().unwrap_or_else(PoisonError::into_inner);
        match Operators::read(&paths.operators) {
            Ok(operators) if operators != loaded.operators => {
                loaded.operators = operators;
                tracing::info!("the console operators file changed: its operators are in use");
            }
            Ok(_) => {}
            Err(why) => tracing::warn!(%why, "the console operators file was not read again"),
        }
        match read_targets(paths.targets.as_deref()) {
            Ok(targets) if targets != *loaded.targets => {
                loaded.targets = Arc::new(targets);
                tracing::info!("the console targets file changed: its targets are in use");
            }
            Ok(_) => {}
            Err(why) => tracing::warn!(%why, "the console targets file was not read again"),
        }
        match BmcCredential::read(&paths.credential) {
            Ok(credential) => loaded.credential = Arc::new(credential),
            Err(why) => tracing::warn!(%why, "the BMC credential file was not read again"),
        }
    }

    /// The targets and the credential as read last, whoever asks: for the coordinator's own
    /// clean-up after a stream, which runs even when its operator lost access. `None` with no
    /// files.
    pub(crate) fn devices(&self) -> Option<Grant> {
        let given = self.given.as_ref()?;
        let loaded = given.1.read().unwrap_or_else(PoisonError::into_inner);
        Some(Grant {
            targets: Arc::clone(&loaded.targets),
            credential: Arc::clone(&loaded.credential),
        })
    }

    /// Whether the files are given: without them nobody has console access.
    pub(crate) fn given(&self) -> bool {
        self.given.is_some()
    }

    /// What the operator `name` may use for a call that needs `need`.
    ///
    /// # Errors
    /// [`Denied`]: no operators file, the name is not in it, or the call needs power and its
    /// entry does not allow it.
    pub(crate) fn grant(&self, name: &str, need: Need) -> Result<Grant, Denied> {
        let given = self.given.as_ref().ok_or(Denied::NoConsoleAccess)?;
        let loaded = given.1.read().unwrap_or_else(PoisonError::into_inner);
        loaded.operators.check(name, need)?;
        Ok(Grant {
            targets: Arc::clone(&loaded.targets),
            credential: Arc::clone(&loaded.credential),
        })
    }
}

/// The targets of the file at `path`; none without one.
fn read_targets(path: Option<&std::path::Path>) -> Result<Targets, TargetsError> {
    path.map_or_else(|| Ok(Targets::default()), Targets::read)
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
