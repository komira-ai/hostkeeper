//! `serve`'s start over `--proxy-cache-dir` (`docs/design/package-proxy.md`, section 4): the
//! directory is made, what a crash left in it is deleted, the coordinator holds the cache, and no
//! flag means no cache and no directory.

mod support;

use std::ffi::OsString;

use hostkeeper_coordinator::config::StartError;
use support::serve::{Files, build_given, flags_of, refusal};

fn with_cache(files: &Files, name: &str) -> Vec<OsString> {
    let mut args = files.args();
    args.extend([
        OsString::from("--proxy-cache-dir"),
        files.path(name).into_os_string(),
    ]);
    args
}

/// Proves: with no flag the coordinator has no cache and makes no directory.
///
/// Catches: a coordinator that writes a cache directory for a farm that does not proxy packages.
#[test]
fn serve_without_the_flag_has_no_cache() {
    let files = Files::new("start-proxy-cache-none");
    let parts = build_given(&flags_of(files.args())).expect("a start");
    assert!(parts.coordinator.proxy_cache().is_none());
}

/// Proves: with the flag the start makes `objects/` and `tmp/`, deletes a partial fill a crash
/// left in `tmp/`, and the coordinator holds the cache.
///
/// Catches: a flag that is accepted and ignored, or a partial file that survives a restart.
#[test]
fn serve_makes_the_cache_directory_and_clears_what_a_crash_left() {
    let files = Files::new("start-proxy-cache-made");
    let args = with_cache(&files, "cache");
    let parts = build_given(&flags_of(args.clone())).expect("a start");
    assert!(parts.coordinator.proxy_cache().is_some());
    drop(parts);
    let cache = files.path("cache");
    assert!(cache.join("objects").is_dir());
    std::fs::write(cache.join("tmp").join("partial.0"), b"half").expect("a partial fill");
    let parts = build_given(&flags_of(args)).expect("a start");
    assert!(parts.coordinator.proxy_cache().is_some());
    assert_eq!(
        std::fs::read_dir(cache.join("tmp")).expect("tmp").count(),
        0
    );
}

/// Proves: a cache directory that cannot be made refuses the start with a text that names the
/// flag.
///
/// Catches: a start that goes on and fails every download later.
#[test]
fn serve_refuses_a_cache_directory_that_cannot_be_made() {
    let files = Files::new("start-proxy-cache-bad");
    files.put("a-file", "x", 0o600);
    let mut args = files.args();
    args.extend([
        OsString::from("--proxy-cache-dir"),
        files.path("a-file").join("cache").into_os_string(),
    ]);
    let StartError::ProxyCache { path, .. } = refusal(args) else {
        panic!("the start is refused for the cache directory");
    };
    assert_eq!(path, files.path("a-file").join("cache"));
}
