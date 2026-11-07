//! The check that a file the operator names (a push certificate, its key, a DEP token or token
//! certificate) is outside every git work tree: keys and tokens are never kept in a repository.

use std::path::Path;

use hostkeeper_mdm_core::path::AbsPath;
use hostkeeper_mdm_step::Host;

use crate::MdmError;

/// Refuses `path` unless it is absolute and no directory above it, once every link on the way is
/// resolved, holds `.git`: a relative path or a link into a work tree cannot get past the check.
///
/// # Errors
/// [`MdmError::Path`] for a path that is not absolute; [`MdmError::FileInGitTree`] with the
/// `.git` found; the host's error.
pub(crate) fn file_outside_git(host: &dyn Host, path: &str) -> Result<(), MdmError> {
    AbsPath::new(path)?;
    let real = host.canonical(path)?;
    for above in Path::new(&real).ancestors().skip(1) {
        let root = above.to_str().unwrap_or_default().trim_end_matches('/');
        let git = format!("{root}/.git");
        if host.stat(&git)?.is_some() {
            return Err(MdmError::FileInGitTree {
                path: path.to_owned(),
                git,
            });
        }
    }
    Ok(())
}
