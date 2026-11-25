//! The Remote Execution API as `mac-farm` speaks it: the messages, clients and servers that
//! `build.rs` generates from the vendored protos of `proto/third_party`, each file pinned by
//! SHA-256 in its `PINS` (the test below holds the files to their lines).
//!
//! The modules mirror the proto packages, because generated code names other packages by
//! relative paths (`super::super::...`); [`v2`] is the REAPI package itself.

#[allow(
    clippy::all,
    dead_code,
    reason = "generated code follows the protos' shapes and comments, and holds every message and \
              service of the protos, of which mac-farm uses a few"
)]
pub(crate) mod build {
    pub mod bazel {
        pub mod remote {
            pub mod execution {
                pub mod v2 {
                    tonic::include_proto!("build.bazel.remote.execution.v2");
                }
            }
        }
        pub mod semver {
            tonic::include_proto!("build.bazel.semver");
        }
    }
}

#[allow(
    clippy::all,
    dead_code,
    reason = "generated code follows the protos' shapes and comments, and holds every message and \
              service of the protos, of which mac-farm uses a few"
)]
pub(crate) mod google {
    pub mod api {
        tonic::include_proto!("google.api");
    }
    pub mod bytestream {
        tonic::include_proto!("google.bytestream");
    }
    pub mod longrunning {
        tonic::include_proto!("google.longrunning");
    }
    pub mod rpc {
        tonic::include_proto!("google.rpc");
    }
}

pub(crate) use build::bazel::remote::execution::v2;

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use crate::install::Sha256;

    /// The files of `dir`, below it, `/`-separated.
    fn files(dir: &Path, at: &str, out: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).expect("list") {
            let entry = entry.expect("an entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if at.is_empty() {
                name
            } else {
                format!("{at}/{name}")
            };
            if entry.file_type().expect("a type").is_dir() {
                files(&entry.path(), &path, out);
            } else {
                out.insert(path);
            }
        }
    }

    /// Catches a vendored proto changed by hand (its bytes no longer the upstream commit's its
    /// line names), a file added without a line, and a line whose file is gone.
    #[test]
    fn the_vendored_protos_are_their_pins() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("proto/third_party");
        let pins = std::fs::read_to_string(root.join("PINS")).expect("PINS");
        let mut listed = BTreeSet::from(["PINS".to_owned()]);
        for line in pins.lines().filter(|l| !l.starts_with('#')) {
            let words: Vec<&str> = line.split(' ').collect();
            assert_eq!(
                words.len(),
                5,
                "`<dir> <repo> <commit> <sha256> <path>`: {line}"
            );
            let (dir, repo, commit, sha256, path) =
                (words[0], words[1], words[2], words[3], words[4]);
            assert!(repo.starts_with("https://github.com/"), "{line}");
            assert_eq!(commit.len(), 40, "{line}");
            let file = format!("{dir}/{path}");
            let bytes = std::fs::read(root.join(&file)).expect("a pinned file");
            assert_eq!(Sha256::of(&bytes).to_string(), sha256, "{file}");
            listed.insert(file);
        }
        let mut present = BTreeSet::new();
        files(&root, "", &mut present);
        assert_eq!(present, listed);
    }
}
