//! What the flags of the package proxy make at start.

use hostkeeper_oidc::Roots;

use super::{ProxyFlags, ProxyParts};
use crate::config::StartError;
use crate::mdm_setup::MdmSetup;
use crate::repository_origin_support::FakeOrigin;
use crate::testutil::scratch;

/// Proves: with the flag the start makes the cache and the reader of origins and the coordinator
/// holds both; without it it makes neither, and asks for no roots; a start with the flag and no
/// root certificate that can check an origin is refused.
///
/// Catches: a cache that pulls pkgs over a TLS client that trusts nothing, and a reader made for a
/// farm that does not proxy.
#[test]
fn the_flag_makes_the_cache_and_the_reader_and_the_reader_needs_roots() {
    let dir = scratch("pp3-config-proxy");
    let setup = MdmSetup::volatile();
    let roots = FakeOrigin::new().roots();
    let flags = ProxyFlags {
        proxy_cache_dir: Some(dir.path().join("cache")),
    };
    let coordinator = crate::host_groups_support::coordinator_listed(
        &crate::store::Fake::new(),
        &std::sync::Arc::new(std::sync::atomic::AtomicU64::new(1_000)),
        None,
        "",
    );
    let both = ProxyParts::open(&flags, &setup, &roots).expect("a start");
    let coordinator = both.attach(coordinator);
    assert!(coordinator.proxy_cache().is_some());
    assert!(coordinator.pkg_origin().is_some());
    let neither = ProxyParts::open(
        &ProxyFlags {
            proxy_cache_dir: None,
        },
        &setup,
        &Roots::Given(Vec::new()),
    )
    .expect("a start with no cache needs no roots");
    let coordinator = neither.attach(coordinator);
    assert!(coordinator.proxy_cache().is_none());
    assert!(coordinator.pkg_origin().is_none());
    let Err(why) = ProxyParts::open(&flags, &setup, &Roots::Given(Vec::new())) else {
        panic!("the start is refused for the roots");
    };
    assert!(matches!(why, StartError::OriginTls(_)), "{why}");
}
